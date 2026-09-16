use std::sync::Arc;

use wake_lint_core::{
    CallType, LintOptions, RuleLevel, RuleSetting, SourceType, TypeId, TypeKind, TypeNode,
    TypeSource, TypedSource,
};

#[test]
fn floating_promises_only_report_top_level_expression_calls() {
    let source = TypeSource::new(
        "a.ts",
        Arc::from(
            "async function run() { task(); await task(); const value = task(); void task(); }",
        ),
        SourceType::TypeScript,
    )
    .unwrap();
    assert_eq!(source.expression_statements().len(), 3);
    let typed = TypedSource::new(
        source,
        vec![
            TypeNode {
                kind: TypeKind::Object,
                ..Default::default()
            },
            TypeNode {
                kind: TypeKind::Object,
                standard_promise: true,
                ..Default::default()
            },
        ],
        vec![
            Some(CallType {
                type_id: TypeId(0),
                returns: vec![TypeKind::Object],
                return_types: vec![TypeId(1)],
                construct_signatures: 0,
            }),
            Some(CallType {
                type_id: TypeId(0),
                returns: vec![TypeKind::Object],
                return_types: vec![TypeId(1)],
                construct_signatures: 0,
            }),
            Some(CallType {
                type_id: TypeId(0),
                returns: vec![TypeKind::Object],
                return_types: vec![TypeId(1)],
                construct_signatures: 0,
            }),
            Some(CallType {
                type_id: TypeId(0),
                returns: vec![TypeKind::Object],
                return_types: vec![TypeId(1)],
                construct_signatures: 0,
            }),
        ],
    )
    .unwrap();
    let typed = typed
        .with_expression_statement_types(vec![TypeId(1), TypeId(0), TypeId(0)])
        .unwrap();
    let result = typed
        .lint(&LintOptions {
            recommended: false,
            rules: [("ts/no-floating-promises".into(), RuleLevel::Error.into())].into(),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(result.diagnostics.len(), 1);
    assert_eq!(result.diagnostics[0].message_id, "floating");
}

#[test]
fn floating_promises_need_return_type_facts() {
    let source = TypeSource::new("a.ts", Arc::from("task();"), SourceType::TypeScript).unwrap();
    assert!(
        TypedSource::new(
            source,
            vec![TypeNode {
                kind: TypeKind::Object,
                ..Default::default()
            }],
            vec![Some(CallType::new(TypeId(0)))],
        )
        .unwrap()
        .with_expression_statement_types(vec![TypeId(0)])
        .unwrap()
        .lint(&LintOptions {
            recommended: false,
            rules: [(
                "ts/no-floating-promises".into(),
                RuleSetting::Level(RuleLevel::Error)
            )]
            .into(),
            ..Default::default()
        })
        .is_err()
    );
}

#[test]
fn floating_promises_need_expression_statement_type_facts() {
    let source = TypeSource::new("a.ts", Arc::from("pending;"), SourceType::TypeScript).unwrap();
    assert!(
        TypedSource::new(
            source,
            vec![TypeNode {
                kind: TypeKind::Object,
                standard_promise: true,
                ..Default::default()
            }],
            Vec::new(),
        )
        .unwrap()
        .lint(&LintOptions {
            recommended: false,
            rules: [("ts/no-floating-promises".into(), RuleLevel::Error.into())].into(),
            ..Default::default()
        })
        .is_err()
    );
}

#[test]
fn floating_promises_report_standard_promise_like_returns() {
    let source = TypeSource::new("a.ts", Arc::from("task();"), SourceType::TypeScript).unwrap();
    let typed = TypedSource::new(
        source,
        vec![
            TypeNode {
                kind: TypeKind::Object,
                ..Default::default()
            },
            TypeNode {
                kind: TypeKind::Object,
                standard_thenable: true,
                ..Default::default()
            },
        ],
        vec![Some(CallType {
            type_id: TypeId(0),
            returns: vec![TypeKind::Object],
            return_types: vec![TypeId(1)],
            construct_signatures: 0,
        })],
    )
    .unwrap();
    let typed = typed
        .with_expression_statement_types(vec![TypeId(1)])
        .unwrap();
    let result = typed
        .lint(&LintOptions {
            recommended: false,
            rules: [("ts/no-floating-promises".into(), RuleLevel::Error.into())].into(),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(result.diagnostics.len(), 1);
    assert_eq!(result.diagnostics[0].message_id, "floating");
}

#[test]
fn floating_promises_report_bare_promise_expression_statements() {
    let source = TypeSource::new(
        "a.ts",
        Arc::from("declare const pending: Promise<void>; pending; await pending; void pending;"),
        SourceType::TypeScript,
    )
    .unwrap();
    assert_eq!(source.expression_statements().len(), 3);
    let typed = TypedSource::new(
        source,
        vec![
            TypeNode {
                kind: TypeKind::Object,
                standard_promise: true,
                ..Default::default()
            },
            TypeNode {
                kind: TypeKind::Void,
                ..Default::default()
            },
        ],
        Vec::new(),
    )
    .unwrap()
    .with_expression_statement_types(vec![TypeId(0), TypeId(1), TypeId(1)])
    .unwrap();
    let result = typed
        .lint(&LintOptions {
            recommended: false,
            rules: [("ts/no-floating-promises".into(), RuleLevel::Error.into())].into(),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(result.diagnostics.len(), 1);
    assert_eq!(result.diagnostics[0].message_id, "floating");
}

#[test]
fn floating_promises_report_dynamic_import_returns() {
    let source = TypeSource::new(
        "a.ts",
        Arc::from(
            "import('./chunk'); await import('./chunk'); const value = import('./chunk'); void import('./chunk');",
        ),
        SourceType::TypeScript,
    )
    .unwrap();
    assert_eq!(source.calls().len(), 4);
    assert!(
        source
            .calls()
            .iter()
            .all(|call| call.kind == wake_lint_core::SourceCallKind::DynamicImport)
    );
    let typed = TypedSource::new(
        source,
        vec![
            TypeNode {
                kind: TypeKind::Object,
                standard_promise: true,
                ..Default::default()
            },
            TypeNode {
                kind: TypeKind::Void,
                ..Default::default()
            },
        ],
        vec![
            Some(CallType {
                type_id: TypeId(0),
                returns: vec![TypeKind::Object],
                return_types: vec![TypeId(0)],
                construct_signatures: 0,
            });
            4
        ],
    )
    .unwrap();
    let typed = typed
        .with_expression_statement_types(vec![TypeId(0), TypeId(1), TypeId(1)])
        .unwrap();
    let result = typed
        .lint(&LintOptions {
            recommended: false,
            rules: [("ts/no-floating-promises".into(), RuleLevel::Error.into())].into(),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(result.diagnostics.len(), 1);
    assert_eq!(result.diagnostics[0].message_id, "floating");
}
