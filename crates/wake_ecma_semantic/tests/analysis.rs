use wake_common::Interner;
use wake_ecma_ast::SourceType;
use wake_ecma_parser::parse;
use wake_ecma_semantic::{DeclKind, ScopeKind, SemanticModel, analyze};

fn analyze_src(src: &str) -> (SemanticModel, Interner) {
    let interner = Interner::new();
    let out = parse(src, &interner, SourceType::Module);
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    let model = out.module.with_ast(|program| analyze(program, &interner));
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
fn named_function_expression_self_binding_is_outside_parameters_and_body() {
    let source = "const f=function self(read=()=>self){var self;return [read,self]}; self; const g=function inner(inner){return inner};";
    let (model, interner) = analyze_src(source);
    let self_bindings = model
        .binding_occurrences
        .iter()
        .filter(|binding| binding.name == interner.intern("self"))
        .collect::<Vec<_>>();
    assert_eq!(self_bindings.len(), 2);
    assert_ne!(self_bindings[0].symbol, self_bindings[1].symbol);
    let references = model
        .references
        .iter()
        .filter(|reference| reference.name == interner.intern("self"))
        .collect::<Vec<_>>();
    assert_eq!(references[0].resolved, Some(self_bindings[0].symbol));
    assert_eq!(references[1].resolved, Some(self_bindings[1].symbol));
    assert_eq!(references[2].resolved, None);
    let names = model
        .binding_occurrences
        .iter()
        .filter(|binding| binding.name == interner.intern("inner"))
        .collect::<Vec<_>>();
    assert_ne!(
        names[0].scope, names[1].scope,
        "the name environment surrounds the parameter environment"
    );
}

#[test]
fn body_var_does_not_merge_with_named_function_expression_self() {
    let (model, interner) = analyze_src("const f=function self(){var self;return self};");
    let names = model
        .binding_occurrences
        .iter()
        .filter(|binding| binding.name == interner.intern("self"))
        .collect::<Vec<_>>();
    assert_eq!(names.len(), 2);
    assert_ne!(names[0].symbol, names[1].symbol);
    assert_ne!(names[0].scope, names[1].scope);
    assert_eq!(
        model.references.last().unwrap().resolved,
        Some(names[1].symbol)
    );
}

#[test]
fn undeclared_is_unresolved() {
    let (model, _) = analyze_src("x + y;");
    assert_eq!(model.unresolved_count(), 2);
}

#[test]
fn ordinary_functions_own_arguments_and_arrows_capture_them() {
    let (model, interner) = analyze_src(
        "const f=function(){return [arguments,()=>arguments,function(){return arguments}]};arguments;",
    );
    let refs = model
        .references
        .iter()
        .filter(|reference| reference.name == interner.intern("arguments"))
        .collect::<Vec<_>>();
    assert_eq!(refs.len(), 4);
    assert!(
        refs[0].resolved.is_some(),
        "ordinary functions instantiate arguments"
    );
    assert_eq!(
        refs[0].resolved, refs[1].resolved,
        "arrows capture the surrounding arguments"
    );
    assert!(refs[2].resolved.is_some());
    assert_ne!(refs[0].resolved, refs[2].resolved);
    assert!(refs[3].resolved.is_none());
    assert!(
        model
            .binding_occurrences
            .iter()
            .all(|binding| binding.name != interner.intern("arguments")),
        "implicit declarations have no source occurrence"
    );
}

#[test]
fn arguments_instantiation_respects_explicit_bindings_and_body_var_initialization() {
    let source = "var arguments=99; function plain(){var arguments;return arguments};function parameter(arguments){return arguments};function defaults(read=()=>arguments){var arguments;return [read,arguments]};";
    let interner = Interner::new();
    let parsed = parse(source, &interner, SourceType::Script);
    assert!(!parsed.has_errors());
    let model = parsed
        .module
        .with_ast(|program| analyze(program, &interner));
    let refs = model
        .references
        .iter()
        .filter(|reference| reference.name == interner.intern("arguments"))
        .collect::<Vec<_>>();
    let root = model.scopes[0].bindings[&interner.intern("arguments")];
    assert!(
        refs.iter()
            .all(|reference| reference.resolved.is_some() && reference.resolved != Some(root))
    );
    assert_ne!(
        refs[2].resolved, refs[3].resolved,
        "parameter expressions and the body have separate environments"
    );
    assert!(model.parameter_copies.iter().any(
        |copy| Some(copy.parameter) == refs[2].resolved && Some(copy.body) == refs[3].resolved
    ));
}

#[test]
fn direct_body_declarations_only_suppress_arguments_without_parameter_expressions() {
    let source = "function f(){function arguments(){};return arguments};function g(){let arguments;return arguments};function h(read=()=>arguments){function arguments(){};return arguments};";
    let interner = Interner::new();
    let parsed = parse(source, &interner, SourceType::Script);
    assert!(!parsed.has_errors());
    let model = parsed
        .module
        .with_ast(|program| analyze(program, &interner));
    let refs = model
        .references
        .iter()
        .filter(|reference| reference.name == interner.intern("arguments"))
        .collect::<Vec<_>>();
    let kinds = refs
        .iter()
        .map(|reference| model.symbols[reference.resolved.unwrap() as usize].decl_kind)
        .collect::<Vec<_>>();
    assert_eq!(
        kinds,
        [
            DeclKind::Function,
            DeclKind::Let,
            DeclKind::Arguments,
            DeclKind::Function
        ]
    );
    assert_ne!(refs[2].resolved, refs[3].resolved);
    for reference in &refs[..2] {
        assert!(!model.symbols.iter().any(
            |symbol| symbol.scope == reference.scope && symbol.decl_kind == DeclKind::Arguments
        ));
    }
    assert!(
        model.parameter_copies.is_empty(),
        "a body function overrides the implicit parameter value"
    );
}

#[test]
fn var_hoisting() {
    let (model, _) = analyze_src("function f() { return v; var v = 1; }");
    assert_eq!(model.unresolved_count(), 0);
}

#[test]
fn strict_block_functions_are_lexical_while_var_still_hoists() {
    let (model, interner) = analyze_src(
        "let first; { first = () => local; function local() {} var shared; } { local; function local() {} } local; shared;",
    );
    let local = interner.intern("local");
    let references = model
        .references
        .iter()
        .filter(|reference| reference.name == local)
        .collect::<Vec<_>>();
    assert_eq!(references.len(), 3);
    assert!(references[0].resolved.is_some());
    assert!(references[1].resolved.is_some());
    assert_ne!(
        references[0].resolved, references[1].resolved,
        "sibling blocks own different functions"
    );
    assert!(
        references[2].resolved.is_none(),
        "a strict block function must not escape"
    );
    assert!(
        model
            .symbols
            .iter()
            .filter(|symbol| symbol.name == local)
            .all(|symbol| symbol.scope != 0)
    );
    assert!(
        model
            .references
            .iter()
            .find(|reference| reference.name == interner.intern("shared"))
            .unwrap()
            .resolved
            .is_some()
    );
}

#[test]
fn strict_function_and_class_contexts_keep_nested_functions_in_their_blocks() {
    let source = "function strictBody() { 'use strict'; { function hidden() {} hidden; } hidden; } const arrow = () => { 'use strict'; { function hidden() {} hidden; } hidden; }; class C { method() { { function hidden() {} hidden; } hidden; } }";
    let interner = Interner::new();
    let parsed = parse(source, &interner, SourceType::Script);
    assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);
    let model = parsed
        .module
        .with_ast(|program| analyze(program, &interner));
    let hidden = interner.intern("hidden");
    let references = model
        .references
        .iter()
        .filter(|reference| reference.name == hidden)
        .collect::<Vec<_>>();
    assert_eq!(references.len(), 6);
    for pair in references.as_chunks::<2>().0 {
        assert!(pair[0].resolved.is_some());
        assert!(pair[1].resolved.is_none());
    }
}

