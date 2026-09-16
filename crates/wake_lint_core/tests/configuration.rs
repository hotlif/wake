use wake_lint_core::{LintOptions, SourceType, lint_text};

#[test]
fn pattern_parameters_are_bounded_validated_and_explained_even_when_disabled() {
    let options = configured(
        serde_json::json!({"rules":{"js/no-unused-vars":{"level":"off","options":{"vars_ignore_pattern":"^_"}}}}),
    );
    let effective = wake_lint_core::effective_configuration(&options).unwrap();
    assert_eq!(
        effective["js/no-unused-vars"].configuration.options["vars_ignore_pattern"],
        "^_"
    );
    let catalog = wake_lint_core::rule_catalog();
    let rule = catalog
        .iter()
        .find(|rule| rule.id == "js/no-unused-vars")
        .unwrap();
    assert_eq!(
        rule.options_schema["properties"]["vars_ignore_pattern"]["format"],
        "rust-regex"
    );
    for pattern in [
        serde_json::json!("["),
        serde_json::json!(true),
        serde_json::json!("x".repeat(4097)),
    ] {
        let options = configured(
            serde_json::json!({"rules":{"js/no-unused-vars":{"level":"off","options":{"vars_ignore_pattern":pattern}}}}),
        );
        assert!(wake_lint_core::effective_configuration(&options).is_err());
    }
}

fn configured(value: serde_json::Value) -> LintOptions {
    serde_json::from_value(value).expect("valid rule configuration")
}

#[test]
fn parameters_control_only_their_documented_rule_behavior() {
    let options = configured(serde_json::json!({"rules": {
        "js/eqeqeq": {"level":"error", "options":{"allow_null":true}},
        "js/no-empty": {"level":"error", "options":{"allow_catch":true}},
        "js/no-constant-condition": {"level":"error", "options":{"check_loops":false}}
    }}));
    let result = lint_text("if (a == null) {} try { call(); } catch {} while (true) { call(); } if (true) { call(); } a == b;", SourceType::Module, &options).unwrap();
    assert!(result.parse_diagnostics.is_empty());
    assert_eq!(
        result
            .diagnostics
            .iter()
            .map(|d| d.rule_id.as_str())
            .collect::<Vec<_>>(),
        ["js/no-empty", "js/no-constant-condition", "js/eqeqeq"]
    );
}

#[test]
fn invalid_options_are_rejected_even_when_rules_are_off() {
    for (id, options) in [
        ("js/eqeqeq", serde_json::json!({"allow_null":"true"})),
        ("js/eqeqeq", serde_json::json!({"allowNull":true})),
        ("js/no-debugger", serde_json::json!({"extra":1})),
        ("style/eol-last", serde_json::json!({"linebreak":"native"})),
    ] {
        let options =
            configured(serde_json::json!({"rules":{id:{"level":"off","options":options}}}));
        assert!(lint_text("", SourceType::Module, &options).is_err(), "{id}");
    }
    for options in [
        serde_json::json!({"checks_void_return":{"arguments":true,"unknown":false}}),
        serde_json::json!({"checks_void_return":{"arguments":"true"}}),
    ] {
        let options = configured(serde_json::json!({
            "rules": {"ts/no-misused-promises": {"level":"off", "options":options}}
        }));
        assert!(lint_text("", SourceType::TypeScript, &options).is_err());
    }
}

#[test]
fn misused_promise_void_return_object_options_are_closed_and_default_missing_members() {
    let options = configured(serde_json::json!({
        "recommended": false,
        "rules": {"ts/no-misused-promises": {"level":"error", "options": {
            "checks_void_return": {"arguments": false, "properties": false}
        }}}
    }));
    let effective = wake_lint_core::effective_configuration(&options).unwrap();
    assert_eq!(
        effective["ts/no-misused-promises"].configuration.options["checks_void_return"],
        serde_json::json!({"arguments":false,"properties":false})
    );
}

#[test]
fn presets_and_parameter_defaults_have_stable_effective_values() {
    let options = configured(
        serde_json::json!({"recommended":false, "presets":["react@1", "style"],
        "rules":{"style/eol-last":{"level":"error","options":{"linebreak":"crlf"}}}}),
    );
    let result = wake_lint_core::fix_text("const x = <X></X>;", SourceType::Jsx, &options).unwrap();
    assert_eq!(result.output, "const x = <X/>;\r\n");
    assert!(!result.result.has_errors());
    let unknown = configured(serde_json::json!({"presets":["react@2"]}));
    assert!(lint_text("", SourceType::Module, &unknown).is_err());
}

#[test]
fn equivalent_parameter_spellings_materialize_identically_and_presets_are_closed() {
    use wake_lint_core::{PRESETS, effective_configuration};
    let mut materialized = Vec::new();
    for setting in [
        serde_json::json!("error"),
        serde_json::json!({"level":"error", "options":{}}),
        serde_json::json!({"level":"error", "options":{"allow_null":false}}),
    ] {
        materialized.push(
            effective_configuration(&configured(
                serde_json::json!({"rules":{"js/eqeqeq":setting}}),
            ))
            .unwrap()["js/eqeqeq"]
                .configuration
                .clone(),
        );
    }
    assert!(materialized.windows(2).all(|pair| pair[0] == pair[1]));
    for preset in PRESETS {
        let by_id = effective_configuration(&configured(
            serde_json::json!({"recommended":false, "presets":[preset.id]}),
        ))
        .unwrap();
        let by_alias = effective_configuration(&configured(
            serde_json::json!({"recommended":false, "presets":[preset.alias]}),
        ))
        .unwrap();
        assert_eq!(
            serde_json::to_value(by_id).unwrap(),
            serde_json::to_value(by_alias).unwrap()
        );
    }
    for value in [
        serde_json::json!({"rules":{"js/eqeqeq":{"level":"off","options":null}}}),
        serde_json::json!({"rules":{"js/eqeqeq":{"level":"off","extra":true}}}),
    ] {
        assert!(serde_json::from_value::<LintOptions>(value).is_err());
    }
}
