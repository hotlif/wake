use std::sync::Arc;

use wake_lint_core::{
    LintOptions, RuleLevel, RuleSetting, SourceType, TypeId, TypeKind, TypeNode, TypeSource,
    TypedSource,
};

#[test]
fn unsafe_returns_report_any_values_and_ignore_safe_values() {
    let source = TypeSource::new(
        "a.ts",
        Arc::from("function read() { return input; } function safe() { return 1; }"),
        SourceType::TypeScript,
    )
    .unwrap();
    assert_eq!(source.returns().len(), 2);
    let typed = TypedSource::new(
        source,
        vec![
            TypeNode {
                kind: TypeKind::Any,
                ..Default::default()
            },
            TypeNode {
                kind: TypeKind::Number,
                ..Default::default()
            },
        ],
        vec![],
    )
    .unwrap()
    .with_return_types(vec![TypeId(0), TypeId(1)])
    .unwrap();
    let result = typed
        .lint(&LintOptions {
            recommended: false,
            rules: [(
                "ts/no-unsafe-return".into(),
                RuleSetting::Level(RuleLevel::Error),
            )]
            .into(),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(result.diagnostics.len(), 1);
    assert_eq!(result.diagnostics[0].message_id, "unsafeReturn");
}

#[test]
fn unsafe_returns_report_any_nested_in_generic_type_arguments() {
    let source = TypeSource::new(
        "a.ts",
        Arc::from("function read() { return value; }"),
        SourceType::TypeScript,
    )
    .unwrap();
    let typed = TypedSource::new(
        source,
        vec![
            TypeNode {
                kind: TypeKind::Any,
                ..Default::default()
            },
            TypeNode {
                kind: TypeKind::Object,
                type_arguments: vec![TypeId(0)],
                ..Default::default()
            },
        ],
        vec![],
    )
    .unwrap()
    .with_return_types(vec![TypeId(1)])
    .unwrap();
    let result = typed
        .lint(&LintOptions {
            recommended: false,
            rules: [("ts/no-unsafe-return".into(), RuleLevel::Error.into())].into(),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(result.diagnostics.len(), 1);
    assert_eq!(result.diagnostics[0].message_id, "unsafeReturn");
}

#[test]
fn unsafe_returns_require_complete_value_facts() {
    let source = TypeSource::new(
        "a.ts",
        Arc::from("function read() { return input; }"),
        SourceType::TypeScript,
    )
    .unwrap();
    assert!(
        TypedSource::new(
            source,
            vec![TypeNode {
                kind: TypeKind::Any,
                ..Default::default()
            }],
            vec![],
        )
        .unwrap()
        .lint(&LintOptions {
            recommended: false,
            rules: [("ts/no-unsafe-return".into(), RuleLevel::Error.into())].into(),
            ..Default::default()
        })
        .is_err()
    );
}
