//! # wake_graph — 模块图分析：符号级 Tree Shaking（DESIGN §5.3）
//!
//! Tree Shaking v1（PLAN §6.6）：**跨模块导出可达性**——从入口出发，计算每个模块「哪些导出
//! 名被别的模块真正 import」，未被任何可达模块使用的导出交给 codegen 移除。
//!
//! 保守而安全的取舍（第一版）：
//! - 入口模块 → **全部导出保留**（它是 bundle 的公共面）；
//! - `import * as ns` / 动态 `import()` / `require()` / `export *` → 目标模块**全部导出视为已用**；
//! - `import { a, b }` → 用 `{a, b}`；`import D` → 用 `default`；`import "x"`（仅副作用）→ 用空集
//!   （模块仍进图运行，只是它的未用导出可被剪）；
//! - `export { a } from 'm'` / re-export → 保守地把 `a` 记为 m 的已用导出（不做跨链传播）。
//!
//! 「移除」的**安全性**由 codegen 侧兜底：只移除「外部未用 + 模块内也未引用 + 无副作用」的
//! 导出声明；否则仅移除 `exports.x = ...` 绑定行（永远安全）。模块的顶层副作用语句一律保留。

use wake_common::{Atom, FxHashMap, FxHashSet, Interner};
use wake_ecma_ast::{
    ExportDefaultKind, Expression, ImportSpecifier, ModuleExportName, Pattern, Program, Statement,
    Visit, walk_expression,
};

/// 一个模块对某个 import/export-from 说明符的使用方式。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportUse {
    /// 具名使用（`default` 以字符串 `"default"` 表示）。
    Names(Vec<String>),
    /// 整体使用（namespace / 动态 import / require）——不可 shake。
    All,
    /// `export * from 'm'` re-export 全量——仅当下游消费本模块导出时才传播至目标。
    ReexportAll,
}

/// 一个模块的已用导出集合。
#[derive(Debug, Clone, Default, PartialEq)]
pub enum Used {
    /// 尚无已知使用（初值）。
    #[default]
    None,
    /// 仅这些具名导出被使用。
    Names(FxHashSet<String>),
    /// 全部导出被使用——不 shake。
    All,
}

impl Used {
    /// 合并一条使用记录。`All` 吸收一切。
    pub fn merge(&mut self, u: &ImportUse) {
        match (&mut *self, u) {
            (Used::All, _) => {}
            (_, ImportUse::All | ImportUse::ReexportAll) => *self = Used::All,
            (Used::None, ImportUse::Names(ns)) => {
                *self = Used::Names(ns.iter().cloned().collect());
            }
            (Used::Names(set), ImportUse::Names(ns)) => {
                set.extend(ns.iter().cloned());
            }
        }
    }

    /// 转成传给 codegen 的「保留导出名」列表：
    /// - `All` → `None`（不 shake，全保留）；
    /// - `Names`/`None` → `Some(已排序去重名单)`（只保留这些）。
    pub fn to_keep_list(&self) -> Option<Vec<String>> {
        match self {
            Used::All => None,
            Used::None => Some(Vec::new()),
            Used::Names(set) => {
                let mut v: Vec<String> = set.iter().cloned().collect();
                v.sort_unstable();
                Some(v)
            }
        }
    }
}

/// 从一个模块的 AST 提取它对每个**静态** import / export-from 说明符的使用。
///
/// 动态 `import()` / `require()` 不在此处（它们是表达式，且说明符经 [`Used::All`] 处理更简单）——
/// 调用方据依赖种类（`DynamicImport`/`Require`）直接把目标标 `All`。
pub fn collect_static_uses(program: &Program, interner: &Interner) -> Vec<(String, ImportUse)> {
    let mut out: Vec<(String, ImportUse)> = Vec::new();
    for stmt in program.body.iter() {
        match stmt {
            Statement::Import(d) => {
                let source = interner.resolve(d.source);
                let mut names = Vec::new();
                let mut all = false;
                for spec in d.specifiers.iter() {
                    match spec {
                        ImportSpecifier::Default { .. } => names.push("default".to_string()),
                        ImportSpecifier::Namespace { .. } => all = true,
                        ImportSpecifier::Named { imported, .. } => {
                            names.push(export_name_string(imported, interner));
                        }
                    }
                }
                out.push((
                    source,
                    if all {
                        ImportUse::All
                    } else {
                        ImportUse::Names(names)
                    },
                ));
            }
            Statement::ExportNamed(s) => {
                if let Some(src) = s.source {
                    let source = interner.resolve(src);
                    let names = s
                        .specifiers
                        .iter()
                        .map(|sp| export_name_string(&sp.local, interner))
                        .collect();
                    out.push((source, ImportUse::Names(names)));
                }
            }
            Statement::ExportAll(s) => {
                out.push((interner.resolve(s.source), ImportUse::ReexportAll));
            }
            _ => {}
        }
    }
    out
}

fn export_name_string(n: &ModuleExportName, interner: &Interner) -> String {
    match n {
        ModuleExportName::Ident(id) => interner.resolve(id.name),
        ModuleExportName::String(a) => interner.resolve(*a),
    }
}

fn export_name_atom(n: &ModuleExportName) -> Atom {
    match n {
        ModuleExportName::Ident(id) => id.name,
        ModuleExportName::String(a) => *a,
    }
}

// ======================================================================
// 绑定级全程序活跃性（PLAN §6.6 增量）：把 Tree Shaking 从「模块 + import 名」粒度细化到
// 「绑定」粒度，识别「被 import 但引用它的代码本身是死代码」的传递性死亡导出。
//
// **安全第一（过近似）**：free-var 不做作用域分析，直接收集声明体内**所有**标识符引用；纯性
// 判断存疑即判「不纯」→ 归为副作用根（恒保留）。两者都只会**多保留**、绝不误删活代码。
// 跨模块的 re-export / 命名空间 / 动态 import 均保守处理（门控于「本模块被消费」→ 目标全保留）。
// ======================================================================

