//! File-system-independent language intelligence for `@crab-dev/css`.

mod facts;
mod virtual_document;

use cssparser::{Parser, ParserInput};
use wake_common::{Diagnostic, Interner, Severity, SourceFile, Span};
use wake_css_in_js::value::Scope;
use wake_css_in_js::{CssTemplateKind, discover_css_templates, transform};
use wake_ecma_ast::SourceType;
use wake_ecma_parser::{ParseOutput, parse};

pub use facts::{CssFacts, PropertyFact, css_facts};
pub use virtual_document::{SourceSegment, VirtualCssDocument};

pub const LANGUAGE_PROTOCOL_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostLanguage {
    JavaScript,
    JavaScriptReact,
    TypeScript,
    TypeScriptReact,
}

impl HostLanguage {
    fn source_type(self) -> SourceType {
        match self {
            Self::JavaScript => SourceType::Module,
            Self::JavaScriptReact => SourceType::Jsx,
            Self::TypeScript => SourceType::TypeScript,
            Self::TypeScriptReact => SourceType::Tsx,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LanguageSeverity {
    Error,
    Warning,
    Information,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LanguageDiagnostic {
    pub span: Span,
    pub severity: LanguageSeverity,
    pub code: String,
    pub message: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompletionKind {
    Property,
    Value,
    Keyword,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Completion {
    pub label: String,
    pub detail: String,
    pub documentation: String,
    pub insert_text: String,
    pub kind: CompletionKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hover {
    pub span: Span,
    pub markdown: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SemanticKind {
    Property,
    Keyword,
    Number,
    String,
    Function,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticToken {
    pub span: Span,
    pub kind: SemanticKind,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DocumentColor {
    pub span: Span,
    pub red: f32,
    pub green: f32,
    pub blue: f32,
    pub alpha: f32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentSymbol {
    pub name: String,
    pub kind: CssTemplateKind,
    pub span: Span,
    pub selection_span: Span,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextEdit {
    pub span: Span,
    pub replacement: String,
}

pub struct LanguageDocument {
    source: SourceFile,
    interner: Interner,
    parsed: ParseOutput,
    virtual_documents: Vec<VirtualCssDocument>,
    diagnostics: Vec<LanguageDiagnostic>,
}

impl LanguageDocument {
    pub fn analyze(
        name: impl Into<String>,
        source: impl Into<String>,
        language: HostLanguage,
    ) -> Self {
        let name = name.into();
        let source = source.into();
        let interner = Interner::new();
        let parsed = parse(&source, &interner, language.source_type());
        let templates = parsed
            .module
            .with_ast(|program| discover_css_templates(program, &interner));
        let virtual_documents = templates
            .iter()
            .map(|template| VirtualCssDocument::from_template(&source, template))
            .collect::<Vec<_>>();
        let mut diagnostics = parser_diagnostics(&parsed.diagnostics);
        for document in &virtual_documents {
            diagnostics.extend(css_diagnostics(&source, document));
        }
        diagnostics.sort_by_key(|diagnostic| (diagnostic.span.lo, diagnostic.span.hi));
        Self {
            source: SourceFile::new(name, source),
            interner,
            parsed,
            virtual_documents,
            diagnostics,
        }
    }

    pub fn source(&self) -> &SourceFile {
        &self.source
    }

    pub fn virtual_documents(&self) -> &[VirtualCssDocument] {
        &self.virtual_documents
    }

    pub fn diagnostics(&self) -> &[LanguageDiagnostic] {
        &self.diagnostics
    }

    pub fn compiler_diagnostics(&self, seed: &str, imported: &Scope) -> Vec<LanguageDiagnostic> {
        self.parsed.module.with_ast(|program| {
            transform(program, &self.interner, self.source.src(), seed, imported)
                .diagnostics
                .iter()
                .filter_map(map_compiler_diagnostic)
                .collect()
        })
    }

    pub fn completions(&self, host_offset: u32) -> Vec<Completion> {
        let Some(document) = self
            .virtual_documents
            .iter()
            .find(|document| document.contains_host_offset(host_offset))
        else {
            return Vec::new();
        };
        let source = self.source.src();
        let segment = document
            .segments
            .iter()
            .find(|segment| segment.host.lo <= host_offset && host_offset <= segment.host.hi)
            .expect("containing segment exists");
        let before = &source[segment.host.lo as usize..host_offset as usize];
        let prefix = identifier_prefix(before);
        if prefix.starts_with('@') {
            return facts::css_facts()
                .at_rules
                .iter()
                .filter(|value| value.starts_with(prefix))
                .map(|value| completion(value, "CSS at-rule", "", CompletionKind::Keyword))
                .collect();
        }
        if prefix.starts_with(':') {
            return facts::css_facts()
                .pseudos
                .iter()
                .filter(|value| value.starts_with(prefix))
                .map(|value| completion(value, "CSS pseudo selector", "", CompletionKind::Keyword))
                .collect();
        }
        if let Some(property_name) = value_context_property(before)
            && let Some(property) = facts::property(property_name)
        {
            return property
                .values
                .iter()
                .map(|value| {
                    completion(
                        value,
                        &format!("Value for {}", property.name),
                        &property.description,
                        CompletionKind::Value,
                    )
                })
                .collect();
        }
        facts::css_facts()
            .properties
            .iter()
            .filter(|property| property.name.starts_with(prefix))
            .map(|property| Completion {
                label: property.name.clone(),
                detail: "CSS property".to_string(),
                documentation: property.description.clone(),
                insert_text: format!("{}: ", property.name),
                kind: CompletionKind::Property,
            })
            .collect()
    }

    pub fn hover(&self, host_offset: u32) -> Option<Hover> {
        let document = self
            .virtual_documents
            .iter()
            .find(|document| document.contains_host_offset(host_offset))?;
        let segment = document
            .segments
            .iter()
            .find(|segment| segment.host.lo <= host_offset && host_offset <= segment.host.hi)?;
        let span = word_span(self.source.src(), host_offset, segment.host)?;
        let word = span.slice(self.source.src());
        let property_word = word.trim_end_matches(':');
        if let Some(property) = facts::property(property_word) {
            return Some(Hover {
                span: Span::new(span.lo, span.lo + property_word.len() as u32),
                markdown: format!("**{}**\n\n{}", property.name, property.description),
            });
        }
        if facts::css_facts()
            .at_rules
            .iter()
            .any(|value| value == word)
        {
            return Some(Hover {
                span,
                markdown: format!("**{word}** CSS at-rule"),
            });
        }
        if facts::css_facts().pseudos.iter().any(|value| value == word) {
            return Some(Hover {
                span,
                markdown: format!("**{word}** CSS pseudo selector"),
            });
        }
        None
    }

    pub fn semantic_tokens(&self) -> Vec<SemanticToken> {
        let mut tokens = Vec::new();
        for document in &self.virtual_documents {
            for segment in &document.segments {
                tokens.extend(semantic_tokens_in_segment(self.source.src(), segment.host));
            }
        }
        tokens.sort_by_key(|token| (token.span.lo, token.span.hi));
        tokens
    }

    pub fn colors(&self) -> Vec<DocumentColor> {
        let mut colors = Vec::new();
        for document in &self.virtual_documents {
            for segment in &document.segments {
                colors.extend(colors_in_segment(self.source.src(), segment.host));
            }
        }
        colors
    }

    pub fn symbols(&self) -> Vec<DocumentSymbol> {
        self.virtual_documents
            .iter()
            .map(|document| {
                let name = match document.kind {
                    CssTemplateKind::Css => "css",
                    CssTemplateKind::Keyframes => "keyframes",
                    CssTemplateKind::GlobalStyle => "globalStyle",
                };
                DocumentSymbol {
                    name: name.to_string(),
                    kind: document.kind,
                    span: document.template_span,
                    selection_span: Span::new(
                        document.template_span.lo,
                        document.template_span.lo.saturating_add(1),
                    ),
                }
            })
            .collect()
    }

    pub fn folding_ranges(&self) -> Vec<Span> {
        let mut ranges = Vec::new();
        for document in &self.virtual_documents {
            let mut stack = Vec::new();
            for segment in &document.segments {
                for (relative, byte) in segment.host.slice(self.source.src()).bytes().enumerate() {
                    let offset = segment.host.lo + relative as u32;
                    match byte {
                        b'{' => stack.push(offset),
                        b'}' => {
                            if let Some(start) = stack.pop()
                                && self.source.location0_utf16(start).0
                                    < self.source.location0_utf16(offset).0
                            {
                                ranges.push(Span::new(start, offset.saturating_add(1)));
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        ranges
    }

    pub fn selection_span(&self, host_offset: u32) -> Option<Span> {
        self.virtual_documents
            .iter()
            .find(|document| document.template_span.contains_offset(host_offset))
            .map(|document| document.template_span)
    }

    pub fn format(&self, requested: Option<Span>) -> Vec<TextEdit> {
        let mut edits = Vec::new();
        for document in &self.virtual_documents {
            let mut indent = 0_usize;
            for segment in &document.segments {
                if requested
                    .is_some_and(|range| range.hi <= segment.host.lo || segment.host.hi <= range.lo)
                {
                    update_indent(segment.host.slice(self.source.src()), &mut indent);
                    continue;
                }
                let original = segment.host.slice(self.source.src());
                let replacement = format_literal(original, &mut indent);
                if replacement != original && document.edit_is_safe(segment.host) {
                    edits.push(TextEdit {
                        span: segment.host,
                        replacement,
                    });
                }
            }
        }
        edits
    }
}

fn parser_diagnostics(diagnostics: &[Diagnostic]) -> Vec<LanguageDiagnostic> {
    diagnostics
        .iter()
        .filter_map(map_compiler_diagnostic)
        .collect()
}

fn map_compiler_diagnostic(diagnostic: &Diagnostic) -> Option<LanguageDiagnostic> {
    let span = diagnostic.primary_span()?;
    let severity = match diagnostic.severity {
        Severity::Error => LanguageSeverity::Error,
        Severity::Warning => LanguageSeverity::Warning,
        Severity::Note | Severity::Help => LanguageSeverity::Information,
    };
    Some(LanguageDiagnostic {
        span,
        severity,
        code: diagnostic
            .code
            .as_deref()
            .unwrap_or("WAKE_PARSE")
            .to_string(),
        message: diagnostic.message.clone(),
    })
}

fn css_diagnostics(source: &str, document: &VirtualCssDocument) -> Vec<LanguageDiagnostic> {
    let mut diagnostics = Vec::new();
    let mut input = ParserInput::new(&document.text);
    let mut parser = Parser::new(&mut input);
    while parser.next_including_whitespace_and_comments().is_ok() {}

    let mut braces = Vec::new();
    for segment in &document.segments {
        let text = segment.host.slice(source);
        for (relative, byte) in text.bytes().enumerate() {
            let offset = segment.host.lo + relative as u32;
            match byte {
                b'{' => braces.push(offset),
                b'}' if braces.pop().is_none() => diagnostics.push(LanguageDiagnostic {
                    span: Span::new(offset, offset + 1),
                    severity: LanguageSeverity::Error,
                    code: "CSS_UNEXPECTED_BRACE".to_string(),
                    message: "Unexpected closing brace".to_string(),
                }),
                b'}' => {}
                _ => {}
            }
        }
        diagnostics.extend(unknown_property_diagnostics(source, segment.host));
    }
    for offset in braces {
        diagnostics.push(LanguageDiagnostic {
            span: Span::new(offset, offset + 1),
            severity: LanguageSeverity::Error,
            code: "CSS_UNCLOSED_BLOCK".to_string(),
            message: "CSS block is not closed".to_string(),
        });
    }
    diagnostics
}

fn unknown_property_diagnostics(source: &str, span: Span) -> Vec<LanguageDiagnostic> {
    let text = span.slice(source);
    let bytes = text.as_bytes();
    let mut diagnostics = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if !is_ident_byte(bytes[index]) {
            index += 1;
            continue;
        }
        let start = index;
        while index < bytes.len() && is_ident_byte(bytes[index]) {
            index += 1;
        }
        let mut next = index;
        while next < bytes.len() && bytes[next].is_ascii_whitespace() {
            next += 1;
        }
        if next >= bytes.len() || bytes[next] != b':' {
            continue;
        }
        let previous = text[..start]
            .bytes()
            .rev()
            .find(|byte| !byte.is_ascii_whitespace());
        if previous.is_some_and(|byte| !matches!(byte, b'{' | b';')) && start != 0 {
            continue;
        }
        let name = &text[start..index];
        if name.starts_with("--") || facts::property(name).is_some() {
            continue;
        }
        diagnostics.push(LanguageDiagnostic {
            span: Span::new(span.lo + start as u32, span.lo + index as u32),
            severity: LanguageSeverity::Warning,
            code: "CSS_UNKNOWN_PROPERTY".to_string(),
            message: format!("Unknown CSS property `{name}`"),
        });
    }
    diagnostics
}

fn completion(label: &str, detail: &str, documentation: &str, kind: CompletionKind) -> Completion {
    Completion {
        label: label.to_string(),
        detail: detail.to_string(),
        documentation: documentation.to_string(),
        insert_text: label.to_string(),
        kind,
    }
}

fn identifier_prefix(value: &str) -> &str {
    let start = value
        .char_indices()
        .rev()
        .find(|(_, ch)| !is_word_char(*ch) && !matches!(ch, ':' | '@'))
        .map_or(0, |(index, ch)| index + ch.len_utf8());
    &value[start..]
}

fn value_context_property(value: &str) -> Option<&str> {
    let boundary = value.rfind([';', '{', '}']).map_or(0, |index| index + 1);
    let declaration = &value[boundary..];
    let colon = declaration.find(':')?;
    let property = declaration[..colon].trim();
    (!property.is_empty() && property.bytes().all(is_ident_byte)).then_some(property)
}

fn word_span(source: &str, offset: u32, bounds: Span) -> Option<Span> {
    let mut lo = offset.min(bounds.hi) as usize;
    let mut hi = lo;
    let lower = bounds.lo as usize;
    let upper = bounds.hi as usize;
    while lo > lower && is_word_char(source[..lo].chars().next_back()?) {
        lo -= source[..lo].chars().next_back()?.len_utf8();
    }
    while hi < upper && is_word_char(source[hi..].chars().next()?) {
        hi += source[hi..].chars().next()?.len_utf8();
    }
    (lo < hi).then_some(Span::new(lo as u32, hi as u32))
}

fn semantic_tokens_in_segment(source: &str, span: Span) -> Vec<SemanticToken> {
    let text = span.slice(source);
    let mut result = Vec::new();
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        let start = index;
        if matches!(bytes[index], b'\'' | b'"') {
            let quote = bytes[index];
            index += 1;
            while index < bytes.len() && bytes[index] != quote {
                index += if bytes[index] == b'\\' && index + 1 < bytes.len() {
                    2
                } else {
                    1
                };
            }
            index = (index + 1).min(bytes.len());
            result.push(token(span, start, index, SemanticKind::String));
        } else if bytes[index].is_ascii_digit() {
            index += 1;
            while index < bytes.len() && (bytes[index].is_ascii_digit() || bytes[index] == b'.') {
                index += 1;
            }
            result.push(token(span, start, index, SemanticKind::Number));
        } else if bytes[index] == b'@' {
            index += 1;
            while index < bytes.len() && is_ident_byte(bytes[index]) {
                index += 1;
            }
            result.push(token(span, start, index, SemanticKind::Keyword));
        } else if is_ident_byte(bytes[index]) {
            index += 1;
            while index < bytes.len() && is_ident_byte(bytes[index]) {
                index += 1;
            }
            let mut next = index;
            while next < bytes.len() && bytes[next].is_ascii_whitespace() {
                next += 1;
            }
            let kind = if next < bytes.len() && bytes[next] == b'(' {
                SemanticKind::Function
            } else if next < bytes.len()
                && bytes[next] == b':'
                && facts::property(&text[start..index]).is_some()
            {
                SemanticKind::Property
            } else {
                SemanticKind::Keyword
            };
            result.push(token(span, start, index, kind));
        } else {
            index += 1;
        }
    }
    result
}

fn token(span: Span, start: usize, end: usize, kind: SemanticKind) -> SemanticToken {
    SemanticToken {
        span: Span::new(span.lo + start as u32, span.lo + end as u32),
        kind,
    }
}

fn colors_in_segment(source: &str, span: Span) -> Vec<DocumentColor> {
    let text = span.slice(source);
    let bytes = text.as_bytes();
    let mut colors = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'#' {
            index += 1;
            continue;
        }
        let start = index;
        index += 1;
        while index < bytes.len() && bytes[index].is_ascii_hexdigit() && index - start <= 8 {
            index += 1;
        }
        let digits = &text[start + 1..index];
        if let Some((red, green, blue, alpha)) = parse_hex_color(digits) {
            colors.push(DocumentColor {
                span: Span::new(span.lo + start as u32, span.lo + index as u32),
                red,
                green,
                blue,
                alpha,
            });
        }
    }
    colors
}

fn parse_hex_color(value: &str) -> Option<(f32, f32, f32, f32)> {
    let expand = |value: u8| ((value << 4) | value) as f32 / 255.0;
    let pair = |part: &str| {
        u8::from_str_radix(part, 16)
            .ok()
            .map(|value| value as f32 / 255.0)
    };
    match value.len() {
        3 => Some((
            expand(u8::from_str_radix(&value[0..1], 16).ok()?),
            expand(u8::from_str_radix(&value[1..2], 16).ok()?),
            expand(u8::from_str_radix(&value[2..3], 16).ok()?),
            1.0,
        )),
        4 => Some((
            expand(u8::from_str_radix(&value[0..1], 16).ok()?),
            expand(u8::from_str_radix(&value[1..2], 16).ok()?),
            expand(u8::from_str_radix(&value[2..3], 16).ok()?),
            expand(u8::from_str_radix(&value[3..4], 16).ok()?),
        )),
        6 => Some((
            pair(&value[0..2])?,
            pair(&value[2..4])?,
            pair(&value[4..6])?,
            1.0,
        )),
        8 => Some((
            pair(&value[0..2])?,
            pair(&value[2..4])?,
            pair(&value[4..6])?,
            pair(&value[6..8])?,
        )),
        _ => None,
    }
}

fn format_literal(source: &str, indent: &mut usize) -> String {
    let mut output = String::with_capacity(source.len());
    for line in source.split_inclusive('\n') {
        let (content, newline) = line.strip_suffix("\r\n").map_or_else(
            || {
                line.strip_suffix('\n')
                    .map_or((line, ""), |value| (value, "\n"))
            },
            |value| (value, "\r\n"),
        );
        let trimmed = content.trim();
        if trimmed.is_empty() {
            output.push_str(newline);
            continue;
        }
        if trimmed.starts_with('}') {
            *indent = indent.saturating_sub(1);
        }
        output.push_str(&"  ".repeat(*indent));
        output.push_str(trimmed);
        output.push_str(newline);
        update_indent_line(trimmed, indent);
    }
    output
}

fn update_indent(source: &str, indent: &mut usize) {
    for line in source.lines() {
        update_indent_line(line.trim(), indent);
    }
}

fn update_indent_line(line: &str, indent: &mut usize) {
    let opens = line.bytes().filter(|byte| *byte == b'{').count();
    let closes = line.bytes().filter(|byte| *byte == b'}').count();
    let leading_close = usize::from(line.starts_with('}'));
    *indent = indent
        .saturating_add(opens)
        .saturating_sub(closes.saturating_sub(leading_close));
}

fn is_ident_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')
}

fn is_word_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | ':' | '@')
}

#[cfg(test)]
mod tests;