#[test]
fn strict_switch_try_and_catch_functions_are_predeclared_in_their_lexical_scopes() {
    let (model, interner) = analyze_src(
        "switch (input) { case 0: inside; break; default: function inside() {} } inside; try { inside; function inside() {} } catch(error) { inside; function inside() {} } finally { inside; function inside() {} } inside;",
    );
    let references = model
        .references
        .iter()
        .filter(|reference| reference.name == interner.intern("inside"))
        .collect::<Vec<_>>();
    assert_eq!(references.len(), 6);
    assert_eq!(
        references
            .iter()
            .map(|reference| reference.resolved.is_some())
            .collect::<Vec<_>>(),
        [true, false, true, true, true, false]
    );
    let scopes = references
        .iter()
        .filter_map(|reference| {
            reference
                .resolved
                .map(|id| model.symbols[id as usize].scope)
        })
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(scopes.len(), 4);
}

#[test]
fn class_static_blocks_own_var_environments_and_predeclare_their_lexical_bindings() {
    let (model, interner) = analyze_src(
        "class C { static { value; helper; later; { var value; } function helper() {} let later; } static { value; var value; helper; later; } } value; helper; later;",
    );
    let refs = |name: &str| {
        model
            .references
            .iter()
            .filter(|reference| reference.name == interner.intern(name))
            .collect::<Vec<_>>()
    };
    let values = refs("value");
    assert_eq!(values.len(), 3);
    assert!(values[0].resolved.is_some());
    assert!(values[1].resolved.is_some());
    assert_ne!(values[0].resolved, values[1].resolved);
    assert!(values[2].resolved.is_none());
    for reference in &values[..2] {
        let symbol = &model.symbols[reference.resolved.unwrap() as usize];
        assert_eq!(
            model.scopes[symbol.scope as usize].kind,
            ScopeKind::StaticBlock
        );
    }
    for name in ["helper", "later"] {
        let references = refs(name);
        assert_eq!(references.len(), 3);
        assert!(references[0].resolved.is_some());
        assert!(
            references[1..]
                .iter()
                .all(|reference| reference.resolved.is_none())
        );
    }
}