/// 一个模块的绑定级活跃性信息。名字用 `Atom`（bundler 全程共享同一 interner，跨模块可比）；
/// 说明符用 `String`（与 `dep_ids` 的说明符匹配）。
#[derive(Debug, Default)]
pub struct ModuleLiveness {
    /// 顶层**纯声明**绑定：绑定名 → 该声明体内引用到的标识符集合（过近似）。仅这些可被 DCE。
    pub decls: Vec<(Atom, FxHashSet<Atom>)>,
    /// 顶层**副作用**代码引用到的标识符（活跃性的「根」：这些语句在模块被 require 时执行）。
    pub root_refs: FxHashSet<Atom>,
    /// 绑定门控的具名 import（含 default，`imported="default"`）：局部名 → (说明符, 目标导出名)。
    pub named_imports: Vec<NamedImport>,
    /// 命名空间 import（`import * as ns`）：局部名 → 说明符——**引用到该局部名的活代码** → 目标全保留。
    pub namespace_imports: Vec<(Atom, String)>,
    /// `export * from 'm'`（真 splat，把 m 的具名导出全量透传）的说明符——用于名级传递解析。
    pub reexport_star: Vec<String>,
    /// `export * as ns from 'm'`（命名空间再导出，仅暴露单个 `ns`）：(导出的 ns 名, 说明符)——
    /// `ns` 被消费 → 目标全保留。
    pub ns_reexports: Vec<(Atom, String)>,
    /// `export { imp as exported } from 'm'`：(本模块暴露的导出名, 说明符, 源模块导出名)——
    /// `exported` 被消费 → 向源 propagate `imp`。
    pub reexport_named: Vec<(Atom, String, Atom)>,
    /// 本地 export：导出名 → 本地绑定名（`Some`）/ `None`（无可门控本地名，如 default 表达式导出 → 保守恒保留）。
    pub exports: Vec<(Atom, Option<Atom>)>,
}

/// 一条绑定门控的具名 import。
#[derive(Debug)]
pub struct NamedImport {
    pub local: Atom,
    pub spec: String,
    pub imported: Atom,
}

/// Linker-owned lowering plan for one plain `export *` declaration, in source order.
///
/// `Exact` is available only when the complete transitive export surface is owned by Wake and can
/// be resolved without runtime inspection. `Runtime` is the conservative boundary for CommonJS,
/// external, or otherwise opaque targets. Explicit exports are listed for the runtime fallback so
/// source order can never let a star overwrite them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportStarPlan {
    pub specifier: String,
    pub resolution: ExportStarResolution,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportStarResolution {
    Exact(Vec<Atom>),
    Runtime { excluded: Vec<Atom> },
}

/// 从一个模块的 AST 提取绑定级活跃性信息。
pub fn collect_module_liveness(program: &Program, interner: &Interner) -> ModuleLiveness {
    let mut ml = ModuleLiveness::default();
    let default_atom = interner.intern("default");
    for stmt in program.body.iter() {
        match stmt {
            Statement::Import(d) => {
                let source = interner.resolve(d.source);
                for spec in d.specifiers.iter() {
                    match spec {
                        ImportSpecifier::Named {
                            imported, local, ..
                        } => {
                            ml.named_imports.push(NamedImport {
                                local: local.name,
                                spec: source.clone(),
                                imported: export_name_atom(imported),
                            });
                        }
                        ImportSpecifier::Default { local, .. } => {
                            ml.named_imports.push(NamedImport {
                                local: local.name,
                                spec: source.clone(),
                                imported: default_atom,
                            });
                        }
                        ImportSpecifier::Namespace { local, .. } => {
                            ml.namespace_imports.push((local.name, source.clone()))
                        }
                    }
                }
            }
            Statement::ExportNamed(s) => {
                if let Some(src) = s.source {
                    // `export { imp as exported } from 'm'`：名级再导出（exported 被消费 → 向源要 imp）。
                    let source = interner.resolve(src);
                    for sp in s.specifiers.iter() {
                        ml.reexport_named.push((
                            export_name_atom(&sp.exported),
                            source.clone(),
                            export_name_atom(&sp.local),
                        ));
                    }
                } else if let Some(decl) = &s.declaration {
                    // `export function/const/class ...`
                    let names = process_decl(decl, &mut ml);
                    for n in names {
                        ml.exports.push((n, Some(n)));
                    }
                } else {
                    // `export { a, b as c }`（本地再导出）。
                    for sp in s.specifiers.iter() {
                        ml.exports.push((
                            export_name_atom(&sp.exported),
                            Some(export_name_atom(&sp.local)),
                        ));
                    }
                }
            }
            Statement::ExportDefault(s) => match &s.declaration {
                ExportDefaultKind::Function(f) if f.id.is_some() => {
                    let id = f.id.unwrap();
                    let mut refs = FxHashSet::default();
                    collect_refs_function(f, &mut refs);
                    refs.remove(&id.name);
                    ml.decls.push((id.name, refs));
                    ml.exports.push((default_atom, Some(id.name)));
                }
                ExportDefaultKind::Function(f) => {
                    let mut refs = FxHashSet::default();
                    collect_refs_function(f, &mut refs);
                    ml.root_refs.extend(refs);
                    ml.exports.push((default_atom, None));
                }
                ExportDefaultKind::Class(c) => {
                    let mut refs = FxHashSet::default();
                    collect_refs_class(c, &mut refs);
                    ml.root_refs.extend(refs);
                    ml.exports.push((default_atom, None));
                }
                ExportDefaultKind::Expression(e) => {
                    let mut refs = FxHashSet::default();
                    collect_refs_expr(e, &mut refs);
                    ml.root_refs.extend(refs);
                    ml.exports.push((default_atom, None));
                }
            },
            Statement::ExportAll(s) => {
                let spec = interner.resolve(s.source);
                match s.exported {
                    // `export * from 'm'`：把 m 的具名导出全量透传（不含 default）。
                    None => ml.reexport_star.push(spec),
                    // `export * as ns from 'm'`：仅暴露命名空间 `ns`（不透传名字）→ ns 被消费则目标全保留。
                    Some(exp) => ml.ns_reexports.push((export_name_atom(&exp), spec)),
                }
            }
            other => {
                process_decl(other, &mut ml);
            }
        }
    }
    ml
}

