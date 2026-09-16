use std::sync::Arc;

use wake_lint_core::{
    LintOptions, RuleLevel, RuleSetting, SourceType, TypeId, TypeKind, TypeLiteral, TypeNode,
    TypeSource, TypedSource,
};

#[test]
fn switch_exhaustiveness_requires_default_for_union_discriminants() {
    let source = TypeSource::new(
        "a.ts",
        Arc::from("declare const state: 'a' | 'b'; switch (state) { case 'a': break; }"),
        SourceType::TypeScript,
    )
    .unwrap();
    assert_eq!(source.switches().len(), 1);
    let typed = TypedSource::new(
        source,
        vec![
            TypeNode {
                kind: TypeKind::String,
                ..Default::default()
            },
            TypeNode {
                kind: TypeKind::String,
                ..Default::default()
            },
            TypeNode {
                kind: TypeKind::Union,
                parts: vec![TypeId(0), TypeId(1)],
                ..Default::default()
            },
        ],
        vec![],
    )
    .unwrap()
    .with_switch_types(vec![TypeId(2)])
    .unwrap();
    let result = typed
        .lint(&LintOptions {
            recommended: false,
            rules: [(
                "ts/switch-exhaustiveness-check".into(),
                RuleSetting::Level(RuleLevel::Error),
            )]
            .into(),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(result.diagnostics.len(), 1);
    assert_eq!(result.diagnostics[0].message_id, "missingDefault");
}

#[test]
fn switch_exhaustiveness_requires_complete_discriminant_facts() {
    let source = TypeSource::new(
        "a.ts",
        Arc::from("switch (state) { default: break; }"),
        SourceType::TypeScript,
    )
    .unwrap();
    assert!(
        TypedSource::new(
            source,
            vec![TypeNode {
                kind: TypeKind::String,
                ..Default::default()
            }],
            vec![],
        )
        .unwrap()
        .lint(&LintOptions {
            recommended: false,
            rules: [(
                "ts/switch-exhaustiveness-check".into(),
                RuleLevel::Error.into()
            )]
            .into(),
            ..Default::default()
        })
        .is_err()
    );
}

#[test]
fn switch_literal_union_cases_prove_exhaustiveness_without_a_default() {
    let source = TypeSource::new(
        "a.ts",
        Arc::from(
            "declare const state: 'a' | 'b'; switch (state) { case 'a': break; case 'b': break; }",
        ),
        SourceType::TypeScript,
    )
    .unwrap();
    let typed = TypedSource::new(
        source,
        vec![
            TypeNode {
                kind: TypeKind::String,
                literal: Some(TypeLiteral::String("a".into())),
                ..Default::default()
            },
            TypeNode {
                kind: TypeKind::String,
                literal: Some(TypeLiteral::String("b".into())),
                ..Default::default()
            },
            TypeNode {
                kind: TypeKind::Union,
                parts: vec![TypeId(0), TypeId(1)],
                ..Default::default()
            },
        ],
        vec![],
    )
    .unwrap()
    .with_switch_types(vec![TypeId(2)])
    .unwrap();
    let result = typed
        .lint(&LintOptions {
            recommended: false,
            rules: [(
                "ts/switch-exhaustiveness-check".into(),
                RuleLevel::Error.into(),
            )]
            .into(),
            ..Default::default()
        })
        .unwrap();
    assert!(result.diagnostics.is_empty());
}

#[test]
fn switch_literal_union_matching_covers_all_supported_primitive_values() {
    let source = TypeSource::new(
        "a.ts",
        Arc::from(
            "declare const state: 'text' | 1 | true | null | undefined; switch (state) { case 'text': break; case 1: break; case true: break; case null: break; case void 0: break; }",
        ),
        SourceType::TypeScript,
    )
    .unwrap();
    let typed = TypedSource::new(
        source,
        vec![
            TypeNode {
                kind: TypeKind::String,
                literal: Some(TypeLiteral::String("text".into())),
                ..Default::default()
            },
            TypeNode {
                kind: TypeKind::Number,
                literal: Some(TypeLiteral::Number(1.0)),
                ..Default::default()
            },
            TypeNode {
                kind: TypeKind::Boolean,
                literal: Some(TypeLiteral::Boolean(true)),
                ..Default::default()
            },
            TypeNode {
                kind: TypeKind::Null,
                literal: Some(TypeLiteral::Null),
                ..Default::default()
            },
            TypeNode {
                kind: TypeKind::Undefined,
                literal: Some(TypeLiteral::Undefined),
                ..Default::default()
            },
            TypeNode {
                kind: TypeKind::Union,
                parts: (0..5).map(TypeId).collect(),
                ..Default::default()
            },
        ],
        vec![],
    )
    .unwrap()
    .with_switch_types(vec![TypeId(5)])
    .unwrap();
    let result = typed
        .lint(&LintOptions {
            recommended: false,
            rules: [(
                "ts/switch-exhaustiveness-check".into(),
                RuleLevel::Error.into(),
            )]
            .into(),
            ..Default::default()
        })
        .unwrap();
    assert!(result.diagnostics.is_empty());
}

#[test]
fn switch_bigint_literals_match_exact_values_without_float_rounding() {
    let source = TypeSource::new(
        "a.ts",
        Arc::from(
            "declare const state: 9007199254740993n | 16n; switch (state) { case 0x20000000000001n: break; case 0x10n: break; }",
        ),
        SourceType::TypeScript,
    )
    .unwrap();
    let typed = TypedSource::new(
        source,
        vec![
            TypeNode {
                kind: TypeKind::BigInt,
                literal: Some(TypeLiteral::BigInt("9007199254740993".into())),
                ..Default::default()
            },
            TypeNode {
                kind: TypeKind::BigInt,
                literal: Some(TypeLiteral::BigInt("16".into())),
                ..Default::default()
            },
            TypeNode {
                kind: TypeKind::Union,
                parts: vec![TypeId(0), TypeId(1)],
                ..Default::default()
            },
        ],
        vec![],
    )
    .unwrap()
    .with_switch_types(vec![TypeId(2)])
    .unwrap();
    let result = typed
        .lint(&LintOptions {
            recommended: false,
            rules: [(
                "ts/switch-exhaustiveness-check".into(),
                RuleSetting::Level(RuleLevel::Error),
            )]
            .into(),
            ..Default::default()
        })
        .unwrap();
    assert!(result.diagnostics.is_empty());
}

#[test]
fn switch_enum_member_type_facts_prove_unknown_case_expressions() {
    let source = TypeSource::new(
        "a.ts",
        Arc::from(
            "enum State { A = 'a', B = 'b' } declare const state: State; switch (state) { case State.A: break; case State.B: break; }",
        ),
        SourceType::TypeScript,
    )
    .unwrap();
    let typed = TypedSource::new(
        source,
        vec![
            TypeNode {
                kind: TypeKind::String,
                literal: Some(TypeLiteral::String("a".into())),
                ..Default::default()
            },
            TypeNode {
                kind: TypeKind::String,
                literal: Some(TypeLiteral::String("b".into())),
                ..Default::default()
            },
            TypeNode {
                kind: TypeKind::Union,
                parts: vec![TypeId(0), TypeId(1)],
                ..Default::default()
            },
        ],
        vec![],
    )
    .unwrap()
    .with_switch_types(vec![TypeId(2)])
    .unwrap()
    .with_switch_case_types(vec![vec![TypeId(0), TypeId(1)]])
    .unwrap();
    let result = typed
        .lint(&LintOptions {
            recommended: false,
            rules: [(
                "ts/switch-exhaustiveness-check".into(),
                RuleLevel::Error.into(),
            )]
            .into(),
            ..Default::default()
        })
        .unwrap();
    assert!(result.diagnostics.is_empty());
}
