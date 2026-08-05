//! # Identifier Mangling — Scope-safe Rename Planner
//!
//! Produces a [`Mangling`] side-table (span → new name) consumed by `wake_ecma_codegen`.
//! Does NOT mutate the immutable arena AST.
//!
//! Renames function/block/catch locals, params, AND module-level symbols that
//! are not exported, not imported, and not function declarations.

use wake_common::{Atom, FxHashMap, FxHashSet, Interner, Span};
use wake_ecma_ast::{
    Expression, ModuleExportName, Pattern, Program, Statement, Visit, walk_expression,
    walk_statement,
};
use wake_ecma_semantic::{DeclKind, ScopeId, SemanticModel, Symbol, SymbolId, analyze};

/// Mangling plan: every identifier occurrence (decl + ref) that gets renamed, mapped to its new name.
#[derive(Debug, Default)]
pub struct Mangling {
    renames: FxHashMap<Span, Atom>,
    pub renamed_symbols: usize,
}

impl Mangling {
    fn empty() -> Self {
        Mangling {
            renames: FxHashMap::default(),
            renamed_symbols: 0,
        }
    }
    pub fn is_empty(&self) -> bool {
        self.renames.is_empty()
    }
    pub fn get(&self, span: Span) -> Option<Atom> {
        self.renames.get(&span).copied()
    }
    pub fn table(&self) -> &FxHashMap<Span, Atom> {
        &self.renames
    }
}

const MODULE_SCOPE: ScopeId = 0;

const RUNTIME_NAMES: &[&str] = &[
    "exports",
    "module",
    "require",
    "__wake_require__",
    "__wake_interop_default",
    "__wake_interop_star",
    "globalThis",
];