/// 处理一条声明/语句：纯声明 → 记入 `decls`；否则（含类、不纯 init、控制流等）→ 引用记入
/// `root_refs`（恒活）。返回其绑定的名字（供导出行门控）。
fn process_decl(stmt: &Statement, ml: &mut ModuleLiveness) -> Vec<Atom> {
    match stmt {
        Statement::FunctionDeclaration(f) => {
            if let Some(id) = f.id {
                let mut refs = FxHashSet::default();
                collect_refs_function(f, &mut refs);
                refs.remove(&id.name);
                ml.decls.push((id.name, refs));
                vec![id.name]
            } else {
                Vec::new()
            }
        }
        // `using` / `await using` 恒作副作用根（落入下面的 `_` 分支）：它们的 dispose 调用是
        // 可观测副作用，即使 init 是纯表达式（`using x = ident;`）也不能作为可 DCE 的纯声明。
        Statement::VariableDeclaration(d) if var_decl_pure(d) && !d.kind.is_using() => {
            let mut names = Vec::new();
            for decl in d.declarations.iter() {
                let mut bn = Vec::new();
                collect_pattern_names(&decl.id, &mut bn);
                let mut refs = FxHashSet::default();
                if let Some(init) = &decl.init {
                    collect_refs_expr(init, &mut refs);
                }
                for n in &bn {
                    ml.decls.push((*n, refs.clone()));
                }
                names.extend(bn);
            }
            names
        }
        _ => {
            // 不纯 / 类 / 控制流 → 副作用根：引用恒活。
            let mut refs = FxHashSet::default();
            collect_refs_stmt(stmt, &mut refs);
            ml.root_refs.extend(refs);
            decl_bound_names(stmt)
        }
    }
}

/// 一条声明语句绑定的名字（用于导出行门控；不纯声明的名字不进 `decls`）。
fn decl_bound_names(stmt: &Statement) -> Vec<Atom> {
    let mut names = Vec::new();
    match stmt {
        Statement::FunctionDeclaration(f) => {
            if let Some(id) = f.id {
                names.push(id.name);
            }
        }
        Statement::ClassDeclaration(c) => {
            if let Some(id) = c.id {
                names.push(id.name);
            }
        }
        Statement::VariableDeclaration(d) => {
            for decl in d.declarations.iter() {
                collect_pattern_names(&decl.id, &mut names);
            }
        }
        _ => {}
    }
    names
}

fn var_decl_pure(d: &wake_ecma_ast::VariableDeclaration) -> bool {
    d.declarations
        .iter()
        .all(|decl| decl.init.as_ref().is_none_or(expr_pure))
}

/// 保守纯性判断（镜像 codegen `expr_is_pure`；类一律判不纯，避免引入 class_is_pure）。存疑即不纯。
fn expr_pure(e: &Expression) -> bool {
    use Expression::*;
    match e {
        NumberLiteral(_) | StringLiteral(_) | BooleanLiteral(_) | NullLiteral(_)
        | BigIntLiteral(_) | RegExpLiteral(_) | Identifier(_) | This(_) | Super(_)
        | MetaProperty(_) | Function(_) | Arrow(_) => true,
        TemplateLiteral(t) => t.expressions.iter().all(expr_pure),
        Array(a) => a.elements.iter().flatten().all(expr_pure),
        Object(o) => o.properties.iter().all(|m| match m {
            wake_ecma_ast::ObjectMember::Property(p) => key_pure(&p.key) && expr_pure(&p.value),
            wake_ecma_ast::ObjectMember::Spread(_) => false,
        }),
        Unary(u) => u.operator != wake_ecma_ast::UnaryOperator::Delete && expr_pure(&u.argument),
        Binary(b) => expr_pure(&b.left) && expr_pure(&b.right),
        Logical(l) => expr_pure(&l.left) && expr_pure(&l.right),
        Conditional(c) => expr_pure(&c.test) && expr_pure(&c.consequent) && expr_pure(&c.alternate),
        Sequence(s) => s.expressions.iter().all(expr_pure),
        _ => false,
    }
}

fn key_pure(key: &wake_ecma_ast::PropertyKey) -> bool {
    match key {
        wake_ecma_ast::PropertyKey::Computed(e) => expr_pure(e),
        _ => true,
    }
}

fn collect_pattern_names(pat: &Pattern, out: &mut Vec<Atom>) {
    match pat {
        Pattern::Ident(id) => out.push(id.name),
        Pattern::Array(a) => {
            for el in a.elements.iter().flatten() {
                collect_pattern_names(el, out);
            }
        }
        Pattern::Object(o) => {
            for p in o.properties.iter() {
                collect_pattern_names(&p.value, out);
            }
            if let Some(r) = &o.rest {
                collect_pattern_names(&r.argument, out);
            }
        }
        Pattern::Assignment(a) => collect_pattern_names(&a.left, out),
        Pattern::Rest(r) => collect_pattern_names(&r.argument, out),
    }
}

/// 过近似引用收集：收集子树内所有**读取位置**的标识符（属性名 / 对象键 / 绑定名不计入——
/// 它们不经 `Expression::Identifier` 发射）。
#[derive(Default)]
struct RefCollector {
    refs: FxHashSet<Atom>,
}

impl<'a> Visit<'a> for RefCollector {
    fn visit_expression(&mut self, node: &Expression<'a>) {
        if let Expression::Identifier(id) = node {
            self.refs.insert(id.name);
        }
        walk_expression(self, node);
    }
}

fn collect_refs_stmt(stmt: &Statement, out: &mut FxHashSet<Atom>) {
    let mut rc = RefCollector::default();
    rc.visit_statement(stmt);
    out.extend(rc.refs);
}

fn collect_refs_expr(e: &Expression, out: &mut FxHashSet<Atom>) {
    let mut rc = RefCollector::default();
    rc.visit_expression(e);
    out.extend(rc.refs);
}

fn collect_refs_function(f: &wake_ecma_ast::Function, out: &mut FxHashSet<Atom>) {
    let mut rc = RefCollector::default();
    rc.visit_function(f);
    out.extend(rc.refs);
}

fn collect_refs_class(c: &wake_ecma_ast::Class, out: &mut FxHashSet<Atom>) {
    let mut rc = RefCollector::default();
    rc.visit_class(c);
    out.extend(rc.refs);
}

/// 绑定级活跃性 mark-sweep 的结果。`retained` keeps declaration bindings alive, while
/// `observed` is the exact set of public keys requested across the module boundary. They differ
/// when a declaration is used internally or one binding is exposed through multiple aliases.
#[derive(Debug, Clone, PartialEq)]
pub enum LiveResult {
    All,
    Names {
        retained: FxHashSet<Atom>,
        observed: FxHashSet<Atom>,
    },
}

