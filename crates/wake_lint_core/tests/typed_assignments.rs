use std::sync::Arc;

use wake_lint_core::{
    LintOptions, RuleLevel, RuleSetting, SourceType, TypeId, TypeKind, TypeNode, TypeSource,
    TypedSource,
};

fn options() -> LintOptions {
    LintOptions {
        recommended: false,
        rules: [(
            "ts/no-unsafe-assignment".into(),
            RuleSetting::Level(RuleLevel::Error),
        )]
        .into_iter()
        .collect(),
        ..Default::default()
    }
}

#[test]
fn unsafe_assignments_report_any_values_from_original_initializers_and_assignments() {
    let source = TypeSource::new(
        "a.ts",
        Arc::from("let first = value; first = value; const safe = 1;"),
        SourceType::TypeScript,
    )
    .unwrap();
    assert_eq!(source.assignments().len(), 3);
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
    .with_assignment_types(vec![TypeId(0), TypeId(0), TypeId(1)])
    .unwrap();
    let result = typed.lint(&options()).unwrap();
    assert_eq!(result.diagnostics.len(), 2);
    assert!(
        result
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.message_id == "unsafeAssignment")
    );
}

#[test]
fn unsafe_assignments_report_any_and_error_nested_in_generic_type_arguments() {
    let source = TypeSource::new(
        "a.ts",
        Arc::from("let anyValue = value; let errorValue = other;"),
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
                kind: TypeKind::Error,
                ..Default::default()
            },
            TypeNode {
                kind: TypeKind::Object,
                type_arguments: vec![TypeId(0)],
                ..Default::default()
            },
            TypeNode {
                kind: TypeKind::Object,
                type_arguments: vec![TypeId(1)],
                ..Default::default()
            },
            TypeNode {
                kind: TypeKind::Object,
                reference_target: Some(TypeId(3)),
                ..Default::default()
            },
        ],
        vec![],
    )
    .unwrap()
    .with_assignment_types(vec![TypeId(2), TypeId(4)])
    .unwrap();
    let result = typed.lint(&options()).unwrap();
    assert_eq!(
        result
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.message_id.as_str())
            .collect::<Vec<_>>(),
        ["unsafeAssignment", "errorAssignment"]
    );
}

#[test]
fn unsafe_assignments_report_any_and_error_nested_in_structural_properties() {
    let source = TypeSource::new(
        "a.ts",
        Arc::from("let anyValue = value; let errorValue = other;"),
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
                kind: TypeKind::Error,
                ..Default::default()
            },
            TypeNode {
                kind: TypeKind::Object,
                properties: vec![TypeId(0)],
                ..Default::default()
            },
            TypeNode {
                kind: TypeKind::Object,
                properties: vec![TypeId(1)],
                ..Default::default()
            },
        ],
        vec![],
    )
    .unwrap()
    .with_assignment_types(vec![TypeId(2), TypeId(3)])
    .unwrap();
    let result = typed.lint(&options()).unwrap();
    assert_eq!(
        result
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.message_id.as_str())
            .collect::<Vec<_>>(),
        ["unsafeAssignment", "errorAssignment"]
    );
}

#[test]
fn unsafe_assignments_propagate_through_recursive_structural_properties() {
    let source = TypeSource::new(
        "a.ts",
        Arc::from("let value = input;"),
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
                properties: vec![TypeId(2)],
                ..Default::default()
            },
            TypeNode {
                kind: TypeKind::Object,
                properties: vec![TypeId(1), TypeId(0)],
                ..Default::default()
            },
        ],
        vec![],
    )
    .unwrap()
    .with_assignment_types(vec![TypeId(1)])
    .unwrap();
    let result = typed.lint(&options()).unwrap();
    assert_eq!(result.diagnostics.len(), 1);
    assert_eq!(result.diagnostics[0].message_id, "unsafeAssignment");
}

#[test]
fn unsafe_assignments_report_any_and_error_from_index_signature_values() {
    let source = TypeSource::new(
        "a.ts",
        Arc::from("let anyValue = value; let errorValue = other;"),
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
                kind: TypeKind::Error,
                ..Default::default()
            },
            TypeNode {
                kind: TypeKind::Object,
                properties: vec![TypeId(0)],
                ..Default::default()
            },
            TypeNode {
                kind: TypeKind::Object,
                properties: vec![TypeId(1)],
                ..Default::default()
            },
        ],
        vec![],
    )
    .unwrap()
    .with_assignment_types(vec![TypeId(2), TypeId(3)])
    .unwrap();
    let result = typed.lint(&options()).unwrap();
    assert_eq!(
        result
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.message_id.as_str())
            .collect::<Vec<_>>(),
        ["unsafeAssignment", "errorAssignment"]
    );
}

#[test]
fn unsafe_assignments_report_any_and_error_from_callable_property_returns() {
    let source = TypeSource::new(
        "a.ts",
        Arc::from("let anyValue = value; let errorValue = other;"),
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
                kind: TypeKind::Error,
                ..Default::default()
            },
            TypeNode {
                kind: TypeKind::Object,
                signature_returns: vec![TypeId(0)],
                ..Default::default()
            },
            TypeNode {
                kind: TypeKind::Object,
                signature_returns: vec![TypeId(1)],
                ..Default::default()
            },
            TypeNode {
                kind: TypeKind::Object,
                properties: vec![TypeId(2)],
                ..Default::default()
            },
            TypeNode {
                kind: TypeKind::Object,
                properties: vec![TypeId(3)],
                ..Default::default()
            },
        ],
        vec![],
    )
    .unwrap()
    .with_assignment_types(vec![TypeId(4), TypeId(5)])
    .unwrap();
    let result = typed.lint(&options()).unwrap();
    assert_eq!(
        result
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.message_id.as_str())
            .collect::<Vec<_>>(),
        ["unsafeAssignment", "errorAssignment"]
    );
}

#[test]
fn unsafe_assignments_require_complete_value_facts() {
    let source = TypeSource::new(
        "a.ts",
        Arc::from("let value = input;"),
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
        .lint(&options())
        .is_err()
    );
}
