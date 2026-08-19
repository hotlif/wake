//! File-system-independent language intelligence for `@crab-dev/css`.

mod facts;
mod virtual_document;

use wake_common::{Diagnostic, Interner, Severity, SourceFile, Span};
use wake_css::syntax::{
    CssBlockKind, CssSyntaxContext, CssSyntaxKind, CssSyntaxNode, CssSyntaxTree,
};
use wake_css_in_js::value::{Scope, StaticExports, collect_imports};
use wake_css_in_js::{
    CssTemplateKind, collect_static_exports_with, discover_css_templates, transform,
};
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
    Value,
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StaticImport {
    pub local: String,
    pub specifier: String,
    pub imported: String,
}

pub struct LanguageDocument {
    source: SourceFile,
    interner: Interner,
    parsed: ParseOutput,
    virtual_documents: Vec<VirtualCssDocument>,
    syntax_trees: Vec<CssSyntaxTree>,
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
        let syntax_trees = virtual_documents
            .iter()
            .map(|document| {
                let context = match document.kind {
                    CssTemplateKind::Css => CssSyntaxContext::StyleBlock,
                    CssTemplateKind::Keyframes => CssSyntaxContext::Keyframes,
                    CssTemplateKind::GlobalStyle => CssSyntaxContext::Stylesheet,
                };
                CssSyntaxTree::parse_with_context(
                    &document.text,
                    document.body_virtual_span(),
                    context,
                )
            })
            .collect::<Vec<_>>();
        let mut diagnostics = parser_diagnostics(&parsed.diagnostics);
        for (document, tree) in virtual_documents.iter().zip(&syntax_trees) {
            diagnostics.extend(css_diagnostics(document, tree));
        }
        diagnostics.sort_by_key(|diagnostic| (diagnostic.span.lo, diagnostic.span.hi));
        Self {
            source: SourceFile::new(name, source),
            interner,
            parsed,
            virtual_documents,
            syntax_trees,
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

    pub fn static_imports(&self) -> Vec<StaticImport> {
        self.parsed.module.with_ast(|program| {
            collect_imports(program, &self.interner)
                .into_iter()
                .map(|(local, specifier, imported)| StaticImport {
                    local,
                    specifier,
                    imported,
                })
                .collect()
        })
    }

    pub fn static_exports(&self, imported: &Scope) -> StaticExports {
        self.parsed.module.with_ast(|program| {
            collect_static_exports_with(program, &self.interner, self.source.name(), imported)
        })
    }

    pub fn completions(&self, host_offset: u32) -> Option<Vec<Completion>> {
        let (index, document) = self
            .virtual_documents
            .iter()
            .enumerate()
            .find(|(_, document)| document.contains_host_offset(host_offset))?;
        let virtual_offset = document.host_to_virtual_offset(host_offset)?;
        let tree = &self.syntax_trees[index];
        let node = tree.node_at_cursor(virtual_offset);
        let prefix = node
            .and_then(|node| {
                document
                    .text
                    .get(node.span.lo as usize..virtual_offset as usize)
            })
            .unwrap_or("");
        if matches!(
            node.map(|node| &node.kind),
            Some(CssSyntaxKind::AtKeyword(_))
        ) {
            return Some(
                facts::css_facts()
                    .at_rules
                    .iter()
                    .filter(|value| value.starts_with(prefix))
                    .map(|value| completion(value, "CSS at-rule", "", CompletionKind::Keyword))
                    .collect(),
            );
        }
        let declaration = tree.declaration_at(virtual_offset).or_else(|| {
            tree.declarations.iter().rev().find(|declaration| {
                declaration.colon_span.hi <= virtual_offset
                    && document
                        .text
                        .get(declaration.colon_span.hi as usize..virtual_offset as usize)
                        .is_some_and(|trailing| trailing.chars().all(char::is_whitespace))
            })
        });
        if let Some(declaration) = declaration
            && virtual_offset >= declaration.colon_span.hi
            && let Some(property) = facts::property(&declaration.name)
        {
            let value_prefix = node
                .filter(|node| {
                    matches!(node.kind, CssSyntaxKind::Ident(_))
                        && declaration.colon_span.hi <= node.span.lo
                })
                .and_then(|node| {
                    document
                        .text
                        .get(node.span.lo as usize..virtual_offset as usize)
                })
                .unwrap_or("");
            return Some(
                property
                    .values
                    .iter()
                    .filter(|value| value.starts_with(value_prefix))
                    .map(|value| {
                        completion(
                            value,
                            &format!("Value for {}", property.name),
                            &property.description,
                            CompletionKind::Value,
                        )
                    })
                    .collect(),
            );
        }
        if node.is_some_and(|node| {
            matches!(node.kind, CssSyntaxKind::Ident(_))
                && tree
                    .previous_significant(node.span)
                    .is_some_and(|previous| matches!(previous.kind, CssSyntaxKind::Colon))
        }) {
            let prefix = format!(":{prefix}");
            return Some(
                facts::css_facts()
                    .pseudos
                    .iter()
                    .filter(|value| value.starts_with(&prefix))
                    .map(|value| {
                        completion(value, "CSS pseudo selector", "", CompletionKind::Keyword)
                    })
                    .collect(),
            );
        }
        Some(
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
                .collect(),
        )
    }

    pub fn hover(&self, host_offset: u32) -> Option<Hover> {
        let (index, document) = self
            .virtual_documents
            .iter()
            .enumerate()
            .find(|(_, document)| document.contains_host_offset(host_offset))?;
        let virtual_offset = document.host_to_virtual_offset(host_offset)?;
        let tree = &self.syntax_trees[index];
        let node = tree.node_at(virtual_offset)?;
        let host_span = document.virtual_to_host_span(node.head_span)?;
        if let Some(declaration) = tree.declaration_with_name_span(node.span)
            && let Some(property) = facts::property(&declaration.name)
        {
            return Some(Hover {
                span: host_span,
                markdown: format!("**{}**\n\n{}", property.name, property.description),
            });
        }
        if let CssSyntaxKind::AtKeyword(name) = &node.kind {
            let word = format!("@{name}");
            if facts::css_facts()
                .at_rules
                .iter()
                .any(|value| value == &word)
            {
                return Some(Hover {
                    span: host_span,
                    markdown: format!("**{word}** CSS at-rule"),
                });
            }
        }
        None
    }

    pub fn semantic_tokens(&self) -> Vec<SemanticToken> {
        let mut tokens = Vec::new();
        for (document, tree) in self.virtual_documents.iter().zip(&self.syntax_trees) {
            tokens.extend(semantic_tokens(document, tree));
        }
        tokens.sort_by_key(|token| (token.span.lo, token.span.hi));
        tokens
    }

    pub fn colors(&self) -> Vec<DocumentColor> {
        let mut colors = Vec::new();
        for (document, tree) in self.virtual_documents.iter().zip(&self.syntax_trees) {
            colors.extend(document_colors(document, tree));
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
        for (document, tree) in self.virtual_documents.iter().zip(&self.syntax_trees) {
            tree.visit(|node| {
                if matches!(node.kind, CssSyntaxKind::Block(CssBlockKind::Curly))
                    && node.closed
                    && let Some(span) = document.virtual_to_host_covering_span(node.span)
                    && self.source.location0_utf16(span.lo).0
                        < self.source.location0_utf16(span.hi.saturating_sub(1)).0
                {
                    ranges.push(span);
                }
            });
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
        for (document, tree) in self.virtual_documents.iter().zip(&self.syntax_trees) {
            for segment in &document.segments {
                if requested
                    .is_some_and(|range| range.hi <= segment.host.lo || segment.host.hi <= range.lo)
                {
                    continue;
                }
                let original = segment.host.slice(self.source.src());
                let replacement = format_literal(original, segment.host, document, tree);
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

fn css_diagnostics(document: &VirtualCssDocument, tree: &CssSyntaxTree) -> Vec<LanguageDiagnostic> {
    let mut diagnostics = Vec::new();
    diagnostics.extend(tree.errors.iter().filter_map(|error| {
        Some(LanguageDiagnostic {
            span: document.virtual_to_host_span(error.span)?,
            severity: LanguageSeverity::Error,
            code: error.code.to_string(),
            message: error.message.to_string(),
        })
    }));
    for declaration in &tree.declarations {
        if declaration.name.starts_with("--") {
            continue;
        }
        let Some(span) = document.virtual_to_host_span(declaration.name_span) else {
            continue;
        };
        if let Some(property) = facts::property(&declaration.name) {
            if property.name != declaration.name {
                diagnostics.push(LanguageDiagnostic {
                    span,
                    severity: LanguageSeverity::Warning,
                    code: "CSS_PROPERTY_CASE".to_string(),
                    message: format!("CSS property should be written as `{}`", property.name),
                });
            }
            continue;
        }
        diagnostics.push(LanguageDiagnostic {
            span,
            severity: LanguageSeverity::Warning,
            code: "CSS_UNKNOWN_PROPERTY".to_string(),
            message: format!("Unknown CSS property `{}`", declaration.name),
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

fn semantic_tokens(document: &VirtualCssDocument, tree: &CssSyntaxTree) -> Vec<SemanticToken> {
    let mut result = Vec::new();
    tree.visit(|node| {
        let (virtual_span, kind) = match &node.kind {
            CssSyntaxKind::QuotedString(_) | CssSyntaxKind::Url(_) => {
                (node.span, SemanticKind::String)
            }
            CssSyntaxKind::Number(_)
            | CssSyntaxKind::Percentage(_)
            | CssSyntaxKind::Dimension { .. } => (node.span, SemanticKind::Number),
            CssSyntaxKind::AtKeyword(_) => (node.span, SemanticKind::Keyword),
            CssSyntaxKind::Function(_) => (
                Span::new(node.head_span.lo, node.head_span.hi.saturating_sub(1)),
                SemanticKind::Function,
            ),
            CssSyntaxKind::Ident(_) => {
                let kind = match tree.declaration_at(node.span.lo) {
                    Some(declaration) if declaration.name_span == node.span => {
                        SemanticKind::Property
                    }
                    Some(declaration)
                        if declaration.value_span.lo <= node.span.lo
                            && node.span.hi <= declaration.value_span.hi =>
                    {
                        SemanticKind::Value
                    }
                    _ => SemanticKind::Keyword,
                };
                (node.span, kind)
            }
            _ => return,
        };
        if let Some(span) = document.virtual_to_host_span(virtual_span) {
            result.push(SemanticToken { span, kind });
        }
    });
    result
}

fn document_colors(document: &VirtualCssDocument, tree: &CssSyntaxTree) -> Vec<DocumentColor> {
    let mut colors = Vec::new();
    tree.visit(|node| match &node.kind {
        CssSyntaxKind::Hash(value) | CssSyntaxKind::IdHash(value) => {
            if let Some((red, green, blue, alpha)) = parse_hex_color(value)
                && let Some(span) = document.virtual_to_host_span(node.span)
            {
                colors.push(document_color(span, red, green, blue, alpha));
            }
        }
        CssSyntaxKind::Function(name)
            if node.closed
                && matches!(
                    name.to_ascii_lowercase().as_str(),
                    "rgb" | "rgba" | "hsl" | "hsla"
                ) =>
        {
            let parsed = parse_color_function(name, &node.children);
            if let Some((red, green, blue, alpha)) = parsed
                && let Some(span) = document.virtual_to_host_span(node.span)
            {
                colors.push(document_color(span, red, green, blue, alpha));
            }
        }
        _ => {}
    });
    colors
}

fn document_color(span: Span, red: f32, green: f32, blue: f32, alpha: f32) -> DocumentColor {
    DocumentColor {
        span,
        red,
        green,
        blue,
        alpha,
    }
}

#[derive(Clone, Copy)]
enum ColorComponent<'a> {
    Number(f32),
    Percentage(f32),
    Dimension(f32, &'a str),
}

fn parse_color_function(name: &str, children: &[CssSyntaxNode]) -> Option<(f32, f32, f32, f32)> {
    let mut values = Vec::new();
    for child in children {
        match &child.kind {
            CssSyntaxKind::Number(value) => values.push(ColorComponent::Number(*value)),
            CssSyntaxKind::Percentage(value) => {
                values.push(ColorComponent::Percentage(*value));
            }
            CssSyntaxKind::Dimension { value, unit } => {
                values.push(ColorComponent::Dimension(*value, unit));
            }
            CssSyntaxKind::Whitespace
            | CssSyntaxKind::Comment
            | CssSyntaxKind::Comma
            | CssSyntaxKind::Delim('/') => {}
            _ => return None,
        }
    }
    if name.eq_ignore_ascii_case("rgb") || name.eq_ignore_ascii_case("rgba") {
        parse_rgb_color(&values)
    } else {
        parse_hsl_color(&values)
    }
}

fn parse_rgb_color(values: &[ColorComponent<'_>]) -> Option<(f32, f32, f32, f32)> {
    if !(3..=4).contains(&values.len()) {
        return None;
    }
    let channel = |part: ColorComponent<'_>| match part {
        ColorComponent::Percentage(value) => Some(value.clamp(0.0, 1.0)),
        ColorComponent::Number(value) => Some(value.clamp(0.0, 255.0) / 255.0),
        ColorComponent::Dimension(_, _) => None,
    };
    Some((
        channel(values[0])?,
        channel(values[1])?,
        channel(values[2])?,
        values
            .get(3)
            .map_or(Some(1.0), |value| parse_alpha(*value))?,
    ))
}

fn parse_hsl_color(values: &[ColorComponent<'_>]) -> Option<(f32, f32, f32, f32)> {
    if !(3..=4).contains(&values.len()) {
        return None;
    }
    let hue = match values[0] {
        ColorComponent::Number(value) => value,
        ColorComponent::Dimension(value, unit) if unit.eq_ignore_ascii_case("deg") => value,
        _ => return None,
    }
    .rem_euclid(360.0)
        / 360.0;
    let ColorComponent::Percentage(saturation) = values[1] else {
        return None;
    };
    let ColorComponent::Percentage(lightness) = values[2] else {
        return None;
    };
    let saturation = saturation.clamp(0.0, 1.0);
    let lightness = lightness.clamp(0.0, 1.0);
    let alpha = values
        .get(3)
        .map_or(Some(1.0), |value| parse_alpha(*value))?;
    if saturation == 0.0 {
        return Some((lightness, lightness, lightness, alpha));
    }
    let q = if lightness < 0.5 {
        lightness * (1.0 + saturation)
    } else {
        lightness + saturation - lightness * saturation
    };
    let p = 2.0 * lightness - q;
    let channel = |mut value: f32| {
        if value < 0.0 {
            value += 1.0;
        }
        if value > 1.0 {
            value -= 1.0;
        }
        if value < 1.0 / 6.0 {
            p + (q - p) * 6.0 * value
        } else if value < 0.5 {
            q
        } else if value < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - value) * 6.0
        } else {
            p
        }
    };
    Some((
        channel(hue + 1.0 / 3.0),
        channel(hue),
        channel(hue - 1.0 / 3.0),
        alpha,
    ))
}

fn parse_alpha(value: ColorComponent<'_>) -> Option<f32> {
    match value {
        ColorComponent::Percentage(value) | ColorComponent::Number(value) => {
            Some(value.clamp(0.0, 1.0))
        }
        ColorComponent::Dimension(_, _) => None,
    }
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

fn format_literal(
    source: &str,
    host_span: Span,
    document: &VirtualCssDocument,
    tree: &CssSyntaxTree,
) -> String {
    let mut output = String::with_capacity(source.len());
    let mut line_offset = 0usize;
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
            line_offset += line.len();
            continue;
        }
        let content_offset = content.len().saturating_sub(content.trim_start().len());
        let host_offset = host_span.lo + (line_offset + content_offset) as u32;
        let indent = document
            .host_to_virtual_offset(host_offset)
            .map_or(0, |offset| tree.curly_depth_at(offset));
        output.push_str(&"  ".repeat(indent));
        output.push_str(trimmed);
        output.push_str(newline);
        line_offset += line.len();
    }
    output
}

#[cfg(test)]
mod tests;
