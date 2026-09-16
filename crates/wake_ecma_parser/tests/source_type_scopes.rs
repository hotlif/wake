use wake_common::Interner;
use wake_ecma_ast::SourceTypeScopeKind;
use wake_ecma_parser::{ParseOptions, SourceType, parse, parse_source};

#[test]
fn infer_constraint_has_its_own_binding_without_leaking_into_the_pattern() {
    let source = "type Test<T> = T extends [infer U extends U, U] ? U : U;";
    let output = parse_source(
        source,
        &Interner::new(),
        SourceType::TypeScript,
        ParseOptions::default(),
    );
    assert!(
        !output.parsed.has_errors(),
        "{:?}",
        output.parsed.diagnostics
    );
    assert_eq!(output.type_scopes.len(), 3);
    let constraint = &output.type_scopes[1];
    let branch = &output.type_scopes[2];
    assert_eq!(constraint.kind, SourceTypeScopeKind::InferConstraint);
    assert_eq!(branch.kind, SourceTypeScopeKind::ConditionalTrue);
    assert_eq!(constraint.bindings, branch.bindings);
    assert_eq!(
        &source[constraint.span.lo as usize..constraint.span.hi as usize],
        "U"
    );
    assert_eq!(
        &source[branch.span.lo as usize..branch.span.hi as usize],
        "U"
    );
    assert_eq!(
        output
            .identifiers
            .iter()
            .filter(|id| **id == constraint.bindings[0])
            .count(),
        1
    );
}

#[test]
fn original_local_type_scopes_follow_grammar_and_do_not_change_compilation() {
    let source = r#"type Outer<T extends U, U = T> = {
        map: { [K in keyof K as K]: T[K] };
        call: <T>(value: T) => T;
        infer: T extends [infer X, X] ? X : X;
        nested: T extends [infer A, U extends infer B ? B : B] ? A : A;
    };
    interface Shape<S> { field: S; method<M>(value: M): S }
    class Box<C> { value: C; method<M>(value: M): C { return this.value } }
    function run<F>(value: F): F { type Inner<I> = [I, F]; return value }
    const arrow = <A,>(value: A): A => value;
    const compare = left < right > other;"#;
    let interner = Interner::new();
    let output = parse_source(
        source,
        &interner,
        SourceType::TypeScript,
        ParseOptions::default(),
    );
    assert!(
        !output.parsed.has_errors(),
        "{:?}",
        output.parsed.diagnostics
    );
    let ordinary = parse(source, &interner, SourceType::TypeScript);
    assert_eq!(
        output.parsed.module.structure_hash(),
        ordinary.module.structure_hash()
    );
    assert_eq!(
        format!("{:?}", output.parsed.dependencies),
        format!("{:?}", ordinary.dependencies)
    );
    let scopes = &output.type_scopes;
    let names: Vec<_> = scopes
        .iter()
        .map(|scope| {
            scope
                .bindings
                .iter()
                .map(|id| id.name.as_str())
                .collect::<Vec<_>>()
        })
        .collect();
    assert_eq!(
        names,
        [
            vec!["T", "U"],
            vec!["K"],
            vec!["T"],
            vec!["X"],
            vec!["B"],
            vec!["A"],
            vec!["S"],
            vec!["M"],
            vec!["C"],
            vec!["M"],
            vec!["F"],
            vec!["I"],
            vec!["A"]
        ]
    );
    let text =
        |index: usize| &source[scopes[index].span.lo as usize..scopes[index].span.hi as usize];
    assert!(text(0).starts_with("<T extends U, U = T>"));
    assert_eq!(scopes[0].parent, None);
    assert_eq!(scopes[1].kind, SourceTypeScopeKind::MappedType);
    assert_eq!(text(1), "as K]: T[K]");
    assert_eq!(text(2), "<T>(value: T) => T");
    for index in [3, 4, 5] {
        assert_eq!(scopes[index].kind, SourceTypeScopeKind::ConditionalTrue);
        assert_eq!(
            text(index),
            if index == 3 {
                "X"
            } else if index == 4 {
                "B"
            } else {
                "A"
            }
        );
    }
    assert!(text(8).ends_with("return this.value } }"));
    assert!(text(10).ends_with("return value }"));
    assert_eq!(text(12), "<A,>(value: A): A => value");
    for (index, scope) in scopes.iter().enumerate() {
        assert!(scope.span.hi > scope.span.lo);
        if let Some(parent) = scope.parent {
            assert!(parent < index);
            assert!(
                scopes[parent].span.lo <= scope.span.lo && scope.span.hi <= scopes[parent].span.hi
            );
        }
        for binding in &scope.bindings {
            assert_eq!(
                binding.role,
                wake_ecma_ast::SourceIdentifierRole::TypeBinding
            );
            assert!(output.identifiers.contains(binding));
        }
    }
}

#[test]
fn cooked_type_bindings_and_speculation_survive_tsx_and_lowering() {
    let source = r#"type Cooked<\u0054 extends T> = { [\u004B in T as K]: K };
    type Inferred<V> = V extends infer \u0058 ? X : X;
    const arrow = <\u0041,>(value: A): A => <UI value={value}/>;
    const object = { method<M>(arg: M): M { const compare = left < right > other; return arg } };
    const element = <Box<Outer<string>> value={left < right} />;"#;
    let interner = Interner::new();
    let output = parse_source(source, &interner, SourceType::Tsx, ParseOptions::default());
    assert!(
        !output.parsed.has_errors(),
        "{:?}",
        output.parsed.diagnostics
    );
    let names: Vec<_> = output
        .type_scopes
        .iter()
        .flat_map(|scope| &scope.bindings)
        .map(|binding| binding.name.as_str())
        .collect();
    assert_eq!(names, ["T", "K", "V", "X", "A", "M"]);
    let mut options = ParseOptions::default();
    options
        .transform_features
        .insert(wake_ecma_transform::EcmaFeature::ArrowFunction);
    let lowered = parse_source(source, &interner, SourceType::Tsx, options);
    assert!(
        !lowered.parsed.has_errors(),
        "{:?}",
        lowered.parsed.diagnostics
    );
    assert_eq!(output.type_scopes, lowered.type_scopes);
    let ordinary = wake_ecma_parser::parse_with(source, &interner, SourceType::Tsx, options);
    assert_eq!(
        lowered.parsed.module.structure_hash(),
        ordinary.module.structure_hash()
    );
}
