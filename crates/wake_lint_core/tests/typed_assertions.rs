use std::sync::Arc;

use wake_lint_core::{
    LintOptions, RuleLevel, RuleSetting, SourceType, TypeId, TypeKind, TypeNode, TypeSource,
    TypedSource, lint_text,
};

fn options() -> LintOptions {
    LintOptions {
        recommended: false,
        rules: [(
            "ts/no-unnecessary-type-assertion".into(),
            RuleSetting::Level(RuleLevel::Error),
        )]
        .into_iter()
        .collect(),
        ..Default::default()
    }
}

#[test]
fn unnecessary_assertions_compare_source_and_asserted_type_categories() {
    let source = TypeSource::new(
        "a.ts",
        Arc::from("const value = 1 as number; const text = value as string;"),
        SourceType::TypeScript,
    )
    .unwrap();
    assert_eq!(source.assertions().len(), 2);
    let typed = TypedSource::new(
        source,
        vec![
            TypeNode {
                kind: TypeKind::Number,
                ..Default::default()
            },
            TypeNode {
                kind: TypeKind::String,
                ..Default::default()
            },
        ],
        vec![],
    )
    .unwrap()
    .with_assertion_types(vec![(TypeId(0), TypeId(0)), (TypeId(0), TypeId(1))])
    .unwrap();
    let result = typed.lint(&options()).unwrap();
    assert_eq!(result.diagnostics.len(), 1);
    assert_eq!(result.diagnostics[0].message_id, "unnecessary");
}

#[test]
fn assertions_require_structural_type_equivalence_not_only_a_shared_category() {
    let source = TypeSource::new(
        "a.ts",
        Arc::from("const value = 'a' as string; const exact = 'a' as 'a';"),
        SourceType::TypeScript,
    )
    .unwrap();
    let typed = TypedSource::new(
        source,
        vec![
            TypeNode {
                kind: TypeKind::String,
                literal: Some(wake_lint_core::TypeLiteral::String("a".into())),
                ..Default::default()
            },
            TypeNode {
                kind: TypeKind::String,
                ..Default::default()
            },
        ],
        vec![],
    )
    .unwrap()
    .with_assertion_types(vec![(TypeId(0), TypeId(1)), (TypeId(0), TypeId(0))])
    .unwrap();
    let result = typed.lint(&options()).unwrap();
    assert_eq!(result.diagnostics.len(), 1);
    assert_eq!(result.diagnostics[0].start, 43);
}

#[test]
fn assertions_do_not_treat_distinct_object_nodes_as_equivalent() {
    let source = TypeSource::new(
        "a.ts",
        Arc::from("const first = {} as {}; const second = {} as {};"),
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
                ..Default::default()
            },
            TypeNode {
                kind: TypeKind::Object,
                ..Default::default()
            },
        ],
        vec![],
    )
    .unwrap()
    .with_assertion_types(vec![(TypeId(0), TypeId(1)), (TypeId(2), TypeId(2))])
    .unwrap();
    let result = typed.lint(&options()).unwrap();
    assert_eq!(result.diagnostics.len(), 1);
}

#[test]
fn assertions_prove_equivalent_named_structural_properties() {
    let source = TypeSource::new(
        "a.ts",
        Arc::from(
            "declare const left: Left; const same = left as Right; const different = left as Other;",
        ),
        SourceType::TypeScript,
    )
    .unwrap();
    let typed = TypedSource::new(
        source,
        vec![
            TypeNode {
                kind: TypeKind::Object,
                structural_complete: true,
                structural_properties: vec![("value".into(), false, TypeId(1))],
                ..Default::default()
            },
            TypeNode {
                kind: TypeKind::String,
                ..Default::default()
            },
            TypeNode {
                kind: TypeKind::Object,
                structural_complete: true,
                structural_properties: vec![("value".into(), false, TypeId(1))],
                ..Default::default()
            },
            TypeNode {
                kind: TypeKind::Object,
                structural_complete: true,
                structural_properties: vec![("other".into(), false, TypeId(1))],
                ..Default::default()
            },
        ],
        vec![],
    )
    .unwrap()
    .with_assertion_types(vec![(TypeId(0), TypeId(2)), (TypeId(0), TypeId(3))])
    .unwrap();
    let result = typed.lint(&options()).unwrap();
    assert_eq!(result.diagnostics.len(), 1);
    assert_eq!(result.diagnostics[0].start, 39);
}

