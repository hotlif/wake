use std::collections::BTreeMap;
use std::sync::Arc;

use wake_lint_core::{
    CallArgumentType, CallType, LintOptions, RuleConfiguration, RuleLevel, RuleSetting, SourceType,
    TypeId, TypeKind, TypeNode, TypeSource, TypedSource,
};

#[test]
fn misused_promises_report_synchronous_conditions_only() {
    let source = TypeSource::new(
        "a.ts",
        Arc::from("declare const promise: Promise<number>; if (promise) {} while (promise) {}"),
        SourceType::TypeScript,
    )
    .unwrap();
    assert_eq!(source.conditions().len(), 2);
    let typed = TypedSource::new(
        source,
        vec![TypeNode {
            kind: TypeKind::Object,
            standard_promise: true,
            ..Default::default()
        }],
        vec![],
    )
    .unwrap()
    .with_condition_types(vec![TypeId(0), TypeId(0)])
    .unwrap();
    let result = typed
        .lint(&LintOptions {
            recommended: false,
            rules: [(
                "ts/no-misused-promises".into(),
                RuleSetting::Level(RuleLevel::Error),
            )]
            .into(),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(result.diagnostics.len(), 2);
    assert!(
        result
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.message_id == "promiseCondition")
    );
}

#[test]
fn misused_promises_can_disable_condition_checks_for_migration() {
    let source = TypeSource::new(
        "a.ts",
        Arc::from("declare const promise: Promise<number>; if (promise) {}"),
        SourceType::TypeScript,
    )
    .unwrap();
    let typed = TypedSource::new(
        source,
        vec![TypeNode {
            kind: TypeKind::Object,
            standard_promise: true,
            ..Default::default()
        }],
        vec![],
    )
    .unwrap()
    .with_condition_types(vec![TypeId(0)])
    .unwrap();
    let result = typed
        .lint(&LintOptions {
            recommended: false,
            rules: [(
                "ts/no-misused-promises".into(),
                RuleSetting::Options(RuleConfiguration {
                    level: RuleLevel::Error,
                    options: BTreeMap::from([("checks_conditionals".into(), false.into())]),
                }),
            )]
            .into(),
            ..Default::default()
        })
        .unwrap();
    assert!(result.diagnostics.is_empty());
}

#[test]
fn disabled_condition_checks_do_not_require_condition_facts() {
    let source = TypeSource::new(
        "a.ts",
        Arc::from("declare const promise: Promise<number>; if (promise) {}"),
        SourceType::TypeScript,
    )
    .unwrap();
    let typed = TypedSource::new(
        source,
        vec![TypeNode {
            kind: TypeKind::Object,
            standard_promise: true,
            ..Default::default()
        }],
        vec![],
    )
    .unwrap();
    let result = typed.lint(&LintOptions {
        recommended: false,
        rules: [(
            "ts/no-misused-promises".into(),
            RuleSetting::Options(RuleConfiguration {
                level: RuleLevel::Error,
                options: BTreeMap::from([("checks_conditionals".into(), false.into())]),
            }),
        )]
        .into(),
        ..Default::default()
    });
    assert!(result.is_ok());
    assert!(result.unwrap().diagnostics.is_empty());
}

#[test]
fn misused_promises_can_disable_void_return_checks_for_migration() {
    let source = TypeSource::new(
        "a.ts",
        Arc::from(
            "declare function consume(callback: (value: number) => void): void; consume(async value => value);",
        ),
        SourceType::TypeScript,
    )
    .unwrap();
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
            TypeNode {
                kind: TypeKind::Object,
                ..Default::default()
            },
            TypeNode {
                kind: TypeKind::Void,
                ..Default::default()
            },
        ],
        vec![Some(CallType::new(TypeId(0)))],
    )
    .unwrap()
    .with_condition_types(Vec::new())
    .unwrap()
    .with_call_argument_types(vec![vec![CallArgumentType {
        actual: Some(CallType {
            type_id: TypeId(0),
            returns: vec![TypeKind::Object],
            return_types: vec![TypeId(1)],
            construct_signatures: 0,
        }),
        contextual: Some(CallType {
            type_id: TypeId(2),
            returns: vec![TypeKind::Void],
            return_types: vec![TypeId(3)],
            construct_signatures: 0,
        }),
    }]])
    .unwrap();
    let result = typed
        .lint(&LintOptions {
            recommended: false,
            rules: [(
                "ts/no-misused-promises".into(),
                RuleSetting::Options(RuleConfiguration {
                    level: RuleLevel::Error,
                    options: BTreeMap::from([("checks_void_return".into(), false.into())]),
                }),
            )]
            .into(),
            ..Default::default()
        })
        .unwrap();
    assert!(result.diagnostics.is_empty());
}

