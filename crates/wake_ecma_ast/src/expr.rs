//! 表达式节点与运算符。复合节点用 `&'a T`，[`Expression`] 本体 ≤ 16 字节。

use wake_common::{Atom, Span};

use crate::AVec;
use crate::Ident;
use crate::literal::{
    BigIntLiteral, BooleanLiteral, NumberLiteral, RegExpLiteral, StringLiteral, TemplateLiteral,
};
use crate::pattern::Pattern;
use crate::stmt::Statement;

/// 表达式。全部复合变体用 arena 引用；`This/Super/Null` 仅带 Span。
#[derive(Clone, Copy, Debug)]
pub enum Expression<'a> {
    NumberLiteral(&'a NumberLiteral),
    StringLiteral(&'a StringLiteral),
    BooleanLiteral(&'a BooleanLiteral),
    NullLiteral(Span),
    BigIntLiteral(&'a BigIntLiteral),
    RegExpLiteral(&'a RegExpLiteral),
    TemplateLiteral(&'a TemplateLiteral<'a>),
    Identifier(&'a Ident),
    This(Span),
    Super(Span),
    /// `new.target` / `import.meta`。
    MetaProperty(&'a MetaProperty),
    Array(&'a ArrayExpression<'a>),
    Object(&'a ObjectExpression<'a>),
    Function(&'a Function<'a>),
    Arrow(&'a ArrowFunction<'a>),
    Class(&'a Class<'a>),
    Unary(&'a UnaryExpression<'a>),
    Update(&'a UpdateExpression<'a>),
    Binary(&'a BinaryExpression<'a>),
    Logical(&'a LogicalExpression<'a>),
    Assignment(&'a AssignmentExpression<'a>),
    Conditional(&'a ConditionalExpression<'a>),
    Call(&'a CallExpression<'a>),
    New(&'a NewExpression<'a>),
    Member(&'a MemberExpression<'a>),
    Sequence(&'a SequenceExpression<'a>),
    TaggedTemplate(&'a TaggedTemplateExpression<'a>),
    Spread(&'a SpreadElement<'a>),
    Await(&'a AwaitExpression<'a>),
    Yield(&'a YieldExpression<'a>),
    /// 动态 `import(specifier)`。
    Import(&'a ImportExpression<'a>),
}

const _: () = assert!(
    std::mem::size_of::<Expression>() <= 16,
    "Expression 必须 ≤ 16 字节（DESIGN §4.2）"
);

impl Expression<'_> {
    /// 该表达式的 Span。
    pub fn span(&self) -> Span {
        use Expression::*;
        match self {
            NumberLiteral(n) => n.span,
            StringLiteral(s) => s.span,
            BooleanLiteral(b) => b.span,
            NullLiteral(s) | This(s) | Super(s) => *s,
            BigIntLiteral(b) => b.span,
            RegExpLiteral(r) => r.span,
            TemplateLiteral(t) => t.span,
            Identifier(i) => i.span,
            MetaProperty(m) => m.span,
            Array(a) => a.span,
            Object(o) => o.span,
            Function(f) => f.span,
            Arrow(a) => a.span,
            Class(c) => c.span,
            Unary(u) => u.span,
            Update(u) => u.span,
            Binary(b) => b.span,
            Logical(l) => l.span,
            Assignment(a) => a.span,
            Conditional(c) => c.span,
            Call(c) => c.span,
            New(n) => n.span,
            Member(m) => m.span,
            Sequence(s) => s.span,
            TaggedTemplate(t) => t.span,
            Spread(s) => s.span,
            Await(a) => a.span,
            Yield(y) => y.span,
            Import(i) => i.span,
        }
    }
}

// ======================================================================
// 运算符
// ======================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnaryOperator {
    Minus,
    Plus,
    LogicalNot,
    BitwiseNot,
    Typeof,
    Void,
    Delete,
}

impl UnaryOperator {
    pub fn as_str(self) -> &'static str {
        match self {
            UnaryOperator::Minus => "-",
            UnaryOperator::Plus => "+",
            UnaryOperator::LogicalNot => "!",
            UnaryOperator::BitwiseNot => "~",
            UnaryOperator::Typeof => "typeof",
            UnaryOperator::Void => "void",
            UnaryOperator::Delete => "delete",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateOperator {
    Increment,
    Decrement,
}

impl UpdateOperator {
    pub fn as_str(self) -> &'static str {
        match self {
            UpdateOperator::Increment => "++",
            UpdateOperator::Decrement => "--",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinaryOperator {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Exp,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    Ushr,
    Eq,
    NotEq,
    StrictEq,
    StrictNotEq,
    Lt,
    Gt,
    LtEq,
    GtEq,
    In,
    Instanceof,
}

impl BinaryOperator {
    pub fn as_str(self) -> &'static str {
        use BinaryOperator::*;
        match self {
            Add => "+",
            Sub => "-",
            Mul => "*",
            Div => "/",
            Rem => "%",
            Exp => "**",
            BitAnd => "&",
            BitOr => "|",
            BitXor => "^",
            Shl => "<<",
            Shr => ">>",
            Ushr => ">>>",
            Eq => "==",
            NotEq => "!=",
            StrictEq => "===",
            StrictNotEq => "!==",
            Lt => "<",
            Gt => ">",
            LtEq => "<=",
            GtEq => ">=",
            In => "in",
            Instanceof => "instanceof",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogicalOperator {
    And,
    Or,
    Coalesce,
}

impl LogicalOperator {
    pub fn as_str(self) -> &'static str {
        match self {
            LogicalOperator::And => "&&",
            LogicalOperator::Or => "||",
            LogicalOperator::Coalesce => "??",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssignmentOperator {
    Assign,
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Exp,
    Shl,
    Shr,
    Ushr,
    BitAnd,
    BitOr,
    BitXor,
    And,
    Or,
    Coalesce,
}

impl AssignmentOperator {
    pub fn as_str(self) -> &'static str {
        use AssignmentOperator::*;
        match self {
            Assign => "=",
            Add => "+=",
            Sub => "-=",
            Mul => "*=",
            Div => "/=",
            Rem => "%=",
            Exp => "**=",
            Shl => "<<=",
            Shr => ">>=",
            Ushr => ">>>=",
            BitAnd => "&=",
            BitOr => "|=",
            BitXor => "^=",
            And => "&&=",
            Or => "||=",
            Coalesce => "??=",
        }
    }
}

// ======================================================================
// 复合表达式节点
// ======================================================================

#[derive(Debug)]
pub struct UnaryExpression<'a> {
    pub span: Span,
    pub operator: UnaryOperator,
    pub argument: Expression<'a>,
}

#[derive(Debug)]
pub struct UpdateExpression<'a> {
    pub span: Span,
    pub operator: UpdateOperator,
    /// 前缀 `++x` 为 true，后缀 `x++` 为 false。
    pub prefix: bool,
    pub argument: Expression<'a>,
}

#[derive(Debug)]
pub struct BinaryExpression<'a> {
    pub span: Span,
    pub operator: BinaryOperator,
    pub left: Expression<'a>,
    pub right: Expression<'a>,
}

#[derive(Debug)]
pub struct LogicalExpression<'a> {
    pub span: Span,
    pub operator: LogicalOperator,
    pub left: Expression<'a>,
    pub right: Expression<'a>,
}

#[derive(Debug)]
pub struct AssignmentExpression<'a> {
    pub span: Span,
    pub operator: AssignmentOperator,
    /// 目标：简单目标（Ident/Member）或解构（Array/Object 表达式重解释）。
    pub left: Expression<'a>,
    pub right: Expression<'a>,
}

#[derive(Debug)]
pub struct ConditionalExpression<'a> {
    pub span: Span,
    pub test: Expression<'a>,
    pub consequent: Expression<'a>,
    pub alternate: Expression<'a>,
}

#[derive(Debug)]
pub struct CallExpression<'a> {
    pub span: Span,
    pub callee: Expression<'a>,
    pub arguments: AVec<'a, Expression<'a>>,
    /// 可选链 `f?.()`。
    pub optional: bool,
}

#[derive(Debug)]
pub struct NewExpression<'a> {
    pub span: Span,
    pub callee: Expression<'a>,
    pub arguments: AVec<'a, Expression<'a>>,
}

#[derive(Debug)]
pub struct MemberExpression<'a> {
    pub span: Span,
    pub object: Expression<'a>,
    pub property: MemberProperty<'a>,
    /// 可选链 `a?.b`。
    pub optional: bool,
}

#[derive(Clone, Copy, Debug)]
pub enum MemberProperty<'a> {
    /// `.name`
    Ident(Ident),
    /// `[expr]`
    Computed(Expression<'a>),
    /// `.#name`
    Private(Ident),
}

#[derive(Debug)]
pub struct SequenceExpression<'a> {
    pub span: Span,
    pub expressions: AVec<'a, Expression<'a>>,
}

#[derive(Debug)]
pub struct TaggedTemplateExpression<'a> {
    pub span: Span,
    pub tag: Expression<'a>,
    pub quasi: &'a TemplateLiteral<'a>,
}

