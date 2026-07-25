//! 字面量节点。

use wake_common::{Atom, Span};

use crate::AVec;
use crate::expr::Expression;

#[derive(Clone, Copy, Debug)]
pub struct NumberLiteral {
    pub span: Span,
    pub value: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct StringLiteral {
    pub span: Span,
    /// 已解码并驻留的值。
    pub value: Atom,
}

#[derive(Clone, Copy, Debug)]
pub struct BooleanLiteral {
    pub span: Span,
    pub value: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct BigIntLiteral {
    pub span: Span,
    /// 原始文本（去掉尾缀 `n` 前，含进制前缀），驻留。
    pub raw: Atom,
}

#[derive(Clone, Copy, Debug)]
pub struct RegExpLiteral {
    pub span: Span,
    pub pattern: Atom,
    pub flags: Atom,
}

/// 模板字面量 `` `a${b}c` ``：`quasis.len() == expressions.len() + 1`。
#[derive(Debug)]
pub struct TemplateLiteral<'a> {
    pub span: Span,
    pub quasis: AVec<'a, TemplateElement>,
    pub expressions: AVec<'a, Expression<'a>>,
}

/// 模板中的一段静态文本。
#[derive(Clone, Copy, Debug)]
pub struct TemplateElement {
    pub span: Span,
    /// cooked 值（转义解码后）；非法转义时为 `None`（tagged 模板允许）。
    pub cooked: Option<Atom>,
    /// raw 文本。
    pub raw: Atom,
    /// 是否是最后一段（`}...\``）。
    pub tail: bool,
}
