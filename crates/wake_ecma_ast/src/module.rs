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

// —— import attributes（`with { type: "json" }`）——

/// 引入属性子句的引导关键字。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttributesKeyword {
    /// 标准 import attributes：`with { type: "json" }`。
    With,
    /// 已废弃的 import assertions：`assert { type: "json" }`（保留以兼容存量代码）。
    Assert,
}

impl AttributesKeyword {
    pub fn as_str(self) -> &'static str {
        match self {
            AttributesKeyword::With => "with",
            AttributesKeyword::Assert => "assert",
        }
    }
}

/// 一条引入属性：`type: "json"` / `"content-type": "text/css"`。值恒为字符串字面量。
#[derive(Clone, Copy, Debug)]
pub struct ImportAttribute {
    pub span: Span,
    pub key: ModuleExportName,
    /// 属性值（已驻留的字符串字面量内容）。
    pub value: Atom,
}

/// `with { .. }` / `assert { .. }` 子句。用 `&'a [_]` 而非 `AVec` 承载条目，
/// 使 [`ExportAllDeclaration`] 等 `Copy` 节点保持 `Copy`。
#[derive(Clone, Copy, Debug)]
pub struct ImportAttributes<'a> {
    pub span: Span,
    pub keyword: AttributesKeyword,
    pub items: &'a [ImportAttribute],
}

// —— import ——

#[derive(Debug)]
pub struct ImportDeclaration<'a> {
    pub span: Span,
    pub specifiers: AVec<'a, ImportSpecifier>,
    /// 模块说明符字符串（已驻留）。
    pub source: Atom,
    /// `with { type: "json" }` 子句（无则 `None`）。
    pub attributes: Option<&'a ImportAttributes<'a>>,
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
    /// `export { a } from '...' with { .. }` 子句（无 `from` 时恒 `None`）。
    pub attributes: Option<&'a ImportAttributes<'a>>,
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
pub struct ExportAllDeclaration<'a> {
    pub span: Span,
    /// `export * as ns from '...'` 的命名空间名。
    pub exported: Option<ModuleExportName>,
    pub source: Atom,
    /// `export * from '...' with { .. }` 子句。
    pub attributes: Option<&'a ImportAttributes<'a>>,
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
