use std::ops::Range;

use cssparser::{ParseError, Parser, ParserInput, Token, TokenSerializationType};
use wake_common::Span;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CssSyntaxContext {
    #[default]
    Stylesheet,
    StyleBlock,
    Keyframes,
    ComponentValues,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CssBlockKind {
    Function,
    Parenthesis,
    Square,
    Curly,
}

#[derive(Clone, Debug, PartialEq)]
pub enum CssSyntaxKind {
    Ident(String),
    AtKeyword(String),
    Hash(String),
    IdHash(String),
    QuotedString(String),
    Url(String),
    Number(f32),
    Percentage(f32),
    Dimension { value: f32, unit: String },
    Function(String),
    Block(CssBlockKind),
    Comment,
    Whitespace,
    Colon,
    Semicolon,
    Comma,
    Delim(char),
    BadString,
    BadUrl,
    UnexpectedClose,
    Other,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CssSyntaxNode {
    pub kind: CssSyntaxKind,
    /// cssparser's token category for semantics-preserving serialization.
    pub serialization_type: TokenSerializationType,
    pub span: Span,
    pub head_span: Span,
    /// The source range containing a decoded token's original payload.
    ///
    /// This is populated for strings and unquoted `url()` tokens so consumers can rewrite the
    /// payload without re-scanning CSS syntax.
    pub value_span: Option<Span>,
    pub children: Vec<CssSyntaxNode>,
    pub closed: bool,
}

impl CssSyntaxNode {
    pub fn is_trivia(&self) -> bool {
        matches!(
            self.kind,
            CssSyntaxKind::Comment | CssSyntaxKind::Whitespace
        )
    }

    pub fn block_kind(&self) -> Option<CssBlockKind> {
        match self.kind {
            CssSyntaxKind::Block(kind) => Some(kind),
            CssSyntaxKind::Function(_) => Some(CssBlockKind::Function),
            _ => None,
        }
    }

    pub fn content_span(&self) -> Span {
        Span::new(
            self.head_span.hi,
            self.span.hi.saturating_sub(u32::from(self.closed)),
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CssDeclaration {
    pub name: String,
    /// Complete declaration range, including leading trivia and a trailing semicolon when present.
    pub span: Span,
    pub name_span: Span,
    pub colon_span: Span,
    pub value_span: Span,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CssSyntaxError {
    pub span: Span,
    pub code: &'static str,
    pub message: &'static str,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CssSyntaxItemKind {
    Declaration(CssDeclaration),
    QualifiedRule,
    KeyframeRule,
    AtRule { name: String },
    Raw,
}

/// A grammar-context-aware item whose node indexes refer to its parent's node slice.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CssSyntaxItem {
    pub kind: CssSyntaxItemKind,
    pub span: Span,
    pub node_range: Range<usize>,
    pub block_index: Option<usize>,
    pub children: Vec<CssSyntaxItem>,
}

impl CssSyntaxItem {
    pub fn nodes<'a>(&self, parent: &'a [CssSyntaxNode]) -> &'a [CssSyntaxNode] {
        parent.get(self.node_range.clone()).unwrap_or_default()
    }

    pub fn block<'a>(&self, parent: &'a [CssSyntaxNode]) -> Option<&'a CssSyntaxNode> {
        self.block_index.and_then(|index| parent.get(index))
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CssSyntaxTree {
    pub context: CssSyntaxContext,
    pub nodes: Vec<CssSyntaxNode>,
    pub items: Vec<CssSyntaxItem>,
    pub declarations: Vec<CssDeclaration>,
    pub errors: Vec<CssSyntaxError>,
}

impl CssSyntaxTree {
    pub fn parse(virtual_css: &str, body: Span) -> Self {
        Self::parse_with_context(virtual_css, body, CssSyntaxContext::Stylesheet)
    }

    pub fn parse_with_context(virtual_css: &str, body: Span, context: CssSyntaxContext) -> Self {
        let Some(source) = virtual_css.get(body.lo as usize..body.hi as usize) else {
            return Self::default();
        };
        let mut input = ParserInput::new(source);
        let mut parser = Parser::new(&mut input);
        let nodes = parse_nodes(&mut parser, source, body.lo);
        let mut declarations = Vec::new();
        let items = collect_items(&nodes, context, &mut declarations);
        let mut tree = Self {
            context,
            nodes,
            items,
            declarations,
            errors: Vec::new(),
        };
        collect_errors(&tree.nodes, &mut tree.errors);
        tree
    }

    pub fn node_at(&self, offset: u32) -> Option<&CssSyntaxNode> {
        find_node_at(&self.nodes, offset)
    }

    pub fn node_at_cursor(&self, offset: u32) -> Option<&CssSyntaxNode> {
        find_node_at_cursor(&self.nodes, offset)
    }

    pub fn declaration_at(&self, offset: u32) -> Option<&CssDeclaration> {
        self.declarations.iter().find(|declaration| {
            (declaration.name_span.lo <= offset && offset <= declaration.name_span.hi)
                || (declaration.value_span.lo <= offset && offset <= declaration.value_span.hi)
        })
    }

    pub fn declaration_with_name_span(&self, span: Span) -> Option<&CssDeclaration> {
        self.declarations
            .iter()
            .find(|declaration| declaration.name_span == span)
    }

    pub fn previous_significant(&self, span: Span) -> Option<&CssSyntaxNode> {
        previous_significant(&self.nodes, span)
    }

    pub fn visit(&self, mut visitor: impl FnMut(&CssSyntaxNode)) {
        visit_nodes(&self.nodes, &mut visitor);
    }

    pub fn curly_depth_at(&self, offset: u32) -> usize {
        curly_depth(&self.nodes, offset)
    }
}

fn parse_nodes(parser: &mut Parser<'_, '_>, source: &str, base: u32) -> Vec<CssSyntaxNode> {
    let mut nodes = Vec::new();
    while !parser.is_exhausted() {
        let start = parser.position().byte_index();
        let Ok(token) = parser.next_including_whitespace_and_comments() else {
            break;
        };
        let token = token.clone();
        let serialization_type = token.serialization_type();
        let head_end = parser.position().byte_index();
        let head_span = span(base, start, head_end);
        let (kind, children, closed) = match token {
            Token::Function(name) => {
                let children = nested_nodes(parser, source, base);
                (CssSyntaxKind::Function(name.to_string()), children, false)
            }
            Token::ParenthesisBlock => {
                let children = nested_nodes(parser, source, base);
                (
                    CssSyntaxKind::Block(CssBlockKind::Parenthesis),
                    children,
                    false,
                )
            }
            Token::SquareBracketBlock => {
                let children = nested_nodes(parser, source, base);
                (CssSyntaxKind::Block(CssBlockKind::Square), children, false)
            }
            Token::CurlyBracketBlock => {
                let children = nested_nodes(parser, source, base);
                (CssSyntaxKind::Block(CssBlockKind::Curly), children, false)
            }
            Token::Ident(value) => (CssSyntaxKind::Ident(value.to_string()), Vec::new(), true),
            Token::AtKeyword(value) => (
                CssSyntaxKind::AtKeyword(value.to_string()),
                Vec::new(),
                true,
            ),
            Token::Hash(value) => (CssSyntaxKind::Hash(value.to_string()), Vec::new(), true),
            Token::IDHash(value) => (CssSyntaxKind::IdHash(value.to_string()), Vec::new(), true),
            Token::QuotedString(value) => (
                CssSyntaxKind::QuotedString(value.to_string()),
                Vec::new(),
                true,
            ),
            Token::UnquotedUrl(value) => (CssSyntaxKind::Url(value.to_string()), Vec::new(), true),
            Token::Number { value, .. } => (CssSyntaxKind::Number(value), Vec::new(), true),
            Token::Percentage { unit_value, .. } => {
                (CssSyntaxKind::Percentage(unit_value), Vec::new(), true)
            }
            Token::Dimension { value, unit, .. } => (
                CssSyntaxKind::Dimension {
                    value,
                    unit: unit.to_string(),
                },
                Vec::new(),
                true,
            ),
            Token::Comment(_) => (CssSyntaxKind::Comment, Vec::new(), true),
            Token::WhiteSpace(_) => (CssSyntaxKind::Whitespace, Vec::new(), true),
            Token::Colon => (CssSyntaxKind::Colon, Vec::new(), true),
            Token::Semicolon => (CssSyntaxKind::Semicolon, Vec::new(), true),
            Token::Comma => (CssSyntaxKind::Comma, Vec::new(), true),
            Token::Delim(value) => (CssSyntaxKind::Delim(value), Vec::new(), true),
            Token::BadString(_) => (CssSyntaxKind::BadString, Vec::new(), true),
            Token::BadUrl(_) => (CssSyntaxKind::BadUrl, Vec::new(), true),
            Token::CloseParenthesis | Token::CloseSquareBracket | Token::CloseCurlyBracket => {
                (CssSyntaxKind::UnexpectedClose, Vec::new(), true)
            }
            _ => (CssSyntaxKind::Other, Vec::new(), true),
        };
        let end = parser.position().byte_index();
        let value_span = token_value_span(&kind, source, base, start, end);
        let closed = if let Some(block) = block_kind(&kind) {
            source.as_bytes().get(end.saturating_sub(1)) == Some(&closing_delimiter(block))
        } else {
            closed
        };
        nodes.push(CssSyntaxNode {
            kind,
            serialization_type,
            span: span(base, start, end),
            head_span,
            value_span,
            children,
            closed,
        });
    }
    nodes
}

fn token_value_span(
    kind: &CssSyntaxKind,
    source: &str,
    base: u32,
    start: usize,
    end: usize,
) -> Option<Span> {
    match kind {
        CssSyntaxKind::QuotedString(_) if end >= start + 2 => Some(span(base, start + 1, end - 1)),
        CssSyntaxKind::Url(_) => {
            let token = source.get(start..end)?;
            let open = token.find('(')? + 1;
            let close = token.rfind(')')?;
            let payload = token.get(open..close)?;
            let leading = payload.len() - payload.trim_start().len();
            let trailing = payload.len() - payload.trim_end().len();
            Some(span(base, start + open + leading, start + close - trailing))
        }
        _ => None,
    }
}

fn nested_nodes(parser: &mut Parser<'_, '_>, source: &str, base: u32) -> Vec<CssSyntaxNode> {
    parser
        .parse_nested_block(|nested| Ok::<_, ParseError<'_, ()>>(parse_nodes(nested, source, base)))
        .unwrap_or_default()
}

fn block_kind(kind: &CssSyntaxKind) -> Option<CssBlockKind> {
    match kind {
        CssSyntaxKind::Function(_) => Some(CssBlockKind::Function),
        CssSyntaxKind::Block(kind) => Some(*kind),
        _ => None,
    }
}

fn closing_delimiter(kind: CssBlockKind) -> u8 {
    match kind {
        CssBlockKind::Function | CssBlockKind::Parenthesis => b')',
        CssBlockKind::Square => b']',
        CssBlockKind::Curly => b'}',
    }
}

fn span(base: u32, start: usize, end: usize) -> Span {
    Span::new(base + start as u32, base + end as u32)
}

fn collect_items(
    nodes: &[CssSyntaxNode],
    context: CssSyntaxContext,
    declarations: &mut Vec<CssDeclaration>,
) -> Vec<CssSyntaxItem> {
    if context == CssSyntaxContext::ComponentValues {
        return Vec::new();
    }

    let mut items = Vec::new();
    let mut cursor = 0usize;
    while cursor < nodes.len() {
        let Some((first, node)) = next_significant(nodes, cursor) else {
            break;
        };
        if matches!(node.kind, CssSyntaxKind::Semicolon) {
            cursor = first + 1;
            continue;
        }

        if let CssSyntaxKind::AtKeyword(name) = &node.kind {
            let boundary = nodes
                .iter()
                .enumerate()
                .skip(first + 1)
                .find(|(_, candidate)| {
                    matches!(
                        candidate.kind,
                        CssSyntaxKind::Semicolon | CssSyntaxKind::Block(CssBlockKind::Curly)
                    )
                });
            let (end, block_index, children) = match boundary {
                Some((index, boundary))
                    if matches!(boundary.kind, CssSyntaxKind::Block(CssBlockKind::Curly)) =>
                {
                    let body_context = at_rule_body_context(name, context);
                    (
                        index + 1,
                        Some(index),
                        collect_items(&boundary.children, body_context, declarations),
                    )
                }
                Some((index, _)) => (index + 1, None, Vec::new()),
                None => (nodes.len(), None, Vec::new()),
            };
            items.push(CssSyntaxItem {
                kind: CssSyntaxItemKind::AtRule { name: name.clone() },
                span: item_span(nodes, cursor, end),
                node_range: cursor..block_index.unwrap_or(end),
                block_index,
                children,
            });
            cursor = end;
            continue;
        }

        if context == CssSyntaxContext::StyleBlock
            && let Some(colon_index) = declaration_colon(nodes, first)
        {
            let semicolon_index = nodes[colon_index + 1..]
                .iter()
                .position(|candidate| matches!(candidate.kind, CssSyntaxKind::Semicolon))
                .map(|offset| colon_index + 1 + offset);
            let end = semicolon_index.map_or(nodes.len(), |index| index + 1);
            let value_end = semicolon_index.map_or_else(
                || {
                    nodes
                        .last()
                        .map_or(nodes[colon_index].span.hi, |last| last.span.hi)
                },
                |index| nodes[index].span.lo,
            );
            let value_start = next_significant(nodes, colon_index + 1)
                .filter(|(index, _)| semicolon_index.is_none_or(|semicolon| *index < semicolon))
                .map_or(nodes[colon_index].span.hi, |(_, value)| value.span.lo);
            let CssSyntaxKind::Ident(name) = &nodes[first].kind else {
                unreachable!("declaration_colon only accepts identifiers")
            };
            let declaration = CssDeclaration {
                name: name.clone(),
                span: item_span(nodes, cursor, end),
                name_span: nodes[first].span,
                colon_span: nodes[colon_index].span,
                value_span: Span::new(value_start, value_end.max(value_start)),
            };
            declarations.push(declaration.clone());
            items.push(CssSyntaxItem {
                kind: CssSyntaxItemKind::Declaration(declaration),
                span: item_span(nodes, cursor, end),
                node_range: cursor..semicolon_index.unwrap_or(end),
                block_index: None,
                children: Vec::new(),
            });
            cursor = end;
            continue;
        }

        let boundary = nodes.iter().enumerate().skip(first).find(|(_, candidate)| {
            matches!(
                candidate.kind,
                CssSyntaxKind::Semicolon | CssSyntaxKind::Block(CssBlockKind::Curly)
            )
        });
        if let Some((block_index, block)) = boundary
            && matches!(block.kind, CssSyntaxKind::Block(CssBlockKind::Curly))
        {
            let child_context = match context {
                CssSyntaxContext::Keyframes => CssSyntaxContext::StyleBlock,
                _ => CssSyntaxContext::StyleBlock,
            };
            let kind = if context == CssSyntaxContext::Keyframes {
                CssSyntaxItemKind::KeyframeRule
            } else {
                CssSyntaxItemKind::QualifiedRule
            };
            let end = block_index + 1;
            items.push(CssSyntaxItem {
                kind,
                span: item_span(nodes, cursor, end),
                node_range: cursor..block_index,
                block_index: Some(block_index),
                children: collect_items(&block.children, child_context, declarations),
            });
            cursor = end;
            continue;
        }

        let end = boundary.map_or(nodes.len(), |(index, _)| index + 1);
        items.push(CssSyntaxItem {
            kind: CssSyntaxItemKind::Raw,
            span: item_span(nodes, cursor, end),
            node_range: cursor..end,
            block_index: None,
            children: Vec::new(),
        });
        cursor = end;
    }
    items
}

fn declaration_colon(nodes: &[CssSyntaxNode], first: usize) -> Option<usize> {
    let CssSyntaxKind::Ident(name) = &nodes.get(first)?.kind else {
        return None;
    };
    let (colon_index, colon) = next_significant(nodes, first + 1)?;
    if !matches!(colon.kind, CssSyntaxKind::Colon) {
        return None;
    }
    let curly_before_semicolon = nodes[colon_index + 1..].iter().find(|candidate| {
        matches!(
            candidate.kind,
            CssSyntaxKind::Semicolon | CssSyntaxKind::Block(CssBlockKind::Curly)
        )
    });
    if !name.starts_with("--")
        && curly_before_semicolon
            .is_some_and(|node| matches!(node.kind, CssSyntaxKind::Block(CssBlockKind::Curly)))
    {
        return None;
    }
    Some(colon_index)
}

fn at_rule_body_context(name: &str, parent: CssSyntaxContext) -> CssSyntaxContext {
    let name = name.to_ascii_lowercase();
    if name.ends_with("keyframes") {
        return CssSyntaxContext::Keyframes;
    }
    if matches!(
        name.as_str(),
        "font-face"
            | "page"
            | "property"
            | "counter-style"
            | "font-palette-values"
            | "view-transition"
    ) {
        return CssSyntaxContext::StyleBlock;
    }
    if matches!(
        name.as_str(),
        "media" | "supports" | "container" | "layer" | "document" | "scope" | "starting-style"
    ) {
        return match parent {
            CssSyntaxContext::StyleBlock => CssSyntaxContext::StyleBlock,
            _ => CssSyntaxContext::Stylesheet,
        };
    }
    CssSyntaxContext::ComponentValues
}

fn item_span(nodes: &[CssSyntaxNode], start: usize, end: usize) -> Span {
    let lo = nodes.get(start).map_or(0, |node| node.span.lo);
    let hi = end
        .checked_sub(1)
        .and_then(|index| nodes.get(index))
        .map_or(lo, |node| node.span.hi);
    Span::new(lo, hi)
}

fn next_significant(nodes: &[CssSyntaxNode], start: usize) -> Option<(usize, &CssSyntaxNode)> {
    nodes
        .iter()
        .enumerate()
        .skip(start)
        .find(|(_, node)| !node.is_trivia())
}

fn collect_errors(nodes: &[CssSyntaxNode], out: &mut Vec<CssSyntaxError>) {
    for node in nodes {
        match node.kind {
            CssSyntaxKind::BadString => out.push(CssSyntaxError {
                span: node.span,
                code: "CSS_BAD_STRING",
                message: "CSS string is not closed",
            }),
            CssSyntaxKind::BadUrl => out.push(CssSyntaxError {
                span: node.span,
                code: "CSS_BAD_URL",
                message: "CSS URL is invalid",
            }),
            CssSyntaxKind::UnexpectedClose => out.push(CssSyntaxError {
                span: node.span,
                code: "CSS_UNEXPECTED_BLOCK_END",
                message: "Unexpected CSS block closing delimiter",
            }),
            _ if node.block_kind().is_some() && !node.closed => out.push(CssSyntaxError {
                span: node.head_span,
                code: "CSS_UNCLOSED_BLOCK",
                message: "CSS block is not closed",
            }),
            _ => {}
        }
        collect_errors(&node.children, out);
    }
}

fn find_node_at(nodes: &[CssSyntaxNode], offset: u32) -> Option<&CssSyntaxNode> {
    for node in nodes {
        if node.span.contains_offset(offset) {
            if let Some(child) = find_node_at(&node.children, offset) {
                return Some(child);
            }
            return Some(node);
        }
    }
    None
}

fn find_node_at_cursor(nodes: &[CssSyntaxNode], offset: u32) -> Option<&CssSyntaxNode> {
    let mut ending = None;
    for node in nodes {
        if node.span.contains_offset(offset) {
            return find_node_at_cursor(&node.children, offset).or(Some(node));
        }
        if node.span.hi == offset && !node.is_trivia() {
            ending = find_node_at_cursor(&node.children, offset).or(Some(node));
        }
    }
    ending
}

fn visit_nodes(nodes: &[CssSyntaxNode], visitor: &mut impl FnMut(&CssSyntaxNode)) {
    for node in nodes {
        visitor(node);
        visit_nodes(&node.children, visitor);
    }
}

fn previous_significant(nodes: &[CssSyntaxNode], target: Span) -> Option<&CssSyntaxNode> {
    for (index, node) in nodes.iter().enumerate() {
        if node.span == target {
            return nodes[..index]
                .iter()
                .rev()
                .find(|candidate| !candidate.is_trivia());
        }
        if node.span.contains(target)
            && let Some(previous) = previous_significant(&node.children, target)
        {
            return Some(previous);
        }
    }
    None
}

fn curly_depth(nodes: &[CssSyntaxNode], offset: u32) -> usize {
    let mut depth = 0usize;
    for node in nodes {
        if matches!(node.kind, CssSyntaxKind::Block(CssBlockKind::Curly))
            && node.head_span.hi <= offset
            && offset < node.span.hi.saturating_sub(u32::from(node.closed))
        {
            depth = depth.max(1 + curly_depth(&node.children, offset));
        }
    }
    depth
}
