use std::sync::Arc;
use wake_lint_core::{
    LintOptions, MemberType, RuleConfiguration, RuleLevel, RuleSetting, SourceType, TypeId,
    TypeKind, TypeNode, TypeSource, TypedSource,
};

fn options(optional: bool) -> LintOptions {
    LintOptions {
        recommended: false,
        rules: [(
            "ts/no-unsafe-member-access".into(),
            RuleSetting::Options(RuleConfiguration {
                level: RuleLevel::Error,
                options: [("allow_optional".into(), serde_json::json!(optional))].into(),
            }),
        )]
        .into(),
        ..Default::default()
    }
}
fn input(source: &str) -> TypeSource {
    TypeSource::new("a.ts", Arc::from(source), SourceType::TypeScript).unwrap()
}
fn types() -> Vec<TypeNode> {
    [TypeKind::Any, TypeKind::Error, TypeKind::Object]
        .into_iter()
        .map(|kind| TypeNode {
            kind,
            ..Default::default()
        })
        .collect()
}
fn member(object: usize, property: Option<usize>) -> MemberType {
    MemberType {
        object: TypeId(object),
        property: property.map(TypeId),
    }
}

#[test]
fn member_receivers_keys_private_names_and_optional_exceptions_are_independent() {
    let source =
        "loose.value; loose?.value; safe[key]; loose[key]; class C { #x; f(v){ return v.#x; } }";
    let parsed = input(source);
    assert_eq!(parsed.members().len(), 5);
    let typed = TypedSource::new(parsed, types(), vec![])
        .unwrap()
        .with_member_types(vec![
            member(0, None),
            member(0, None),
            member(2, Some(0)),
            member(0, Some(0)),
            member(1, None),
        ])
        .unwrap();
    let result = typed.lint(&options(false)).unwrap();
    assert_eq!(
        result
            .diagnostics
            .iter()
            .map(|d| d.message_id.as_str())
            .collect::<Vec<_>>(),
        [
            "unsafeMember",
            "unsafeMember",
            "unsafeKey",
            "unsafeKey",
            "unsafeMember",
            "errorMember"
        ]
    );
    assert!(result.diagnostics.iter().all(|d| d.fix.is_none()
        && matches!(
            &source[d.start as usize..d.end as usize],
            "value" | "key" | "#x"
        )));
    assert_eq!(typed.lint(&options(true)).unwrap().diagnostics.len(), 5);
}

#[test]
fn member_facts_validate_identity_and_require_computed_key_types() {
    let make = || TypedSource::new(input("safe[key]"), types(), vec![]).unwrap();
    assert!(make().lint(&options(false)).is_err());
    for facts in [
        vec![],
        vec![member(2, None)],
        vec![member(9, Some(0))],
        vec![member(2, Some(9))],
    ] {
        assert!(make().with_member_types(facts).is_err());
    }
    assert!(
        TypedSource::new(input("safe.name"), types(), vec![])
            .unwrap()
            .with_member_types(vec![member(2, Some(0))])
            .is_err()
    );
    let mut types = types();
    types.push(TypeNode {
        kind: TypeKind::Parameter,
        constraint: Some(TypeId(0)),
        ..Default::default()
    });
    let typed = TypedSource::new(
        input("// wake-lint-disable-next-line ts/no-unsafe-member-access\nvalue.name;\nvalue.next"),
        types,
        vec![],
    )
    .unwrap()
    .with_member_types(vec![member(3, None), member(3, None)])
    .unwrap();
    assert_eq!(typed.lint(&options(false)).unwrap().diagnostics.len(), 1);
}

#[test]
fn separate_fact_attachments_cannot_bypass_the_combined_relation_budget() {
    let make = || {
        let types = vec![
            TypeNode::default(),
            TypeNode {
                kind: TypeKind::Union,
                parts: vec![TypeId(0); TypedSource::MAX_RELATIONS - 2],
                ..Default::default()
            },
        ];
        TypedSource::new(input("`${value}`; receiver[key]"), types, vec![]).unwrap()
    };
    assert!(
        make()
            .with_template_types(vec![Some(vec![TypeId(0)])])
            .unwrap()
            .with_member_types(vec![member(0, Some(0))])
            .is_err()
    );
    assert!(
        make()
            .with_member_types(vec![member(0, Some(0))])
            .unwrap()
            .with_template_types(vec![Some(vec![TypeId(0)])])
            .is_err()
    );
}
