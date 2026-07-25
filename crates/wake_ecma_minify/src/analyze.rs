use wake_common::{Atom, FxHashMap, FxHashSet, Interner, Span};
use wake_ecma_ast::*;
use wake_ecma_parser::analyze;
use wake_ecma_parser::semantic::{DeclKind, ScopeId, SymbolId};

use crate::const_eval::expr_is_pure;
use crate::mangle::has_hazard;

const MODULE_SCOPE: ScopeId = 0;

#[derive(Debug, Default)]
pub struct VarAnalysis {
    pub unused_vars: FxHashSet<Atom>,
    pub single_use_vars: FxHashMap<Atom, Span>,
    pub inline_candidates: FxHashMap<Atom, Span>,
}

pub fn analyze_vars(program: &Program, interner: &Interner) -> VarAnalysis {
    if has_hazard(program, interner) {
        return VarAnalysis::default();
    }

    let model = analyze(program);

    if model.symbols.is_empty() || model.scopes.is_empty() {
        return VarAnalysis::default();
    }

    let mut ref_counts: FxHashMap<SymbolId, usize> = FxHashMap::default();
    for r in &model.references {
        if let Some(sid) = r.resolved {
            *ref_counts.entry(sid).or_default() += 1;
        }
    }

    let exported = collect_exported_names(program);
    let init_map = collect_init_map(program);

    let mut unused_vars = FxHashSet::default();
    let mut single_use_vars = FxHashMap::default();
    let mut inline_candidates = FxHashMap::default();

    for (sid, sym) in model.symbols.iter().enumerate() {
        let sid = sid as SymbolId;

        if sym.decl_kind == DeclKind::Import {
            continue;
        }

        if sym.scope == MODULE_SCOPE && exported.contains(&sym.name) {
            continue;
        }

        let ref_count = ref_counts.get(&sid).copied().unwrap_or(0);

        if ref_count == 0 {
            unused_vars.insert(sym.name);
        } else if ref_count == 1 {
            let ref_span = model
                .references
                .iter()
                .find(|r| r.resolved == Some(sid))
                .map(|r| r.span)
                .expect("single-use var must have a reference");
            single_use_vars.insert(sym.name, ref_span);

            if let Some(init) = init_map.get(&sym.span) {
                if expr_is_pure(init) {
                    inline_candidates.insert(sym.name, sym.span);
                }
            }
        }
    }

    VarAnalysis { unused_vars, single_use_vars, inline_candidates }
}

// ── Export name collection ──

fn collect_exported_names(program: &Program) -> FxHashSet<Atom> {
    let mut names = FxHashSet::default();
    for stmt in program.body.iter() {
        match stmt {
            Statement::ExportNamed(e) => {
                if let Some(d) = &e.declaration {
                    exported_names_in_decl(d, &mut names);
                }
                for spec in e.specifiers.iter() {
                    if let ModuleExportName::Ident(ident) = spec.local {
                        names.insert(ident.name);
                    }
                }
            }
            Statement::ExportDefault(e) => match &e.declaration {
                ExportDefaultKind::Function(f) => {
                    if let Some(id) = f.id {
                        names.insert(id.name);
                    }
                }
                ExportDefaultKind::Class(c) => {
                    if let Some(id) = c.id {
                        names.insert(id.name);
                    }
                }
                ExportDefaultKind::Expression(_) => {}
            },
            _ => {}
        }
    }
    names
}

fn exported_names_in_decl(stmt: &Statement, names: &mut FxHashSet<Atom>) {
    match stmt {
        Statement::VariableDeclaration(d) => {
            for decl in d.declarations.iter() {
                pattern_names(&decl.id, names);
            }
        }
        Statement::FunctionDeclaration(f) => {
            if let Some(id) = f.id {
                names.insert(id.name);
            }
        }
        Statement::ClassDeclaration(c) => {
            if let Some(id) = c.id {
                names.insert(id.name);
            }
        }
        _ => {}
    }
}

fn pattern_names(pat: &Pattern, names: &mut FxHashSet<Atom>) {
    match pat {
        Pattern::Ident(id) => {
            names.insert(id.name);
        }
        Pattern::Array(a) => {
            for el in a.elements.iter().flatten() {
                pattern_names(el, names);
            }
        }
        Pattern::Object(o) => {
            for p in o.properties.iter() {
                pattern_names(&p.value, names);
            }
            if let Some(rest) = &o.rest {
                pattern_names(&rest.argument, names);
            }
        }
        Pattern::Assignment(a) => pattern_names(&a.left, names),
        Pattern::Rest(r) => pattern_names(&r.argument, names),
    }
}

// ── Initializer map ──