#[test]
fn assertions_preserve_structural_property_optionalness() {
    let source = TypeSource::new(
        "a.ts",
        Arc::from(
            "declare const left: Left; const same = left as Right; const changed = left as Optional;",
        ),
        SourceType::TypeScript,
    )
    .unwrap();
    let typed = TypedSource::new(
        source,
        vec![
            TypeNode {
                kind: TypeKind::Object,
                structural_complete: true,
                structural_properties: vec![("value".into(), false, TypeId(1))],
                ..Default::default()
            },
            TypeNode {
                kind: TypeKind::String,
                ..Default::default()
            },
            TypeNode {
                kind: TypeKind::Object,
                structural_complete: true,
                structural_properties: vec![("value".into(), false, TypeId(1))],
                ..Default::default()
            },
            TypeNode {
                kind: TypeKind::Object,
                structural_complete: true,
                structural_properties: vec![("value".into(), true, TypeId(1))],
                ..Default::default()
            },
        ],
        vec![],
    )
    .unwrap()
    .with_assertion_types(vec![(TypeId(0), TypeId(2)), (TypeId(0), TypeId(3))])
    .unwrap();
    let result = typed.lint(&options()).unwrap();
    assert_eq!(result.diagnostics.len(), 1);
}

#[test]
fn const_assertions_are_not_redundant_when_the_contextual_types_match() {
    let source = TypeSource::new(
        "a.ts",
        Arc::from("1 as const; <const>1; value as Target;"),
        SourceType::TypeScript,
    )
    .unwrap();
    let typed = TypedSource::new(
        source,
        vec![TypeNode {
            kind: TypeKind::Object,
            ..Default::default()
        }],
        vec![],
    )
    .unwrap()
    .with_assertion_types(vec![(TypeId(0), TypeId(0)); 3])
    .unwrap();
    let result = typed.lint(&options()).unwrap();
    assert_eq!(result.diagnostics.len(), 1);
    assert_eq!(result.diagnostics[0].start, 22);
}

#[test]
fn assertions_preserve_structural_property_writability() {
    let source = TypeSource::new(
        "a.ts",
        Arc::from("left as Mutable; left as ReadonlyCopy;"),
        SourceType::TypeScript,
    )
    .unwrap();
    let property = |readonly| TypeNode {
        kind: TypeKind::Object,
        structural_complete: true,
        structural_properties: vec![("value".into(), false, TypeId(0))],
        readonly_properties: if readonly {
            ["value".into()].into()
        } else {
            Default::default()
        },
        ..Default::default()
    };
    let typed = TypedSource::new(
        source,
        vec![
            TypeNode {
                kind: TypeKind::String,
                ..Default::default()
            },
            property(true),
            property(false),
            property(true),
        ],
        vec![],
    )
    .unwrap()
    .with_assertion_types(vec![(TypeId(1), TypeId(2)), (TypeId(1), TypeId(3))])
    .unwrap();
    let result = typed.lint(&options()).unwrap();
    assert_eq!(result.diagnostics.len(), 1);
    assert_eq!(result.diagnostics[0].start, 17);
}

#[test]
fn structural_shape_facts_reject_inconsistent_permissions_and_relations() {
    let valid = TypeNode {
        kind: TypeKind::Object,
        structural_complete: true,
        structural_properties: vec![("value".into(), false, TypeId(1))],
        ..Default::default()
    };
    let mut duplicate = valid.clone();
    duplicate
        .structural_properties
        .push(("value".into(), true, TypeId(1)));
    let mut missing_readonly = valid.clone();
    missing_readonly
        .readonly_properties
        .insert("missing".into());
    let mut callable = valid.clone();
    callable.signature_returns.push(TypeId(1));
    let mut nominal = valid.clone();
    nominal.class_identity = Some(0);
    let mut invalid_index = valid.clone();
    invalid_index
        .index_signatures
        .push((TypeId(1), TypeId(10), false));
    let mut non_object = valid;
    non_object.kind = TypeKind::String;
    for node in [
        duplicate,
        missing_readonly,
        callable,
        nominal,
        invalid_index,
        non_object,
    ] {
        let source = TypeSource::new("a.ts", Arc::from(""), SourceType::TypeScript).unwrap();
        assert!(
            TypedSource::new(
                source,
                vec![
                    node,
                    TypeNode {
                        kind: TypeKind::String,
                        ..Default::default()
                    }
                ],
                vec![]
            )
            .is_err()
        );
    }
}