#[test]
fn disabled_void_return_checks_do_not_require_callback_facts() {
    let source = TypeSource::new(
        "a.ts",
        Arc::from("let callback: () => void = async () => 1;"),
        SourceType::TypeScript,
    )
    .unwrap();
    let typed = TypedSource::new(
        source,
        vec![TypeNode {
            kind: TypeKind::Object,
            standard_promise: true,
            ..Default::default()
        }],
        vec![],
    )
    .unwrap()
    .with_condition_types(Vec::new())
    .unwrap();
    let result = typed.lint(&LintOptions {
        recommended: false,
        rules: [(
            "ts/no-misused-promises".into(),
            RuleSetting::Options(RuleConfiguration {
                level: RuleLevel::Error,
                options: BTreeMap::from([("checks_void_return".into(), false.into())]),
            }),
        )]
        .into(),
        ..Default::default()
    });
    assert!(result.is_ok());
    assert!(result.unwrap().diagnostics.is_empty());
}

#[test]
fn void_return_object_options_can_disable_only_call_arguments() {
    let source = TypeSource::new(
        "a.ts",
        Arc::from(
            "declare function consume(callback: (value: number) => void): void; consume(async value => value);",
        ),
        SourceType::TypeScript,
    )
    .unwrap();
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
            TypeNode {
                kind: TypeKind::Object,
                ..Default::default()
            },
            TypeNode {
                kind: TypeKind::Void,
                ..Default::default()
            },
        ],
        vec![Some(CallType::new(TypeId(0)))],
    )
    .unwrap()
    .with_condition_types(Vec::new())
    .unwrap()
    .with_call_argument_types(vec![vec![CallArgumentType {
        actual: Some(CallType {
            type_id: TypeId(0),
            returns: vec![TypeKind::Object],
            return_types: vec![TypeId(1)],
            construct_signatures: 0,
        }),
        contextual: Some(CallType {
            type_id: TypeId(2),
            returns: vec![TypeKind::Void],
            return_types: vec![TypeId(3)],
            construct_signatures: 0,
        }),
    }]])
    .unwrap();
    let result = typed
        .lint(&LintOptions {
            recommended: false,
            rules: [(
                "ts/no-misused-promises".into(),
                RuleSetting::Options(RuleConfiguration {
                    level: RuleLevel::Error,
                    options: BTreeMap::from([(
                        "checks_void_return".into(),
                        serde_json::json!({"arguments":false}),
                    )]),
                }),
            )]
            .into(),
            ..Default::default()
        })
        .unwrap();
    assert!(result.diagnostics.is_empty());
}

