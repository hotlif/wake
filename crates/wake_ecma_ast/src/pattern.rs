//! 绑定 / 解构模式（函数参数、`let` 解构、catch 参数、for-of/in 目标）。

use wake_common::Span;

use crate::AVec;
use crate::Ident;
use crate::expr::{Expression, PropertyKey};

/// 绑定模式。
#[derive(Clone, Copy, Debug)]
pub enum Pattern<'a> {
    /// 简单绑定 `x`。
    Ident(&'a Ident),
    /// 数组解构 `[a, b]`。
    Array(&'a ArrayPattern<'a>),
    /// 对象解构 `{a, b}`。
    Object(&'a ObjectPattern<'a>),
    /// 带默认值 `x = 1`。
    Assignment(&'a AssignmentPattern<'a>),
    /// 剩余 `...rest`。
    Rest(&'a RestElement<'a>),
}

impl Pattern<'_> {
    pub fn span(&self) -> Span {
        match self {
            Pattern::Ident(i) => i.span,
            Pattern::Array(a) => a.span,
            Pattern::Object(o) => o.span,
            Pattern::Assignment(a) => a.span,
            Pattern::Rest(r) => r.span,
        }
    }
}

#[derive(Debug)]
pub struct ArrayPattern<'a> {
    pub span: Span,
    /// `None` = 空位（elision）。
    pub elements: AVec<'a, Option<Pattern<'a>>>,
}

#[derive(Debug)]
pub struct ObjectPattern<'a> {
    pub span: Span,
    pub properties: AVec<'a, ObjectPatternProperty<'a>>,
    /// `...rest`。
    pub rest: Option<&'a RestElement<'a>>,
}

#[derive(Debug)]
pub struct ObjectPatternProperty<'a> {
    pub span: Span,
    pub key: PropertyKey<'a>,
    pub value: Pattern<'a>,
    pub shorthand: bool,
    pub computed: bool,
}

#[derive(Debug)]
pub struct AssignmentPattern<'a> {
    pub span: Span,
    pub left: Pattern<'a>,
    pub right: Expression<'a>,
}

#[derive(Debug)]
pub struct RestElement<'a> {
    pub span: Span,
    pub argument: Pattern<'a>,
}
