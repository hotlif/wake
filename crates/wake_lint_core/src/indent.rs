use std::collections::{BTreeMap, BinaryHeap};

use wake_common::{FxHashSet, Span};
use wake_ecma_ast::{
    Expression, SourceNodeKind, Statement, Visit, walk_expression, walk_statement,
};
use wake_ecma_parser::{SourceParseOutput, SourceTokenKind as Token};

use crate::{EffectiveRule, LintDiagnostic, LintFix, RuleLevel, TextEdit};

/// A sweep over parser-owned delimiters and native AST regions. No source re-lexing, binding
/// reconstruction, or dependence on the user's current indentation. Additive regions form the
/// structural depth; hanging expressions supply a minimum, rather than compounding each other.
#[derive(Default)]
struct Depth {
    events: Vec<(u32, i32)>,
    positions: Vec<u32>,
    values: Vec<usize>,
}

impl Depth {
    fn add(&mut self, start: u32, end: u32) {
        if start < end {
            self.events.extend([(start, 1), (end, -1)]);
        }
    }

    fn finish(&mut self) {
        self.events.sort_unstable_by_key(|event| event.0);
        let mut depth = 0;
        let mut index = 0;
        while index < self.events.len() {
            let position = self.events[index].0;
            while index < self.events.len() && self.events[index].0 == position {
                depth += self.events[index].1;
                index += 1;
            }
            self.positions.push(position);
            self.values.push(depth.max(0) as usize);
        }
    }

    fn at(&self, position: u32) -> usize {
        self.positions
            .partition_point(|&start| start <= position)
            .checked_sub(1)
            .map_or(0, |index| self.values[index])
    }
}

#[derive(Default)]
struct Regions {
    bodies: Vec<Span>,
    hanging: Vec<Span>,
}

impl Regions {
    fn bare_body(&mut self, body: Statement<'_>) {
        if !matches!(body, Statement::Block(_) | Statement::Empty(_)) {
            self.bodies.push(body.span());
        }
    }
}

impl<'a> Visit<'a> for Regions {
    fn visit_statement(&mut self, statement: &Statement<'a>) {
        match statement {
            Statement::If(node) => {
                self.bare_body(node.consequent);
                if let Some(body) = node.alternate
                    && !matches!(body, Statement::If(_))
                {
                    self.bare_body(body);
                }
            }
            Statement::For(node) => self.bare_body(node.body),
            Statement::ForIn(node) => self.bare_body(node.body),
            Statement::ForOf(node) => self.bare_body(node.body),
            Statement::While(node) => self.bare_body(node.body),
            Statement::DoWhile(node) => self.bare_body(node.body),
            Statement::With(node) => self.bare_body(node.body),
            Statement::Labeled(node) => self.bare_body(node.body),
            Statement::Switch(node) => {
                for case in &node.cases {
                    if let (Some(first), Some(last)) =
                        (case.consequent.first(), case.consequent.last())
                    {
                        self.bodies.push(Span::new(first.span().lo, last.span().hi));
                    }
                }
            }
            Statement::VariableDeclaration(_)
            | Statement::Expression(_)
            | Statement::Return(_)
            | Statement::Throw(_) => self.hanging.push(statement.span()),
            _ => {}
        }
        walk_statement(self, statement);
    }

    fn visit_expression(&mut self, expression: &Expression<'a>) {
        if matches!(
            expression,
            Expression::Binary(_)
                | Expression::Logical(_)
                | Expression::Conditional(_)
                | Expression::Assignment(_)
                | Expression::Member(_)
        ) {
            self.hanging.push(expression.span());
        }
        walk_expression(self, expression);
    }
}

fn lines(source: &str) -> Vec<u32> {
    let mut starts = vec![0];
    let mut chars = source.char_indices().peekable();
    while let Some((index, ch)) = chars.next() {
        if ch == '\r' && chars.peek().is_some_and(|(_, next)| *next == '\n') {
            chars.next();
            starts.push((index + 2) as u32);
        } else if matches!(ch, '\r' | '\n' | '\u{2028}' | '\u{2029}') {
            starts.push((index + ch.len_utf8()) as u32);
        }
    }
    starts
}

