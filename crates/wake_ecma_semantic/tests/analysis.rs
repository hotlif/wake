use wake_common::Interner;
use wake_ecma_ast::SourceType;
use wake_ecma_parser::parse;
use wake_ecma_semantic::{DeclKind, ScopeKind, SemanticModel, analyze};

fn analyze_src(src: &str) -> (SemanticModel, Interner) {
    let interner = Interner::new();
    let out = parse(src, &interner, SourceType::Module);
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    let model = out.module.with_ast(analyze);
    (model, interner)
}

#[test]
fn basic_bindings_and_refs() {
    let (model, interner) = analyze_src("const a = 1; let b = a + 1; b;");
    let names: Vec<String> = model
        .symbols
        .iter()
        .map(|symbol| interner.resolve(symbol.name))
        .collect();
    assert!(names.contains(&"a".to_string()));
    assert!(names.contains(&"b".to_string()));
    assert_eq!(model.unresolved_count(), 0);
}

#[test]
fn undeclared_is_unresolved() {
    let (model, _) = analyze_src("x + y;");
    assert_eq!(model.unresolved_count(), 2);
}

#[test]
fn var_hoisting() {
    let (model, _) = analyze_src("function f() { return v; var v = 1; }");
    assert_eq!(model.unresolved_count(), 0);
}

#[test]
fn block_scoping() {
    let (model, interner) = analyze_src("{ let c = 1; } c;");
    let c = interner.intern("c");
    let outer_reference = model
        .references
        .iter()
        .find(|reference| reference.name == c)
        .unwrap();
    assert!(outer_reference.resolved.is_none());
}

#[test]
fn function_params_and_closures() {
    let (model, _) =
        analyze_src("function outer(a, b) { return function inner(c) { return a + b + c; }; }");
    assert_eq!(model.unresolved_count(), 0);
}

#[test]
fn destructuring_bindings() {
    let (model, _) = analyze_src("const { x, y: z, ...rest } = obj; x; z; rest;");
    assert_eq!(model.unresolved_count(), 1);
}

#[test]
fn imports_are_bound() {
    let (model, _) = analyze_src("import def, { named } from 'mod'; def; named;");
    assert_eq!(model.unresolved_count(), 0);
}

#[test]
fn scope_tree_shape() {
    let (model, _) = analyze_src("function f() { { let a = 1; } }");
    assert!(model.scopes.len() >= 3);
    assert_eq!(model.scopes[0].kind, ScopeKind::Module);
}

#[test]
fn lexical_bindings_resolve_before_their_declaration_node() {
    let (model, interner) =
        analyze_src("let x=0;let read;{read=()=>x;let x=1;}let b=0;{let a=b,b=1;}read();");
    let x = interner.intern("x");
    let b = interner.intern("b");
    let x_reference = model
        .references
        .iter()
        .find(|reference| reference.name == x)
        .expect("arrow reads x");
    let b_reference = model
        .references
        .iter()
        .find(|reference| reference.name == b)
        .expect("first declarator reads b");
    let x_symbol = x_reference.resolved.expect("inner x is predeclared");
    let b_symbol = b_reference.resolved.expect("inner b is predeclared");
    assert_ne!(model.symbols[x_symbol as usize].scope, 0);
    assert_ne!(model.symbols[b_symbol as usize].scope, 0);
    assert_eq!(model.symbols[x_symbol as usize].decl_kind, DeclKind::Let);
    assert_eq!(model.symbols[b_symbol as usize].decl_kind, DeclKind::Let);
}

#[test]
fn class_and_import_bindings_are_predeclared() {
    let (model, interner) =
        analyze_src("let read=()=>C;export default class C{};use(value);import value from 'm';");
    for name in [interner.intern("C"), interner.intern("value")] {
        let reference = model
            .references
            .iter()
            .find(|reference| reference.name == name)
            .expect("pre-declaration reference exists");
        assert!(reference.resolved.is_some());
    }
    assert_eq!(model.unresolved_count(), 1, "only use() is global");
}