#[test]
fn void_return_object_options_skip_disabled_property_fact_validation() {
    let source = TypeSource::new(
        "a.ts",
        Arc::from(
            "const value: { callback: (value: number) => void } = { callback: async value => value };",
        ),
        SourceType::TypeScript,
    )
    .unwrap();
    let typed = TypedSource::new(
        source,
        vec![TypeNode {
            kind: TypeKind::Object,
            standard_promise: true,
            ..Default::default()
        }],
        vec![],
    )
    .unwrap()
    .with_condition_types(Vec::new())
    .unwrap();
    let result = typed.lint(&LintOptions {
        recommended: false,
        rules: [(
            "ts/no-misused-promises".into(),
            RuleSetting::Options(RuleConfiguration {
                level: RuleLevel::Error,
                options: BTreeMap::from([(
                    "checks_void_return".into(),
                    serde_json::json!({"properties":false,"variables":false}),
                )]),
            }),
        )]
        .into(),
        ..Default::default()
    });
    assert!(result.is_ok(), "{result:?}");
    assert!(result.unwrap().diagnostics.is_empty());
}

#[test]
fn misused_promises_require_complete_condition_facts() {
    let source =
        TypeSource::new("a.ts", Arc::from("if (promise) {}"), SourceType::TypeScript).unwrap();
    assert!(
        TypedSource::new(
            source,
            vec![TypeNode {
                kind: TypeKind::Object,
                standard_promise: true,
                ..Default::default()
            }],
            vec![],
        )
        .unwrap()
        .lint(&LintOptions {
            recommended: false,
            rules: [("ts/no-misused-promises".into(), RuleLevel::Error.into())].into(),
            ..Default::default()
        })
        .is_err()
    );
}

#[test]
fn misused_promises_require_complete_assignment_callback_facts() {
    let source = TypeSource::new(
        "a.ts",
        Arc::from("let callback: () => void = async () => 1;"),
        SourceType::TypeScript,
    )
    .unwrap();
    assert!(
        TypedSource::new(
            source,
            vec![TypeNode {
                kind: TypeKind::Object,
                standard_promise: true,
                ..Default::default()
            }],
            vec![],
        )
        .unwrap()
        .with_condition_types(Vec::new())
        .unwrap()
        .lint(&LintOptions {
            recommended: false,
            rules: [("ts/no-misused-promises".into(), RuleLevel::Error.into())].into(),
            ..Default::default()
        })
        .is_err()
    );
}

#[test]
fn misused_promises_require_complete_return_callback_facts() {
    let source = TypeSource::new(
        "a.ts",
        Arc::from("function factory(): () => void { return async () => 1; }"),
        SourceType::TypeScript,
    )
    .unwrap();
    assert!(
        TypedSource::new(
            source,
            vec![TypeNode {
                kind: TypeKind::Object,
                standard_promise: true,
                ..Default::default()
            }],
            vec![],
        )
        .unwrap()
        .with_condition_types(Vec::new())
        .unwrap()
        .lint(&LintOptions {
            recommended: false,
            rules: [("ts/no-misused-promises".into(), RuleLevel::Error.into())].into(),
            ..Default::default()
        })
        .is_err()
    );
}