/// 每模块的活跃性索引（mark-sweep 的工作视图，借用 [`ModuleLiveness`]）。
struct LiveIdx<'a> {
    decl_names: FxHashSet<Atom>,
    decl_refs: FxHashMap<Atom, &'a FxHashSet<Atom>>,
    named: FxHashMap<Atom, (&'a str, Atom)>,
    namespace: FxHashMap<Atom, &'a str>,
    local_exports: FxHashMap<Atom, Option<Atom>>,
    reexport_named: FxHashMap<Atom, (&'a str, Atom)>,
    ns_reexports: FxHashMap<Atom, &'a str>,
    /// 本模块直接提供的所有具名导出（local ∪ reexport_named ∪ ns）——resolves_export 用。
    provides: FxHashSet<Atom>,
    /// `export * from` 已解析的目标模块 id。
    splat_targets: Vec<u32>,
}

enum Ev {
    Ref(u32, Atom),
    Live(u32, Atom),
    Export(u32, Atom),
    All(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum ResolvedBinding {
    Local(u32, Atom),
    Namespace(u32),
    OpaqueExplicit(u32, Atom),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExportResolution {
    Missing,
    Resolved(ResolvedBinding),
    Ambiguous,
}

fn resolve_export_binding(
    idx: &FxHashMap<u32, LiveIdx<'_>>,
    resolve: &dyn Fn(u32, &str) -> Option<u32>,
    module: u32,
    name: Atom,
    resolve_set: &mut FxHashSet<(u32, Atom)>,
) -> ExportResolution {
    if !resolve_set.insert((module, name)) {
        return ExportResolution::Missing;
    }
    let Some(info) = idx.get(&module) else {
        resolve_set.remove(&(module, name));
        return ExportResolution::Missing;
    };

    let direct = if let Some(local) = info.local_exports.get(&name).copied() {
        Some(ExportResolution::Resolved(ResolvedBinding::Local(
            module,
            local.unwrap_or(name),
        )))
    } else if let Some((specifier, imported)) = info.reexport_named.get(&name).copied() {
        Some(resolve(module, specifier).map_or(
            ExportResolution::Resolved(ResolvedBinding::OpaqueExplicit(module, name)),
            |target| resolve_export_binding(idx, resolve, target, imported, resolve_set),
        ))
    } else {
        info.ns_reexports.get(&name).copied().map(|specifier| {
            ExportResolution::Resolved(resolve(module, specifier).map_or(
                ResolvedBinding::OpaqueExplicit(module, name),
                ResolvedBinding::Namespace,
            ))
        })
    };
    if let Some(direct) = direct {
        resolve_set.remove(&(module, name));
        return direct;
    }

    let mut found = None;
    for &target in &info.splat_targets {
        match resolve_export_binding(idx, resolve, target, name, resolve_set) {
            ExportResolution::Missing => {}
            ExportResolution::Ambiguous => {
                resolve_set.remove(&(module, name));
                return ExportResolution::Ambiguous;
            }
            ExportResolution::Resolved(binding) => match found {
                None => found = Some(binding),
                Some(previous) if previous == binding => {}
                Some(_) => {
                    resolve_set.remove(&(module, name));
                    return ExportResolution::Ambiguous;
                }
            },
        }
    }
    resolve_set.remove(&(module, name));
    found.map_or(ExportResolution::Missing, ExportResolution::Resolved)
}

fn collect_exported_names(
    idx: &FxHashMap<u32, LiveIdx<'_>>,
    module: u32,
    default_export: Atom,
    visited: &mut FxHashSet<u32>,
    names: &mut FxHashSet<Atom>,
) {
    if !visited.insert(module) {
        return;
    }
    let Some(info) = idx.get(&module) else {
        return;
    };
    names.extend(
        info.provides
            .iter()
            .copied()
            .filter(|name| *name != default_export),
    );
    for &target in &info.splat_targets {
        collect_exported_names(idx, target, default_export, visited, names);
    }
}

fn liveness_index<'a>(
    mods: &'a FxHashMap<u32, &ModuleLiveness>,
    resolve: &dyn Fn(u32, &str) -> Option<u32>,
) -> FxHashMap<u32, LiveIdx<'a>> {
    let mut idx = FxHashMap::default();
    for (&module, &liveness) in mods {
        let decl_names = liveness.decls.iter().map(|(name, _)| *name).collect();
        let decl_refs = liveness
            .decls
            .iter()
            .map(|(name, refs)| (*name, refs))
            .collect();
        let named = liveness
            .named_imports
            .iter()
            .map(|import| (import.local, (import.spec.as_str(), import.imported)))
            .collect();
        let namespace = liveness
            .namespace_imports
            .iter()
            .map(|(local, specifier)| (*local, specifier.as_str()))
            .collect();
        let local_exports = liveness.exports.iter().copied().collect();
        let reexport_named = liveness
            .reexport_named
            .iter()
            .map(|(exported, specifier, imported)| (*exported, (specifier.as_str(), *imported)))
            .collect();
        let ns_reexports = liveness
            .ns_reexports
            .iter()
            .map(|(name, specifier)| (*name, specifier.as_str()))
            .collect();
        let mut provides = FxHashSet::default();
        provides.extend(liveness.exports.iter().map(|(name, _)| *name));
        provides.extend(liveness.reexport_named.iter().map(|(name, _, _)| *name));
        provides.extend(liveness.ns_reexports.iter().map(|(name, _)| *name));
        let splat_targets = liveness
            .reexport_star
            .iter()
            .filter_map(|specifier| resolve(module, specifier))
            .collect();
        idx.insert(
            module,
            LiveIdx {
                decl_names,
                decl_refs,
                named,
                namespace,
                local_exports,
                reexport_named,
                ns_reexports,
                provides,
                splat_targets,
            },
        );
    }
    idx
}

/// Resolve every plain `export *` to the exact names it owns whenever its complete transitive ESM
/// surface is available. The returned vectors follow source order, and each exact name is assigned
/// to the earliest star edge that resolves to the module's unique final binding.
pub fn compute_export_star_plans(
    mods: &FxHashMap<u32, &ModuleLiveness>,
    resolve: &dyn Fn(u32, &str) -> Option<u32>,
    statically_analyzable_esm: &FxHashSet<u32>,
    default_export: Atom,
) -> FxHashMap<u32, Vec<ExportStarPlan>> {
    let idx = liveness_index(mods, resolve);
    let mut closed: FxHashSet<u32> = statically_analyzable_esm
        .iter()
        .copied()
        .filter(|module| idx.contains_key(module))
        .collect();
    loop {
        let rejected: Vec<u32> = closed
            .iter()
            .copied()
            .filter(|module| {
                mods.get(module).is_some_and(|liveness| {
                    liveness.reexport_star.iter().any(|specifier| {
                        resolve(*module, specifier).is_none_or(|target| !closed.contains(&target))
                    })
                })
            })
            .collect();
        if rejected.is_empty() {
            break;
        }
        for module in rejected {
            closed.remove(&module);
        }
    }

    let mut output = FxHashMap::default();
    for (&module, &liveness) in mods {
        let explicit = idx
            .get(&module)
            .map(|info| info.provides.clone())
            .unwrap_or_default();
        let mut claimed = FxHashSet::default();
        let mut plans = Vec::with_capacity(liveness.reexport_star.len());
        for specifier in &liveness.reexport_star {
            let resolution = closed
                .contains(&module)
                .then(|| resolve(module, specifier))
                .flatten()
                .filter(|target| closed.contains(target))
                .map_or_else(
                    || {
                        let excluded: Vec<_> = explicit.iter().copied().collect();
                        ExportStarResolution::Runtime { excluded }
                    },
                    |target| {
                        let mut candidates = FxHashSet::default();
                        collect_exported_names(
                            &idx,
                            target,
                            default_export,
                            &mut FxHashSet::default(),
                            &mut candidates,
                        );
                        let candidates: Vec<_> = candidates.into_iter().collect();
                        let mut names = Vec::new();
                        for name in candidates {
                            if explicit.contains(&name) || claimed.contains(&name) {
                                continue;
                            }
                            let root = resolve_export_binding(
                                &idx,
                                resolve,
                                module,
                                name,
                                &mut FxHashSet::default(),
                            );
                            let edge = resolve_export_binding(
                                &idx,
                                resolve,
                                target,
                                name,
                                &mut FxHashSet::default(),
                            );
                            if matches!((root, edge), (ExportResolution::Resolved(left), ExportResolution::Resolved(right)) if left == right)
                            {
                                claimed.insert(name);
                                names.push(name);
                            }
                        }
                        ExportStarResolution::Exact(names)
                    },
                );
            plans.push(ExportStarPlan {
                specifier: specifier.clone(),
                resolution,
            });
        }
        output.insert(module, plans);
    }
    output
}

/// 绑定级全程序活跃性 mark-sweep。返回每个模块的「活导出」结论（供 codegen tree-shaking）。
///
/// - `mods`：模块 id → [`ModuleLiveness`]。**必须覆盖图中所有会被分析的模块**——不在其中的模块
///   由 `force_all` 兜底为全保留（调用方对缺分析的模块——如缓存摘要命中——须传入 `force_all`）。
/// - `resolve(m, spec)`：把模块 `m` 内的说明符解析为目标模块 id（外部/未解析 → `None`）。
/// - `entry_id`：入口模块——整体保留。
/// - `force_all`：强制全保留的模块（动态 import / require 目标、缺绑定分析的模块）。
///
/// **安全**：任何存疑（未知导出名、命名空间、动态、缺分析）都升级为 `All`（多保留）；绝不误删活代码。
pub fn compute_live_keep(
    mods: &FxHashMap<u32, &ModuleLiveness>,
    resolve: &dyn Fn(u32, &str) -> Option<u32>,
    entry_id: u32,
    force_all: &FxHashSet<u32>,
) -> FxHashMap<u32, LiveResult> {
    let idx = liveness_index(mods, resolve);

    let mut all_used: FxHashSet<u32> = FxHashSet::default();
    let mut live: FxHashMap<u32, FxHashSet<Atom>> = FxHashMap::default();
    // consumed[m]：被以具名方式请求（Export）的导出名——用于 re-export 行的保留判定。
    let mut consumed: FxHashMap<u32, FxHashSet<Atom>> = FxHashMap::default();
    let mut wl: Vec<Ev> = Vec::new();
    wl.push(Ev::All(entry_id));
    for &m in force_all {
        wl.push(Ev::All(m));
    }
    // 所有模块的顶层副作用代码在被 require 时执行 → 其引用是活跃性的根。
    for (&m, ml) in mods.iter() {
        for r in &ml.root_refs {
            wl.push(Ev::Ref(m, *r));
        }
    }

    while let Some(ev) = wl.pop() {
        match ev {
            Ev::Ref(m, r) => {
                let Some(mi) = idx.get(&m) else { continue };
                if mi.decl_names.contains(&r) {
                    wl.push(Ev::Live(m, r));
                } else if let Some((spec, imported)) = mi.named.get(&r).copied() {
                    if let Some(t) = resolve(m, spec) {
                        wl.push(Ev::Export(t, imported));
                    }
                } else if let Some(spec) = mi.namespace.get(&r).copied()
                    && let Some(t) = resolve(m, spec)
                {
                    wl.push(Ev::All(t));
                }
                // 否则：全局 / 不纯本地绑定 → 忽略（后者本就恒保留）。
            }
            Ev::Live(m, local) => {
                if live.entry(m).or_default().insert(local)
                    && let Some(mi) = idx.get(&m)
                {
                    if let Some(refs) = mi.decl_refs.get(&local) {
                        for r in refs.iter() {
                            wl.push(Ev::Ref(m, *r));
                        }
                    }
                    // A local export may directly expose an imported binding:
                    // `import { x } from "dep"; export { x };`. In that case
                    // there is no local declaration whose references can carry
                    // liveness to the dependency, so propagate the live binding
                    // through the import itself.
                    if let Some((spec, imported)) = mi.named.get(&local).copied()
                        && let Some(t) = resolve(m, spec)
                    {
                        wl.push(Ev::Export(t, imported));
                    }
                    if let Some(spec) = mi.namespace.get(&local).copied()
                        && let Some(t) = resolve(m, spec)
                    {
                        wl.push(Ev::All(t));
                    }
                }
            }
            Ev::Export(m, name) => {
                consumed.entry(m).or_default().insert(name);
                let Some(mi) = idx.get(&m) else { continue };
                if let Some(spec) = mi.ns_reexports.get(&name).copied() {
                    // `export * as name from spec` 被消费 → 目标全保留。
                    if let Some(t) = resolve(m, spec) {
                        wl.push(Ev::All(t));
                    }
                } else if let Some((spec, imp)) = mi.reexport_named.get(&name).copied() {
                    // `export { imp as name } from spec` → 向源要 imp。
                    if let Some(t) = resolve(m, spec) {
                        wl.push(Ev::Export(t, imp));
                    }
                } else if let Some(local) = mi.local_exports.get(&name).copied() {
                    if let Some(l) = local {
                        wl.push(Ev::Live(m, l));
                    }
                    // None：非门控导出（default 表达式等）——输出阶段恒保留。
                } else {
                    // 本模块未直接提供 → 只沿着解析到同一最终绑定的 star 边传播。冲突、缺失或
                    // opaque 目标无法形成唯一绑定，继续走保守 All。
                    let targets = mi.splat_targets.clone();
                    let mut any = false;
                    if let ExportResolution::Resolved(root) =
                        resolve_export_binding(&idx, resolve, m, name, &mut FxHashSet::default())
                    {
                        for t in targets {
                            if resolve_export_binding(
                                &idx,
                                resolve,
                                t,
                                name,
                                &mut FxHashSet::default(),
                            ) == ExportResolution::Resolved(root)
                            {
                                wl.push(Ev::Export(t, name));
                                any = true;
                            }
                        }
                    }
                    if !any {
                        wl.push(Ev::All(m));
                    }
                }
            }
            Ev::All(m) => {
                if all_used.insert(m)
                    && let Some(mi) = idx.get(&m)
                {
                    for (spec, imported) in mi.named.values() {
                        if let Some(t) = resolve(m, spec) {
                            wl.push(Ev::Export(t, *imported));
                        }
                    }
                    for spec in mi.namespace.values() {
                        if let Some(t) = resolve(m, spec) {
                            wl.push(Ev::All(t));
                        }
                    }
                    for (spec, imp) in mi.reexport_named.values() {
                        if let Some(t) = resolve(m, spec) {
                            wl.push(Ev::Export(t, *imp));
                        }
                    }
                    for spec in mi.ns_reexports.values() {
                        if let Some(t) = resolve(m, spec) {
                            wl.push(Ev::All(t));
                        }
                    }
                    for &t in &mi.splat_targets {
                        wl.push(Ev::All(t));
                    }
                }
            }
        }
    }

    // 汇总：每模块的活导出。
    let mut out: FxHashMap<u32, LiveResult> = FxHashMap::default();
    for (&m, &ml) in mods.iter() {
        if all_used.contains(&m) {
            out.insert(m, LiveResult::All);
            continue;
        }
        let live_m = live.get(&m);
        let consumed_m = consumed.get(&m);
        let mut names: FxHashSet<Atom> = FxHashSet::default();
        let observed = consumed_m.cloned().unwrap_or_default();
        // 本地导出：局部绑定活 → 保留导出行；非门控恒保留。
        for (name, local) in &ml.exports {
            let keep = match local {
                None => true,
                Some(l) => live_m.is_some_and(|s| s.contains(l)),
            };
            if keep {
                names.insert(*name);
            }
        }
        // re-export：被具名消费才保留（否则该导出行是死的）。
        for (exported, _, _) in &ml.reexport_named {
            if consumed_m.is_some_and(|s| s.contains(exported)) {
                names.insert(*exported);
            }
        }
        for (exported, _) in &ml.ns_reexports {
            if consumed_m.is_some_and(|s| s.contains(exported)) {
                names.insert(*exported);
            }
        }
        // A named request resolved through `export *` still needs the barrel's runtime forwarding
        // even though the public name has no module-local declaration SymbolId. Preserve that name
        // in the module-boundary proof; otherwise an empty local keep set is indistinguishable from
        // a side-effect-only barrel and codegen may erase the forwarding object.
        if let (Some(mi), Some(consumed_m)) = (idx.get(&m), consumed_m) {
            for &name in consumed_m {
                if mi.provides.contains(&name) {
                    continue;
                }
                if let ExportResolution::Resolved(root) =
                    resolve_export_binding(&idx, resolve, m, name, &mut FxHashSet::default())
                    && mi.splat_targets.iter().copied().any(|target| {
                        resolve_export_binding(
                            &idx,
                            resolve,
                            target,
                            name,
                            &mut FxHashSet::default(),
                        ) == ExportResolution::Resolved(root)
                    })
                {
                    names.insert(name);
                }
            }
        }
        out.insert(
            m,
            LiveResult::Names {
                retained: names,
                observed,
            },
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn used_merge_all_absorbs() {
        let mut u = Used::Names(["a".to_string()].into_iter().collect());
        u.merge(&ImportUse::All);
        assert!(matches!(u, Used::All));
        // All 之后再并具名仍是 All。
        u.merge(&ImportUse::Names(vec!["b".into()]));
        assert!(matches!(u, Used::All));
    }

    #[test]
    fn used_merge_names_union() {
        let mut u = Used::None;
        u.merge(&ImportUse::Names(vec!["a".into(), "b".into()]));
        u.merge(&ImportUse::Names(vec!["b".into(), "c".into()]));
        let keep = u.to_keep_list().unwrap();
        assert_eq!(keep, vec!["a", "b", "c"]);
    }

    #[test]
    fn to_keep_list_all_is_none() {
        assert!(Used::All.to_keep_list().is_none());
        assert_eq!(Used::None.to_keep_list(), Some(vec![]));
    }

    // —— 绑定级活跃性 mark-sweep —— (手构 ModuleLiveness, 免 parser 依赖)

    fn named(local: Atom, spec: &str, imported: Atom) -> NamedImport {
        NamedImport {
            local,
            spec: spec.to_string(),
            imported,
        }
    }

    #[test]
    fn liveness_drops_unreferenced_import_and_unused_export() {
        // m1(runtime): export function createElement(){}
        // m2(hooks):   import {createElement}; export function useState(){}  ← 不引用 createElement
        // entry(0):    仅副作用 require, 不 import 任何导出
        // 期望: createElement 死(被 import 但引用它的代码不存在), useState 死(无人用)。
        let it = Interner::new();
        let create = it.intern("createElement");
        let use_state = it.intern("useState");

        let m1 = ModuleLiveness {
            decls: vec![(create, FxHashSet::default())],
            exports: vec![(create, Some(create))],
            ..Default::default()
        };
        let mut m2 = ModuleLiveness {
            decls: vec![(use_state, FxHashSet::default())],
            exports: vec![(use_state, Some(use_state))],
            ..Default::default()
        };
        m2.named_imports.push(named(create, "runtime", create));

        let m0 = ModuleLiveness::default();
        let mut mods: FxHashMap<u32, &ModuleLiveness> = FxHashMap::default();
        mods.insert(0u32, &m0);
        mods.insert(1u32, &m1);
        mods.insert(2u32, &m2);
        let resolve = |_m: u32, spec: &str| (spec == "runtime").then_some(1u32);
        let keep = compute_live_keep(&mods, &resolve, 0, &FxHashSet::default());

        assert_eq!(keep[&0], LiveResult::All, "entry 全保留");
        match &keep[&1] {
            LiveResult::Names { retained, .. } => {
                assert!(!retained.contains(&create), "createElement 应死")
            }
            _ => panic!("m1 不应 All"),
        }
        match &keep[&2] {
            LiveResult::Names { retained, .. } => {
                assert!(!retained.contains(&use_state), "useState 应死")
            }
            _ => panic!("m2 不应 All"),
        }
    }

    #[test]
    fn liveness_keeps_transitively_used() {
        // entry 顶层调用 useState(); useState 体内引用 createElement → 两者都活。
        let it = Interner::new();
        let create = it.intern("createElement");
        let use_state = it.intern("useState");

        let m1 = ModuleLiveness {
            decls: vec![(create, FxHashSet::default())],
            exports: vec![(create, Some(create))],
            ..Default::default()
        };
        let mut refs = FxHashSet::default();
        refs.insert(create);
        let mut m2 = ModuleLiveness {
            decls: vec![(use_state, refs)],
            exports: vec![(use_state, Some(use_state))],
            ..Default::default()
        };
        m2.named_imports.push(named(create, "runtime", create));

        let mut m0 = ModuleLiveness::default();
        m0.named_imports.push(named(use_state, "hooks", use_state));
        m0.root_refs.insert(use_state); // 顶层副作用调用 useState()

        let mut mods: FxHashMap<u32, &ModuleLiveness> = FxHashMap::default();
        mods.insert(0u32, &m0);
        mods.insert(1u32, &m1);
        mods.insert(2u32, &m2);
        let resolve = |_m: u32, spec: &str| match spec {
            "runtime" => Some(1u32),
            "hooks" => Some(2u32),
            _ => None,
        };
        let keep = compute_live_keep(&mods, &resolve, 0, &FxHashSet::default());

        if let LiveResult::Names { retained, .. } = &keep[&2] {
            assert!(retained.contains(&use_state), "useState 应活(entry 用了)");
        }
        if let LiveResult::Names { retained, .. } = &keep[&1] {
            assert!(
                retained.contains(&create),
                "createElement 应活(useState 传递引用)"
            )
        }
    }

    #[test]
    fn liveness_separates_retained_binding_aliases_from_observed_public_names() {
        let it = Interner::new();
        let value = it.intern("value");
        let kept = it.intern("kept");
        let dead_alias = it.intern("deadAlias");
        let imported = it.intern("imported");

        let library = ModuleLiveness {
            decls: vec![(value, FxHashSet::default())],
            exports: vec![(kept, Some(value)), (dead_alias, Some(value))],
            ..Default::default()
        };
        let mut entry = ModuleLiveness::default();
        entry.named_imports.push(named(imported, "library", kept));
        entry.root_refs.insert(imported);

        let mut mods: FxHashMap<u32, &ModuleLiveness> = FxHashMap::default();
        mods.insert(0, &entry);
        mods.insert(1, &library);
        let resolve = |_module: u32, specifier: &str| (specifier == "library").then_some(1);
        let keep = compute_live_keep(&mods, &resolve, 0, &FxHashSet::default());

        let LiveResult::Names { retained, observed } = &keep[&1] else {
            panic!("library should retain exact liveness")
        };
        assert_eq!(retained, &FxHashSet::from_iter([kept, dead_alias]));
        assert_eq!(observed, &FxHashSet::from_iter([kept]));
    }

    #[test]
    fn liveness_local_reexport_of_named_import_propagates_to_source() {
        // runtime(1): import { Icon } from "barrel"; export { Icon };
        // barrel(2):  export { default as Icon } from "icon";
        // icon(3):    const IconImpl = createIcon(); export { IconImpl as default };
        // entry(0):   import { Icon } from "runtime"; use(Icon);
        //
        // The local export in runtime makes an imported binding live. That must
        // request the named export from barrel and ultimately keep icon's default.
        let it = Interner::new();
        let icon = it.intern("Icon");
        let icon_impl = it.intern("IconImpl");
        let default = it.intern("default");

        let source = ModuleLiveness {
            decls: vec![(icon_impl, FxHashSet::default())],
            exports: vec![(default, Some(icon_impl))],
            ..Default::default()
        };
        let barrel = ModuleLiveness {
            reexport_named: vec![(icon, "icon".to_string(), default)],
            ..Default::default()
        };
        let mut runtime = ModuleLiveness {
            exports: vec![(icon, Some(icon))],
            ..Default::default()
        };
        runtime.named_imports.push(named(icon, "barrel", icon));

        let mut entry = ModuleLiveness::default();
        entry.named_imports.push(named(icon, "runtime", icon));
        entry.root_refs.insert(icon);

        let mut mods: FxHashMap<u32, &ModuleLiveness> = FxHashMap::default();
        mods.insert(0u32, &entry);
        mods.insert(1u32, &runtime);
        mods.insert(2u32, &barrel);
        mods.insert(3u32, &source);
        let resolve = |m: u32, spec: &str| match (m, spec) {
            (0, "runtime") => Some(1u32),
            (1, "barrel") => Some(2u32),
            (2, "icon") => Some(3u32),
            _ => None,
        };
        let keep = compute_live_keep(&mods, &resolve, 0, &FxHashSet::default());

        match &keep[&3] {
            LiveResult::Names {
                retained: names, ..
            } => {
                assert!(
                    names.contains(&default),
                    "icon default export should be live"
                )
            }
            LiveResult::All => {}
        }
    }

    #[test]
    fn liveness_local_reexport_of_namespace_import_keeps_source_all() {
        // runtime(1): import * as icons from "icons"; export { icons };
        // entry(0):   import { icons } from "runtime"; use(icons);
        let it = Interner::new();
        let icons = it.intern("icons");
        let value = it.intern("value");

        let source = ModuleLiveness {
            decls: vec![(value, FxHashSet::default())],
            exports: vec![(value, Some(value))],
            ..Default::default()
        };
        let runtime = ModuleLiveness {
            namespace_imports: vec![(icons, "icons".to_string())],
            exports: vec![(icons, Some(icons))],
            ..Default::default()
        };
        let mut entry = ModuleLiveness::default();
        entry.named_imports.push(named(icons, "runtime", icons));
        entry.root_refs.insert(icons);

        let mut mods: FxHashMap<u32, &ModuleLiveness> = FxHashMap::default();
        mods.insert(0u32, &entry);
        mods.insert(1u32, &runtime);
        mods.insert(2u32, &source);
        let resolve = |m: u32, spec: &str| match (m, spec) {
            (0, "runtime") => Some(1u32),
            (1, "icons") => Some(2u32),
            _ => None,
        };
        let keep = compute_live_keep(&mods, &resolve, 0, &FxHashSet::default());

        assert_eq!(keep[&2], LiveResult::All);
    }

    #[test]
    fn liveness_barrel_star_resolves_by_name() {
        // barrel(1): export * from 'a'; export * from 'b'
        // a(2): export function fa(){}   b(3): export function fb(){}
        // entry(0): import { fa } from 'barrel'; 顶层调用 fa()
        // 期望: a.fa 活；b.fb 死且 b 不被整体 All（名级解析只把 fa 传给真正提供它的 a）。
        let it = Interner::new();
        let fa = it.intern("fa");
        let fb = it.intern("fb");
        let a = ModuleLiveness {
            decls: vec![(fa, FxHashSet::default())],
            exports: vec![(fa, Some(fa))],
            ..Default::default()
        };
        let b = ModuleLiveness {
            decls: vec![(fb, FxHashSet::default())],
            exports: vec![(fb, Some(fb))],
            ..Default::default()
        };
        let barrel = ModuleLiveness {
            reexport_star: vec!["a".to_string(), "b".to_string()],
            ..Default::default()
        };
        let mut entry = ModuleLiveness::default();
        entry.named_imports.push(named(fa, "barrel", fa));
        entry.root_refs.insert(fa);

        let mut mods: FxHashMap<u32, &ModuleLiveness> = FxHashMap::default();
        mods.insert(0u32, &entry);
        mods.insert(1u32, &barrel);
        mods.insert(2u32, &a);
        mods.insert(3u32, &b);
        let resolve = |_m: u32, spec: &str| match spec {
            "barrel" => Some(1u32),
            "a" => Some(2u32),
            "b" => Some(3u32),
            _ => None,
        };
        let keep = compute_live_keep(&mods, &resolve, 0, &FxHashSet::default());

        let LiveResult::Names {
            retained: barrel_names,
            observed,
        } = &keep[&1]
        else {
            panic!("barrel 应保持精确名级活跃性，而不是退化为 All")
        };
        assert_eq!(
            barrel_names,
            &FxHashSet::from_iter([fa]),
            "barrel 必须且只需保留 fa 的 export-star 转发"
        );
        assert_eq!(observed, &FxHashSet::from_iter([fa]));

        match &keep[&2] {
            LiveResult::Names { retained, .. } => assert!(retained.contains(&fa), "a.fa 应活"),
            LiveResult::All => {} // All 也可接受
        }
        match &keep[&3] {
            LiveResult::Names { retained, .. } => assert!(!retained.contains(&fb), "b.fb 应死"),
            LiveResult::All => panic!("b 不应被整体 All（名级解析应只命中 a）"),
        }
    }

    #[test]
    fn export_star_plan_excludes_explicit_and_ambiguous_names() {
        let interner = Interner::new();
        let value = interner.intern("value");
        let only_a = interner.intern("onlyA");
        let only_b = interner.intern("onlyB");
        let default = interner.intern("default");
        let a = ModuleLiveness {
            exports: vec![(value, Some(value)), (only_a, Some(only_a))],
            ..Default::default()
        };
        let b = ModuleLiveness {
            exports: vec![(value, Some(value)), (only_b, Some(only_b))],
            ..Default::default()
        };
        let ambiguous = ModuleLiveness {
            reexport_star: vec!["a".into(), "b".into()],
            ..Default::default()
        };
        let explicit = ModuleLiveness {
            reexport_named: vec![(value, "b".into(), value)],
            reexport_star: vec!["a".into()],
            ..Default::default()
        };
        let mods = FxHashMap::from_iter([(0, &ambiguous), (1, &a), (2, &b), (3, &explicit)]);
        let resolve = |module, specifier: &str| match (module, specifier) {
            (0 | 3, "a") => Some(1),
            (0 | 3, "b") => Some(2),
            _ => None,
        };
        let plans = compute_export_star_plans(
            &mods,
            &resolve,
            &FxHashSet::from_iter([0, 1, 2, 3]),
            default,
        );

        assert_eq!(
            plans[&0],
            vec![
                ExportStarPlan {
                    specifier: "a".into(),
                    resolution: ExportStarResolution::Exact(vec![only_a]),
                },
                ExportStarPlan {
                    specifier: "b".into(),
                    resolution: ExportStarResolution::Exact(vec![only_b]),
                },
            ]
        );
        assert_eq!(
            plans[&3],
            vec![ExportStarPlan {
                specifier: "a".into(),
                resolution: ExportStarResolution::Exact(vec![only_a]),
            }]
        );
    }

    #[test]
    fn export_star_plan_deduplicates_one_binding_and_closes_cycles() {
        let interner = Interner::new();
        let value = interner.intern("value");
        let a_name = interner.intern("a");
        let b_name = interner.intern("b");
        let default = interner.intern("default");
        let source = ModuleLiveness {
            exports: vec![(value, Some(value))],
            ..Default::default()
        };
        let left = ModuleLiveness {
            reexport_star: vec!["source".into()],
            ..Default::default()
        };
        let right = ModuleLiveness {
            reexport_star: vec!["source".into()],
            ..Default::default()
        };
        let diamond = ModuleLiveness {
            reexport_star: vec!["left".into(), "right".into()],
            ..Default::default()
        };
        let cycle_a = ModuleLiveness {
            exports: vec![(a_name, Some(a_name))],
            reexport_star: vec!["cycle-b".into()],
            ..Default::default()
        };
        let cycle_b = ModuleLiveness {
            exports: vec![(b_name, Some(b_name))],
            reexport_star: vec!["cycle-a".into()],
            ..Default::default()
        };
        let mods = FxHashMap::from_iter([
            (0, &diamond),
            (1, &left),
            (2, &right),
            (3, &source),
            (4, &cycle_a),
            (5, &cycle_b),
        ]);
        let resolve = |module, specifier: &str| match (module, specifier) {
            (0, "left") => Some(1),
            (0, "right") => Some(2),
            (1 | 2, "source") => Some(3),
            (4, "cycle-b") => Some(5),
            (5, "cycle-a") => Some(4),
            _ => None,
        };
        let plans = compute_export_star_plans(
            &mods,
            &resolve,
            &FxHashSet::from_iter([0, 1, 2, 3, 4, 5]),
            default,
        );

        assert_eq!(
            plans[&0][0].resolution,
            ExportStarResolution::Exact(vec![value])
        );
        assert_eq!(
            plans[&0][1].resolution,
            ExportStarResolution::Exact(Vec::new())
        );
        assert_eq!(
            plans[&4][0].resolution,
            ExportStarResolution::Exact(vec![b_name])
        );
        assert_eq!(
            plans[&5][0].resolution,
            ExportStarResolution::Exact(vec![a_name])
        );
    }
}
