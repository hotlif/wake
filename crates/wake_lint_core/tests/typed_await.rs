use std::sync::Arc;

use wake_lint_core::{
    LintOptions, RuleLevel, RuleSetting, SourceType, TypeId, TypeKind, TypeNode, TypeSource,
    TypedSource, lint_text,
};

fn options() -> LintOptions {
    LintOptions {
        recommended: false,
        rules: [(
            "ts/await-thenable".into(),
            RuleSetting::Level(RuleLevel::Error),
        )]
        .into_iter()
        .collect(),
        ..Default::default()
    }
}

#[test]
fn await_thenable_uses_source_bound_operand_types() {
    let source = TypeSource::new(
        "a.ts",
        Arc::from("async function run(value: number, promise: Promise<number>) { await value; await promise; }"),
        SourceType::TypeScript,
    )
    .unwrap();
    assert_eq!(source.awaits().len(), 2);
    let typed = TypedSource::new(
        source,
        vec![
            TypeNode {
                kind: TypeKind::Number,
                ..Default::default()
            },
            TypeNode {
                kind: TypeKind::Object,
                standard_promise: true,
                ..Default::default()
            },
        ],
        vec![],
    )
    .unwrap()
    .with_await_types(vec![TypeId(0), TypeId(1)])
    .unwrap();
    let result = typed.lint(&options()).unwrap();
    assert_eq!(result.diagnostics.len(), 1);
    assert_eq!(result.diagnostics[0].rule_id, "ts/await-thenable");
    assert_eq!(result.diagnostics[0].message_id, "notThenable");
}

#[test]
fn await_thenable_accepts_standard_promise_like_identity() {
    let source = TypeSource::new(
        "a.ts",
        Arc::from("async function run(value: PromiseLike<number>) { await value; }"),
        SourceType::TypeScript,
    )
    .unwrap();
    let typed = TypedSource::new(
        source,
        vec![TypeNode {
            kind: TypeKind::Object,
            standard_thenable: true,
            ..Default::default()
        }],
        vec![],
    )
    .unwrap()
    .with_await_types(vec![TypeId(0)])
    .unwrap();
    let result = typed.lint(&options()).unwrap();
    assert!(result.diagnostics.is_empty());
}

#[test]
fn await_thenable_proof_propagates_through_type_relations() {
    let source = TypeSource::new(
        "a.ts",
        Arc::from("async function run(a: A, b: B, c: C, d: D, e: E) { await a; await b; await c; await d; await e; }"),
        SourceType::TypeScript,
    )
    .unwrap();
    let typed = TypedSource::new(
        source,
        vec![
            TypeNode {
                kind: TypeKind::Object,
                standard_thenable: true,
                ..Default::default()
            },
            TypeNode {
                kind: TypeKind::Object,
                reference_target: Some(TypeId(0)),
                ..Default::default()
            },
            TypeNode {
                kind: TypeKind::Union,
                parts: vec![TypeId(0), TypeId(1)],
                ..Default::default()
            },
            TypeNode {
                kind: TypeKind::Intersection,
                parts: vec![TypeId(1), TypeId(2)],
                ..Default::default()
            },
            TypeNode {
                kind: TypeKind::Parameter,
                constraint: Some(TypeId(3)),
                ..Default::default()
            },
        ],
        vec![],
    )
    .unwrap()
    .with_await_types((0..5).map(TypeId).collect())
    .unwrap();
    let result = typed.lint(&options()).unwrap();
    assert!(result.diagnostics.is_empty());
}

#[test]
fn await_thenable_requires_complete_facts() {
    let source = TypeSource::new(
        "a.ts",
        Arc::from("async function run(value: number) { await value; }"),
        SourceType::TypeScript,
    )
    .unwrap();
    assert!(
        TypedSource::new(
            source,
            vec![TypeNode {
                kind: TypeKind::Number,
                ..Default::default()
            }],
            vec![],
        )
        .unwrap()
        .lint(&options())
        .is_err()
    );
}

#[test]
fn plain_text_await_rule_requires_type_facts() {
    let error = lint_text(
        "async function run(value: number) { await value; }",
        SourceType::TypeScript,
        &options(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("source-bound"), "{error}");
}