#[derive(Debug)]
pub struct SpreadElement<'a> {
    pub span: Span,
    pub argument: Expression<'a>,
}

#[derive(Debug)]
pub struct AwaitExpression<'a> {
    pub span: Span,
    pub argument: Expression<'a>,
}

#[derive(Debug)]
pub struct YieldExpression<'a> {
    pub span: Span,
    pub argument: Option<Expression<'a>>,
    /// `yield*` 委托。
    pub delegate: bool,
}

#[derive(Debug)]
pub struct ImportExpression<'a> {
    pub span: Span,
    pub source: Expression<'a>,
    /// 第二参数（import attributes），可选。
    pub options: Option<Expression<'a>>,
}

#[derive(Clone, Copy, Debug)]
pub struct MetaProperty {
    pub span: Span,
    /// `new` 或 `import`。
    pub meta: Atom,
    /// `target` 或 `meta`。
    pub property: Atom,
}

// —— 数组 / 对象 ——

#[derive(Debug)]
pub struct ArrayExpression<'a> {
    pub span: Span,
    /// 元素；`None` 表示空位（elision，如 `[1,,3]`）。spread 为 `Expression::Spread`。
    pub elements: AVec<'a, Option<Expression<'a>>>,
}

#[derive(Debug)]
pub struct ObjectExpression<'a> {
    pub span: Span,
    pub properties: AVec<'a, ObjectMember<'a>>,
}