#[test]
fn named_class_expression_has_a_private_lexical_binding() {
    let (model, interner) = analyze_src(
        "const C = class Promise extends Promise { field = Promise; method() { return Promise; } }; Promise;",
    );
    let refs: Vec<_> = model
        .references
        .iter()
        .filter(|reference| interner.resolve(reference.name) == "Promise")
        .collect();
    assert_eq!(refs.len(), 4);
    let symbol = refs[0]
        .resolved
        .expect("class heritage sees the inner binding, even in TDZ");
    assert!(
        refs[..3]
            .iter()
            .all(|reference| reference.resolved == Some(symbol))
    );
    assert!(refs[3].resolved.is_none());
    assert_eq!(model.symbols[symbol as usize].decl_kind, DeclKind::Class);
}

#[test]
fn reference_access_distinguishes_writes_from_target_evaluation() {
    use wake_ecma_semantic::ReferenceAccess::{Read, ReadWrite, Write};
    let (model, interner) = analyze_src(
        "let a, b, obj, key, fallback, values; a = b; a += b; a++; obj[key] = a; [a = fallback, ...b] = values; ({ x: a, [key]: b } = obj); for (a of values) {} for (obj[key] in values) {}",
    );
    let references: Vec<_> = model
        .references
        .iter()
        .map(|reference| (interner.resolve(reference.name), reference.access))
        .collect();
    let expected = [
        ("a", Write),
        ("b", Read),
        ("a", ReadWrite),
        ("b", Read),
        ("a", ReadWrite),
        ("obj", Read),
        ("key", Read),
        ("a", Read),
        ("a", Write),
        ("fallback", Read),
        ("b", Write),
        ("values", Read),
        ("a", Write),
        ("key", Read),
        ("b", Write),
        ("obj", Read),
        ("a", Write),
        ("values", Read),
        ("obj", Read),
        ("key", Read),
        ("values", Read),
    ];
    assert_eq!(
        references,
        expected
            .into_iter()
            .map(|(name, access)| (name.into(), access))
            .collect::<Vec<_>>()
    );
    assert_eq!(model.unresolved_count(), 0);
}

