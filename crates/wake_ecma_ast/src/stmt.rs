//! 语句与声明节点。

use wake_common::Span;

use crate::AVec;
use crate::Ident;
use crate::expr::{Class, Expression, Function};
use crate::module::{
    ExportAllDeclaration, ExportDefaultDeclaration, ExportNamedDeclaration, ImportDeclaration,
};
use crate::pattern::Pattern;

/// 语句 / 模块项。复合变体用 arena 引用；`Empty/Debugger` 仅带 Span。
#[derive(Clone, Copy, Debug)]
pub enum Statement<'a> {
    // —— 声明 ——
    VariableDeclaration(&'a VariableDeclaration<'a>),
    FunctionDeclaration(&'a Function<'a>),
    ClassDeclaration(&'a Class<'a>),

    // —— 控制流 / 其它 ——
    Block(&'a BlockStatement<'a>),
    Empty(Span),
    Expression(&'a ExpressionStatement<'a>),
    If(&'a IfStatement<'a>),
    For(&'a ForStatement<'a>),
    ForIn(&'a ForInStatement<'a>),
    ForOf(&'a ForOfStatement<'a>),
    While(&'a WhileStatement<'a>),
    DoWhile(&'a DoWhileStatement<'a>),
    Switch(&'a SwitchStatement<'a>),
    Return(&'a ReturnStatement<'a>),
    Break(&'a BreakStatement),
    Continue(&'a ContinueStatement),
    Throw(&'a ThrowStatement<'a>),
    Try(&'a TryStatement<'a>),
    Labeled(&'a LabeledStatement<'a>),
    With(&'a WithStatement<'a>),
    Debugger(Span),

    // —— 模块项 ——
    Import(&'a ImportDeclaration<'a>),
    ExportNamed(&'a ExportNamedDeclaration<'a>),
    ExportDefault(&'a ExportDefaultDeclaration<'a>),
    ExportAll(&'a ExportAllDeclaration),
}

const _: () = assert!(
    std::mem::size_of::<Statement>() <= 16,
    "Statement 必须 ≤ 16 字节（DESIGN §4.2）"
);

impl Statement<'_> {
    pub fn span(&self) -> Span {
        use Statement::*;
        match self {
            VariableDeclaration(s) => s.span,
            FunctionDeclaration(f) => f.span,
            ClassDeclaration(c) => c.span,
            Block(b) => b.span,
            Empty(s) | Debugger(s) => *s,
            Expression(e) => e.span,
            If(s) => s.span,
            For(s) => s.span,
            ForIn(s) => s.span,
            ForOf(s) => s.span,
            While(s) => s.span,
            DoWhile(s) => s.span,
            Switch(s) => s.span,
            Return(s) => s.span,
            Break(s) => s.span,
            Continue(s) => s.span,
            Throw(s) => s.span,
            Try(s) => s.span,
            Labeled(s) => s.span,
            With(s) => s.span,
            Import(s) => s.span,
            ExportNamed(s) => s.span,
            ExportDefault(s) => s.span,
            ExportAll(s) => s.span,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VarKind {
    Var,
    Let,
    Const,
}

impl VarKind {
    pub fn as_str(self) -> &'static str {
        match self {
            VarKind::Var => "var",
            VarKind::Let => "let",
            VarKind::Const => "const",
        }
    }
}

#[derive(Debug)]
pub struct VariableDeclaration<'a> {
    pub span: Span,
    pub kind: VarKind,
    pub declarations: AVec<'a, VariableDeclarator<'a>>,
}

#[derive(Debug)]
pub struct VariableDeclarator<'a> {
    pub span: Span,
    pub id: Pattern<'a>,
    pub init: Option<Expression<'a>>,
}

#[derive(Debug)]
pub struct ExpressionStatement<'a> {
    pub span: Span,
    pub expression: Expression<'a>,
}

#[derive(Debug)]
pub struct BlockStatement<'a> {
    pub span: Span,
    pub body: AVec<'a, Statement<'a>>,
}

#[derive(Debug)]
pub struct IfStatement<'a> {
    pub span: Span,
    pub test: Expression<'a>,
    pub consequent: Statement<'a>,
    pub alternate: Option<Statement<'a>>,
}

/// `for` 的初始化子句。
#[derive(Clone, Copy, Debug)]
pub enum ForInit<'a> {
    Variable(&'a VariableDeclaration<'a>),
    Expression(Expression<'a>),
}

/// `for-in` / `for-of` 的左侧目标。
#[derive(Clone, Copy, Debug)]
pub enum ForLeft<'a> {
    Variable(&'a VariableDeclaration<'a>),
    Target(Expression<'a>),
}

#[derive(Debug)]
pub struct ForStatement<'a> {
    pub span: Span,
    pub init: Option<ForInit<'a>>,
    pub test: Option<Expression<'a>>,
    pub update: Option<Expression<'a>>,
    pub body: Statement<'a>,
}

#[derive(Debug)]
pub struct ForInStatement<'a> {
    pub span: Span,
    pub left: ForLeft<'a>,
    pub right: Expression<'a>,
    pub body: Statement<'a>,
}

#[derive(Debug)]
pub struct ForOfStatement<'a> {
    pub span: Span,
    pub left: ForLeft<'a>,
    pub right: Expression<'a>,
    pub body: Statement<'a>,
    pub is_await: bool,
}

#[derive(Debug)]
pub struct WhileStatement<'a> {
    pub span: Span,
    pub test: Expression<'a>,
    pub body: Statement<'a>,
}

#[derive(Debug)]
pub struct DoWhileStatement<'a> {
    pub span: Span,
    pub body: Statement<'a>,
    pub test: Expression<'a>,
}

#[derive(Debug)]
pub struct SwitchStatement<'a> {
    pub span: Span,
    pub discriminant: Expression<'a>,
    pub cases: AVec<'a, SwitchCase<'a>>,
}

#[derive(Debug)]
pub struct SwitchCase<'a> {
    pub span: Span,
    /// `None` 表示 `default:`。
    pub test: Option<Expression<'a>>,
    pub consequent: AVec<'a, Statement<'a>>,
}

#[derive(Debug)]
pub struct ReturnStatement<'a> {
    pub span: Span,
    pub argument: Option<Expression<'a>>,
}

#[derive(Clone, Copy, Debug)]
pub struct BreakStatement {
    pub span: Span,
    pub label: Option<Ident>,
}

#[derive(Clone, Copy, Debug)]
pub struct ContinueStatement {
    pub span: Span,
    pub label: Option<Ident>,
}

#[derive(Debug)]
pub struct ThrowStatement<'a> {
    pub span: Span,
    pub argument: Expression<'a>,
}

#[derive(Debug)]
pub struct TryStatement<'a> {
    pub span: Span,
    pub block: &'a BlockStatement<'a>,
    pub handler: Option<&'a CatchClause<'a>>,
    pub finalizer: Option<&'a BlockStatement<'a>>,
}

#[derive(Debug)]
pub struct CatchClause<'a> {
    pub span: Span,
    /// 可选的 catch 绑定（`catch {}` 无绑定）。
    pub param: Option<Pattern<'a>>,
    pub body: &'a BlockStatement<'a>,
}

#[derive(Debug)]
pub struct LabeledStatement<'a> {
    pub span: Span,
    pub label: Ident,
    pub body: Statement<'a>,
}

#[derive(Debug)]
pub struct WithStatement<'a> {
    pub span: Span,
    pub object: Expression<'a>,
    pub body: Statement<'a>,
}
