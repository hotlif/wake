use std::sync::Arc;
use wake_lint_core::{
    CallType, LintError, LintOptions, RuleLevel, SourceType, TypeId, TypeKind, TypeNode,
    TypeSource, TypedSource, lint_text,
};

fn options() -> LintOptions {
    LintOptions {
        recommended: false,
        rules: [("ts/no-unsafe-call".into(), RuleLevel::Error.into())].into(),
        ..Default::default()
    }
}
fn input(source: &str) -> TypeSource {
    TypeSource::new("a.ts", Arc::from(source), SourceType::TypeScript).unwrap()
}
fn node(kind: TypeKind) -> TypeNode {
    TypeNode {
        kind,
        ..Default::default()
    }
}
fn facts(source: &str, types: Vec<TypeNode>, call: CallType) -> TypedSource {
    let source = input(source);
    let calls = source.calls().iter().map(|_| Some(call.clone())).collect();
    TypedSource::new(source, types, calls).unwrap()
}

#[test]
fn unsafe_calls_are_source_bound_and_share_directives_with_local_rules() {
    assert!(matches!(
        lint_text("", SourceType::TypeScript, &options()),
        Err(LintError::Analysis(_))
    ));
    let source = "// wake-lint-disable-next-line ts/no-unsafe-call\na();\na?.(); new a(); a`text`; import('pkg');";
    let typed = facts(source, vec![node(TypeKind::Any)], CallType::new(TypeId(0)));
    let result = typed.lint(&options()).unwrap();
    assert_eq!(
        result
            .diagnostics
            .iter()
            .map(|d| d.message_id.as_str())
            .collect::<Vec<_>>(),
        ["unsafeCall", "unsafeNew", "unsafeTag"]
    );
    for diagnostic in result.diagnostics {
        assert!(diagnostic.fix.is_none());
        assert!(diagnostic.start < diagnostic.end);
    }
    let error = facts(
        "missing()",
        vec![node(TypeKind::Error)],
        CallType::new(TypeId(0)),
    );
    assert_eq!(
        error.lint(&options()).unwrap().diagnostics[0].message_id,
        "errorCall"
    );
    let info = wake_lint_core::rule_catalog()
        .into_iter()
        .find(|rule| rule.id == "ts/no-unsafe-call")
        .unwrap();
    assert_eq!(info.analysis, "type-information");
    assert_eq!(info.default_level, RuleLevel::Off);
}

#[test]
fn standard_function_identity_constraints_and_signature_kinds_control_findings() {
    let mut standard = node(TypeKind::Object);
    standard.standard_function = true;
    let mut derived = node(TypeKind::Object);
    derived.bases = vec![TypeId(0)];
    let mut parameter = node(TypeKind::Parameter);
    parameter.constraint = Some(TypeId(1));
    let mut union = node(TypeKind::Union);
    union.parts = vec![TypeId(0), TypeId(1)];
    let mut intersection = node(TypeKind::Intersection);
    intersection.parts = vec![TypeId(0), TypeId(5)];
    let types = vec![
        standard,
        derived,
        parameter,
        union,
        intersection,
        node(TypeKind::Other),
    ];
    for index in 0..6 {
        for (returns, constructs, expected) in [
            (vec![], 0, 3),
            (vec![TypeKind::Void], 0, 1),
            (vec![TypeKind::Other], 0, 0),
            (vec![], 1, 0),
        ] {
            let mut call = CallType::new(TypeId(index));
            call.returns = returns;
            call.construct_signatures = constructs;
            let typed = facts("value(); new value(); value`text`;", types.clone(), call);
            assert_eq!(
                typed.lint(&options()).unwrap().diagnostics.len(),
                if index == 5 { 0 } else { expected },
                "index {index}"
            );
        }
    }
    let mut mixed = node(TypeKind::Union);
    mixed.parts = vec![TypeId(0), TypeId(5)];
    let mut all = types;
    all.push(mixed);
    assert!(
        facts("value()", all, CallType::new(TypeId(6)))
            .lint(&options())
            .unwrap()
            .diagnostics
            .is_empty()
    );
}

#[test]
fn incomplete_dangling_and_cyclic_type_proofs_are_not_safe_results() {
    assert!(TypedSource::new(input("f()"), vec![], vec![None]).is_err());
    assert!(
        TypedSource::new(
            input("import('pkg')"),
            vec![node(TypeKind::Any)],
            vec![None]
        )
        .is_err()
    );
    assert!(TypedSource::new(input("f()"), vec![], vec![Some(CallType::new(TypeId(0)))]).is_err());
    let mut cyclic = node(TypeKind::Parameter);
    cyclic.constraint = Some(TypeId(0));
    assert!(
        TypedSource::new(
            input("f()"),
            vec![cyclic],
            vec![Some(CallType::new(TypeId(0)))]
        )
        .is_err()
    );
    let broken = TypedSource::new(input("function ("), vec![], vec![])
        .unwrap()
        .lint(&options())
        .unwrap();
    assert!(!broken.parse_diagnostics.is_empty());
}

#[test]
fn types_cannot_be_applied_to_a_different_module_source_or_path() {
    use wake_lint_core::{ModuleFile, ModuleGraph, ModuleId};
    let typed = facts("f()", vec![node(TypeKind::Any)], CallType::new(TypeId(0)));
    for (path, source, matches) in [
        ("a.ts", "f()", true),
        ("a.ts", "g()", false),
        ("b.ts", "f()", false),
    ] {
        let file = ModuleFile::new(
            path.into(),
            path.into(),
            Arc::from(source),
            SourceType::TypeScript,
        )
        .unwrap();
        let graph = ModuleGraph::new(vec![file]).unwrap();
        assert_eq!(
            typed
                .lint_with_module(&graph, ModuleId(0), &options())
                .is_ok(),
            matches
        );
    }
    let large = vec![node(TypeKind::Other); TypedSource::MAX_TYPES + 1];
    assert!(TypedSource::new(input(""), large, vec![]).is_err());
    let mut call = CallType::new(TypeId(0));
    call.construct_signatures = u32::MAX;
    assert!(
        TypedSource::new(input("f()"), vec![node(TypeKind::Object)], vec![Some(call)]).is_err()
    );
}