/// Collect names that appear in `export` declarations — they must NOT be renamed.
fn collect_exported_names(program: &Program) -> FxHashSet<Atom> {
    let mut exported = FxHashSet::default();
    for stmt in &program.body {
        match stmt {
            Statement::ExportNamed(decl) => {
                if let Some(inner) = &decl.declaration {
                    exported_names_from_decl(inner, &mut exported);
                }
                for spec in &decl.specifiers {
                    match &spec.local {
                        ModuleExportName::Ident(id) => {
                            exported.insert(id.name);
                        }
                        ModuleExportName::String(s) => {
                            exported.insert(*s);
                        }
                    }
                }
            }
            Statement::ExportDefault(_) => {
                // export default function foo() {}  — foo excluded by DeclKind::Function
                // export default class Foo {}       — Foo  CAN be renamed
                // export default 42                 — no binding
            }
            Statement::ExportAll(decl) => {
                // export * as ns from '...' — ns is an import-like binding, add to guard set
                if let Some(exported_name) = &decl.exported {
                    match exported_name {
                        ModuleExportName::Ident(id) => {
                            exported.insert(id.name);
                        }
                        ModuleExportName::String(s) => {
                            exported.insert(*s);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    exported
}

fn exported_names_from_decl(stmt: &Statement, exported: &mut FxHashSet<Atom>) {
    match stmt {
        Statement::VariableDeclaration(var_decl) => {
            for d in &var_decl.declarations {
                pattern_binding_names(&d.id, exported);
            }
        }
        Statement::FunctionDeclaration(f) => {
            if let Some(id) = &f.id {
                exported.insert(id.name);
            }
        }
        Statement::ClassDeclaration(c) => {
            if let Some(id) = &c.id {
                exported.insert(id.name);
            }
        }
        _ => {}
    }
}

fn pattern_binding_names(pat: &Pattern, names: &mut FxHashSet<Atom>) {
    match pat {
        Pattern::Ident(id) => {
            names.insert(id.name);
        }
        Pattern::Array(arr) => {
            for p in (&arr.elements).into_iter().flatten() {
                pattern_binding_names(p, names);
            }
        }
        Pattern::Object(obj) => {
            for prop in &obj.properties {
                pattern_binding_names(&prop.value, names);
            }
            if let Some(rest) = &obj.rest {
                pattern_binding_names(&rest.argument, names);
            }
        }
        Pattern::Assignment(assign) => {
            pattern_binding_names(&assign.left, names);
        }
        Pattern::Rest(rest) => {
            pattern_binding_names(&rest.argument, names);
        }
    }
}

/// 生成标识符压缩计划。
///
/// `reserved`：调用方（bundler）声明的、mangler **绝不可生成**的名字。emit 把每个模块包成
/// `function(m,$,_r){…}` 并在 mangle **之后**把 `exports`→`$`、`__wake_require__`→`_r`、
/// `module.exports`→`m.$` 压缩，故这些压缩名(`m`/`$`/`_r`)不在 [`RUNTIME_NAMES`]（那是压缩前的名）。
/// 若某模块可压缩绑定 ≥13 个，`nth_name` 会排到 `m`，与包装器参数 `m` 撞成 `class m` 之类的重复声明。
/// 由 bundler 经此参数声明，mangler 保持通用、不硬编码 bundler 约定。
pub fn plan_mangle(program: &Program, interner: &Interner, reserved: &[&str]) -> Mangling {
    if has_hazard(program, interner) {
        return Mangling::empty();
    }
    let model = analyze(program);
    plan_mangle_with_model(program, interner, reserved, &model)
}

/// [`plan_mangle`] variant for callers that already built the semantic model.
///
/// The caller must perform the `eval` / `with` hazard check before using this
/// function. Bundling runs several semantic minification passes, so sharing one
/// model avoids repeatedly walking large vendor modules.
pub fn plan_mangle_with_model(
    program: &Program,
    interner: &Interner,
    reserved: &[&str],
    model: &SemanticModel,
) -> Mangling {
    plan_mangle_with_model_and_protected(program, interner, reserved, model, &[])
}

/// Shared-model mangle planner with source ranges whose referenced bindings
/// must keep their original names (used by verbatim expression replacements).
pub fn plan_mangle_with_model_and_protected(
    program: &Program,
    interner: &Interner,
    reserved: &[&str],
    model: &SemanticModel,
    protected_ranges: &[Span],
) -> Mangling {
    if model.symbols.is_empty() || model.scopes.is_empty() {
        return Mangling::empty();
    }

    let n_scopes = model.scopes.len();
    let mut scope_symbols: Vec<Vec<SymbolId>> = vec![Vec::new(); n_scopes];
    for (sid, sym) in model.symbols.iter().enumerate() {
        scope_symbols[sym.scope as usize].push(sid as SymbolId);
    }
    let mut children: Vec<Vec<ScopeId>> = vec![Vec::new(); n_scopes];
    for (id, sc) in model.scopes.iter().enumerate() {
        if let Some(p) = sc.parent {
            children[p as usize].push(id as ScopeId);
        }
    }

    let exported_names = collect_exported_names(program);

    let mut forbidden: FxHashSet<Atom> = FxHashSet::default();
    for r in &model.references {
        if r.resolved.is_none() {
            forbidden.insert(r.name);
        }
    }
    for name in RUNTIME_NAMES {
        forbidden.insert(interner.intern(name));
    }
    // 调用方声明的保留名（如 emit 包装器参数 `m`/`$`/`_r`）——并入 forbidden 即 `assign` 的 `path`
    // 种子，`nth_name` 候选命中即跳过，从根本上不会生成它们。
    for name in reserved {
        forbidden.insert(interner.intern(name));
    }

    // Every scope starts assigning from the same short-name sequence. Generate
    // and intern that sequence once instead of allocating `a`, `b`, ... again
    // for every function scope (large vendor bundles have thousands of them).
    let candidate_count = model.symbols.len() + forbidden.len();
    let mut candidates = Vec::with_capacity(candidate_count);
    let mut candidate_index = 0usize;
    while candidates.len() < candidate_count {
        let candidate = nth_name(candidate_index);
        candidate_index += 1;
        if !is_reserved(&candidate) {
            candidates.push(interner.intern(&candidate));
        }
    }

    let mut synthetic: FxHashSet<SymbolId> = FxHashSet::default();
    for (sid, sym) in model.symbols.iter().enumerate() {
        if sym.span.is_dummy() {
            synthetic.insert(sid as SymbolId);
        }
    }
    for r in &model.references {
        if r.span.is_dummy()
            && let Some(sid) = r.resolved
        {
            synthetic.insert(sid);
        }
        if protected_ranges
            .iter()
            .any(|range| range.lo <= r.span.lo && r.span.hi <= range.hi)
            && let Some(sid) = r.resolved
        {
            synthetic.insert(sid);
        }
    }

    let mut new_name: Vec<Option<Atom>> = vec![None; model.symbols.len()];
    let mut ctx = AssignCtx {
        model,
        scope_symbols: &scope_symbols,
        children: &children,
        candidates: &candidates,
        synthetic: &synthetic,
        new_name: &mut new_name,
        exported_names: &exported_names,
    };
    ctx.assign(MODULE_SCOPE, &mut forbidden);

    let mut renames: FxHashMap<Span, Atom> = FxHashMap::with_capacity_and_hasher(
        model.symbols.len() + model.references.len(),
        Default::default(),
    );
    let mut renamed_symbols = 0usize;
    for (sid, sym) in model.symbols.iter().enumerate() {
        if let Some(nn) = new_name[sid]
            && !sym.span.is_dummy()
        {
            renames.insert(sym.span, nn);
            renamed_symbols += 1;
        }
    }
    for r in &model.references {
        if !r.span.is_dummy()
            && let Some(sid) = r.resolved
            && let Some(nn) = new_name[sid as usize]
        {
            renames.insert(r.span, nn);
        }
    }
    Mangling {
        renames,
        renamed_symbols,
    }
}

fn is_renameable(sym: &Symbol, _exported_names: &FxHashSet<Atom>) -> bool {
    // 仅 `Import` 绑定不重命名（其局部名与源模块导出名相关；保守，收益小）。
    // **函数声明**与**模块顶层已导出名**现在也重命名：单包 concat 下顶层为块/闭包作用域，
    // 导出经 `$["原名"]` 字符串键透传，重命名标识符对外透明——codegen 的导出赋值/默认导出的**值**
    // 已改为按绑定 span 查 rename 表发新名（见 `renamed_or`/`emit_export_binding`）。
    !matches!(sym.decl_kind, DeclKind::Import)
}

struct AssignCtx<'a> {
    model: &'a SemanticModel,
    scope_symbols: &'a [Vec<SymbolId>],
    children: &'a [Vec<ScopeId>],
    candidates: &'a [Atom],
    synthetic: &'a FxHashSet<SymbolId>,
    new_name: &'a mut [Option<Atom>],
    exported_names: &'a FxHashSet<Atom>,
}

impl AssignCtx<'_> {
    fn assign(&mut self, scope: ScopeId, path: &mut FxHashSet<Atom>) {
        let mut added: Vec<Atom> = Vec::new();
        // Hoisted declarations are recorded before lexical/import bindings, regardless of their
        // source order. Reserve every binding that must keep its original name before assigning
        // short names, otherwise a hoisted function can steal a later import's name (for example
        // `import { useEffect as a } ...; function compare() {}`).
        for &sid in &self.scope_symbols[scope as usize] {
            let sym = &self.model.symbols[sid as usize];
            if (!is_renameable(sym, self.exported_names) || self.synthetic.contains(&sid))
                && path.insert(sym.name)
            {
                added.push(sym.name);
            }
        }

        let mut counter = 0usize;
        for &sid in &self.scope_symbols[scope as usize] {
            let sym = &self.model.symbols[sid as usize];
            if is_renameable(sym, self.exported_names) && !self.synthetic.contains(&sid) {
                let atom = loop {
                    let atom = self.candidates[counter];
                    counter += 1;
                    if !path.contains(&atom) {
                        break atom;
                    }
                };
                self.new_name[sid as usize] = Some(atom);
                path.insert(atom);
                added.push(atom);
            }
        }
        for &child in &self.children[scope as usize] {
            self.assign(child, path);
        }
        for a in added {
            path.remove(&a);
        }
    }
}

// ── Hazard detection: eval / with ──

pub fn has_hazard(program: &Program, interner: &Interner) -> bool {
    let mut h = Hazard {
        eval_atom: interner.intern("eval"),
        found: false,
    };
    h.visit_program(program);
    h.found
}

struct Hazard {
    eval_atom: Atom,
    found: bool,
}

impl<'a> Visit<'a> for Hazard {
    fn visit_statement(&mut self, node: &Statement<'a>) {
        if self.found {
            return;
        }
        if matches!(node, Statement::With(_)) {
            self.found = true;
            return;
        }
        walk_statement(self, node);
    }
    fn visit_expression(&mut self, node: &Expression<'a>) {
        if self.found {
            return;
        }
        if let Expression::Call(c) = node
            && let Expression::Identifier(id) = &c.callee
            && id.name == self.eval_atom
        {
            self.found = true;
            return;
        }
        walk_expression(self, node);
    }
}

// ── Name generation ──

const FIRST: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
const REST: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";

pub(crate) fn nth_name(mut n: usize) -> String {
    let mut s = String::new();
    s.push(FIRST[n % FIRST.len()] as char);
    n /= FIRST.len();
    while n > 0 {
        n -= 1;
        s.push(REST[n % REST.len()] as char);
        n /= REST.len();
    }
    s
}

pub(crate) fn is_reserved(name: &str) -> bool {
    matches!(
        name,
        "await"
            | "break"
            | "case"
            | "catch"
            | "class"
            | "const"
            | "continue"
            | "debugger"
            | "default"
            | "delete"
            | "do"
            | "else"
            | "enum"
            | "export"
            | "extends"
            | "false"
            | "finally"
            | "for"
            | "function"
            | "if"
            | "import"
            | "in"
            | "instanceof"
            | "new"
            | "null"
            | "return"
            | "super"
            | "switch"
            | "this"
            | "throw"
            | "true"
            | "try"
            | "typeof"
            | "var"
            | "void"
            | "while"
            | "with"
            | "yield"
            | "let"
            | "static"
            // `using`：语句起始处的 `using x` 被解析为 using 声明，故生成的名字不能叫 `using`。
            | "using"
            | "implements"
            | "interface"
            | "package"
            | "private"
            | "protected"
            | "public"
            | "arguments"
            | "eval"
            | "undefined"
            | "NaN"
            | "Infinity"
    )
}
