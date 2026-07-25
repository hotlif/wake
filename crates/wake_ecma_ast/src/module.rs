//! 模块语法（import / export）与依赖记录（DESIGN §4.4 依赖同步提取）。

use wake_common::{Atom, Span};

use crate::AVec;
use crate::Ident;
use crate::expr::{Class, Expression, Function};
use crate::stmt::Statement;

/// 模块导出名：普通标识符或字符串名（`export { "a-b" as c }`）。
#[derive(Clone, Copy, Debug)]
pub enum ModuleExportName {
    Ident(Ident),
    String(Atom),
}

// —— import ——

#[derive(Debug)]
pub struct ImportDeclaration<'a> {
    pub span: Span,
    pub specifiers: AVec<'a, ImportSpecifier>,
    /// 模块说明符字符串（已驻留）。
    pub source: Atom,
}

#[derive(Clone, Copy, Debug)]
pub enum ImportSpecifier {
    /// `import { a as b }`。
    Named {
        span: Span,
        imported: ModuleExportName,
        local: Ident,
    },
    /// `import x`。
    Default { span: Span, local: Ident },
    /// `import * as ns`。
    Namespace { span: Span, local: Ident },
}

// —— export ——

#[derive(Debug)]
pub struct ExportNamedDeclaration<'a> {
    pub span: Span,
    /// `export const x = 1` / `export function f(){}` 等。
    pub declaration: Option<Statement<'a>>,
    /// `export { a, b as c }`。
    pub specifiers: AVec<'a, ExportSpecifier>,
    /// `export { a } from '...'` 的来源（已驻留）。
    pub source: Option<Atom>,
}

#[derive(Clone, Copy, Debug)]
pub struct ExportSpecifier {
    pub span: Span,
    pub local: ModuleExportName,
    pub exported: ModuleExportName,
}

#[derive(Debug)]
pub struct ExportDefaultDeclaration<'a> {
    pub span: Span,
    pub declaration: ExportDefaultKind<'a>,
}

#[derive(Clone, Copy, Debug)]
pub enum ExportDefaultKind<'a> {
    Function(&'a Function<'a>),
    Class(&'a Class<'a>),
    Expression(Expression<'a>),
}

#[derive(Clone, Copy, Debug)]
pub struct ExportAllDeclaration {
    pub span: Span,
    /// `export * as ns from '...'` 的命名空间名。
    pub exported: Option<ModuleExportName>,
    pub source: Atom,
}

// ======================================================================
// 依赖记录（解析时同步产出，DESIGN §4.4）
// ======================================================================

/// 一条依赖：模块说明符 + 种类 + 位置。解析时收集，供 Scan 扇出（不进 arena）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dependency {
    pub specifier: Atom,
    pub kind: DependencyKind,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DependencyKind {
    /// 静态 `import ... from`。
    Import,
    /// `export ... from`。
    ExportFrom,
    /// 动态 `import(...)`。
    DynamicImport,
    /// `require(...)`。
    Require,
}