#[derive(Debug)]
pub enum ObjectMember<'a> {
    Property(&'a ObjectProperty<'a>),
    Spread(&'a SpreadElement<'a>),
}

#[derive(Debug)]
pub struct ObjectProperty<'a> {
    pub span: Span,
    pub key: PropertyKey<'a>,
    pub value: Expression<'a>,
    pub kind: PropertyKind,
    pub method: bool,
    pub shorthand: bool,
    pub computed: bool,
}

#[derive(Clone, Copy, Debug)]
pub enum PropertyKey<'a> {
    Ident(Ident),
    String(&'a StringLiteral),
    Number(&'a NumberLiteral),
    Computed(Expression<'a>),
    Private(Ident),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PropertyKind {
    Init,
    Get,
    Set,
}

// —— 函数 / 箭头 / 类 ——

#[derive(Debug)]
pub struct Function<'a> {
    pub span: Span,
    pub id: Option<Ident>,
    pub params: AVec<'a, Pattern<'a>>,
    pub body: Option<&'a FunctionBody<'a>>,
    pub is_async: bool,
    pub is_generator: bool,
}

#[derive(Debug)]
pub struct FunctionBody<'a> {
    pub span: Span,
    pub statements: AVec<'a, Statement<'a>>,
    /// 含 `"use strict"` 指令。
    pub strict: bool,
}

#[derive(Debug)]
pub struct ArrowFunction<'a> {
    pub span: Span,
    pub params: AVec<'a, Pattern<'a>>,
    pub body: ArrowBody<'a>,
    pub is_async: bool,
}

#[derive(Clone, Copy, Debug)]
pub enum ArrowBody<'a> {
    Block(&'a FunctionBody<'a>),
    Expression(Expression<'a>),
}

#[derive(Debug)]
pub struct Class<'a> {
    pub span: Span,
    pub id: Option<Ident>,
    pub super_class: Option<Expression<'a>>,
    pub body: AVec<'a, ClassMember<'a>>,
}

#[derive(Debug)]
pub enum ClassMember<'a> {
    Method(&'a MethodDefinition<'a>),
    Property(&'a PropertyDefinition<'a>),
    StaticBlock(&'a StaticBlock<'a>),
}

#[derive(Debug)]
pub struct MethodDefinition<'a> {
    pub span: Span,
    pub key: PropertyKey<'a>,
    pub value: &'a Function<'a>,
    pub kind: MethodKind,
    pub is_static: bool,
    pub computed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MethodKind {
    Constructor,
    Method,
    Get,
    Set,
}

#[derive(Debug)]
pub struct PropertyDefinition<'a> {
    pub span: Span,
    pub key: PropertyKey<'a>,
    pub value: Option<Expression<'a>>,
    pub is_static: bool,
    pub computed: bool,
}

#[derive(Debug)]
pub struct StaticBlock<'a> {
    pub span: Span,
    pub body: AVec<'a, Statement<'a>>,
}