pub fn collect_init_map<'a>(program: &'a Program<'a>) -> FxHashMap<Span, &'a Expression<'a>> {
    let mut map = FxHashMap::default();
    stmt_init_map(&program.body, &mut map);
    map
}

fn stmt_init_map<'a>(
    stmts: &'a [Statement<'a>],
    map: &mut FxHashMap<Span, &'a Expression<'a>>,
) {
    for stmt in stmts {
        walk_stmt_for_inits(stmt, map);
    }
}

fn walk_stmt_for_inits<'a>(
    stmt: &'a Statement<'a>,
    map: &mut FxHashMap<Span, &'a Expression<'a>>,
) {
    match stmt {
        Statement::VariableDeclaration(d) => {
            for decl in d.declarations.iter() {
                if let Some(init) = &decl.init {
                    simple_binding_inits(&decl.id, init, map);
                }
            }
        }
        Statement::Block(b) => stmt_init_map(&b.body, map),
        Statement::If(s) => {
            walk_stmt_for_inits(&s.consequent, map);
            if let Some(a) = &s.alternate {
                walk_stmt_for_inits(a, map);
            }
        }
        Statement::For(s) => {
            if let Some(init) = &s.init {
                if let ForInit::Variable(d) = init {
                    for decl in d.declarations.iter() {
                        if let Some(init) = &decl.init {
                            simple_binding_inits(&decl.id, init, map);
                        }
                    }
                }
            }
            walk_stmt_for_inits(&s.body, map);
        }
        Statement::ForIn(s) => walk_stmt_for_inits(&s.body, map),
        Statement::ForOf(s) => walk_stmt_for_inits(&s.body, map),
        Statement::While(s) => walk_stmt_for_inits(&s.body, map),
        Statement::DoWhile(s) => walk_stmt_for_inits(&s.body, map),
        Statement::Switch(s) => {
            for case in s.cases.iter() {
                stmt_init_map(&case.consequent, map);
            }
        }
        Statement::Try(s) => {
            stmt_init_map(&s.block.body, map);
            if let Some(h) = &s.handler {
                stmt_init_map(&h.body.body, map);
            }
            if let Some(f) = &s.finalizer {
                stmt_init_map(&f.body, map);
            }
        }
        Statement::Labeled(s) => walk_stmt_for_inits(&s.body, map),
        Statement::ExportNamed(s) => {
            if let Some(d) = &s.declaration {
                walk_stmt_for_inits(d, map);
            }
        }
        Statement::FunctionDeclaration(f) => {
            if let Some(body) = f.body {
                stmt_init_map(&body.statements, map);
            }
        }
        Statement::ExportDefault(e) => {
            if let ExportDefaultKind::Function(f) = &e.declaration {
                if let Some(body) = f.body {
                    stmt_init_map(&body.statements, map);
                }
            }
        }
        _ => {}
    }
}

