//! 作用域 / 符号 / 引用分析（PLAN §2.5，DESIGN §4.5）。
//!
//! Phase 2 先做 **后置遍历** 版（一遍 Visit 建 scope 树 + symbol 表 + 解析引用）。DESIGN 目标是
//! 保持独立语义所有权，供 tree-shaking（P6）与 minifier（P7）复用。是否融合遍历属于未来
//! 性能实验，不能重新引入 parser façade 或反向依赖。
//!
//! 独立 crate 边界避免 parser 改动触发 minifier 与 codegen 的级联重编译。
//!
//! 覆盖：作用域层级（module/function/block/catch）、var/function 提升（hoisting）、
//! let/const/class 块级绑定、参数/导入/catch 绑定、标识符引用解析（解析不到 = 全局/未声明）。
//! 具名类表达式的名字仅在类的 extends、成员与方法中可见，不泄漏到外层。
//! 严格模式的块级函数声明在所属块、switch 或 try/catch/finally 词法环境中提前绑定，
//! 不提升到外层函数/模块；普通 var 仍穿透块提升。函数自身 strict 指令和类的严格上下文被继承。
//! 类静态块拥有独立 var 环境；其 var/function/lexical 声明在遍历前绑定，不泄漏到其它静态块。
//! 无参数表达式时，参数与同名 var/function 共享绑定。含默认值或计算解构键时，函数体拥有
//! 子 var 环境，参数初始化闭包不能看到函数体声明；声明 occurrence 保留各自的声明类别。
//! 具名函数表达式的自引用名称位于参数环境外侧；参数和函数体声明不能与其合并。
//! 非严格脚本中的块级普通函数同样有词法绑定；Annex B 的可选外层 var 与声明求值时的复制
//! 分别记录，参数和沿途词法冲突会阻止复制。隐式 var 不伪造原始声明 occurrence。

use wake_common::{Atom, FxHashMap, FxHashSet, Interner, Span};
use wake_ecma_ast::*;

fn parameter_contains_expression(pattern: &Pattern<'_>) -> bool {
    match pattern {
        Pattern::Ident(_) => false,
        Pattern::Assignment(_) => true,
        Pattern::Rest(rest) => parameter_contains_expression(&rest.argument),
        Pattern::Array(array) => array
            .elements
            .iter()
            .flatten()
            .any(parameter_contains_expression),
        Pattern::Object(object) => {
            object
                .properties
                .iter()
                .any(|property| property.computed || parameter_contains_expression(&property.value))
                || object
                    .rest
                    .is_some_and(|rest| parameter_contains_expression(&rest.argument))
        }
    }
}

mod annex_b;
mod call_execution;
mod control_flow;
mod source_exports;
mod source_queries;
mod source_types;
pub use call_execution::{
    CallExecution, CallExecutionError, CallExecutionFacts, CallExecutionLimits,
    ExecutionRegionKind, analyze_call_execution, analyze_call_execution_with_limits,
};
pub use control_flow::{ControlFlowFacts, Fallthrough, FunctionFlow, analyze_control_flow};
use source_exports::ExportCollector;
use source_queries::QueryCollector;
pub use source_types::{
    SourceTypeInput, SourceTypeModel, TypeDeclaration, TypeDeclarationKind, TypeReference,
    TypeResolution, TypeScope, TypeScopeId, TypeScopeKind, TypeSymbol, TypeSymbolId,
    analyze_source_types,
};

fn pattern_binds_name(pattern: &Pattern<'_>, name: Atom) -> bool {
    match pattern {
        Pattern::Ident(id) => id.name == name,
        Pattern::Assignment(assignment) => pattern_binds_name(&assignment.left, name),
        Pattern::Rest(rest) => pattern_binds_name(&rest.argument, name),
        Pattern::Array(array) => array
            .elements
            .iter()
            .flatten()
            .any(|pattern| pattern_binds_name(pattern, name)),
        Pattern::Object(object) => {
            object
                .properties
                .iter()
                .any(|property| pattern_binds_name(&property.value, name))
                || object
                    .rest
                    .is_some_and(|rest| pattern_binds_name(&rest.argument, name))
        }
    }
}

fn insert_source_binding(
    model: &mut SemanticModel,
    name: Atom,
    span: Span,
    scope: ScopeId,
    decl_kind: DeclKind,
) -> SymbolId {
    let symbol = if let Some(&symbol) = model.scopes[scope as usize].bindings.get(&name) {
        symbol
    } else {
        let symbol = model.symbols.len() as SymbolId;
        model.symbols.push(Symbol {
            name,
            decl_kind,
            scope,
            span,
        });
        model.scopes[scope as usize].bindings.insert(name, symbol);
        symbol
    };
    if !model
        .binding_occurrences
        .iter()
        .any(|binding| binding.name == name && binding.span == span && binding.scope == scope)
    {
        model.binding_occurrences.push(BindingOccurrence {
            name,
            span,
            scope,
            symbol,
            decl_kind,
        });
    }
    symbol
}

fn ambient_namespace_is_eligible(namespace: &SourceNamespace, syntax: &[SourceNode]) -> bool {
    if !namespace.is_ambient || namespace.ambient.is_some() || namespace.body.is_none() {
        return false;
    }
    let mut parent = syntax.get(namespace.node).and_then(|node| node.parent);
    while let Some(index) = parent {
        if syntax
            .get(index)
            .is_some_and(|node| node.kind == SourceNodeKind::TsAmbientModule)
        {
            return false;
        }
        parent = syntax.get(index).and_then(|node| node.parent);
    }
    true
}

fn ambient_module_is_eligible(namespace: &SourceNamespace, syntax: &[SourceNode]) -> bool {
    if namespace.ambient.is_none() || namespace.body.is_none() {
        return false;
    }
    let mut parent = syntax.get(namespace.node).and_then(|node| node.parent);
    while let Some(index) = parent {
        if syntax
            .get(index)
            .is_some_and(|node| node.kind == SourceNodeKind::TsAmbientModule)
        {
            return false;
        }
        parent = syntax.get(index).and_then(|node| node.parent);
    }
    true
}

fn namespace_is_global_augmentation(namespace: &SourceNamespace, syntax: &[SourceNode]) -> bool {
    let mut parent = syntax.get(namespace.node).and_then(|node| node.parent);
    while let Some(index) = parent {
        if syntax
            .get(index)
            .is_some_and(|node| node.kind == SourceNodeKind::TsGlobalAugmentation)
        {
            return true;
        }
        parent = syntax.get(index).and_then(|node| node.parent);
    }
    false
}

fn source_binding_decl_kind(
    input: &SourceSemanticInput<'_>,
    span: Span,
    name: Atom,
    interner: &Interner,
) -> DeclKind {
    let Some(identifier) = input.identifiers.iter().find(|identifier| {
        identifier.role == SourceIdentifierRole::ValueBinding
            && identifier.span == span
            && interner.intern(&identifier.name) == name
    }) else {
        return DeclKind::Const;
    };
    if let Some(kind) = identifier.value_kind {
        return match kind {
            SourceValueBindingKind::Var => DeclKind::Var,
            SourceValueBindingKind::Let => DeclKind::Let,
            SourceValueBindingKind::Const => DeclKind::Const,
            SourceValueBindingKind::Using => DeclKind::Using,
            SourceValueBindingKind::Function => DeclKind::Function,
            SourceValueBindingKind::Class => DeclKind::Class,
        };
    }
    let first_value_binding = |container: Span| {
        input
            .identifiers
            .iter()
            .filter(|candidate| {
                candidate.role == SourceIdentifierRole::ValueBinding
                    && container.lo <= candidate.span.lo
                    && candidate.span.hi <= container.hi
            })
            .min_by_key(|candidate| (candidate.span.lo, candidate.span.hi))
            .is_some_and(|candidate| candidate.span == identifier.span)
    };
    if input.functions.iter().any(|function| {
        function.body.is_none()
            && function.scope.lo == function.span.lo
            && function.scope.lo <= span.lo
            && span.hi <= function.scope.hi
            && first_value_binding(function.scope)
    }) {
        return DeclKind::Function;
    }
    if input.syntax.iter().any(|node| {
        matches!(node.kind, SourceNodeKind::JsClass | SourceNodeKind::TsEnum)
            && node.span.lo <= span.lo
            && span.hi <= node.span.hi
            && first_value_binding(node.span)
    }) {
        return if input.syntax.iter().any(|node| {
            node.kind == SourceNodeKind::TsEnum
                && node.span.lo <= span.lo
                && span.hi <= node.span.hi
                && first_value_binding(node.span)
        }) {
            DeclKind::Const
        } else {
            DeclKind::Class
        };
    }
    DeclKind::Const
}

/// 作用域 id（`scopes` 索引）。
pub type ScopeId = u32;
/// 符号 id（`symbols` 索引）。
pub type SymbolId = u32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScopeKind {
    /// 模块/脚本顶层。
    Module,
    /// 函数体（var 提升到此）。
    Function,
    /// Immutable self-name environment surrounding a named function expression's parameters.
    FunctionName,
    /// Body var environment below parameters which contain expressions.
    FunctionBody,
    /// 块 `{}` / for / switch 等。
    Block,
    /// catch 子句。
    Catch,
    /// Class static initialization block: a separate var environment, without function parameters.
    StaticBlock,
}

/// 声明类型。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeclKind {
    Var,
    Let,
    Const,
    Function,
    Class,
    Param,
    /// Implicit function arguments object; no source declaration occurrence is fabricated.
    Arguments,
    Import,
    CatchParam,
    /// `using` / `await using` 绑定。作用域规则同 `Const`，但**带副作用**（离开作用域时
    /// 调用 dispose），故 minify 的「无引用即删」「单次引用内联」都必须放过它。
    Using,
}

#[derive(Debug)]
pub struct Scope {
    pub kind: ScopeKind,
    pub parent: Option<ScopeId>,
    /// 名字 → 符号（本作用域直接绑定）。
    pub bindings: FxHashMap<Atom, SymbolId>,
}

#[derive(Debug)]
pub struct Symbol {
    pub name: Atom,
    pub decl_kind: DeclKind,
    pub scope: ScopeId,
    pub span: Span,
}

/// One concrete declaration occurrence and the stable symbol it contributes to.
///
/// [`Symbol::span`] is the canonical declaration location.  JavaScript permits the same `var` or
/// function binding to be declared more than once, however, and parser transforms can preserve
/// each occurrence at a different source anchor.  Consumers which attach semantic identity to an
/// owned syntax tree must therefore use this occurrence table instead of guessing that every
/// declaration has the canonical span.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BindingOccurrence {
    pub name: Atom,
    pub span: Span,
    pub scope: ScopeId,
    pub symbol: SymbolId,
    pub decl_kind: DeclKind,
}

#[derive(Debug)]
pub struct Reference {
    pub name: Atom,
    pub span: Span,
    pub scope: ScopeId,
    /// 解析到的符号；`None` 表示全局/未声明。
    pub resolved: Option<SymbolId>,
    pub access: ReferenceAccess,
}

/// Evaluation of an identifier occurrence; member bases and computed keys are always reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReferenceAccess {
    Read,
    Write,
    ReadWrite,
}

impl ReferenceAccess {
    pub fn is_read(self) -> bool {
        matches!(self, Self::Read | Self::ReadWrite)
    }
    pub fn is_write(self) -> bool {
        matches!(self, Self::Write | Self::ReadWrite)
    }
}

