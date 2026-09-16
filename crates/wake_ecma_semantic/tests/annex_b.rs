use wake_common::Interner;
use wake_ecma_parser::{SourceType, parse};
use wake_ecma_semantic::{DeclKind, ScopeKind, analyze};

#[test]
fn arguments_copy_records_a_runtime_created_body_binding_without_inventing_a_static_target() {
    let interner = Interner::new();
    let parsed = parse(
        "function f(p=1){const before=arguments;{function arguments(){}}return arguments}",
        &interner,
        SourceType::Script,
    );
    assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);
    let model = parsed
        .module
        .with_ast(|program| analyze(program, &interner));
    assert_eq!(model.annex_b_copies.len(), 1);
    let copy = &model.annex_b_copies[0];
    assert_eq!(copy.outer, None);
    assert_eq!(
        model.scopes[copy.target_scope as usize].kind,
        ScopeKind::FunctionBody
    );
}

#[test]
fn copy_candidates_respect_cases_catch_patterns_loop_bindings_and_duplicate_functions() {
    for (source, count, outer) in [
        ("{ function item(){} function item(){} } item;", 2, true),
        (
            "switch(input){case 0: function item(){}; break; default: item;} item;",
            1,
            true,
        ),
        (
            "if(flag) function item(){} else function item(){} item;",
            2,
            true,
        ),
        ("try{}catch(item){{function item(){}}} item;", 1, true),
        ("try{}catch({item}){{function item(){}}} item;", 0, false),
        ("for(let item of input){function item(){}} item;", 0, false),
        ("for(var item of input){function item(){}} item;", 1, true),
        ("{function item(){} {function item(){}}} item;", 1, true),
        ("{function item(){'use strict'}} item;", 1, true),
    ] {
        let interner = Interner::new();
        let parsed = parse(source, &interner, SourceType::Script);
        assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);
        let model = parsed
            .module
            .with_ast(|program| analyze(program, &interner));
        assert_eq!(model.annex_b_copies.len(), count, "{source}");
        assert_eq!(
            model.references.last().unwrap().resolved.is_some(),
            outer,
            "{source}"
        );
        for copy in &model.annex_b_copies {
            assert_ne!(Some(copy.lexical), copy.outer);
            assert_eq!(model.symbols[copy.outer.unwrap() as usize].scope, 0);
        }
    }
}

#[test]
fn sloppy_block_functions_have_lexical_bindings_and_separate_implicit_var_copies() {
    let source = "before; { inside; function inside() { return inside; } } inside; if (flag) { function before() {} } before;";
    let interner = Interner::new();
    let parsed = parse(source, &interner, SourceType::Script);
    assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);
    let model = parsed
        .module
        .with_ast(|program| analyze(program, &interner));
    let uses = model
        .references
        .iter()
        .filter(|r| r.name == interner.intern("inside"))
        .collect::<Vec<_>>();
    assert_eq!(uses.len(), 3);
    assert_eq!(uses[0].resolved, uses[1].resolved);
    assert_ne!(uses[1].resolved, uses[2].resolved);
    let lexical = uses[0].resolved.unwrap();
    let outer = uses[2].resolved.unwrap();
    assert_eq!(
        model.scopes[model.symbols[lexical as usize].scope as usize].kind,
        ScopeKind::Block
    );
    assert_eq!(model.symbols[outer as usize].decl_kind, DeclKind::Var);
    assert_eq!(model.symbols[outer as usize].scope, 0);
    assert!(!model.binding_occurrences.iter().any(|b| b.symbol == outer));
    assert_eq!(model.annex_b_copies.len(), 2);
    assert!(
        model
            .annex_b_copies
            .iter()
            .any(|copy| copy.lexical == lexical && copy.outer == Some(outer))
    );
    assert!(
        model
            .references
            .iter()
            .filter(|r| r.name == interner.intern("before"))
            .all(|r| r.resolved.is_some())
    );
}

#[test]
fn parameters_and_intervening_lexical_declarations_block_annex_b_hoisting() {
    let source = "function parameter(same) { { function same() {} } return same; } function lexical() { let same = 1; { function same() {} } return same; } function nested() { { let same = 1; { function same() {} } } return typeof same; }";
    let interner = Interner::new();
    let parsed = parse(source, &interner, SourceType::Script);
    assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);
    let model = parsed
        .module
        .with_ast(|program| analyze(program, &interner));
    assert!(model.annex_b_copies.is_empty());
    let refs = model
        .references
        .iter()
        .filter(|r| r.name == interner.intern("same"))
        .collect::<Vec<_>>();
    assert_eq!(refs.len(), 3);
    assert_eq!(
        model.symbols[refs[0].resolved.unwrap() as usize].decl_kind,
        DeclKind::Param
    );
    assert_eq!(
        model.symbols[refs[1].resolved.unwrap() as usize].decl_kind,
        DeclKind::Let
    );
    assert_eq!(refs[2].resolved, None);
}

#[test]
fn strict_async_and_generator_block_functions_never_create_legacy_outer_bindings() {
    for (source, kind) in [
        ("{ function only() {} } only;", SourceType::Module),
        (
            "'use strict'; { function only() {} } only;",
            SourceType::Script,
        ),
        ("{ async function only() {} } only;", SourceType::Script),
        ("{ function* only() {} } only;", SourceType::Script),
    ] {
        let interner = Interner::new();
        let parsed = parse(source, &interner, kind);
        assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);
        let model = parsed
            .module
            .with_ast(|program| analyze(program, &interner));
        assert!(model.annex_b_copies.is_empty());
        assert_eq!(model.references.last().unwrap().resolved, None, "{source}");
        let function = model
            .symbols
            .iter()
            .find(|s| s.name == interner.intern("only"))
            .unwrap();
        assert_eq!(model.scopes[function.scope as usize].kind, ScopeKind::Block);
    }
}
