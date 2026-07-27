//! # wake_ecma_ast — arena 分配的 ECMAScript AST
//!
//! DESIGN §4.2：arena 分配 + 生命周期参数（Oxc 路线）。节点小而多，arena 让分配变指针碰撞、
//! 释放变整块 drop。复合节点一律用 `&'a T` 引用，使 [`Expression`] / [`Statement`] 本体保持
//! ≤ 16 字节（静态断言钉死）。
//!
//! 本 crate 也承载 **Spike ①**（PLAN §0.5 / DESIGN §10.4）的自引用持有者 [`ModuleAst`]。
//!
//! **覆盖度**：ES2022 常见构造（模块/类/函数/控制流/全部表达式运算符/解构/模板/可选链等）。
//! TS/JSX 节点是 Phase 4 的增量；此处聚焦 JS。

use bumpalo::Bump;
use wake_common::{Atom, Span};

pub mod expr;
pub mod holder;
pub mod literal;
pub mod module;
pub mod pattern;
pub mod stmt;
pub mod visit;

mod hash;

pub use expr::*;
pub use holder::ModuleAst;
pub use literal::*;
pub use module::*;
pub use pattern::*;
pub use stmt::*;
pub use visit::{
    Visit, walk_class, walk_expression, walk_function, walk_pattern, walk_program, walk_statement,
};

/// arena 分配的 `Vec` 别名。
pub type AVec<'a, T> = bumpalo::collections::Vec<'a, T>;

/// 一个模块的 AST 根。
#[derive(Debug)]
pub struct Program<'a> {
    pub span: Span,
    /// 源类型：模块（含 import/export）或脚本。
    pub source_type: SourceType,
    /// 顶层语句 / 模块项。
    pub body: AVec<'a, Statement<'a>>,
    /// 严格模式（模块恒为严格；脚本看 `"use strict"` 指令）。
    pub strict: bool,
}

impl<'a> Program<'a> {
    pub fn new_in(arena: &'a Bump, source_type: SourceType) -> Program<'a> {
        Program {
            span: Span::DUMMY,
            source_type,
            body: bumpalo::collections::Vec::new_in(arena),
            strict: source_type.is_module(),
        }
    }
}

/// 源类型。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceType {
    /// ES 模块（有 import/export，恒严格模式）。
    Module,
    /// 传统脚本。
    Script,
    /// TypeScript 模块（严格模式 + 类型语法擦除，DESIGN §4.1）。
    TypeScript,
    /// TypeScript + JSX（`.tsx`）：类型擦除 + JSX 降级（DESIGN §4.1/§4.3）。
    Tsx,
    /// JavaScript + JSX（`.jsx`）：JSX 降级。
    Jsx,
}

impl SourceType {
    /// 是否为 TypeScript（解析时跳过类型语法）。`.ts` 与 `.tsx` 均为真。
    pub fn is_typescript(self) -> bool {
        matches!(self, SourceType::TypeScript | SourceType::Tsx)
    }

    /// 是否启用 JSX 解析（`.jsx` 与 `.tsx`）。
    pub fn is_jsx(self) -> bool {
        matches!(self, SourceType::Jsx | SourceType::Tsx)
    }

    /// 是否为模块（恒严格模式）。JSX/TSX 视为模块。
    pub fn is_module(self) -> bool {
        matches!(
            self,
            SourceType::Module | SourceType::TypeScript | SourceType::Tsx | SourceType::Jsx
        )
    }
}

/// 标识符（变量引用 / 声明 / 属性名统一用此，位置区分语义）。名字已驻留为 [`Atom`]。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ident {
    pub span: Span,
    pub name: Atom,
}

impl Ident {
    pub fn new(span: Span, name: Atom) -> Ident {
        Ident { span, name }
    }
}

const _: () = assert!(std::mem::size_of::<Ident>() <= 16, "Ident 应紧凑");

/// 在 arena 中手工构建一个样例 AST：`let sum = 0 + 1 + ... + depth;`（左结合链）。
///
/// 供 Spike ①（[`ModuleAst`]）的 demo/测试用——制造大量嵌套节点压测 arena 与自引用持有者。
/// 真正的 AST 由 wake_ecma_parser 产出；这里手工构建以避免 crate 依赖环。
pub fn build_sample<'a>(
    arena: &'a Bump,
    interner: &wake_common::Interner,
    depth: u32,
) -> Program<'a> {
    let mut expr = Expression::NumberLiteral(arena.alloc(NumberLiteral {
        span: Span::new(0, 1),
        value: 0.0,
    }));
    for i in 1..=depth {
        let right = Expression::NumberLiteral(arena.alloc(NumberLiteral {
            span: Span::new(0, i),
            value: i as f64,
        }));
        let bin = arena.alloc(BinaryExpression {
            span: Span::new(0, i),
            operator: BinaryOperator::Add,
            left: expr,
            right,
        });
        expr = Expression::Binary(bin);
    }
    let span = Span::new(0, depth.max(1));
    let id = &*arena.alloc(Ident::new(span, interner.intern("sum")));
    let mut decls = AVec::new_in(arena);
    decls.push(VariableDeclarator {
        span,
        id: Pattern::Ident(id),
        init: Some(expr),
    });
    let var = arena.alloc(VariableDeclaration {
        span,
        kind: VarKind::Let,
        declarations: decls,
    });

    let mut program = Program::new_in(arena, SourceType::Script);
    program.body.push(Statement::VariableDeclaration(var));
    program.span = span;
    program
}

/// 结构指纹（不含指针地址，DESIGN §10.4）。见 [`hash::structure_hash`]。
pub use hash::structure_hash;
