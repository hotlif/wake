use wake_lint_core::{
    LintOptions, ModuleFile, ModuleGraph, ModuleId, RuleLevel, SourceType, rule_catalog,
};

#[test]
fn catalog_is_sorted_closed_and_describes_enabled_diagnostics() {
    let catalog = rule_catalog();
    assert!(catalog.windows(2).all(|pair| pair[0].id < pair[1].id));
    let rule = |id: &str| catalog.iter().find(|rule| rule.id == id).unwrap();
    assert_eq!(rule("js/no-debugger").default_level, RuleLevel::Error);
    assert_eq!(rule("style/quotes").default_level, RuleLevel::Off);
    assert_eq!(rule("js/no-console").analysis, "scope");
    assert_eq!(rule("js/no-unreachable").analysis, "control-flow");
    assert_eq!(
        rule("react-hooks/rules-of-hooks").analysis,
        "scope-control-flow"
    );
    assert_eq!(rule("js/no-constant-binary-expression").analysis, "syntax");
    assert_eq!(
        rule("js/no-constant-binary-expression").default_level,
        RuleLevel::Off
    );
    assert_eq!(rule("ts/no-explicit-any").languages, ["ts", "tsx"]);
    assert!(
        rule("ts/no-misused-promises")
            .message_ids
            .contains(&"promiseCallback".into())
    );
    assert_eq!(
        rule("ts/no-misused-promises").options_schema["properties"]["checks_conditionals"]["default"],
        true
    );
    assert_eq!(
        rule("ts/no-misused-promises").options_schema["properties"]["checks_void_return"]["oneOf"]
            [1]["properties"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        [
            "arguments",
            "attributes",
            "properties",
            "returns",
            "variables"
        ]
    );
    assert_eq!(rule("react/no-danger").languages, ["jsx", "tsx"]);
    assert!(rule("style/quotes").fixable);
    assert_eq!(
        rule("style/quotes").options_schema["properties"]["quote"]["enum"],
        serde_json::json!(["single", "double"])
    );
    assert_eq!(
        rule("js/no-debugger").options_schema["additionalProperties"],
        false
    );
    let options = LintOptions {
        recommended: false,
        rules: catalog
            .iter()
            // Source-bound type metadata and diagnostics have their own typed_calls fixture.
            .filter(|rule| rule.analysis != "type-information")
            .map(|rule| (rule.id.clone(), RuleLevel::Error.into()))
            .collect(),
        ..LintOptions::default()
    };
    let source = "// @ts-ignore\nnamespace N { export const n = 1; } interface Empty {} const x: any = value!; var a = [,,]; debugger; console.log(a); function f(ok) { if (ok) return 1; } const C = <div key='a' key='b' children='x' dangerouslySetInnerHTML={x}></div>; a = a; a == a; x === NaN;";
    let graph = ModuleGraph::new(vec![
        ModuleFile::new(
            "fixture".into(),
            "fixture.tsx".into(),
            source.into(),
            SourceType::Tsx,
        )
        .unwrap(),
    ])
    .unwrap();
    let result = graph.lint(ModuleId(0), &options).unwrap();
    assert!(result.parse_diagnostics.is_empty(), "{:?}", result);
    assert!(result.diagnostics.len() > 10);
    for diagnostic in result.diagnostics {
        let metadata = rule(&diagnostic.rule_id);
        assert!(
            metadata.message_ids.contains(&diagnostic.message_id),
            "{:?}",
            diagnostic
        );
        assert!(!metadata.documentation.is_empty());
        if diagnostic.fix.is_some() {
            assert!(metadata.fixable);
        }
    }
}