#[test]
fn source_projection_excludes_jsx_helpers_and_synthetic_namespace_reads() {
    let source = "namespace N { export const x = input; } const unused = value; const el = <UI.Item value={N.x} />;";
    let interner = Interner::new();
    let parsed =
        wake_ecma_parser::parse_source(source, &interner, SourceType::Tsx, Default::default());
    assert!(
        !parsed.parsed.has_errors(),
        "{:?}",
        parsed.parsed.diagnostics
    );
    let facts = parsed.parsed.module.with_ast(|program| {
        wake_ecma_semantic::analyze_source(
            program,
            &interner,
            wake_ecma_semantic::SourceSemanticInput {
                identifiers: &parsed.identifiers,
                exports: &parsed.exports,
                syntax: &parsed.syntax,
                functions: &parsed.functions,
                namespaces: &parsed.namespaces,
            },
        )
    });
    let references: Vec<_> = facts
        .references
        .iter()
        .map(|index| interner.resolve(facts.model.references[*index].name))
        .collect();
    assert_eq!(references, ["input", "value", "UI", "N"]);
    assert!(facts.source_symbols.iter().all(|id| {
        !interner
            .resolve(facts.model.symbols[*id as usize].name)
            .starts_with("_jsx")
    }));
    assert_eq!(
        facts.model.references[facts.references[3]]
            .resolved
            .map(|id| interner.resolve(facts.model.symbols[id as usize].name)),
        Some("N".into())
    );
}