#[test]
fn misused_promises_report_standard_promise_like_conditions() {
    let source = TypeSource::new(
        "a.ts",
        Arc::from("declare const thenable: PromiseLike<number>; if (thenable) {}"),
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
    .with_condition_types(vec![TypeId(0)])
    .unwrap();
    let result = typed
        .lint(&LintOptions {
            recommended: false,
            rules: [("ts/no-misused-promises".into(), RuleLevel::Error.into())].into(),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(result.diagnostics.len(), 1);
    assert_eq!(result.diagnostics[0].message_id, "promiseCondition");
}

#[test]
fn misused_promises_report_conditional_and_logical_operands() {
    let source = TypeSource::new(
        "a.ts",
        Arc::from(
            "declare const thenable: PromiseLike<number>; const left = thenable && true; const right = thenable ? 1 : 0;",
        ),
        SourceType::TypeScript,
    )
    .unwrap();
    assert_eq!(source.conditions().len(), 3);
    let typed = TypedSource::new(
        source,
        vec![
            TypeNode {
                kind: TypeKind::Object,
                standard_thenable: true,
                ..Default::default()
            },
            TypeNode {
                kind: TypeKind::Boolean,
                ..Default::default()
            },
        ],
        vec![],
    )
    .unwrap()
    .with_condition_types(vec![TypeId(0), TypeId(1), TypeId(0)])
    .unwrap()
    .with_assignment_call_types(vec![
        CallArgumentType {
            actual: None,
            contextual: None,
        },
        CallArgumentType {
            actual: None,
            contextual: None,
        },
    ])
    .unwrap();
    let result = typed
        .lint(&LintOptions {
            recommended: false,
            rules: [("ts/no-misused-promises".into(), RuleLevel::Error.into())].into(),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(result.diagnostics.len(), 2);
    assert!(
        result
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.message_id == "promiseCondition")
    );
}

#[test]
fn misused_promises_report_promise_callbacks_at_void_parameters() {
    let source = TypeSource::new(
        "a.ts",
        Arc::from(
            "declare function consume(callback: (value: number) => void): void; consume(async value => value);",
        ),
        SourceType::TypeScript,
    )
    .unwrap();
    assert_eq!(source.calls().len(), 1);
    assert_eq!(source.calls()[0].arguments.len(), 1);
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
            TypeNode {
                kind: TypeKind::Object,
                ..Default::default()
            },
            TypeNode {
                kind: TypeKind::Void,
                ..Default::default()
            },
        ],
        vec![Some(CallType::new(TypeId(0)))],
    )
    .unwrap()
    .with_condition_types(Vec::new())
    .unwrap()
    .with_call_argument_types(vec![vec![CallArgumentType {
        actual: Some(CallType {
            type_id: TypeId(0),
            returns: vec![TypeKind::Object],
            return_types: vec![TypeId(1)],
            construct_signatures: 0,
        }),
        contextual: Some(CallType {
            type_id: TypeId(2),
            returns: vec![TypeKind::Void],
            return_types: vec![TypeId(3)],
            construct_signatures: 0,
        }),
    }]])
    .unwrap();
    let result = typed
        .lint(&LintOptions {
            recommended: false,
            rules: [("ts/no-misused-promises".into(), RuleLevel::Error.into())].into(),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(result.diagnostics.len(), 1);
    assert_eq!(result.diagnostics[0].message_id, "promiseCallback");
}

#[test]
fn misused_promises_report_promise_callbacks_at_void_assignments() {
    let source = TypeSource::new(
        "a.ts",
        Arc::from("let callback: (value: number) => void = async value => value;"),
        SourceType::TypeScript,
    )
    .unwrap();
    assert_eq!(source.assignments().len(), 1);
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
            TypeNode {
                kind: TypeKind::Object,
                ..Default::default()
            },
            TypeNode {
                kind: TypeKind::Void,
                ..Default::default()
            },
        ],
        vec![],
    )
    .unwrap()
    .with_condition_types(Vec::new())
    .unwrap()
    .with_assignment_call_types(vec![CallArgumentType {
        actual: Some(CallType {
            type_id: TypeId(0),
            returns: vec![TypeKind::Object],
            return_types: vec![TypeId(1)],
            construct_signatures: 0,
        }),
        contextual: Some(CallType {
            type_id: TypeId(2),
            returns: vec![TypeKind::Void],
            return_types: vec![TypeId(3)],
            construct_signatures: 0,
        }),
    }])
    .unwrap();
    let result = typed
        .lint(&LintOptions {
            recommended: false,
            rules: [("ts/no-misused-promises".into(), RuleLevel::Error.into())].into(),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(result.diagnostics.len(), 1);
    assert_eq!(result.diagnostics[0].message_id, "promiseCallback");
}

#[test]
fn misused_promises_report_promise_callbacks_at_void_returns() {
    let source = TypeSource::new(
        "a.ts",
        Arc::from("function factory(): () => void { return async () => 1; }"),
        SourceType::TypeScript,
    )
    .unwrap();
    assert_eq!(source.returns().len(), 1);
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
            TypeNode {
                kind: TypeKind::Object,
                ..Default::default()
            },
            TypeNode {
                kind: TypeKind::Void,
                ..Default::default()
            },
        ],
        vec![],
    )
    .unwrap()
    .with_condition_types(Vec::new())
    .unwrap()
    .with_return_call_types(vec![CallArgumentType {
        actual: Some(CallType {
            type_id: TypeId(0),
            returns: vec![TypeKind::Object],
            return_types: vec![TypeId(1)],
            construct_signatures: 0,
        }),
        contextual: Some(CallType {
            type_id: TypeId(2),
            returns: vec![TypeKind::Void],
            return_types: vec![TypeId(3)],
            construct_signatures: 0,
        }),
    }])
    .unwrap();
    let result = typed
        .lint(&LintOptions {
            recommended: false,
            rules: [("ts/no-misused-promises".into(), RuleLevel::Error.into())].into(),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(result.diagnostics.len(), 1);
    assert_eq!(result.diagnostics[0].message_id, "promiseCallback");
}

#[test]
fn misused_promises_report_promise_callbacks_at_void_object_properties() {
    let source = TypeSource::new(
        "a.ts",
        Arc::from(
            "const value: { callback: (value: number) => void } = { callback: async value => value };",
        ),
        SourceType::TypeScript,
    )
    .unwrap();
    assert_eq!(source.callbacks().len(), 1);
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
            TypeNode {
                kind: TypeKind::Object,
                ..Default::default()
            },
            TypeNode {
                kind: TypeKind::Void,
                ..Default::default()
            },
        ],
        vec![],
    )
    .unwrap()
    .with_condition_types(Vec::new())
    .unwrap()
    .with_assignment_call_types(vec![CallArgumentType {
        actual: None,
        contextual: None,
    }])
    .unwrap()
    .with_callback_types(vec![CallArgumentType {
        actual: Some(CallType {
            type_id: TypeId(0),
            returns: vec![TypeKind::Object],
            return_types: vec![TypeId(1)],
            construct_signatures: 0,
        }),
        contextual: Some(CallType {
            type_id: TypeId(2),
            returns: vec![TypeKind::Void],
            return_types: vec![TypeId(3)],
            construct_signatures: 0,
        }),
    }])
    .unwrap();
    let result = typed
        .lint(&LintOptions {
            recommended: false,
            rules: [("ts/no-misused-promises".into(), RuleLevel::Error.into())].into(),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(result.diagnostics.len(), 1);
    assert_eq!(result.diagnostics[0].message_id, "promiseCallback");
}

#[test]
fn misused_promises_report_promise_callbacks_at_void_jsx_attributes() {
    let source = TypeSource::new(
        "a.tsx",
        Arc::from("const view = <Widget onClick={async () => 1} />;"),
        SourceType::Tsx,
    )
    .unwrap();
    assert_eq!(source.callbacks().len(), 1);
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
            TypeNode {
                kind: TypeKind::Object,
                ..Default::default()
            },
            TypeNode {
                kind: TypeKind::Void,
                ..Default::default()
            },
        ],
        vec![],
    )
    .unwrap()
    .with_condition_types(Vec::new())
    .unwrap()
    .with_assignment_call_types(vec![CallArgumentType {
        actual: None,
        contextual: None,
    }])
    .unwrap()
    .with_callback_types(vec![CallArgumentType {
        actual: Some(CallType {
            type_id: TypeId(0),
            returns: vec![TypeKind::Object],
            return_types: vec![TypeId(1)],
            construct_signatures: 0,
        }),
        contextual: Some(CallType {
            type_id: TypeId(2),
            returns: vec![TypeKind::Void],
            return_types: vec![TypeId(3)],
            construct_signatures: 0,
        }),
    }])
    .unwrap();
    let result = typed
        .lint(&LintOptions {
            recommended: false,
            rules: [("ts/no-misused-promises".into(), RuleLevel::Error.into())].into(),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(result.diagnostics.len(), 1);
    assert_eq!(result.diagnostics[0].message_id, "promiseCallback");
}