fn structural(
    parsed: &SourceParseOutput,
    source: &str,
    regions: &Regions,
) -> (Depth, FxHashSet<u32>) {
    let mut depth = Depth::default();
    let mut closing = FxHashSet::default();
    let mut stack = Vec::new();
    for token in &parsed.tokens {
        let expected = match token.kind {
            Token::RParen => Some(Token::LParen),
            Token::RBracket => Some(Token::LBracket),
            Token::RBrace => Some(Token::LBrace),
            Token::TemplateMiddle | Token::TemplateTail => Some(Token::TemplateHead),
            _ => None,
        };
        if let Some(expected) = expected
            && let Some(&(kind, start)) = stack.last()
            && kind == expected
        {
            stack.pop();
            depth.add(start, token.span.lo);
            closing.insert(token.span.lo);
        }
        match token.kind {
            Token::LParen | Token::LBracket | Token::LBrace | Token::TemplateHead => {
                stack.push((token.kind, token.span.hi))
            }
            Token::TemplateMiddle => stack.push((Token::TemplateHead, token.span.hi)),
            _ => {}
        }
    }
    for body in &regions.bodies {
        depth.add(body.lo, body.hi);
    }
    let mut openings = vec![None; parsed.syntax.len()];
    let mut names = vec![None; parsed.syntax.len()];
    for node in &parsed.syntax {
        match node.kind {
            SourceNodeKind::TsTypeArguments | SourceNodeKind::TsTypeParameters
                if node.span.hi > node.span.lo + 1 =>
            {
                depth.add(node.span.lo + 1, node.span.hi - 1);
                closing.insert(node.span.hi - 1);
            }
            SourceNodeKind::JsxName => {
                if let Some(parent) = node.parent
                    && parsed.syntax[parent].kind == SourceNodeKind::JsxOpeningElement
                {
                    names[parent] = Some(node.span);
                }
            }
            SourceNodeKind::JsxOpeningElement => {
                if let Some(parent) = node.parent {
                    openings[parent] = Some(node.span);
                }
            }
            _ => {}
        }
    }
    for (index, node) in parsed.syntax.iter().enumerate() {
        if node.kind == SourceNodeKind::JsxOpeningElement {
            let tail = if source[node.span.lo as usize..node.span.hi as usize].ends_with("/>") {
                2
            } else {
                1
            };
            let end = node.span.hi - tail;
            depth.add(names[index].map_or(node.span.lo + 1, |name| name.hi), end);
            closing.insert(end);
        } else if node.kind == SourceNodeKind::JsxClosingElement {
            closing.insert(node.span.lo);
            if let Some(parent) = node.parent
                && let Some(opening) = openings[parent]
            {
                depth.add(opening.hi, node.span.lo);
            }
        }
    }
    depth.finish();
    (depth, closing)
}