#[test]
fn repeated_var_declarations_report_each_occurrence_with_one_symbol() {
    let (model, interner) = analyze_src("var value=0;{var value;}value=1;");
    let value = interner.intern("value");
    let occurrences: Vec<_> = model
        .binding_occurrences
        .iter()
        .filter(|occurrence| occurrence.name == value)
        .collect();
    assert!(occurrences.len() >= 2);
    assert!(
        occurrences
            .iter()
            .all(|occurrence| occurrence.symbol == occurrences[0].symbol)
    );
    assert!(
        occurrences
            .iter()
            .any(|occurrence| occurrence.span != occurrences[0].span),
        "the concrete redeclaration location must not collapse into the canonical symbol span"
    );
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
fn parameters_and_var_function_redeclarations_share_one_binding_without_parameter_expressions() {
    let (model, interner) = analyze_src(
        "function f(value) { var value; return value; } function g({ item }) { var item; return item; } function h(value) { function value() {} return value; } var mixed; function mixed() {} mixed;",
    );
    for name in ["value", "item", "mixed"] {
        let name = interner.intern(name);
        let mut per_scope = std::collections::HashMap::<_, std::collections::HashSet<_>>::new();
        for binding in &model.binding_occurrences {
            if binding.name == name {
                per_scope
                    .entry(binding.scope)
                    .or_default()
                    .insert(binding.symbol);
            }
        }
        assert!(
            per_scope.values().all(|symbols| symbols.len() == 1),
            "{per_scope:?}"
        );
    }
    let value = interner.intern("value");
    let kinds = model
        .binding_occurrences
        .iter()
        .filter(|binding| binding.name == value)
        .map(|binding| binding.decl_kind)
        .collect::<Vec<_>>();
    assert!(
        kinds.contains(&DeclKind::Param)
            && kinds.contains(&DeclKind::Var)
            && kinds.contains(&DeclKind::Function)
    );
}

#[test]
fn parameter_initialization_closures_cannot_resolve_body_bindings() {
    let (model, interner) = analyze_src(
        "const body = 'outer'; function f(value, read = () => value, external = () => body) { var value = 3; var body = 'inner'; return [read(), external(), value, body]; } const arrow = (read = () => body) => { var body; return read; };",
    );
    let value = interner.intern("value");
    let refs = model
        .references
        .iter()
        .filter(|reference| reference.name == value)
        .collect::<Vec<_>>();
    assert_eq!(refs.len(), 2);
    let parameter = &model.symbols[refs[0].resolved.unwrap() as usize];
    let body = &model.symbols[refs[1].resolved.unwrap() as usize];
    assert_eq!(parameter.decl_kind, DeclKind::Param);
    assert_eq!(body.decl_kind, DeclKind::Var);
    assert_ne!(parameter.scope, body.scope);
    assert_eq!(
        model.scopes[body.scope as usize].parent,
        Some(parameter.scope)
    );
    assert_eq!(model.parameter_copies.len(), 1);
    assert_eq!(
        model.parameter_copies[0].parameter,
        refs[0].resolved.unwrap()
    );
    assert_eq!(model.parameter_copies[0].body, refs[1].resolved.unwrap());
    let outer_body = model
        .symbols
        .iter()
        .enumerate()
        .find(|(_, symbol)| symbol.name == interner.intern("body") && symbol.scope == 0)
        .unwrap()
        .0 as u32;
    let body_refs = model
        .references
        .iter()
        .filter(|reference| reference.name == interner.intern("body"))
        .collect::<Vec<_>>();
    assert_eq!(body_refs.len(), 3);
    assert_eq!(body_refs[0].resolved, Some(outer_body));
    assert_eq!(body_refs[2].resolved, Some(outer_body));
}

#[test]
fn computed_parameter_keys_split_body_scope_but_plain_rest_does_not() {
    let (model, interner) = analyze_src(
        "const key='item'; function computed({[key]: value}) { var value; var key; return value; } function rest(...items) { var items; return items; } function replaced(value=1) { function value() {} return value; }",
    );
    assert_eq!(
        model.parameter_copies.len(),
        1,
        "a body function replaces rather than copies the parameter"
    );
    let copy = model.parameter_copies[0];
    assert_eq!(
        interner.resolve(model.symbols[copy.parameter as usize].name),
        "value"
    );
    assert_ne!(
        model.symbols[copy.parameter as usize].scope,
        model.symbols[copy.body as usize].scope
    );
    let key_reference = model
        .references
        .iter()
        .find(|reference| reference.name == interner.intern("key"))
        .unwrap();
    assert_eq!(
        model.symbols[key_reference.resolved.unwrap() as usize].scope,
        0
    );
    let rest_bindings = model
        .binding_occurrences
        .iter()
        .filter(|binding| binding.name == interner.intern("items"))
        .collect::<Vec<_>>();
    assert_eq!(rest_bindings.len(), 2);
    assert_eq!(rest_bindings[0].symbol, rest_bindings[1].symbol);
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
fn source_import_projection_keeps_unaliased_locals_and_excludes_erased_type_bindings() {
    let source = "import { Named, Other as Alias, type Shape } from 'm'; import type Only from 'types'; Named(); Alias();";
    let interner = Interner::new();
    let parsed = wake_ecma_parser::parse_source(
        source,
        &interner,
        SourceType::TypeScript,
        wake_ecma_parser::ParseOptions::default(),
    );
    assert!(!parsed.parsed.has_errors());
    let model = parsed.parsed.module.with_ast(|program| {
        wake_ecma_semantic::analyze_source(
            program,
            &interner,
            wake_ecma_semantic::SourceSemanticInput {
                identifiers: &parsed.identifiers,
                exports: &parsed.exports,
                syntax: &parsed.syntax,
                functions: &parsed.functions,
                namespaces: &parsed.namespaces,
            },
        )
    });
    let mut names = model
        .source_symbols
        .iter()
        .map(|id| interner.resolve(model.model.symbols[*id as usize].name))
        .collect::<Vec<_>>();
    names.sort();
    assert_eq!(names, ["Alias", "Named"]);
    assert!(
        model
            .references
            .iter()
            .all(|index| model.model.references[*index].resolved.is_some())
    );
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
