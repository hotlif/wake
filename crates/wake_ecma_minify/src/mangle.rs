//! # Identifier Mangling — Scope-safe Rename Planner
//!
//! Produces a [`Mangling`] side-table (span → new name) consumed by `wake_ecma_codegen`.
//! Does NOT mutate the immutable arena AST.
//!
//! Renames function/block/catch locals, params, AND module-level symbols that
//! are not exported, not imported, and not function declarations.

use wake_common::{Atom, FxHashMap, FxHashSet, Interner, Span};
use wake_ecma_ast::{
    Expression, Pattern, Program, Statement, Visit, walk_expression, walk_statement,
    ModuleExportName,
};
use wake_ecma_parser::analyze;
use wake_ecma_parser::semantic::{DeclKind, ScopeId, Symbol, SymbolId};

/// Mangling plan: every identifier occurrence (decl + ref) that gets renamed, mapped to its new name.
#[derive(Debug, Default)]
pub struct Mangling {
    renames: FxHashMap<Span, Atom>,
    pub renamed_symbols: usize,
}

impl Mangling {
    fn empty() -> Self {
        Mangling { renames: FxHashMap::default(), renamed_symbols: 0 }
    }
    pub fn is_empty(&self) -> bool { self.renames.is_empty() }
    pub fn get(&self, span: Span) -> Option<Atom> { self.renames.get(&span).copied() }
    pub fn table(&self) -> &FxHashMap<Span, Atom> { &self.renames }
}

const MODULE_SCOPE: ScopeId = 0;

const RUNTIME_NAMES: &[&str] = &[
    "exports", "module", "require",
    "__wake_require__", "__wake_interop_default", "__wake_interop_star",
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
                        ModuleExportName::Ident(id) => { exported.insert(id.name); }
                        ModuleExportName::String(s) => { exported.insert(*s); }
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
                        ModuleExportName::Ident(id) => { exported.insert(id.name); }
                        ModuleExportName::String(s) => { exported.insert(*s); }
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
        Pattern::Ident(id) => { names.insert(id.name); }
        Pattern::Array(arr) => {
            for elem in &arr.elements {
                if let Some(p) = elem {
                    pattern_binding_names(p, names);
                }
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

pub fn plan_mangle(program: &Program, interner: &Interner) -> Mangling {
    if has_hazard(program, interner) {
        return Mangling::empty();
    }
    let model = analyze(program);
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

    let mut synthetic: FxHashSet<SymbolId> = FxHashSet::default();
    for (sid, sym) in model.symbols.iter().enumerate() {
        if sym.span.is_dummy() { synthetic.insert(sid as SymbolId); }
    }
    for r in &model.references {
        if r.span.is_dummy() && let Some(sid) = r.resolved {
            synthetic.insert(sid);
        }
    }

    let mut new_name: Vec<Option<Atom>> = vec![None; model.symbols.len()];
    let mut ctx = AssignCtx {
        model: &model, interner, scope_symbols: &scope_symbols,
        children: &children, synthetic: &synthetic, new_name: &mut new_name,
        exported_names: &exported_names,
    };
    ctx.assign(MODULE_SCOPE, &mut forbidden);

    let mut renames: FxHashMap<Span, Atom> = FxHashMap::default();
    let mut renamed_symbols = 0usize;
    for (sid, sym) in model.symbols.iter().enumerate() {
        if let Some(nn) = new_name[sid] && !sym.span.is_dummy() {
            renames.insert(sym.span, nn); renamed_symbols += 1;
        }
    }
    for r in &model.references {
        if !r.span.is_dummy() && let Some(sid) = r.resolved && let Some(nn) = new_name[sid as usize] {
            renames.insert(r.span, nn);
        }
    }
    Mangling { renames, renamed_symbols }
}

fn is_renameable(sym: &Symbol, exported_names: &FxHashSet<Atom>) -> bool {
    if matches!(sym.decl_kind, DeclKind::Import | DeclKind::Function) {
        return false;
    }
    if sym.scope == MODULE_SCOPE {
        return !exported_names.contains(&sym.name);
    }
    true
}

struct AssignCtx<'a> {
    model: &'a wake_ecma_parser::SemanticModel,
    interner: &'a Interner,
    scope_symbols: &'a [Vec<SymbolId>],
    children: &'a [Vec<ScopeId>],
    synthetic: &'a FxHashSet<SymbolId>,
    new_name: &'a mut [Option<Atom>],
    exported_names: &'a FxHashSet<Atom>,
}

impl AssignCtx<'_> {
    fn assign(&mut self, scope: ScopeId, path: &mut FxHashSet<Atom>) {
        let mut added: Vec<Atom> = Vec::new();
        let mut counter = 0usize;
        for &sid in &self.scope_symbols[scope as usize] {
            let sym = &self.model.symbols[sid as usize];
            if is_renameable(sym, self.exported_names) && !self.synthetic.contains(&sid) {
                let atom = loop {
                    let cand = nth_name(counter); counter += 1;
                    if is_reserved(&cand) { continue; }
                    let atom = self.interner.intern(&cand);
                    if !path.contains(&atom) { break atom; }
                };
                self.new_name[sid as usize] = Some(atom);
                path.insert(atom); added.push(atom);
            } else {
                if path.insert(sym.name) { added.push(sym.name); }
            }
        }
        for &child in &self.children[scope as usize] {
            self.assign(child, path);
        }
        for a in added { path.remove(&a); }
    }
}

// ── Hazard detection: eval / with ──

pub(crate) fn has_hazard(program: &Program, interner: &Interner) -> bool {
    let mut h = Hazard { eval_atom: interner.intern("eval"), found: false };
    h.visit_program(program);
    h.found
}

struct Hazard { eval_atom: Atom, found: bool }

impl<'a> Visit<'a> for Hazard {
    fn visit_statement(&mut self, node: &Statement<'a>) {
        if self.found { return; }
        if matches!(node, Statement::With(_)) { self.found = true; return; }
        walk_statement(self, node);
    }
    fn visit_expression(&mut self, node: &Expression<'a>) {
        if self.found { return; }
        if let Expression::Call(c) = node
            && let Expression::Identifier(id) = &c.callee
            && id.name == self.eval_atom
        { self.found = true; return; }
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
    while n > 0 { n -= 1; s.push(REST[n % REST.len()] as char); n /= REST.len(); }
    s
}

pub(crate) fn is_reserved(name: &str) -> bool {
    matches!(name,
        "await" | "break" | "case" | "catch" | "class" | "const" | "continue"
        | "debugger" | "default" | "delete" | "do" | "else" | "enum" | "export"
        | "extends" | "false" | "finally" | "for" | "function" | "if" | "import"
        | "in" | "instanceof" | "new" | "null" | "return" | "super" | "switch"
        | "this" | "throw" | "true" | "try" | "typeof" | "var" | "void" | "while"
        | "with" | "yield" | "let" | "static"
        | "implements" | "interface" | "package" | "private" | "protected" | "public"
        | "arguments" | "eval" | "undefined" | "NaN" | "Infinity"
    )
}