#[test]
fn assertions_keep_distinct_enum_declarations_nominal_even_when_values_match() {
    let source = TypeSource::new(
        "a.ts",
        Arc::from(
            "declare const first: First, second: Second, same: First; const a = first as Second; const b = same as First;",
        ),
        SourceType::TypeScript,
    )
    .unwrap();
    let typed = TypedSource::new(
        source,
        vec![
            TypeNode {
                kind: TypeKind::Number,
                literal: Some(wake_lint_core::TypeLiteral::Number(1.0)),
                enum_identity: Some(1),
                ..Default::default()
            },
            TypeNode {
                kind: TypeKind::Number,
                literal: Some(wake_lint_core::TypeLiteral::Number(1.0)),
                enum_identity: Some(2),
                ..Default::default()
            },
            TypeNode {
                kind: TypeKind::Number,
                literal: Some(wake_lint_core::TypeLiteral::Number(1.0)),
                enum_identity: Some(1),
                ..Default::default()
            },
        ],
        vec![],
    )
    .unwrap()
    .with_assertion_types(vec![(TypeId(0), TypeId(1)), (TypeId(2), TypeId(0))])
    .unwrap();
    let result = typed.lint(&options()).unwrap();
    assert_eq!(result.diagnostics.len(), 1);
    assert_eq!(result.diagnostics[0].message_id, "unnecessary");
}

#[test]
fn assertions_keep_distinct_unique_symbols_nominal() {
    let source = TypeSource::new(
        "a.ts",
        Arc::from(
            "declare const first: unique symbol, second: unique symbol; const a = first as typeof second; const b = first as typeof first;",
        ),
        SourceType::TypeScript,
    )
    .unwrap();
    let typed = TypedSource::new(
        source,
        vec![
            TypeNode {
                kind: TypeKind::Symbol,
                unique_symbol_identity: Some(1),
                ..Default::default()
            },
            TypeNode {
                kind: TypeKind::Symbol,
                unique_symbol_identity: Some(2),
                ..Default::default()
            },
            TypeNode {
                kind: TypeKind::Symbol,
                unique_symbol_identity: Some(1),
                ..Default::default()
            },
        ],
        vec![],
    )
    .unwrap()
    .with_assertion_types(vec![(TypeId(0), TypeId(1)), (TypeId(2), TypeId(0))])
    .unwrap();
    let result = typed.lint(&options()).unwrap();
    assert_eq!(result.diagnostics.len(), 1);
    assert_eq!(result.diagnostics[0].message_id, "unnecessary");
}

#[test]
fn assertions_compare_same_generic_reference_target_and_arguments() {
    let source = TypeSource::new(
        "a.ts",
        Arc::from("declare const value: Box<string>; const exact = value as Box<string>;"),
        SourceType::TypeScript,
    )
    .unwrap();
    assert_eq!(source.assertions().len(), 1);
    let typed = TypedSource::new(
        source,
        vec![
            TypeNode {
                kind: TypeKind::String,
                ..Default::default()
            },
            TypeNode {
                kind: TypeKind::Object,
                reference_target: Some(TypeId(2)),
                type_arguments: vec![TypeId(0)],
                ..Default::default()
            },
            TypeNode {
                kind: TypeKind::Object,
                ..Default::default()
            },
            TypeNode {
                kind: TypeKind::Object,
                reference_target: Some(TypeId(2)),
                type_arguments: vec![TypeId(0)],
                ..Default::default()
            },
        ],
        vec![],
    )
    .unwrap()
    .with_assertion_types(vec![(TypeId(1), TypeId(3))])
    .unwrap();
    let result = typed.lint(&options()).unwrap();
    assert_eq!(result.diagnostics.len(), 1);
    assert_eq!(result.diagnostics[0].message_id, "unnecessary");
}

#[test]
fn assertions_compare_same_standard_promise_like_identity() {
    let source = TypeSource::new(
        "a.ts",
        Arc::from(
            "declare const value: PromiseLike<number>; const exact = value as PromiseLike<number>;",
        ),
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
                standard_thenable: true,
                ..Default::default()
            },
        ],
        vec![],
    )
    .unwrap()
    .with_assertion_types(vec![(TypeId(0), TypeId(1))])
    .unwrap();
    let result = typed.lint(&options()).unwrap();
    assert_eq!(result.diagnostics.len(), 1);
    assert_eq!(result.diagnostics[0].message_id, "unnecessary");
}

#[test]
fn assertions_do_not_equate_standard_and_nonstandard_thenable_identities() {
    let source = TypeSource::new(
        "a.ts",
        Arc::from("declare const value: Box<number>; const cast = value as Box<number>;"),
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
                standard_thenable: true,
                reference_target: Some(TypeId(0)),
                ..Default::default()
            },
            TypeNode {
                kind: TypeKind::Object,
                reference_target: Some(TypeId(0)),
                ..Default::default()
            },
        ],
        vec![],
    )
    .unwrap()
    .with_assertion_types(vec![(TypeId(1), TypeId(2))])
    .unwrap();
    let result = typed.lint(&options()).unwrap();
    assert!(result.diagnostics.is_empty());
}