pub(crate) fn check(
    source: &str,
    parsed: &SourceParseOutput,
    rules: &BTreeMap<String, EffectiveRule>,
    diagnostics: &mut Vec<LintDiagnostic>,
) {
    let rule = &rules["style/indent"].configuration;
    if rule.level == RuleLevel::Off {
        return;
    }
    let mut regions = Regions::default();
    parsed
        .parsed
        .module
        .with_ast(|program| regions.visit_program(program));
    for node in &parsed.syntax {
        if node.kind == SourceNodeKind::TsTypeAlias {
            regions.hanging.push(node.span);
        }
    }
    let starts = lines(source);
    let (depth, closing) = structural(parsed, source, &regions);
    // (first non-indent byte, structural depth, starts with a closing delimiter)
    let mut line_info = Vec::with_capacity(starts.len());
    for &start in &starts {
        let mut position = start as usize;
        while source
            .as_bytes()
            .get(position)
            .is_some_and(|byte| matches!(byte, b' ' | b'\t'))
        {
            position += 1;
        }
        let mut evaluated = position as u32;
        let closes = closing.contains(&evaluated);
        if closes {
            let index = parsed
                .tokens
                .partition_point(|token| token.span.lo < evaluated);
            let mut after = evaluated;
            for token in &parsed.tokens[index..] {
                if token.span.lo < after {
                    continue;
                }
                if !closing.contains(&token.span.lo)
                    || source[after as usize..token.span.lo as usize]
                        .contains(['\n', '\r', '\u{2028}', '\u{2029}'])
                {
                    break;
                }
                evaluated = token.span.lo;
                after = token.span.hi;
            }
        }
        line_info.push((position as u32, depth.at(evaluated), closes));
    }
    let mut hanging = Vec::new();
    for span in regions.hanging {
        if span.lo >= span.hi || span.hi as usize > source.len() {
            continue;
        }
        let line = starts.partition_point(|&start| start <= span.lo) - 1;
        if let Some(&next) = starts.get(line + 1)
            && next < span.hi
        {
            hanging.push((next, span.hi, line_info[line].1 + 1));
        }
    }
    hanging.sort_unstable();
    let mut pending = 0;
    let mut active = BinaryHeap::new();
    let unit = if rule.options["style"] == "tabs" {
        "\t".to_string()
    } else {
        " ".repeat(rule.options["width"].as_u64().expect("validated width") as usize)
    };
    for (line, &(position, mut level, closes)) in line_info.iter().enumerate() {
        while pending < hanging.len() && hanging[pending].0 <= position {
            active.push((hanging[pending].2, hanging[pending].1));
            pending += 1;
        }
        while active.peek().is_some_and(|&(_, end)| end <= position) {
            active.pop();
        }
        if !closes && let Some(&(minimum, _)) = active.peek() {
            level = level.max(minimum);
        }
        let Some(ch) = source[position as usize..].chars().next() else {
            continue;
        };
        if matches!(ch, '\n' | '\r' | '\u{2028}' | '\u{2029}') {
            continue;
        }
        // JSX trims indentation only following LF in the current original text grammar.
        // CR/LS/PS are JS line terminators but may leave significant JSX text before a tag.
        let token_index = parsed
            .tokens
            .partition_point(|token| token.span.hi <= position);
        if let Some(previous) = token_index
            .checked_sub(1)
            .and_then(|index| parsed.tokens.get(index))
            && previous.kind == Token::JsxText
            && previous.span.hi == position
            && (previous.span.lo >= starts[line]
                || starts[line] == 0
                || source.as_bytes()[starts[line] as usize - 1] != b'\n')
        {
            continue;
        }
        // The start of a literal/comment can be indented; its internal lines cannot.
        let token = parsed.tokens.get(token_index);
        if token.is_some_and(|token| {
            token.span.lo < position
                && matches!(
                    token.kind,
                    Token::Str
                        | Token::TemplateNoSub
                        | Token::TemplateHead
                        | Token::TemplateMiddle
                        | Token::TemplateTail
                        | Token::JsxText
                        | Token::Regex
                )
        }) {
            continue;
        }
        let comment = parsed.comments.get(
            parsed
                .comments
                .partition_point(|comment| comment.span.hi <= position),
        );
        if comment.is_some_and(|comment| comment.span.lo < position) {
            continue;
        }
        let expected = unit.repeat(level);
        let start = starts[line];
        if source[start as usize..position as usize] == expected {
            continue;
        }
        diagnostics.push(LintDiagnostic {
            rule_id: "style/indent".into(),
            level: rule.level,
            message_id: "indent".into(),
            message: format!(
                "Expected {level} indentation levels using {}.",
                rule.options["style"].as_str().expect("validated style")
            ),
            start,
            end: position,
            fix: Some(LintFix {
                edits: vec![TextEdit {
                    start,
                    end: position,
                    text: expected,
                }],
            }),
        });
    }
}
