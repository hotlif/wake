use std::sync::Arc;
use wake_lint_core::{
    LintOptions, RuleConfiguration, RuleLevel, RuleSetting, SourceType, TypeId, TypeKind, TypeNode,
    TypeSource, TypedSource,
};

fn options(values: serde_json::Value) -> LintOptions {
    LintOptions {
        recommended: false,
        rules: [(
            "ts/restrict-template-expressions".into(),
            RuleSetting::Options(RuleConfiguration {
                level: RuleLevel::Error,
                options: serde_json::from_value(values).unwrap(),
            }),
        )]
        .into(),
        ..Default::default()
    }
}
fn node(kind: TypeKind) -> TypeNode {
    TypeNode {
        kind,
        ..Default::default()
    }
}
fn typed(types: Vec<TypeNode>, selected: TypeId) -> TypedSource {
    let input = TypeSource::new("a.ts", Arc::from("`${value}`"), SourceType::TypeScript).unwrap();
    TypedSource::new(input, types, vec![])
        .unwrap()
        .with_template_types(vec![Some(vec![selected])])
        .unwrap()
}

#[test]
fn template_categories_and_closed_options_use_types_without_name_guesses() {
    for (kind, option, allowed) in [
        (TypeKind::String, None, true),
        (TypeKind::Number, Some("allow_number"), true),
        (TypeKind::BigInt, Some("allow_number"), true),
        (TypeKind::Boolean, Some("allow_boolean"), true),
        (TypeKind::Null, Some("allow_nullish"), true),
        (TypeKind::Undefined, Some("allow_nullish"), true),
        (TypeKind::Any, Some("allow_any"), true),
        (TypeKind::Error, Some("allow_any"), true),
        (TypeKind::Symbol, None, false),
        (TypeKind::Unknown, None, false),
        (TypeKind::Void, None, false),
        (TypeKind::Object, None, false),
        (TypeKind::Parameter, None, false),
        (TypeKind::Never, Some("allow_never"), false),
    ] {
        let typed = typed(vec![node(kind)], TypeId(0));
        assert_eq!(
            typed
                .lint(&options(serde_json::json!({})))
                .unwrap()
                .diagnostics
                .is_empty(),
            allowed,
            "{kind:?}"
        );
        if let Some(option) = option {
            assert_eq!(
                typed
                    .lint(&options(serde_json::json!({option:!allowed})))
                    .unwrap()
                    .diagnostics
                    .is_empty(),
                !allowed,
                "{kind:?}"
            );
        }
    }
    let mut regexp = node(TypeKind::Object);
    regexp.standard_regexp = true;
    let typed = typed(vec![regexp], TypeId(0));
    assert_eq!(
        typed
            .lint(&options(serde_json::json!({})))
            .unwrap()
            .diagnostics[0]
            .message_id,
        "invalid"
    );
    assert!(
        typed
            .lint(&options(serde_json::json!({"allow_regexp":true})))
            .unwrap()
            .diagnostics
            .is_empty()
    );
    assert!(
        typed
            .lint(&options(serde_json::json!({"allow_name":"RegExp"})))
            .is_err()
    );
}

#[test]
fn template_unions_intersections_and_constraints_follow_the_finite_type_graph() {
    let mut union = node(TypeKind::Union);
    union.parts = vec![TypeId(0), TypeId(1)];
    let mut intersection = node(TypeKind::Intersection);
    intersection.parts = vec![TypeId(0), TypeId(1)];
    let mut parameter = node(TypeKind::Parameter);
    parameter.constraint = Some(TypeId(3));
    let types = vec![
        node(TypeKind::String),
        node(TypeKind::Object),
        union,
        intersection,
        parameter,
    ];
    for (id, allowed) in [(2, false), (3, true), (4, true)] {
        assert_eq!(
            typed(types.clone(), TypeId(id))
                .lint(&options(serde_json::json!({})))
                .unwrap()
                .diagnostics
                .is_empty(),
            allowed
        );
    }
}

#[test]
fn missing_and_detached_template_facts_are_errors_and_directives_use_original_spans() {
    let input = || {
        TypeSource::new("a.ts", Arc::from("// wake-lint-disable-next-line ts/restrict-template-expressions\n`${value}`;\n`${value}`"), SourceType::TypeScript).unwrap()
    };
    let make = || TypedSource::new(input(), vec![node(TypeKind::Object)], vec![]).unwrap();
    assert!(make().lint(&options(serde_json::json!({}))).is_err());
    assert!(make().with_template_types(vec![]).is_err());
    assert!(make().with_template_types(vec![None, None]).is_err());
    assert!(
        make()
            .with_template_types(vec![Some(vec![TypeId(99)]), Some(vec![TypeId(0)])])
            .is_err()
    );
    let typed = make()
        .with_template_types(vec![Some(vec![TypeId(0)]), Some(vec![TypeId(0)])])
        .unwrap();
    let result = typed.lint(&options(serde_json::json!({}))).unwrap();
    assert_eq!(result.diagnostics.len(), 1);
    let finding = &result.diagnostics[0];
    assert_eq!(
        &typed.input().source()[finding.start as usize..finding.end as usize],
        "value"
    );
    assert!(finding.fix.is_none());
}