#[test]
fn assertions_do_not_equate_distinct_type_parameters_with_same_constraint() {
    let source = TypeSource::new(
        "a.ts",
        Arc::from("declare const value: T; const cast = value as U;"),
        SourceType::TypeScript,
    )
    .unwrap();
    let typed = TypedSource::new(
        source,
        vec![
            TypeNode {
                kind: TypeKind::String,
                ..Default::default()
            },
            TypeNode {
                kind: TypeKind::Parameter,
                constraint: Some(TypeId(0)),
                ..Default::default()
            },
            TypeNode {
                kind: TypeKind::Parameter,
                constraint: Some(TypeId(0)),
                ..Default::default()
            },
        ],
        vec![],
    )
    .unwrap()
    .with_assertion_types(vec![(TypeId(1), TypeId(2))])
    .unwrap();
    let result = typed.lint(&options()).unwrap();
    assert!(result.diagnostics.is_empty());
}

#[test]
fn assertions_fail_closed_for_distinct_unmodeled_type_nodes() {
    let source = TypeSource::new(
        "a.ts",
        Arc::from("declare const value: Conditional; const cast = value as Indexed;"),
        SourceType::TypeScript,
    )
    .unwrap();
    let typed = TypedSource::new(
        source,
        vec![
            TypeNode {
                kind: TypeKind::Other,
                ..Default::default()
            },
            TypeNode {
                kind: TypeKind::Other,
                ..Default::default()
            },
        ],
        vec![],
    )
    .unwrap()
    .with_assertion_types(vec![(TypeId(0), TypeId(1))])
    .unwrap();
    let result = typed.lint(&options()).unwrap();
    assert!(result.diagnostics.is_empty());
}

#[test]
fn assertions_skip_generic_references_with_unknown_arguments() {
    let source = TypeSource::new(
        "a.ts",
        Arc::from("declare const value: Box<unknown>; const exact = value as Box<unknown>;"),
        SourceType::TypeScript,
    )
    .unwrap();
    let typed = TypedSource::new(
        source,
        vec![
            TypeNode {
                kind: TypeKind::Unknown,
                ..Default::default()
            },
            TypeNode {
                kind: TypeKind::Object,
                reference_target: Some(TypeId(2)),
                type_arguments: vec![TypeId(0)],
                ..Default::default()
            },
            TypeNode {
                kind: TypeKind::Object,
                ..Default::default()
            },
            TypeNode {
                kind: TypeKind::Object,
                reference_target: Some(TypeId(2)),
                type_arguments: vec![TypeId(0)],
                ..Default::default()
            },
        ],
        vec![],
    )
    .unwrap()
    .with_assertion_types(vec![(TypeId(1), TypeId(3))])
    .unwrap();
    let result = typed.lint(&options()).unwrap();
    assert!(result.diagnostics.is_empty());
}

#[test]
fn assertion_rule_requires_complete_facts_and_plain_text_fails_closed() {
    let source = TypeSource::new(
        "a.ts",
        Arc::from("const value = 1 as number;"),
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
            vec![]
        )
        .unwrap()
        .lint(&options())
        .is_err()
    );
    assert!(
        lint_text(
            "const value = 1 as number;",
            SourceType::TypeScript,
            &options()
        )
        .is_err()
    );
}

#[test]
fn assertions_compare_generic_arguments_by_position() {
    let source = TypeSource::new(
        "a.ts",
        Arc::from("pair as Swapped; pair as Same;"),
        SourceType::TypeScript,
    )
    .unwrap();
    let target = TypeNode {
        kind: TypeKind::Object,
        ..Default::default()
    };
    let instance = |args| TypeNode {
        kind: TypeKind::Object,
        reference_target: Some(TypeId(0)),
        type_arguments: args,
        ..Default::default()
    };
    let typed = TypedSource::new(
        source,
        vec![
            target,
            TypeNode {
                kind: TypeKind::String,
                ..Default::default()
            },
            TypeNode {
                kind: TypeKind::Number,
                ..Default::default()
            },
            instance(vec![TypeId(1), TypeId(2)]),
            instance(vec![TypeId(2), TypeId(1)]),
            instance(vec![TypeId(1), TypeId(2)]),
        ],
        vec![],
    )
    .unwrap()
    .with_assertion_types(vec![(TypeId(3), TypeId(4)), (TypeId(3), TypeId(5))])
    .unwrap();
    let result = typed.lint(&options()).unwrap();
    assert_eq!(result.diagnostics.len(), 1);
    assert_eq!(result.diagnostics[0].start, 17);
}