fn simple_binding_inits<'a>(
    pat: &'a Pattern<'a>,
    init: &'a Expression<'a>,
    map: &mut FxHashMap<Span, &'a Expression<'a>>,
) {
    if let Pattern::Ident(id) = pat {
        map.insert(id.span, init);
    }
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;
    use wake_common::Interner;
    use wake_ecma_ast::SourceType;
    use wake_ecma_parser::parse;

    fn analyze(src: &str) -> (VarAnalysis, Interner) {
        let it = Interner::new();
        let out = parse(src, &it, SourceType::Module);
        assert!(!out.has_errors(), "parse errors: {:?}", out.diagnostics);
        let a = out.module.with_ast(|p| analyze_vars(p, &it));
        (a, it)
    }

    fn contains_in_set(set: &FxHashSet<Atom>, it: &Interner, name: &str) -> bool {
        set.iter().any(|a| it.resolve(*a) == name)
    }

    fn contains_in_map(map: &FxHashMap<Atom, Span>, it: &Interner, name: &str) -> bool {
        map.keys().any(|a| it.resolve(*a) == name)
    }

    #[test]
    fn unused_local_var() {
        let (a, it) = analyze("function f(){ var x = 1; return 2; }");
        assert!(contains_in_set(&a.unused_vars, &it, "x"));
        assert!(a.single_use_vars.is_empty());
        assert!(a.inline_candidates.is_empty());
    }

    #[test]
    fn used_var_not_unused() {
        let (a, it) = analyze("function f(){ var x = 1; return x; }");
        assert!(!contains_in_set(&a.unused_vars, &it, "x"));
        assert!(contains_in_map(&a.single_use_vars, &it, "x"));
        assert!(contains_in_map(&a.inline_candidates, &it, "x"));
    }

    #[test]
    fn module_level_var_not_unused_when_used() {
        let (a, it) = analyze("const x = 5; console.log(x);");
        assert!(!contains_in_set(&a.unused_vars, &it, "x"));
    }

    #[test]
    fn import_not_marked_unused() {
        let (a, it) = analyze("import { readFile } from 'fs';");
        assert!(!contains_in_set(&a.unused_vars, &it, "readFile"));
    }

    #[test]
    fn single_use_var_detected() {
        let (a, it) = analyze("function f(){ var x = 5; return x + 1; }");
        assert!(contains_in_map(&a.single_use_vars, &it, "x"));
        assert!(contains_in_map(&a.inline_candidates, &it, "x"));
    }

    #[test]
    fn multi_use_var_not_single_use() {
        let (a, it) = analyze("function f(){ var x = 5; return x + x; }");
        assert!(!contains_in_map(&a.single_use_vars, &it, "x"));
        assert!(!contains_in_map(&a.inline_candidates, &it, "x"));
    }

    #[test]
    fn unused_function_param() {
        let (a, it) = analyze("function f(x){ return 2; }");
        assert!(contains_in_set(&a.unused_vars, &it, "x"));
    }

    #[test]
    fn eval_hazard_bails() {
        let (a, _) = analyze("var x = 1; eval('x');");
        assert!(a.unused_vars.is_empty());
        assert!(a.single_use_vars.is_empty());
        assert!(a.inline_candidates.is_empty());
    }

    #[test]
    fn with_hazard_bails() {
        let (a, _) = analyze("var x = 1; with(obj){ x; }");
        assert!(a.unused_vars.is_empty());
        assert!(a.single_use_vars.is_empty());
        assert!(a.inline_candidates.is_empty());
    }

    #[test]
    fn nested_scope_shadowing() {
        let (a, it) = analyze("function f(){ var x = 1; { var x = 2; return x; } }");
        let x_count = a
            .single_use_vars
            .keys()
            .filter(|a| it.resolve(**a) == "x")
            .count();
        assert!(x_count == 1 || a.unused_vars.is_empty());
    }

    #[test]
    fn exported_var_not_unused() {
        let (a, it) = analyze("export const x = 1;");
        assert!(!contains_in_set(&a.unused_vars, &it, "x"));
    }

    #[test]
    fn re_exported_var_not_unused() {
        let (a, it) = analyze("const x = 1; export { x };");
        assert!(!contains_in_set(&a.unused_vars, &it, "x"));
    }

    #[test]
    fn module_level_unused_var() {
        let (a, it) = analyze("const x = 1; const y = 2; console.log(y);");
        assert!(contains_in_set(&a.unused_vars, &it, "x"));
        assert!(!contains_in_set(&a.unused_vars, &it, "y"));
    }

    #[test]
    fn impure_init_not_inline() {
        let (a, it) = analyze("function f(){ var x = (y = 1); return x; }");
        assert!(contains_in_map(&a.single_use_vars, &it, "x"));
        assert!(!contains_in_map(&a.inline_candidates, &it, "x"));
    }

    #[test]
    fn no_vars_returns_empty() {
        let (a, _) = analyze("console.log('hello');");
        assert!(a.unused_vars.is_empty());
        assert!(a.single_use_vars.is_empty());
        assert!(a.inline_candidates.is_empty());
    }

    #[test]
    fn empty_program() {
        let (a, _) = analyze("");
        assert!(a.unused_vars.is_empty());
        assert!(a.single_use_vars.is_empty());
        assert!(a.inline_candidates.is_empty());
    }
}

pub fn is_undefined_shadowed(program: &Program, interner: &Interner) -> bool {
    struct UndefChecker<'a> {
        interner: &'a Interner,
        found: bool,
    }
    impl<'a, 'ast> Visit<'ast> for UndefChecker<'a> {
        fn visit_pattern(&mut self, pat: &Pattern<'ast>) {
            if let Pattern::Ident(id) = pat {
                if self.interner.resolve(id.name) == "undefined" {
                    self.found = true;
                    return;
                }
            }
            walk_pattern(self, pat);
        }
        fn visit_function(&mut self, node: &Function<'ast>) {
            if let Some(id) = node.id {
                if self.interner.resolve(id.name) == "undefined" {
                    self.found = true;
                    return;
                }
            }
            walk_function(self, node);
        }
        fn visit_class(&mut self, node: &Class<'ast>) {
            if let Some(id) = node.id {
                if self.interner.resolve(id.name) == "undefined" {
                    self.found = true;
                    return;
                }
            }
            walk_class(self, node);
        }
    }
    let mut c = UndefChecker { interner, found: false };
    c.visit_program(program);
    c.found
}