/// 语义模型：作用域树 + 符号表 + 引用列表。
#[derive(Debug)]
pub struct SemanticModel {
    pub scopes: Vec<Scope>,
    pub symbols: Vec<Symbol>,
    pub binding_occurrences: Vec<BindingOccurrence>,
    pub references: Vec<Reference>,
    /// Implicit parameter-to-body-var initialization, without an evaluated source reference.
    pub parameter_copies: Vec<ParameterCopy>,
    /// Name-sensitive compatibility copy executed at a sloppy block function declaration.
    pub annex_b_copies: Vec<AnnexBCopy>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AnnexBCopy {
    pub span: Span,
    pub lexical: SymbolId,
    /// None when a non-strict SetMutableBinding creates the body binding at runtime.
    pub outer: Option<SymbolId>,
    pub target_scope: ScopeId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParameterCopy {
    pub parameter: SymbolId,
    pub body: SymbolId,
}

/// Original-source occurrences projected onto the same semantic symbol IDs used by compilation.
/// Erased declarations without a represented value scope remain unavailable. Type queries are
/// separate from evaluated references and retain an explicit Unavailable result when source
/// scopes are incomplete.
/// Export uses are separate from evaluated reads: exporting a live binding does not evaluate
/// its value or trigger its temporal dead zone. Only parser-owned local value exports count.
#[derive(Debug)]
pub struct SourceSemanticModel {
    pub model: SemanticModel,
    pub source_symbols: FxHashSet<SymbolId>,
    /// Symbols restored from erased declarations or source-only scopes. They have source identity
    /// for value resolution, but no executable body that rules such as Hooks can inspect.
    pub ambient_value_symbols: FxHashSet<SymbolId>,
    /// Original value declarations erased or otherwise absent from the compilation binding table.
    /// Until their complete scopes are represented, consumers must not assume these names have
    /// no value binding. Resolution applies the guard conservatively, with an exception for a
    /// separately represented local value shadowing a string ambient module declaration.
    pub incomplete_value_names: FxHashSet<Atom>,
    pub references: Vec<usize>,
    pub exports: Vec<SourceExportUse>,
    pub type_queries: Vec<SourceTypeQuery>,
}

impl SourceSemanticModel {
    /// Project source-only global ambient declarations from another file in the same lint graph.
    ///
    /// The project graph owns the cross-file name set; this method only adds synthetic ambient
    /// symbols to the current module scope and re-resolves otherwise unresolved source reads.
    /// No binding occurrence is fabricated, so declaration rules cannot report the external value
    /// as a local declaration.
    pub fn project_external_ambient_values(&mut self, names: &[String], interner: &Interner) {
        let mut projected = FxHashMap::default();
        for name in names {
            let atom = interner.intern(name);
            if self.model.scopes[0].bindings.contains_key(&atom) {
                continue;
            }
            let symbol = self.model.symbols.len() as SymbolId;
            self.model.symbols.push(Symbol {
                name: atom,
                decl_kind: DeclKind::Const,
                scope: 0,
                span: Span::DUMMY,
            });
            self.model.scopes[0].bindings.insert(atom, symbol);
            self.ambient_value_symbols.insert(symbol);
            projected.insert(atom, symbol);
        }
        if projected.is_empty() {
            return;
        }
        for index in 0..self.model.references.len() {
            let (scope, name, unresolved) = {
                let reference = &self.model.references[index];
                (
                    reference.scope,
                    reference.name,
                    reference.resolved.is_none(),
                )
            };
            if unresolved
                && let Some(symbol) = self.model.resolve_in(scope, name)
                && projected.get(&name) == Some(&symbol)
            {
                self.model.references[index].resolved = Some(symbol);
            }
        }
        for query in &mut self.type_queries {
            if query.scope.is_some()
                && !matches!(query.resolution, SourceTypeQueryResolution::Resolved(_))
                && let Some(&symbol) = projected.get(&query.name)
            {
                query.resolution = SourceTypeQueryResolution::Resolved(symbol);
            }
        }
    }
}

#[derive(Clone, Copy)]
pub struct SourceSemanticInput<'a> {
    pub identifiers: &'a [SourceIdentifier],
    pub exports: &'a [SourceExport],
    pub syntax: &'a [SourceNode],
    pub functions: &'a [SourceFunction],
    pub namespaces: &'a [SourceNamespace],
}

/// Unresolved only describes represented value environments; it is not proof of an undeclared
/// TypeScript name. Unavailable must never fall back to an identically named outer binding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceTypeQueryResolution {
    Resolved(SymbolId),
    Unresolved,
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceTypeQuery {
    pub name: Atom,
    pub span: Span,
    pub scope: Option<ScopeId>,
    pub resolution: SourceTypeQueryResolution,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceExportUseKind {
    Named,
    Declaration,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceExportUse {
    pub name: Atom,
    pub span: Span,
    pub scope: ScopeId,
    pub resolved: Option<SymbolId>,
    pub kind: SourceExportUseKind,
}

pub fn analyze_source(
    program: &Program,
    interner: &Interner,
    input: SourceSemanticInput<'_>,
) -> SourceSemanticModel {
    let needs_source_scopes = input.identifiers.iter().any(|identifier| {
        matches!(
            identifier.role,
            SourceIdentifierRole::ValueBinding | SourceIdentifierRole::EnumMemberBinding
        ) && input.syntax.iter().any(|node| {
            matches!(
                node.kind,
                SourceNodeKind::JsBlock
                    | SourceNodeKind::JsFunctionBody
                    | SourceNodeKind::JsSwitchBody
                    | SourceNodeKind::TsSignature
                    | SourceNodeKind::TsEnum
            ) && node.span.lo <= identifier.span.lo
                && identifier.span.hi <= node.span.hi
        })
    });
    let (mut model, exports, mut queries) = analyze_with_exports(
        program,
        interner,
        Some(ExportCollector::new(input.exports, interner)),
        (input
            .identifiers
            .iter()
            .any(|id| id.role == SourceIdentifierRole::TypeQuery)
            || needs_source_scopes)
            .then(|| QueryCollector::new(input, interner)),
    );
    let exports = exports.expect("source export capture was requested");
    let mut bindings = FxHashSet::default();
    let mut values = FxHashSet::default();
    for identifier in input.identifiers {
        let key = (identifier.span, interner.intern(&identifier.name));
        match identifier.role {
            SourceIdentifierRole::ValueBinding | SourceIdentifierRole::EnumMemberBinding => {
                bindings.insert(key);
            }
            SourceIdentifierRole::ValueReference => {
                values.insert(key);
                // Cover grammar can consume arrow parameters as expressions before the final
                // AST classifies them as bindings. Only real AST binding occurrences count.
                bindings.insert(key);
            }
            _ => {}
        }
    }
    // A top-level ambient declaration is erased from the executable AST, but it still creates a
    // value binding in the TypeScript source environment. Restore only declarations whose source
    // node is directly under `declare`/`declare global`; namespace members are projected into
    // their own source scopes below, while string ambient module members remain unavailable.
    // A declaration signature contributes one value binding for the function/class-like name,
    // followed by parameter bindings which belong to the erased signature scope. Keep only the
    // declaration name when projecting a top-level ambient declaration into module scope. The
    // parser records the original function span even though its body is absent, so the first
    // value binding in that span is the stable declaration anchor and the remaining bindings are
    // signature-local parameters.
    let signature_parameters: FxHashSet<_> = input
        .functions
        .iter()
        .filter(|function| function.body.is_none())
        .flat_map(|function| {
            let mut bindings: Vec<_> = input
                .identifiers
                .iter()
                .filter(|identifier| {
                    identifier.role == SourceIdentifierRole::ValueBinding
                        && function.scope.lo <= identifier.span.lo
                        && identifier.span.hi <= function.scope.hi
                })
                .map(|identifier| (identifier.span, interner.intern(&identifier.name)))
                .collect();
            bindings.sort_by_key(|(span, _)| (span.lo, span.hi));
            // A top-level `declare function` starts its source function at the `function`
            // keyword, so its name is the first binding in the same range. A method/signature
            // starts its scope after the member name; every binding in that range is therefore a
            // parameter and belongs to the synthetic signature scope.
            let first_is_declaration = usize::from(function.scope.lo == function.span.lo);
            bindings.into_iter().skip(first_is_declaration)
        })
        .collect();
    // Pure function and method type signatures are erased without a SourceFunction record, but
    // the parser still records their value parameter bindings under a TsSignature node. Assign
    // each binding to the smallest containing signature so nested signatures cannot capture or
    // project one another's parameters.
    let signature_nodes: Vec<_> = input
        .syntax
        .iter()
        .filter(|node| node.kind == SourceNodeKind::TsSignature)
        .collect();
    let mut nearest_type_signatures: FxHashMap<(Span, Atom), Span> = FxHashMap::default();
    for identifier in input.identifiers.iter().filter(|identifier| {
        identifier.role == SourceIdentifierRole::ValueBinding
            && !signature_parameters.contains(&(identifier.span, interner.intern(&identifier.name)))
    }) {
        if let Some(candidate) = signature_nodes
            .iter()
            .filter(|candidate| {
                candidate.span.lo <= identifier.span.lo && identifier.span.hi <= candidate.span.hi
            })
            .min_by_key(|candidate| candidate.span.hi - candidate.span.lo)
        {
            nearest_type_signatures.insert(
                (identifier.span, interner.intern(&identifier.name)),
                candidate.span,
            );
        }
    }
    let mut type_signature_bindings: Vec<(Span, Vec<(Span, Atom)>)> = Vec::new();
    let mut type_signature_parameters = FxHashSet::default();
    for node in signature_nodes {
        let bindings: Vec<_> = input
            .identifiers
            .iter()
            .filter_map(|identifier| {
                let binding = (identifier.span, interner.intern(&identifier.name));
                (identifier.role == SourceIdentifierRole::ValueBinding
                    && nearest_type_signatures.get(&binding) == Some(&node.span))
                .then_some(binding)
            })
            .collect();
        type_signature_parameters.extend(bindings.iter().copied());
        // Keep an explicit synthetic scope even when the signature has no value parameters.
        // Such a signature still needs to resolve complete enclosing value bindings (for
        // example `type Fn = () => typeof outer`), while the unavailable-region guard below
        // continues to reject names whose source environment is missing.
        type_signature_bindings.push((node.span, bindings));
    }
    let mut signature_parameters = signature_parameters;
    signature_parameters.extend(type_signature_parameters);
    let mut ambient_value_symbols = FxHashSet::default();
    // A declaration overload is erased to an empty executable statement, so the ordinary
    // resolver never creates its parameter scope. Recreate only the source signature scope;
    // parameters remain local to that signature and cannot leak into the module or another
    // overload. Pure type signatures use the same synthetic representation when the parser
    // supplied explicit value parameter bindings; signatures without those facts remain
    // unavailable.
    let mut signature_scopes = Vec::new();
    for function in input
        .functions
        .iter()
        .filter(|function| function.body.is_none())
    {
        let bindings: Vec<_> = input
            .identifiers
            .iter()
            .filter(|identifier| {
                identifier.role == SourceIdentifierRole::ValueBinding
                    && signature_parameters
                        .contains(&(identifier.span, interner.intern(&identifier.name)))
                    && function.scope.lo <= identifier.span.lo
                    && identifier.span.hi <= function.scope.hi
            })
            .collect();
        if bindings.is_empty() {
            continue;
        }
        let (parent, parent_depth) = queries
            .as_ref()
            .and_then(|queries| queries.enclosing_scope(function.scope))
            .unwrap_or((0, 1));
        let scope = model.scopes.len() as ScopeId;
        model.scopes.push(Scope {
            kind: ScopeKind::Function,
            parent: Some(parent),
            bindings: FxHashMap::default(),
        });
        for identifier in bindings {
            let atom = interner.intern(&identifier.name);
            insert_source_binding(&mut model, atom, identifier.span, scope, DeclKind::Param);
        }
        signature_scopes.push((function.scope, scope, parent_depth + 1));
    }
    for (span, bindings) in type_signature_bindings {
        let (parent, parent_depth) = queries
            .as_ref()
            .and_then(|queries| queries.enclosing_scope(span))
            .unwrap_or((0, 1));
        let scope = model.scopes.len() as ScopeId;
        model.scopes.push(Scope {
            kind: ScopeKind::Function,
            parent: Some(parent),
            bindings: FxHashMap::default(),
        });
        for (binding_span, name) in bindings {
            insert_source_binding(&mut model, name, binding_span, scope, DeclKind::Param);
        }
        signature_scopes.push((span, scope, parent_depth + 1));
    }
    // Ordinary blocks survive lowering even when a direct `declare` member inside them is
    // erased. Reuse the resolver's existing block scope so the source binding is visible only
    // inside that block and remains absent from the enclosing module scope.
    let mut block_bindings: FxHashMap<Span, Vec<(Span, Atom)>> = FxHashMap::default();
    for identifier in input.identifiers.iter().filter(|identifier| {
        identifier.role == SourceIdentifierRole::ValueBinding
            && !signature_parameters.contains(&(identifier.span, interner.intern(&identifier.name)))
    }) {
        let in_declare = input.syntax.iter().any(|node| {
            node.kind == SourceNodeKind::TsDeclare
                && node.span.lo <= identifier.span.lo
                && identifier.span.hi <= node.span.hi
        });
        if !in_declare
            || input.syntax.iter().any(|node| {
                matches!(
                    node.kind,
                    SourceNodeKind::TsAmbientModule
                        | SourceNodeKind::TsNamespace
                        | SourceNodeKind::TsSignature
                ) && node.span.lo <= identifier.span.lo
                    && identifier.span.hi <= node.span.hi
            })
        {
            continue;
        }
        let Some(block) = input
            .syntax
            .iter()
            .filter(|node| {
                node.kind == SourceNodeKind::JsBlock
                    && node.span.lo <= identifier.span.lo
                    && identifier.span.hi <= node.span.hi
            })
            .min_by_key(|node| node.span.hi - node.span.lo)
        else {
            continue;
        };
        block_bindings
            .entry(block.span)
            .or_default()
            .push((identifier.span, interner.intern(&identifier.name)));
    }
    if let Some(queries) = queries.as_ref() {
        for (block, bindings) in block_bindings {
            let Some(scope) = queries.scope_for(block) else {
                continue;
            };
            for (span, name) in bindings {
                let decl_kind = source_binding_decl_kind(&input, span, name, interner);
                let symbol = insert_source_binding(&mut model, name, span, scope, decl_kind);
                ambient_value_symbols.insert(symbol);
            }
        }
    }
    // A function body remains represented in the resolver even when a direct `declare` member
    // inside it is erased. Reuse that existing function-body scope; nested blocks are handled by
    // the block projection above and must not be flattened into the function scope.
    let mut function_body_bindings: FxHashMap<Span, Vec<(Span, Atom)>> = FxHashMap::default();
    for identifier in input.identifiers.iter().filter(|identifier| {
        identifier.role == SourceIdentifierRole::ValueBinding
            && !signature_parameters.contains(&(identifier.span, interner.intern(&identifier.name)))
    }) {
        let in_declare = input.syntax.iter().any(|node| {
            node.kind == SourceNodeKind::TsDeclare
                && node.span.lo <= identifier.span.lo
                && identifier.span.hi <= node.span.hi
        });
        if !in_declare {
            continue;
        }
        let Some(body) = input
            .syntax
            .iter()
            .filter(|node| {
                node.kind == SourceNodeKind::JsFunctionBody
                    && node.span.lo <= identifier.span.lo
                    && identifier.span.hi <= node.span.hi
            })
            .min_by_key(|node| node.span.hi - node.span.lo)
        else {
            continue;
        };
        if input.syntax.iter().any(|node| {
            node.kind == SourceNodeKind::JsBlock
                && node.span.lo <= identifier.span.lo
                && identifier.span.hi <= node.span.hi
                && node.span.hi - node.span.lo < body.span.hi - body.span.lo
        }) {
            continue;
        }
        function_body_bindings
            .entry(body.span)
            .or_default()
            .push((identifier.span, interner.intern(&identifier.name)));
    }
    if let Some(queries) = queries.as_ref() {
        for (body, bindings) in function_body_bindings {
            let Some(scope) = queries.scope_for(body) else {
                continue;
            };
            for (span, name) in bindings {
                let decl_kind = source_binding_decl_kind(&input, span, name, interner);
                let symbol = insert_source_binding(&mut model, name, span, scope, decl_kind);
                ambient_value_symbols.insert(symbol);
            }
        }
    }
    // Switch cases share the resolver's switch block scope even when a direct `declare` member
    // is erased. Select the nearest switch environment and keep nested blocks/functions on their
    // own projection paths.
    let mut switch_bindings: Vec<(Span, Atom)> = Vec::new();
    for identifier in input.identifiers.iter().filter(|identifier| {
        identifier.role == SourceIdentifierRole::ValueBinding
            && !signature_parameters.contains(&(identifier.span, interner.intern(&identifier.name)))
    }) {
        let in_declare = input.syntax.iter().any(|node| {
            node.kind == SourceNodeKind::TsDeclare
                && node.span.lo <= identifier.span.lo
                && identifier.span.hi <= node.span.hi
        });
        if !in_declare {
            continue;
        }
        let Some(switch) = input
            .syntax
            .iter()
            .filter(|node| {
                node.kind == SourceNodeKind::JsSwitchBody
                    && node.span.lo <= identifier.span.lo
                    && identifier.span.hi <= node.span.hi
            })
            .min_by_key(|node| node.span.hi - node.span.lo)
        else {
            continue;
        };
        if input.syntax.iter().any(|node| {
            matches!(
                node.kind,
                SourceNodeKind::JsBlock
                    | SourceNodeKind::JsFunctionBody
                    | SourceNodeKind::TsAmbientModule
                    | SourceNodeKind::TsNamespace
                    | SourceNodeKind::TsSignature
            ) && node.span.lo <= identifier.span.lo
                && identifier.span.hi <= node.span.hi
                && node.span.hi - node.span.lo < switch.span.hi - switch.span.lo
        }) {
            continue;
        }
        switch_bindings.push((identifier.span, interner.intern(&identifier.name)));
    }
    if let Some(queries) = queries.as_ref() {
        for (span, name) in switch_bindings {
            let Some((scope, _)) = queries.enclosing_scope(span) else {
                continue;
            };
            let decl_kind = source_binding_decl_kind(&input, span, name, interner);
            let symbol = insert_source_binding(&mut model, name, span, scope, decl_kind);
            ambient_value_symbols.insert(symbol);
        }
    }
    let ambient_bindings: Vec<_> = input
        .identifiers
        .iter()
        .filter(|identifier| {
            identifier.role == SourceIdentifierRole::ValueBinding
                && !signature_parameters
                    .contains(&(identifier.span, interner.intern(&identifier.name)))
                && input
                    .syntax
                    .iter()
                    .filter(|node| {
                        node.span.lo <= identifier.span.lo && identifier.span.hi <= node.span.hi
                    })
                    .any(|node| {
                        matches!(
                            node.kind,
                            SourceNodeKind::TsDeclare | SourceNodeKind::TsGlobalAugmentation
                        )
                    })
                && !input
                    .syntax
                    .iter()
                    .filter(|node| {
                        node.span.lo <= identifier.span.lo && identifier.span.hi <= node.span.hi
                    })
                    .any(|node| {
                        matches!(
                            node.kind,
                            SourceNodeKind::TsAmbientModule
                                | SourceNodeKind::TsNamespace
                                | SourceNodeKind::JsBlock
                                | SourceNodeKind::JsFunctionBody
                        )
                    })
        })
        .map(|identifier| (identifier.span, interner.intern(&identifier.name)))
        .collect();
    for (span, name) in ambient_bindings {
        if let Some(existing_symbol) = model
            .binding_occurrences
            .iter()
            .find(|binding| binding.span == span && binding.name == name)
            .map(|binding| binding.symbol)
        {
            if ambient_value_symbols.contains(&existing_symbol) {
                let decl_kind = source_binding_decl_kind(&input, span, name, interner);
                let symbol = insert_source_binding(&mut model, name, span, 0, decl_kind);
                ambient_value_symbols.insert(symbol);
            }
            ambient_value_symbols.insert(existing_symbol);
            continue;
        }
        if model.scopes[0].bindings.contains_key(&name) {
            // A module-local declaration/import already owns this spelling in the module
            // scope. `declare global` is a separate source layer and must not relabel the
            // local SymbolId as ambient; the local binding continues to shadow the global one.
            let existing_symbol = model.scopes[0].bindings[&name];
            if ambient_value_symbols.contains(&existing_symbol) {
                let decl_kind = source_binding_decl_kind(&input, span, name, interner);
                let symbol = insert_source_binding(&mut model, name, span, 0, decl_kind);
                ambient_value_symbols.insert(symbol);
            }
            continue;
        }
        let decl_kind = source_binding_decl_kind(&input, span, name, interner);
        let symbol = model.symbols.len() as SymbolId;
        model.symbols.push(Symbol {
            name,
            decl_kind,
            scope: 0,
            span,
        });
        model.scopes[0].bindings.insert(name, symbol);
        model.binding_occurrences.push(BindingOccurrence {
            name,
            span,
            scope: 0,
            symbol,
            decl_kind,
        });
        ambient_value_symbols.insert(symbol);
    }

    // String ambient modules are erased as executable statements, but direct value members still
    // form an isolated external-module source scope. Unknown names remain unavailable because
    // resolving the module's external identity belongs to the project graph.
    let mut ambient_module_ids: FxHashMap<wake_common::JsString, ScopeId> = FxHashMap::default();
    let mut ambient_module_scopes: Vec<(Span, ScopeId)> = Vec::new();
    for namespace in input
        .namespaces
        .iter()
        .filter(|namespace| ambient_module_is_eligible(namespace, input.syntax))
    {
        let Some(body) = namespace.body else {
            continue;
        };
        let module_name = namespace
            .ambient
            .as_ref()
            .expect("eligible ambient module has a specifier")
            .value
            .clone();
        let scope = if let Some(&scope) = ambient_module_ids.get(&module_name) {
            scope
        } else {
            let scope = model.scopes.len() as ScopeId;
            model.scopes.push(Scope {
                kind: ScopeKind::Module,
                parent: None,
                bindings: FxHashMap::default(),
            });
            ambient_module_ids.insert(module_name, scope);
            scope
        };
        for identifier in input.identifiers.iter().filter(|identifier| {
            identifier.role == SourceIdentifierRole::ValueBinding
                && body.lo <= identifier.span.lo
                && identifier.span.hi <= body.hi
                && input.syntax.iter().all(|node| {
                    if !(node.span.lo <= identifier.span.lo && identifier.span.hi <= node.span.hi) {
                        return true;
                    }
                    if node.kind == SourceNodeKind::TsAmbientModule {
                        return node.span == input.syntax[namespace.node].span;
                    }
                    !matches!(
                        node.kind,
                        SourceNodeKind::JsBlock
                            | SourceNodeKind::JsFunctionBody
                            | SourceNodeKind::JsSwitchBody
                            | SourceNodeKind::TsNamespace
                            | SourceNodeKind::TsSignature
                    )
                })
        }) {
            let atom = interner.intern(&identifier.name);
            let decl_kind = source_binding_decl_kind(&input, identifier.span, atom, interner);
            let symbol = insert_source_binding(&mut model, atom, identifier.span, scope, decl_kind);
            ambient_value_symbols.insert(symbol);
        }
        ambient_module_scopes.push((body, scope));
    }

    // Ambient namespaces are erased as executable statements, but their value roots and direct
    // members still form nested source scopes. Build those scopes from parser-owned namespace
    // containers before resolving source type queries.
    let eligible_namespaces: Vec<_> = input
        .namespaces
        .iter()
        .filter(|namespace| ambient_namespace_is_eligible(namespace, input.syntax))
        .collect();
    // Runtime namespace declarations are emitted as IIFEs. Their parameter scope is the
    // namespace value scope, so register those roots before projecting ambient declarations.
    // This lets a same-name `declare namespace` reuse the runtime value scope and preserves
    // TypeScript's declaration merging across declaration kind and source order.
    let mut runtime_namespace_scopes: FxHashMap<(ScopeId, Atom), ScopeId> = FxHashMap::default();
    for namespace in input.namespaces.iter().filter(|namespace| {
        !namespace.is_ambient && namespace.ambient.is_none() && namespace.body.is_some()
    }) {
        let mut parent_scope = 0;
        for (index, name) in namespace.names.iter().enumerate() {
            let atom = interner.intern(&name.name);
            let scope = model
                .binding_occurrences
                .iter()
                .find(|binding| {
                    binding.name == atom
                        && binding.span == Span::at(name.span.hi)
                        && binding.decl_kind == DeclKind::Param
                })
                .map(|binding| binding.scope)
                .or_else(|| {
                    if index + 1 == namespace.names.len() {
                        namespace
                            .names
                            .last()
                            .and_then(|last| {
                                let last_atom = interner.intern(&last.name);
                                model.binding_occurrences.iter().find(|binding| {
                                    binding.name == last_atom
                                        && binding.span == Span::at(last.span.hi)
                                        && binding.decl_kind == DeclKind::Param
                                })
                            })
                            .map(|binding| binding.scope)
                    } else {
                        None
                    }
                });
            let Some(scope) = scope else {
                break;
            };
            let canonical_scope = *runtime_namespace_scopes
                .entry((parent_scope, atom))
                .or_insert(scope);
            parent_scope = canonical_scope;
        }
    }
    let global_scope = input
        .namespaces
        .iter()
        .any(|namespace| namespace_is_global_augmentation(namespace, input.syntax))
        .then(|| {
            let scope = model.scopes.len() as ScopeId;
            model.scopes.push(Scope {
                kind: ScopeKind::Block,
                parent: None,
                bindings: FxHashMap::default(),
            });
            scope
        });
    let mut namespace_scopes: FxHashMap<(ScopeId, Atom), ScopeId> = FxHashMap::default();
    let mut namespace_nodes: FxHashMap<usize, (ScopeId, usize)> = FxHashMap::default();
    let mut projected_namespaces = Vec::new();
    for namespace in &eligible_namespaces {
        // Walk through declaration wrappers (`declare namespace` has a TsDeclare parent) until
        // the nearest namespace ancestor. Runtime namespaces are projected from their emitted
        // IIFE parameter below, so recover that parameter scope here before creating the ambient
        // child and prevent the child from leaking into the module root.
        let mut ancestor = input
            .syntax
            .get(namespace.node)
            .and_then(|node| node.parent);
        let is_global = namespace_is_global_augmentation(namespace, input.syntax);
        let mut parent_scope = if is_global {
            global_scope.expect("global scope")
        } else {
            0
        };
        let mut depth = 1;
        while let Some(parent) = ancestor {
            if let Some((scope, parent_depth)) = namespace_nodes.get(&parent).copied() {
                parent_scope = scope;
                depth = parent_depth;
                break;
            }
            if let Some(parent_namespace) = input.namespaces.iter().find(|candidate| {
                candidate.node == parent && !candidate.is_ambient && candidate.ambient.is_none()
            }) && let Some(name) = parent_namespace.names.last()
            {
                let atom = interner.intern(&name.name);
                if let Some(binding) = model.binding_occurrences.iter().find(|binding| {
                    binding.name == atom
                        && binding.span == Span::at(name.span.hi)
                        && binding.decl_kind == DeclKind::Param
                }) {
                    parent_scope = binding.scope;
                    depth = 1;
                    break;
                }
            }
            ancestor = input.syntax.get(parent).and_then(|node| node.parent);
        }
        for (index, name) in namespace.names.iter().enumerate() {
            let atom = interner.intern(&name.name);
            let decl_kind = source_binding_decl_kind(&input, name.span, atom, interner);
            let symbol =
                insert_source_binding(&mut model, atom, name.span, parent_scope, decl_kind);
            ambient_value_symbols.insert(symbol);
            if is_global && parent_scope == global_scope.expect("global scope") && index == 0 {
                model.scopes[0].bindings.entry(atom).or_insert(symbol);
            }
            let child_scope = if let Some(&scope) = namespace_scopes.get(&(parent_scope, atom)) {
                scope
            } else if !is_global
                && let Some(&scope) = runtime_namespace_scopes.get(&(parent_scope, atom))
            {
                namespace_scopes.insert((parent_scope, atom), scope);
                scope
            } else {
                let scope = model.scopes.len() as ScopeId;
                model.scopes.push(Scope {
                    kind: ScopeKind::Block,
                    parent: Some(parent_scope),
                    bindings: FxHashMap::default(),
                });
                namespace_scopes.insert((parent_scope, atom), scope);
                scope
            };
            parent_scope = child_scope;
            depth += 1;
        }
        namespace_nodes.insert(namespace.node, (parent_scope, depth));
        if let Some(body) = namespace.body {
            projected_namespaces.push((namespace.node, body, parent_scope, depth));
        }
    }
    // Runtime namespaces lower to an IIFE whose parameter keeps the original namespace name
    // span. Reuse that parser-owned parameter binding as the bridge to the generated function
    // scope; the source body itself has no executable AST node after lowering. This lets erased
    // declarations inside a runtime namespace shadow outer values without fabricating a second
    // namespace root or changing the emitted program.
    for namespace in input.namespaces.iter().filter(|namespace| {
        !namespace.is_ambient && namespace.ambient.is_none() && namespace.body.is_some()
    }) {
        let Some(body) = namespace.body else {
            continue;
        };
        if namespace.names.is_empty() {
            continue;
        }
        let mut parent_scope = 0;
        let mut canonical_scope = None;
        for name in &namespace.names {
            let atom = interner.intern(&name.name);
            let Some(scope) = runtime_namespace_scopes
                .get(&(parent_scope, atom))
                .copied()
                .or_else(|| {
                    model
                        .binding_occurrences
                        .iter()
                        .find(|binding| {
                            binding.name == atom
                                && binding.span == Span::at(name.span.hi)
                                && binding.decl_kind == DeclKind::Param
                        })
                        .map(|binding| binding.scope)
                })
            else {
                canonical_scope = None;
                break;
            };
            canonical_scope = Some(scope);
            parent_scope = scope;
        }
        let Some(scope) = canonical_scope else {
            continue;
        };
        projected_namespaces.push((namespace.node, body, scope, namespace.names.len()));
    }
    for identifier in input
        .identifiers
        .iter()
        .filter(|identifier| identifier.role == SourceIdentifierRole::ValueBinding)
    {
        if signature_parameters.contains(&(identifier.span, interner.intern(&identifier.name))) {
            continue;
        }
        let atom = interner.intern(&identifier.name);
        let source_scope = queries
            .as_ref()
            .and_then(|queries| queries.enclosing_scope(identifier.span))
            .map(|(scope, _)| scope);
        if let Some(source_scope) = source_scope
            && model.binding_occurrences.iter().any(|binding| {
                binding.span == identifier.span
                    && binding.name == atom
                    && binding.scope == source_scope
            })
        {
            // Resolver-owned loop/for-in/for-of bindings already live in their own source scope.
            // Copying them into the namespace root would make a header binding visible after the
            // loop. Erased declarations have no occurrence here and are still projected below.
            let source_scope_is_namespace = projected_namespaces
                .iter()
                .any(|(_, _, namespace_scope, _)| *namespace_scope == source_scope);
            if !source_scope_is_namespace {
                continue;
            }
        }
        // A catch parameter is declared on the catch clause, outside its body `JsBlock`.
        // Runtime namespace projection must leave it in the resolver-owned Catch scope rather
        // than copying it into the namespace scope.
        if queries.as_ref().is_some_and(|queries| {
            queries
                .enclosing_scope(identifier.span)
                .is_some_and(|(scope, _)| model.scopes[scope as usize].kind == ScopeKind::Catch)
        }) {
            continue;
        }
        let Some((_, _, scope, _)) = projected_namespaces
            .iter()
            .filter(|(_, body, _, _)| {
                body.lo <= identifier.span.lo && identifier.span.hi <= body.hi
            })
            .min_by_key(|(_, body, _, _)| body.hi - body.lo)
        else {
            continue;
        };
        if input.syntax.iter().any(|node| {
            node.span.lo <= identifier.span.lo
                && identifier.span.hi <= node.span.hi
                && matches!(
                    node.kind,
                    SourceNodeKind::TsAmbientModule
                        | SourceNodeKind::JsBlock
                        | SourceNodeKind::JsFunctionBody
                        | SourceNodeKind::JsSwitchBody
                )
        }) {
            continue;
        }
        let decl_kind = source_binding_decl_kind(&input, identifier.span, atom, interner);
        let symbol = insert_source_binding(&mut model, atom, identifier.span, *scope, decl_kind);
        ambient_value_symbols.insert(symbol);
    }
    // Enum member initializers run in a source-only lexical environment. The lowered IIFE keeps
    // the enum object but cannot represent bare member names, so project parser-owned member
    // identities into a dedicated scope without adding them to the enclosing module.
    let mut enum_scopes: Vec<(Span, ScopeId, usize)> = Vec::new();
    for node in input
        .syntax
        .iter()
        .filter(|node| node.kind == SourceNodeKind::TsEnum)
    {
        let container = projected_namespaces
            .iter()
            .filter(|(_, body, _, _)| body.lo <= node.span.lo && node.span.hi <= body.hi)
            .map(|(_, body, scope, depth)| (body.hi - body.lo, *scope, *depth))
            .chain(
                ambient_module_scopes
                    .iter()
                    .filter(|(body, _)| body.lo <= node.span.lo && node.span.hi <= body.hi)
                    .map(|(body, scope)| (body.hi - body.lo, *scope, 2)),
            )
            .min_by_key(|(size, _, _)| *size);
        let (parent, depth) = container
            .map(|(_, scope, depth)| (scope, depth + 1))
            .or_else(|| {
                queries
                    .as_ref()
                    .and_then(|queries| queries.enclosing_scope(node.span))
                    .map(|(scope, depth)| (scope, depth + 1))
            })
            .unwrap_or((0, 2));
        let scope = model.scopes.len() as ScopeId;
        model.scopes.push(Scope {
            kind: ScopeKind::Block,
            parent: Some(parent),
            bindings: FxHashMap::default(),
        });
        for identifier in input.identifiers.iter().filter(|identifier| {
            identifier.role == SourceIdentifierRole::EnumMemberBinding
                && node.span.lo <= identifier.span.lo
                && identifier.span.hi <= node.span.hi
        }) {
            let atom = interner.intern(&identifier.name);
            let symbol =
                insert_source_binding(&mut model, atom, identifier.span, scope, DeclKind::Const);
            ambient_value_symbols.insert(symbol);
        }
        enum_scopes.push((node.span, scope, depth));
    }
    // Resolver regions inside an erased ambient container otherwise retain the module parent
    // because the container has no executable AST node. Re-parent represented block/function
    // scopes to the nearest projected namespace/module while preserving a function's parameter
    // scope above its body scope.
    if let Some(queries) = queries.as_ref() {
        let container_scope = |span: Span| {
            projected_namespaces
                .iter()
                .filter(|(_, body, _, _)| body.lo <= span.lo && span.hi <= body.hi)
                .map(|(_, body, scope, _)| (body.hi - body.lo, *scope))
                .chain(
                    ambient_module_scopes
                        .iter()
                        .filter(|(body, _)| body.lo <= span.lo && span.hi <= body.hi)
                        .map(|(body, scope)| (body.hi - body.lo, *scope)),
                )
                .min_by_key(|(size, _)| *size)
                .map(|(_, scope)| scope)
        };
        for function in input
            .functions
            .iter()
            .filter(|function| function.body.is_some())
        {
            let Some(parent) = container_scope(function.span) else {
                continue;
            };
            if let Some((scope, _)) = queries.enclosing_scope(function.scope) {
                // A function region is recorded on the function scope itself. Preserve an
                // already represented lexical parent (static block, ordinary block, catch or
                // enclosing function) so namespace projection cannot erase captures. Only a
                // root module scope needs to be reparented into an ambient module container.
                let current_parent = model.scopes[scope as usize].parent;
                let can_reparent = current_parent.is_none_or(|current| {
                    current == parent || model.scopes[current as usize].kind == ScopeKind::Module
                });
                if can_reparent {
                    model.scopes[scope as usize].parent = Some(parent);
                }
            }
        }
        for node in input.syntax.iter().filter(|node| {
            matches!(
                node.kind,
                SourceNodeKind::JsBlock
                    | SourceNodeKind::JsFunctionBody
                    | SourceNodeKind::JsSwitchBody
            )
        }) {
            let Some(parent) = container_scope(node.span) else {
                continue;
            };
            if node.kind == SourceNodeKind::JsFunctionBody {
                continue;
            }
            let scope = queries.scope_for(node.span).or_else(|| {
                (node.kind == SourceNodeKind::JsSwitchBody)
                    .then(|| {
                        input
                            .identifiers
                            .iter()
                            .find(|identifier| {
                                identifier.role == SourceIdentifierRole::ValueBinding
                                    && node.span.lo <= identifier.span.lo
                                    && identifier.span.hi <= node.span.hi
                            })
                            .and_then(|identifier| {
                                queries
                                    .enclosing_scope(identifier.span)
                                    .map(|(scope, _)| scope)
                            })
                    })
                    .flatten()
            });
            if let Some(scope) = scope {
                // Reparent top-level source containers into an erased namespace, but preserve a
                // resolver-owned loop/catch/inner-block parent that is already part of the source
                // scope tree.
                let current_parent = model.scopes[scope as usize].parent;
                let can_reparent = current_parent.is_none_or(|current| {
                    current == parent
                        || matches!(
                            model.scopes[current as usize].kind,
                            ScopeKind::Module | ScopeKind::Function | ScopeKind::FunctionBody
                        )
                });
                if can_reparent {
                    model.scopes[scope as usize].parent = Some(parent);
                }
            }
        }
    }
    // Synthetic signatures are created before erased namespace/module containers are projected
    // so their parameter identities are available to all later passes. Re-parent those scopes to
    // the nearest represented erased container now, preserving namespace/module shadowing.
    for (span, scope, _) in &signature_scopes {
        if let Some((_, _body, parent, _)) = projected_namespaces
            .iter()
            .filter(|(_, body, _, _)| body.lo <= span.lo && span.hi <= body.hi)
            .min_by_key(|(_, body, _, _)| body.hi - body.lo)
        {
            model.scopes[*scope as usize].parent = Some(*parent);
        } else if let Some((_, parent)) = ambient_module_scopes
            .iter()
            .filter(|(body, _)| body.lo <= span.lo && span.hi <= body.hi)
            .min_by_key(|(body, _)| body.hi - body.lo)
        {
            model.scopes[*scope as usize].parent = Some(*parent);
        }
    }
    if let Some(queries) = queries.as_mut() {
        for (body, scope) in &ambient_module_scopes {
            queries.ambient_module(*body, *scope, 2);
        }
        for (span, scope, depth) in &signature_scopes {
            queries.signature(*span, *scope, *depth);
        }
        for (node, body, scope, depth) in &projected_namespaces {
            queries.namespace(input.syntax[*node].span, *body, *scope, *depth);
        }
        for (span, scope, depth) in &enum_scopes {
            queries.source_scope(*span, *scope, *depth);
        }
    }
    // Source references to erased declarations can be absent from the lowered executable scope or
    // initially resolve to an outer binding before a block/signature projection is installed.
    // Re-run every exact source value occurrence through the now-complete represented scope so a
    // restored inner binding can shadow that provisional resolution; names with no represented
    // target remain unresolved.
    for index in 0..model.references.len() {
        let (span, name, scope) = {
            let reference = &model.references[index];
            (reference.span, reference.name, reference.scope)
        };
        if values.contains(&(span, name)) {
            // Enum initializers are lowered into an IIFE, whose resolver scope cannot see the
            // source-only enum member environment. Reparent exact source references to the
            // nearest enum scope before resolving, just as type queries use `source_scope`.
            let source_scope = enum_scopes
                .iter()
                .filter(|(enum_span, _, _)| enum_span.lo <= span.lo && span.hi <= enum_span.hi)
                .min_by_key(|(enum_span, _, _)| enum_span.hi - enum_span.lo)
                .map_or(scope, |(_, scope, _)| *scope);
            if let Some(symbol) = model.resolve_in(source_scope, name) {
                model.references[index].scope = source_scope;
                model.references[index].resolved = Some(symbol);
            }
        }
    }
    let source_symbols: FxHashSet<SymbolId> = model
        .binding_occurrences
        .iter()
        .filter(|binding| bindings.contains(&(binding.span, binding.name)))
        .map(|binding| binding.symbol)
        .collect();
    let retained_bindings: FxHashSet<_> = model
        .binding_occurrences
        .iter()
        .map(|binding| (binding.span, binding.name))
        .collect();
    let incomplete_value_names = input
        .identifiers
        .iter()
        .filter(|id| id.role == SourceIdentifierRole::ValueBinding)
        .map(|id| (id.span, interner.intern(&id.name)))
        .filter(|binding| !retained_bindings.contains(binding))
        .map(|(_, name)| name)
        .collect();
    let incomplete_external_names: FxHashSet<_> = input
        .identifiers
        .iter()
        .filter(|id| {
            id.role == SourceIdentifierRole::ValueBinding
                && input.syntax.iter().any(|node| {
                    node.kind == SourceNodeKind::TsAmbientModule
                        && node.span.lo <= id.span.lo
                        && id.span.hi <= node.span.hi
                })
        })
        .map(|id| interner.intern(&id.name))
        .collect();
    let incomplete_local_regions: Vec<_> = input
        .identifiers
        .iter()
        .filter(|id| {
            id.role == SourceIdentifierRole::ValueBinding
                && !retained_bindings.contains(&(id.span, interner.intern(&id.name)))
                && !input.syntax.iter().any(|node| {
                    node.kind == SourceNodeKind::TsAmbientModule
                        && node.span.lo <= id.span.lo
                        && id.span.hi <= node.span.hi
                })
        })
        .filter_map(|id| {
            input
                .syntax
                .iter()
                .filter(|node| {
                    matches!(
                        node.kind,
                        SourceNodeKind::JsBlock
                            | SourceNodeKind::JsFunctionBody
                            | SourceNodeKind::TsNamespace
                            | SourceNodeKind::TsSignature
                    ) && node.span.lo <= id.span.lo
                        && id.span.hi <= node.span.hi
                })
                .min_by_key(|node| node.span.hi - node.span.lo)
                .map(|node| (node.span, interner.intern(&id.name)))
        })
        .collect();
    let references = model
        .references
        .iter()
        .enumerate()
        .filter(|(_, reference)| {
            values.contains(&(reference.span, reference.name))
                && !exports.is_named(reference.span, reference.name)
        })
        .map(|(index, _)| index)
        .collect();
    let exports = exports.finish(&model, &source_symbols);
    let type_queries = queries.map_or_else(Vec::new, |queries| {
        queries.finish(
            &model,
            &source_symbols,
            &incomplete_value_names,
            &incomplete_local_regions,
            &incomplete_external_names,
        )
    });
    SourceSemanticModel {
        model,
        source_symbols,
        ambient_value_symbols,
        incomplete_value_names,
        references,
        exports,
        type_queries,
    }
}

impl SemanticModel {
    /// 未解析（全局/未声明）的引用数。
    pub fn unresolved_count(&self) -> usize {
        self.references
            .iter()
            .filter(|r| r.resolved.is_none())
            .count()
    }

    /// 在 `scope` 及其祖先中查找名字对应的符号。
    pub fn resolve_in(&self, mut scope: ScopeId, name: Atom) -> Option<SymbolId> {
        loop {
            if let Some(&sym) = self.scopes[scope as usize].bindings.get(&name) {
                return Some(sym);
            }
            scope = self.scopes[scope as usize].parent?;
        }
    }
}

/// Analyze using the program's Interner, including language-defined implicit bindings.
pub fn analyze(program: &Program, interner: &Interner) -> SemanticModel {
    analyze_with_exports(program, interner, None, None).0
}

fn analyze_with_exports(
    program: &Program,
    interner: &Interner,
    exports: Option<ExportCollector>,
    queries: Option<QueryCollector>,
) -> (
    SemanticModel,
    Option<ExportCollector>,
    Option<QueryCollector>,
) {
    let mut r = Resolver {
        scopes: Vec::new(),
        symbols: Vec::new(),
        binding_occurrences: Vec::new(),
        binding_occurrence_keys: FxHashSet::default(),
        references: Vec::new(),
        parameter_copies: Vec::new(),
        annex_b_copies: Vec::new(),
        annex_b_targets: FxHashMap::default(),
        stack: Vec::new(),
        exports,
        queries,
        strict: program.strict,
        arguments_name: interner.intern("arguments"),
    };
    let module = r.push_scope(ScopeKind::Module, None);
    r.stack.push(module);
    // 顶层提升。
    r.hoist(&program.body, module);
    // Lexical/module bindings are instantiated before any statement executes. Besides ordinary
    // TDZ correctness, this is required by transform-generated dormant declarations whose
    // binding deliberately affects expressions that precede the declaration node.
    r.predeclare_lexical(&program.body, module);
    r.prepare_annex_b(&program.body, module, &[]);
    for stmt in program.body.iter() {
        r.visit_statement(stmt);
    }
    r.stack.pop();
    (
        SemanticModel {
            scopes: r.scopes,
            symbols: r.symbols,
            binding_occurrences: r.binding_occurrences,
            references: r.references,
            parameter_copies: r.parameter_copies,
            annex_b_copies: r.annex_b_copies,
        },
        r.exports,
        r.queries,
    )
}

struct Resolver {
    scopes: Vec<Scope>,
    symbols: Vec<Symbol>,
    binding_occurrences: Vec<BindingOccurrence>,
    binding_occurrence_keys: FxHashSet<(Atom, Span, ScopeId, SymbolId)>,
    references: Vec<Reference>,
    parameter_copies: Vec<ParameterCopy>,
    annex_b_copies: Vec<AnnexBCopy>,
    annex_b_targets: FxHashMap<Span, (ScopeId, Option<SymbolId>)>,
    stack: Vec<ScopeId>,
    exports: Option<ExportCollector>,
    queries: Option<QueryCollector>,
    strict: bool,
    arguments_name: Atom,
}

impl Resolver {
    fn cur_scope(&self) -> ScopeId {
        *self.stack.last().unwrap()
    }

    fn push_scope(&mut self, kind: ScopeKind, parent: Option<ScopeId>) -> ScopeId {
        let id = self.scopes.len() as ScopeId;
        self.scopes.push(Scope {
            kind,
            parent,
            bindings: FxHashMap::default(),
        });
        id
    }

    fn enter(&mut self, kind: ScopeKind) -> ScopeId {
        let parent = self.cur_scope();
        let id = self.push_scope(kind, Some(parent));
        self.stack.push(id);
        id
    }

    fn exit(&mut self) {
        self.stack.pop();
    }

    fn source_region(&mut self, span: Span) {
        let scope = self.cur_scope();
        let depth = self.stack.len();
        if let Some(queries) = &mut self.queries {
            queries.region(span, scope, depth);
        }
    }

    fn source_function_region(&mut self, span: Span, separate_body: bool, is_arrow: bool) {
        let scope = self.cur_scope();
        let depth = self.stack.len();
        if let Some(queries) = &mut self.queries {
            queries.function(span, scope, depth, separate_body, is_arrow);
        }
    }

    /// 最近的函数/模块作用域（var 提升目标）。
    fn nearest_var_scope(&self) -> ScopeId {
        for &s in self.stack.iter().rev() {
            if matches!(
                self.scopes[s as usize].kind,
                ScopeKind::Function
                    | ScopeKind::FunctionBody
                    | ScopeKind::Module
                    | ScopeKind::StaticBlock
            ) {
                return s;
            }
        }
        self.stack[0]
    }

    fn declare(&mut self, scope: ScopeId, name: Atom, decl_kind: DeclKind, span: Span) -> SymbolId {
        // 已存在同名绑定（如 var 重复声明）时复用，避免重复。
        let symbol = if let Some(&existing) = self.scopes[scope as usize].bindings.get(&name)
            && (self.symbols[existing as usize].decl_kind == decl_kind
                || matches!(
                    (self.symbols[existing as usize].decl_kind, decl_kind),
                    (DeclKind::Var, DeclKind::Function)
                        | (DeclKind::Function, DeclKind::Var)
                        | (DeclKind::Param, DeclKind::Var | DeclKind::Function)
                        | (DeclKind::Arguments, DeclKind::Var | DeclKind::Function)
                )) {
            existing
        } else {
            let id = self.symbols.len() as SymbolId;
            self.symbols.push(Symbol {
                name,
                decl_kind,
                scope,
                span,
            });
            self.scopes[scope as usize].bindings.insert(name, id);
            id
        };
        if self
            .binding_occurrence_keys
            .insert((name, span, scope, symbol))
        {
            self.binding_occurrences.push(BindingOccurrence {
                name,
                span,
                scope,
                symbol,
                decl_kind,
            });
        }
        symbol
    }

    fn reference(&mut self, name: Atom, span: Span) {
        self.reference_access(name, span, ReferenceAccess::Read);
    }

    fn reference_access(&mut self, name: Atom, span: Span, access: ReferenceAccess) {
        let scope = self.cur_scope();
        if let Some(exports) = &mut self.exports
            && exports.is_named(span, name)
        {
            exports.record(Ident::new(span, name), scope, SourceExportUseKind::Named);
        }
        let resolved = SemanticModel::resolve_in_impl(&self.scopes, scope, name);
        self.references.push(Reference {
            access,
            name,
            span,
            scope,
            resolved,
        });
    }

    // ==================================================================
    // 提升（hoisting）：把 var / function 声明预绑定到目标作用域
    // ==================================================================

    /// 在 `func_scope` 上提升语句列表里的 var（穿透块，不穿透函数）与 function 声明（当前块）。
    fn hoist(&mut self, stmts: &AVec<Statement>, func_scope: ScopeId) {
        for stmt in stmts.iter() {
            self.hoist_stmt(stmt, func_scope, true);
        }
    }

    fn hoist_nested(&mut self, stmts: &[Statement], func_scope: ScopeId) {
        for stmt in stmts {
            self.hoist_stmt(stmt, func_scope, false);
        }
    }

    fn prepare_annex_b(
        &mut self,
        statements: &[Statement],
        scope: ScopeId,
        parameters: &[Pattern],
    ) {
        if self.strict {
            return;
        }
        for id in annex_b::candidates(statements, parameters) {
            let existing = self.scopes[scope as usize].bindings.get(&id.name).copied();
            let dynamic_arguments = id.name == self.arguments_name
                && self.scopes[scope as usize].kind == ScopeKind::FunctionBody;
            let outer = if let Some(existing) = existing {
                Some(existing)
            } else if dynamic_arguments {
                None
            } else {
                let symbol = self.symbols.len() as SymbolId;
                self.symbols.push(Symbol {
                    name: id.name,
                    decl_kind: DeclKind::Var,
                    scope,
                    span: Span::DUMMY,
                });
                self.scopes[scope as usize].bindings.insert(id.name, symbol);
                Some(symbol)
            };
            self.annex_b_targets.insert(id.span, (scope, outer));
        }
    }

    fn visit_if_arm(&mut self, statement: &Statement) {
        if !self.strict
            && let Statement::FunctionDeclaration(function) = statement
            && !function.is_async
            && !function.is_generator
        {
            let scope = self.enter(ScopeKind::Block);
            self.source_region(function.span);
            self.predeclare_statement_lexical(statement, scope);
            self.visit_statement(statement);
            self.exit();
        } else {
            self.visit_statement(statement);
        }
    }

    fn hoist_stmt(&mut self, stmt: &Statement, func_scope: ScopeId, functions: bool) {
        match stmt {
            Statement::VariableDeclaration(d) if d.kind == VarKind::Var => {
                for decl in d.declarations.iter() {
                    self.declare_pattern(func_scope, &decl.id, DeclKind::Var);
                }
            }
            Statement::FunctionDeclaration(f) if functions => {
                if let Some(id) = f.id {
                    self.declare(func_scope, id.name, DeclKind::Function, id.span);
                }
            }
            // var 穿透以下控制流结构（但不穿透嵌套函数/类）。
            Statement::Block(b) => self.hoist_nested(&b.body, func_scope),
            Statement::If(s) => {
                self.hoist_stmt(&s.consequent, func_scope, false);
                if let Some(a) = &s.alternate {
                    self.hoist_stmt(a, func_scope, false);
                }
            }
            Statement::For(s) => {
                if let Some(ForInit::Variable(d)) = &s.init
                    && d.kind == VarKind::Var
                {
                    for decl in d.declarations.iter() {
                        self.declare_pattern(func_scope, &decl.id, DeclKind::Var);
                    }
                }
                self.hoist_stmt(&s.body, func_scope, false);
            }
            Statement::ForIn(s) => self.hoist_for_left(&s.left, &s.body, func_scope),
            Statement::ForOf(s) => self.hoist_for_left(&s.left, &s.body, func_scope),
            Statement::While(s) => self.hoist_stmt(&s.body, func_scope, false),
            Statement::DoWhile(s) => self.hoist_stmt(&s.body, func_scope, false),
            Statement::Switch(s) => {
                for case in s.cases.iter() {
                    self.hoist_nested(&case.consequent, func_scope);
                }
            }
            Statement::Try(s) => {
                self.hoist_nested(&s.block.body, func_scope);
                if let Some(h) = &s.handler {
                    self.hoist_nested(&h.body.body, func_scope);
                }
                if let Some(f) = &s.finalizer {
                    self.hoist_nested(&f.body, func_scope);
                }
            }
            Statement::Labeled(s) => self.hoist_stmt(&s.body, func_scope, !self.strict),
            // `export function/var …`：把被包裹声明按同规则提升到 enclosing scope（否则 export 函数名
            // 只落在 own scope，兄弟 export 函数 mangle 时同名碰撞）。const/let 不提升（visit 时声明）。
            Statement::ExportNamed(s) => {
                if let Some(d) = &s.declaration {
                    self.hoist_stmt(d, func_scope, functions);
                }
            }
            // `export default function foo`：foo 是模块作用域的提升声明（供内部/导出引用一致解析）。
            Statement::ExportDefault(s) => {
                if let ExportDefaultKind::Function(f) = &s.declaration
                    && let Some(id) = f.id
                {
                    self.declare(func_scope, id.name, DeclKind::Function, id.span);
                }
            }
            _ => {}
        }
    }

    fn hoist_for_left(&mut self, left: &ForLeft, body: &Statement, func_scope: ScopeId) {
        if let ForLeft::Variable(d) = left
            && d.kind == VarKind::Var
        {
            for decl in d.declarations.iter() {
                self.declare_pattern(func_scope, &decl.id, DeclKind::Var);
            }
        }
        self.hoist_stmt(body, func_scope, false);
    }

    /// Predeclare direct lexical bindings for one statement list.
    ///
    /// ECMAScript creates `let`/`const`/`class`/`using` and import bindings when entering their
    /// scope, not when execution reaches the declaration. Keeping this phase separate from
    /// [`Self::hoist`] is useful: these bindings resolve earlier references, but retain TDZ
    /// runtime behavior and must never be treated as `var`/function declarations.
    fn predeclare_lexical(&mut self, stmts: &[Statement], scope: ScopeId) {
        for statement in stmts {
            self.predeclare_statement_lexical(statement, scope);
        }
    }

    fn predeclare_statement_lexical(&mut self, statement: &Statement, scope: ScopeId) {
        match statement {
            Statement::FunctionDeclaration(function) => {
                if let Some(id) = function.id {
                    self.declare(scope, id.name, DeclKind::Function, id.span);
                }
            }
            Statement::VariableDeclaration(declaration) if declaration.kind != VarKind::Var => {
                let kind = match declaration.kind {
                    VarKind::Let => DeclKind::Let,
                    VarKind::Const => DeclKind::Const,
                    VarKind::Using | VarKind::AwaitUsing => DeclKind::Using,
                    VarKind::Var => unreachable!("var was excluded by the match guard"),
                };
                for declarator in declaration.declarations.iter() {
                    self.declare_pattern(scope, &declarator.id, kind);
                }
            }
            Statement::ClassDeclaration(class) => {
                if let Some(id) = class.id {
                    self.declare(scope, id.name, DeclKind::Class, id.span);
                }
            }
            Statement::Import(declaration) => {
                for specifier in declaration.specifiers.iter() {
                    let local = match specifier {
                        ImportSpecifier::Named { local, .. }
                        | ImportSpecifier::Default { local, .. }
                        | ImportSpecifier::Namespace { local, .. } => local,
                    };
                    self.declare(scope, local.name, DeclKind::Import, local.span);
                }
            }
            Statement::ExportNamed(export) => {
                if let Some(declaration) = &export.declaration {
                    self.predeclare_statement_lexical(declaration, scope);
                }
            }
            Statement::ExportDefault(export) => {
                if let ExportDefaultKind::Class(class) = export.declaration
                    && let Some(id) = class.id
                {
                    self.declare(scope, id.name, DeclKind::Class, id.span);
                }
            }
            _ => {}
        }
    }

    // ==================================================================
    // 绑定模式
    // ==================================================================

    fn declare_pattern(&mut self, scope: ScopeId, pat: &Pattern, kind: DeclKind) {
        match pat {
            Pattern::Ident(id) => {
                self.declare(scope, id.name, kind, id.span);
            }
            Pattern::Array(a) => {
                for el in a.elements.iter().flatten() {
                    self.declare_pattern(scope, el, kind);
                }
            }
            Pattern::Object(o) => {
                for p in o.properties.iter() {
                    self.declare_pattern(scope, &p.value, kind);
                }
                if let Some(rest) = &o.rest {
                    self.declare_pattern(scope, &rest.argument, kind);
                }
            }
            Pattern::Assignment(a) => self.declare_pattern(scope, &a.left, kind),
            Pattern::Rest(r) => self.declare_pattern(scope, &r.argument, kind),
        }
    }

    /// 参数模式里的默认值表达式需要在函数作用域内解析引用。
    fn visit_pattern_defaults(&mut self, pat: &Pattern) {
        match pat {
            Pattern::Ident(_) => {}
            Pattern::Array(a) => {
                for el in a.elements.iter().flatten() {
                    self.visit_pattern_defaults(el);
                }
            }
            Pattern::Object(o) => {
                for p in o.properties.iter() {
                    if let PropertyKey::Computed(e) = &p.key {
                        self.visit_expression(e);
                    }
                    self.visit_pattern_defaults(&p.value);
                }
                if let Some(rest) = &o.rest {
                    self.visit_pattern_defaults(&rest.argument);
                }
            }
            Pattern::Assignment(a) => {
                self.visit_pattern_defaults(&a.left);
                self.visit_expression(&a.right);
            }
            Pattern::Rest(r) => self.visit_pattern_defaults(&r.argument),
        }
    }

    // ==================================================================
    // 遍历
    // ==================================================================

    fn visit_statement(&mut self, stmt: &Statement) {
        if self
            .exports
            .as_ref()
            .is_some_and(|exports| exports.is_declaration(stmt.span()))
        {
            match stmt {
                Statement::VariableDeclaration(declaration) => {
                    let scope = if declaration.kind == VarKind::Var {
                        self.nearest_var_scope()
                    } else {
                        self.cur_scope()
                    };
                    for declarator in &declaration.declarations {
                        self.export_pattern(&declarator.id, scope);
                    }
                }
                Statement::FunctionDeclaration(function) => {
                    if let Some(id) = function.id {
                        self.export_binding(id, self.cur_scope());
                    }
                }
                Statement::ClassDeclaration(class) => {
                    if let Some(id) = class.id {
                        self.export_binding(id, self.cur_scope());
                    }
                }
                _ => {}
            }
        }
        match stmt {
            Statement::VariableDeclaration(d) => {
                let scope = if d.kind == VarKind::Var {
                    self.nearest_var_scope()
                } else {
                    self.cur_scope()
                };
                for decl in d.declarations.iter() {
                    // let/const 在当前作用域绑定（var 已在 hoist 阶段绑定，这里重复 declare 会命中复用）。
                    let kind = match d.kind {
                        VarKind::Var => DeclKind::Var,
                        VarKind::Let => DeclKind::Let,
                        VarKind::Const => DeclKind::Const,
                        VarKind::Using | VarKind::AwaitUsing => DeclKind::Using,
                    };
                    self.declare_pattern(scope, &decl.id, kind);
                    // 默认值/computed key 里的引用。
                    self.visit_pattern_defaults(&decl.id);
                    if let Some(init) = &decl.init {
                        self.visit_expression(init);
                    }
                }
            }
            // 函数声明：名字已由外层 hoist 声明在 enclosing scope → 传 true 跳过 own-scope 重复声明。
            Statement::FunctionDeclaration(f) => {
                if let Some(id) = f.id
                    && let Some(&(target_scope, outer)) = self.annex_b_targets.get(&id.span)
                    && let Some(&lexical) = self.scopes[self.cur_scope() as usize]
                        .bindings
                        .get(&id.name)
                {
                    self.annex_b_copies.push(AnnexBCopy {
                        span: f.span,
                        lexical,
                        outer,
                        target_scope,
                    });
                }
                self.visit_function(f, true);
            }
            Statement::ClassDeclaration(c) => {
                if let Some(id) = c.id {
                    self.declare(self.cur_scope(), id.name, DeclKind::Class, id.span);
                }
                self.visit_class(c, false);
            }
            Statement::Block(b) => {
                let scope = self.enter(ScopeKind::Block);
                self.source_region(b.span);
                self.predeclare_lexical(&b.body, scope);
                for s in b.body.iter() {
                    self.visit_statement(s);
                }
                self.exit();
            }
            Statement::Expression(e) => self.visit_expression(&e.expression),
            Statement::If(s) => {
                self.visit_expression(&s.test);
                self.visit_if_arm(&s.consequent);
                if let Some(a) = &s.alternate {
                    self.visit_if_arm(a);
                }
            }
            Statement::For(s) => {
                self.enter(ScopeKind::Block);
                self.source_region(s.span);
                if let Some(init) = &s.init {
                    match init {
                        ForInit::Variable(d) => {
                            self.visit_statement(&Statement::VariableDeclaration(d))
                        }
                        ForInit::Expression(e) => self.visit_expression(e),
                    }
                }
                if let Some(t) = &s.test {
                    self.visit_expression(t);
                }
                if let Some(u) = &s.update {
                    self.visit_expression(u);
                }
                self.visit_statement(&s.body);
                self.exit();
            }
            Statement::ForIn(s) => self.visit_for_in_of(s.span, &s.left, &s.right, &s.body),
            Statement::ForOf(s) => self.visit_for_in_of(s.span, &s.left, &s.right, &s.body),
            Statement::While(s) => {
                self.visit_expression(&s.test);
                self.visit_statement(&s.body);
            }
            Statement::DoWhile(s) => {
                self.visit_statement(&s.body);
                self.visit_expression(&s.test);
            }
            Statement::Switch(s) => {
                self.visit_expression(&s.discriminant);
                let scope = self.enter(ScopeKind::Block);
                if let Some(case) = s.cases.first() {
                    self.source_region(Span::new(case.span.lo, s.span.hi));
                }
                for case in s.cases.iter() {
                    self.predeclare_lexical(&case.consequent, scope);
                }
                for case in s.cases.iter() {
                    if let Some(t) = &case.test {
                        self.visit_expression(t);
                    }
                    for st in case.consequent.iter() {
                        self.visit_statement(st);
                    }
                }
                self.exit();
            }
            Statement::Return(s) => {
                if let Some(a) = &s.argument {
                    self.visit_expression(a);
                }
            }
            Statement::Throw(s) => self.visit_expression(&s.argument),
            Statement::Try(s) => {
                let scope = self.enter(ScopeKind::Block);
                self.source_region(s.block.span);
                self.predeclare_lexical(&s.block.body, scope);
                for st in s.block.body.iter() {
                    self.visit_statement(st);
                }
                self.exit();
                if let Some(h) = &s.handler {
                    self.enter(ScopeKind::Catch);
                    self.source_region(h.span);
                    // The parser-owned JsBlock for a catch body is the source container that
                    // erased declarations inhabit. Keep the catch region for parameter facts,
                    // and attach the exact body span so source projection can reuse this scope.
                    self.source_region(h.body.span);
                    if let Some(p) = &h.param {
                        self.declare_pattern(self.cur_scope(), p, DeclKind::CatchParam);
                        self.visit_pattern_defaults(p);
                    }
                    self.predeclare_lexical(&h.body.body, self.cur_scope());
                    for st in h.body.body.iter() {
                        self.visit_statement(st);
                    }
                    self.exit();
                }
                if let Some(f) = &s.finalizer {
                    let scope = self.enter(ScopeKind::Block);
                    self.source_region(f.span);
                    self.predeclare_lexical(&f.body, scope);
                    for st in f.body.iter() {
                        self.visit_statement(st);
                    }
                    self.exit();
                }
            }
            Statement::Labeled(s) => self.visit_statement(&s.body),
            Statement::With(s) => {
                self.visit_expression(&s.object);
                self.visit_statement(&s.body);
            }
            Statement::ExportNamed(s) => {
                let scope = self.cur_scope();
                if s.source.is_none()
                    && let Some(exports) = &mut self.exports
                {
                    for specifier in &s.specifiers {
                        if let ModuleExportName::Ident(id) = specifier.local
                            && exports.is_named(id.span, id.name)
                        {
                            exports.record(id, scope, SourceExportUseKind::Named);
                        }
                    }
                }
                if let Some(d) = &s.declaration {
                    self.visit_statement(d);
                }
            }
            Statement::ExportDefault(s) => {
                if self
                    .exports
                    .as_ref()
                    .is_some_and(|exports| exports.is_default(s.span))
                {
                    let id = match s.declaration {
                        ExportDefaultKind::Function(function) => function.id,
                        ExportDefaultKind::Class(class) => class.id,
                        _ => None,
                    };
                    if let Some(id) = id {
                        self.export_binding(id, self.cur_scope());
                    }
                }
                match s.declaration {
                    // 默认导出的命名函数：名字已由 hoist 提升到模块作用域 → true 跳过 own-scope 重复声明。
                    ExportDefaultKind::Function(f) => self.visit_function(f, true),
                    ExportDefaultKind::Class(c) => self.visit_class(c, false),
                    ExportDefaultKind::Expression(e) => self.visit_expression(&e),
                }
            }
            Statement::Import(d) => {
                for spec in d.specifiers.iter() {
                    let (span, name) = match spec {
                        ImportSpecifier::Named { local, .. }
                        | ImportSpecifier::Default { local, .. }
                        | ImportSpecifier::Namespace { local, .. } => (local.span, local.name),
                    };
                    self.declare(self.cur_scope(), name, DeclKind::Import, span);
                }
            }
            Statement::Empty(_)
            | Statement::Debugger(_)
            | Statement::Break(_)
            | Statement::Continue(_)
            | Statement::ExportAll(_) => {}
        }
    }

    fn visit_for_in_of(
        &mut self,
        span: Span,
        left: &ForLeft,
        right: &Expression,
        body: &Statement,
    ) {
        self.enter(ScopeKind::Block);
        self.source_region(span);
        match left {
            ForLeft::Variable(d) => self.visit_statement(&Statement::VariableDeclaration(d)),
            ForLeft::Target(e) => self.visit_assignment_target(e, ReferenceAccess::Write),
        }
        self.visit_expression(right);
        self.visit_statement(body);
        self.exit();
    }

    fn export_binding(&mut self, id: Ident, scope: ScopeId) {
        if let Some(exports) = &mut self.exports {
            exports.record(id, scope, SourceExportUseKind::Declaration);
        }
    }

    fn export_pattern(&mut self, pattern: &Pattern, scope: ScopeId) {
        match pattern {
            Pattern::Ident(id) => self.export_binding(**id, scope),
            Pattern::Array(array) => {
                for pattern in array.elements.iter().flatten() {
                    self.export_pattern(pattern, scope);
                }
            }
            Pattern::Object(object) => {
                for property in &object.properties {
                    self.export_pattern(&property.value, scope);
                }
                if let Some(rest) = object.rest {
                    self.export_pattern(&rest.argument, scope);
                }
            }
            Pattern::Assignment(assignment) => self.export_pattern(&assignment.left, scope),
            Pattern::Rest(rest) => self.export_pattern(&rest.argument, scope),
        }
    }

    /// `name_hoisted`：函数**声明**的名字已由外层 `hoist` 声明在 enclosing scope（true）——此时**不得**
    /// 在函数自身作用域再声明一次，否则各函数自作用域名字计数各自从 0 → mangle 时兄弟函数同名碰撞
    /// （`function a`/`function a`）。具名函数表达式拥有参数环境外侧的自引用名字环境。
    fn visit_function(&mut self, f: &Function, name_hoisted: bool) {
        let saved_strict = self.strict;
        self.strict |= f.body.is_some_and(|body| body.strict);
        let name_environment = f.id.is_some() && !name_hoisted;
        if let Some(id) = f.id
            && !name_hoisted
        {
            let scope = self.enter(ScopeKind::FunctionName);
            self.declare(scope, id.name, DeclKind::Function, id.span);
        }
        self.enter(ScopeKind::Function);
        let fscope = self.cur_scope();
        for p in f.params.iter() {
            self.declare_pattern(fscope, p, DeclKind::Param);
        }
        let separate_body = f.params.iter().any(parameter_contains_expression);
        self.source_function_region(f.span, separate_body, false);
        let arguments_name = self.arguments_name;
        let arguments_shadowed = !separate_body
            && f.body.is_some_and(|body| {
                body.statements.iter().any(|statement| match statement {
                    Statement::FunctionDeclaration(function) => {
                        function.id.is_some_and(|id| id.name == arguments_name)
                    }
                    Statement::ClassDeclaration(class) => {
                        class.id.is_some_and(|id| id.name == arguments_name)
                    }
                    Statement::VariableDeclaration(declaration)
                        if declaration.kind != VarKind::Var =>
                    {
                        declaration
                            .declarations
                            .iter()
                            .any(|declaration| pattern_binds_name(&declaration.id, arguments_name))
                    }
                    _ => false,
                })
            });
        if !self.scopes[fscope as usize]
            .bindings
            .contains_key(&arguments_name)
            && !arguments_shadowed
        {
            let symbol = self.symbols.len() as SymbolId;
            self.symbols.push(Symbol {
                name: arguments_name,
                decl_kind: DeclKind::Arguments,
                scope: fscope,
                span: Span::DUMMY,
            });
            self.scopes[fscope as usize]
                .bindings
                .insert(arguments_name, symbol);
        }
        for p in f.params.iter() {
            self.visit_pattern_defaults(p);
        }
        if let Some(body) = f.body {
            let body_scope = if separate_body {
                self.enter(ScopeKind::FunctionBody)
            } else {
                fscope
            };
            self.source_region(body.span);
            let first_binding = self.binding_occurrences.len();
            self.hoist(&body.statements, body_scope);
            if separate_body {
                self.record_parameter_copies(fscope, body_scope, first_binding);
            }
            self.predeclare_lexical(&body.statements, body_scope);
            self.prepare_annex_b(&body.statements, body_scope, &f.params);
            for st in body.statements.iter() {
                self.visit_statement(st);
            }
            if separate_body {
                self.exit();
            }
        }
        self.exit();
        if name_environment {
            self.exit();
        }
        self.strict = saved_strict;
    }

    fn record_parameter_copies(
        &mut self,
        parameters: ScopeId,
        body: ScopeId,
        first_binding: usize,
    ) {
        let functions = self.binding_occurrences[first_binding..]
            .iter()
            .filter(|binding| binding.scope == body && binding.decl_kind == DeclKind::Function)
            .map(|binding| binding.name)
            .collect::<FxHashSet<_>>();
        let mut copies = self.scopes[body as usize]
            .bindings
            .iter()
            .filter_map(|(name, &symbol)| {
                if functions.contains(name) {
                    return None;
                }
                let parameter = *self.scopes[parameters as usize].bindings.get(name)?;
                matches!(
                    self.symbols[parameter as usize].decl_kind,
                    DeclKind::Param | DeclKind::Arguments
                )
                .then_some(ParameterCopy {
                    parameter,
                    body: symbol,
                })
            })
            .collect::<Vec<_>>();
        copies.sort_by_key(|copy| (copy.parameter, copy.body));
        self.parameter_copies.extend(copies);
    }

    fn visit_class(&mut self, c: &Class, expression: bool) {
        // 装饰器表达式是真实运行时引用，必须计入作用域分析——否则 mangler 重命名被引用的
        // 装饰器函数后，装饰器处仍写旧名（ReferenceError），tree-shaking 也会误删它们。
        for d in c.decorators.iter() {
            self.visit_expression(d);
        }
        let saved_strict = self.strict;
        self.strict = true;
        if expression {
            let scope = self.enter(ScopeKind::Block);
            self.source_region(Span::new(
                c.id.map_or(c.span.lo, |id| id.span.lo),
                c.span.hi,
            ));
            if let Some(id) = c.id {
                self.declare(scope, id.name, DeclKind::Class, id.span);
            }
        }
        if let Some(sc) = &c.super_class {
            self.visit_expression(sc);
        }
        for member in c.body.iter() {
            match member {
                ClassMember::Method(m) => {
                    for d in m.decorators.iter() {
                        self.visit_expression(d);
                    }
                    if let PropertyKey::Computed(e) = &m.key {
                        self.visit_expression(e);
                    }
                    // 方法：其函数无独立声明名进入 enclosing，own-scope 处理即可。
                    self.visit_function(m.value, false);
                }
                ClassMember::Property(p) => {
                    for d in p.decorators.iter() {
                        self.visit_expression(d);
                    }
                    if let PropertyKey::Computed(e) = &p.key {
                        self.visit_expression(e);
                    }
                    if let Some(v) = &p.value {
                        self.visit_expression(v);
                    }
                }
                ClassMember::StaticBlock(b) => {
                    let scope = self.enter(ScopeKind::StaticBlock);
                    self.source_region(b.span);
                    self.hoist(&b.body, scope);
                    self.predeclare_lexical(&b.body, scope);
                    for st in b.body.iter() {
                        self.visit_statement(st);
                    }
                    self.exit();
                }
            }
        }
        if expression {
            self.exit();
        }
        self.strict = saved_strict;
    }

    fn visit_expression(&mut self, expr: &Expression) {
        match expr {
            Expression::Identifier(id) => self.reference(id.name, id.span),
            Expression::NumberLiteral(_)
            | Expression::StringLiteral(_)
            | Expression::BooleanLiteral(_)
            | Expression::NullLiteral(_)
            | Expression::BigIntLiteral(_)
            | Expression::RegExpLiteral(_)
            | Expression::This(_)
            | Expression::Super(_)
            | Expression::MetaProperty(_) => {}
            Expression::TemplateLiteral(t) => {
                for e in t.expressions.iter() {
                    self.visit_expression(e);
                }
            }
            Expression::Array(a) => {
                for el in a.elements.iter().flatten() {
                    self.visit_expression(el);
                }
            }
            Expression::Object(o) => {
                for m in o.properties.iter() {
                    match m {
                        ObjectMember::Property(p) => {
                            if let PropertyKey::Computed(e) = &p.key {
                                self.visit_expression(e);
                            }
                            self.visit_expression(&p.value);
                        }
                        ObjectMember::Spread(s) => self.visit_expression(&s.argument),
                    }
                }
            }
            // 具名函数表达式：名字仅在函数自身作用域可见（供自引用）→ false，own-scope 声明。
            Expression::Function(f) => self.visit_function(f, false),
            Expression::Arrow(a) => {
                let saved_strict = self.strict;
                self.strict |= matches!(a.body, ArrowBody::Block(body) if body.strict);
                self.enter(ScopeKind::Function);
                let fscope = self.cur_scope();
                for p in a.params.iter() {
                    self.declare_pattern(fscope, p, DeclKind::Param);
                }
                for p in a.params.iter() {
                    self.visit_pattern_defaults(p);
                }
                let separate_body = a.params.iter().any(parameter_contains_expression);
                self.source_function_region(a.span, separate_body, true);
                let body_scope = if separate_body {
                    self.enter(ScopeKind::FunctionBody)
                } else {
                    fscope
                };
                match a.body {
                    ArrowBody::Block(b) => {
                        self.source_region(b.span);
                        let first_binding = self.binding_occurrences.len();
                        self.hoist(&b.statements, body_scope);
                        if separate_body {
                            self.record_parameter_copies(fscope, body_scope, first_binding);
                        }
                        self.predeclare_lexical(&b.statements, body_scope);
                        self.prepare_annex_b(&b.statements, body_scope, &a.params);
                        for st in b.statements.iter() {
                            self.visit_statement(st);
                        }
                    }
                    ArrowBody::Expression(e) => self.visit_expression(&e),
                }
                if separate_body {
                    self.exit();
                }
                self.exit();
                self.strict = saved_strict;
            }
            Expression::Class(c) => self.visit_class(c, true),
            Expression::Unary(u) => self.visit_expression(&u.argument),
            Expression::Update(u) => {
                self.visit_assignment_target(&u.argument, ReferenceAccess::ReadWrite)
            }
            Expression::Binary(b) => {
                self.visit_expression(&b.left);
                self.visit_expression(&b.right);
            }
            Expression::PrivateIn(p) => self.visit_expression(&p.right),
            Expression::Logical(l) => {
                self.visit_expression(&l.left);
                self.visit_expression(&l.right);
            }
            Expression::Assignment(a) => {
                self.visit_assignment_target(
                    &a.left,
                    if a.operator == AssignmentOperator::Assign {
                        ReferenceAccess::Write
                    } else {
                        ReferenceAccess::ReadWrite
                    },
                );
                self.visit_expression(&a.right);
            }
            Expression::Conditional(c) => {
                self.visit_expression(&c.test);
                self.visit_expression(&c.consequent);
                self.visit_expression(&c.alternate);
            }
            Expression::Call(c) => {
                self.visit_expression(&c.callee);
                for arg in c.arguments.iter() {
                    self.visit_expression(arg);
                }
            }
            Expression::New(n) => {
                self.visit_expression(&n.callee);
                for arg in n.arguments.iter() {
                    self.visit_expression(arg);
                }
            }
            Expression::Member(m) => {
                self.visit_expression(&m.object);
                if let MemberProperty::Computed(e) = &m.property {
                    self.visit_expression(e);
                }
            }
            Expression::Sequence(s) => {
                for e in s.expressions.iter() {
                    self.visit_expression(e);
                }
            }
            Expression::TaggedTemplate(t) => {
                self.visit_expression(&t.tag);
                for e in t.quasi.expressions.iter() {
                    self.visit_expression(e);
                }
            }
            Expression::Spread(s) => self.visit_expression(&s.argument),
            Expression::Await(a) => self.visit_expression(&a.argument),
            Expression::Yield(y) => {
                if let Some(a) = &y.argument {
                    self.visit_expression(a);
                }
            }
            Expression::Import(i) => {
                self.visit_expression(&i.source);
                if let Some(o) = &i.options {
                    self.visit_expression(o);
                }
            }
        }
    }

    fn visit_assignment_target(&mut self, target: &Expression, access: ReferenceAccess) {
        match target {
            Expression::Identifier(identifier) => {
                self.reference_access(identifier.name, identifier.span, access)
            }
            Expression::Member(member) => {
                self.visit_expression(&member.object);
                if let MemberProperty::Computed(key) = &member.property {
                    self.visit_expression(key);
                }
            }
            Expression::Array(array) => {
                for element in array.elements.iter().flatten() {
                    self.visit_assignment_target(element, access);
                }
            }
            Expression::Object(object) => {
                for member in &object.properties {
                    match member {
                        ObjectMember::Property(property) => {
                            if let PropertyKey::Computed(key) = &property.key {
                                self.visit_expression(key);
                            }
                            self.visit_assignment_target(&property.value, access);
                        }
                        ObjectMember::Spread(spread) => {
                            self.visit_assignment_target(&spread.argument, access)
                        }
                    }
                }
            }
            Expression::Spread(spread) => self.visit_assignment_target(&spread.argument, access),
            Expression::Assignment(default) if default.operator == AssignmentOperator::Assign => {
                self.visit_assignment_target(&default.left, access);
                self.visit_expression(&default.right);
            }
            // Invalid/recovered assignment shapes retain the old traversal, without inventing
            // writes to expression operands that are only evaluated.
            _ => self.visit_expression(target),
        }
    }
}

impl SemanticModel {
    fn resolve_in_impl(scopes: &[Scope], mut scope: ScopeId, name: Atom) -> Option<SymbolId> {
        loop {
            if let Some(&sym) = scopes[scope as usize].bindings.get(&name) {
                return Some(sym);
            }
            scope = scopes[scope as usize].parent?;
        }
    }
}