#[test]
fn assertions_require_complete_shapes_including_index_signatures() {
    let source = TypeSource::new(
        "a.ts",
        Arc::from("a as B; a as C; a as D; a as E; f as G;"),
        SourceType::TypeScript,
    )
    .unwrap();
    let object = |indices| TypeNode {
        kind: TypeKind::Object,
        structural_complete: true,
        structural_properties: vec![("value".into(), false, TypeId(0))],
        index_signatures: indices,
        ..Default::default()
    };
    let typed = TypedSource::new(
        source,
        vec![
            TypeNode {
                kind: TypeKind::String,
                ..Default::default()
            },
            TypeNode {
                kind: TypeKind::Number,
                ..Default::default()
            },
            object(vec![(TypeId(0), TypeId(0), false)]),
            object(vec![]),
            object(vec![(TypeId(0), TypeId(0), false)]),
            object(vec![(TypeId(1), TypeId(0), false)]),
            TypeNode {
                structural_complete: false,
                ..object(vec![])
            },
            TypeNode {
                kind: TypeKind::Object,
                structural_complete: true,
                ..Default::default()
            },
            TypeNode {
                kind: TypeKind::Object,
                structural_complete: true,
                ..Default::default()
            },
        ],
        vec![],
    )
    .unwrap()
    .with_assertion_types(vec![
        (TypeId(2), TypeId(3)),
        (TypeId(2), TypeId(4)),
        (TypeId(2), TypeId(5)),
        (TypeId(3), TypeId(6)),
        (TypeId(7), TypeId(8)),
    ])
    .unwrap();
    let result = typed.lint(&options()).unwrap();
    assert_eq!(
        result
            .diagnostics
            .iter()
            .map(|d| d.start)
            .collect::<Vec<_>>(),
        [8, 32]
    );
}

#[test]
fn assertions_discard_failed_recursive_comparison_assumptions() {
    let source =
        TypeSource::new("a.ts", Arc::from("left as Right;"), SourceType::TypeScript).unwrap();
    let object = |parameter, value| TypeNode {
        kind: TypeKind::Object,
        structural_complete: true,
        structural_properties: vec![("a".into(), false, parameter), ("b".into(), false, value)],
        ..Default::default()
    };
    let typed = TypedSource::new(
        source,
        vec![
            TypeNode {
                kind: TypeKind::String,
                ..Default::default()
            },
            TypeNode {
                kind: TypeKind::Number,
                ..Default::default()
            },
            TypeNode {
                kind: TypeKind::Parameter,
                constraint: Some(TypeId(0)),
                ..Default::default()
            },
            TypeNode {
                kind: TypeKind::Parameter,
                constraint: Some(TypeId(0)),
                ..Default::default()
            },
            object(TypeId(2), TypeId(0)),
            object(TypeId(3), TypeId(1)),
            object(TypeId(3), TypeId(0)),
            TypeNode {
                kind: TypeKind::Union,
                parts: vec![TypeId(4), TypeId(5)],
                ..Default::default()
            },
            TypeNode {
                kind: TypeKind::Union,
                parts: vec![TypeId(5), TypeId(6)],
                ..Default::default()
            },
        ],
        vec![],
    )
    .unwrap()
    .with_assertion_types(vec![(TypeId(7), TypeId(8))])
    .unwrap();
    assert!(typed.lint(&options()).unwrap().diagnostics.is_empty());
}

#[test]
fn assertions_bound_deep_structural_comparisons() {
    let source =
        TypeSource::new("a.ts", Arc::from("left as Right;"), SourceType::TypeScript).unwrap();
    let mut nodes = vec![TypeNode {
        kind: TypeKind::String,
        ..Default::default()
    }];
    let mut ends = vec![];
    for _ in 0..2 {
        let mut child = TypeId(0);
        for _ in 0..512 {
            nodes.push(TypeNode {
                kind: TypeKind::Object,
                structural_complete: true,
                structural_properties: vec![("next".into(), false, child)],
                ..Default::default()
            });
            child = TypeId(nodes.len() - 1);
        }
        ends.push(child);
    }
    let typed = TypedSource::new(source, nodes, vec![])
        .unwrap()
        .with_assertion_types(vec![(ends[0], ends[1])])
        .unwrap();
    assert!(typed.lint(&options()).unwrap().diagnostics.is_empty());
}
