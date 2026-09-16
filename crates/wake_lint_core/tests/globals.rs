use wake_lint_core::{GlobalMode, LintOptions, SourceType, effective_globals, lint_text};

#[test]
fn globals_have_frozen_standard_defaults_and_explicit_modes() {
    assert_eq!(
        effective_globals(&LintOptions::default()).unwrap().len(),
        58
    );
    let options: LintOptions = serde_json::from_value(serde_json::json!({"globals":{
        "Promise":"off", "injected":"writable", "中文":"readonly"
    }}))
    .unwrap();
    let globals = effective_globals(&options).unwrap();
    assert_eq!(globals["Math"].mode, GlobalMode::Readonly);
    assert_eq!(globals["Math"].source, "standard:es2024@1");
    assert_eq!(globals["Promise"].mode, GlobalMode::Off);
    assert_eq!(globals["Promise"].source, "globals");
    assert_eq!(globals["injected"].mode, GlobalMode::Writable);
    assert_eq!(globals["中文"].mode, GlobalMode::Readonly);
    assert!(!globals.contains_key("console"));
    assert!(!globals.contains_key("window"));
    assert!(!globals.contains_key("process"));
}

#[test]
fn globals_are_validated_even_without_name_rules_or_when_turned_off() {
    for name in [
        "",
        " spaced",
        "spaced ",
        "object.field",
        "call()",
        "a; b",
        "a/*comment*/",
        "\\u0061",
        "new",
        "0",
        "(name)",
    ] {
        let options: LintOptions =
            serde_json::from_value(serde_json::json!({"globals":{name:"off"}})).unwrap();
        assert!(effective_globals(&options).is_err(), "{name}");
        assert!(
            lint_text("", SourceType::Module, &options).is_err(),
            "{name}"
        );
    }
    assert!(
        serde_json::from_value::<LintOptions>(serde_json::json!({"globals":{"valid":true}}))
            .is_err()
    );
}

#[test]
fn unicode_globals_follow_the_same_identifier_grammar_as_runtime_references() {
    for name in [
        "a\u{0301}",
        "a\u{203f}",
        "\u{2118}",
        "\u{309b}",
        "a\u{30fb}",
    ] {
        let options: LintOptions = serde_json::from_value(serde_json::json!({"recommended":false,"globals":{name:"readonly"},"rules":{"js/no-undef":"error"}})).unwrap();
        let result = lint_text(&format!("{name};"), SourceType::Module, &options).unwrap();
        assert!(
            result.parse_diagnostics.is_empty(),
            "{name:?}: {:?}",
            result.parse_diagnostics
        );
        assert!(result.diagnostics.is_empty(), "{name:?}");
    }
    for name in ["\u{0345}x", "a\u{00b2}"] {
        let options: LintOptions =
            serde_json::from_value(serde_json::json!({"globals":{name:"off"}})).unwrap();
        assert!(effective_globals(&options).is_err(), "{name:?}");
    }
}

#[test]
fn browser_and_node_environment_sets_are_explicit_and_versioned() {
    let options: LintOptions = serde_json::from_value(serde_json::json!({
        "environments": ["browser", "node"]
    }))
    .unwrap();
    let globals = effective_globals(&options).unwrap();
    assert_eq!(globals["window"].mode, GlobalMode::Readonly);
    assert_eq!(globals["window"].source, "environment:browser@1");
    assert_eq!(globals["process"].mode, GlobalMode::Readonly);
    assert_eq!(globals["process"].source, "environment:node@1");
    assert_eq!(globals["console"].source, "environment:node@1");

    let options: LintOptions = serde_json::from_value(serde_json::json!({
        "environments": ["browser"],
        "globals": {"window": "off", "custom": "writable"}
    }))
    .unwrap();
    let globals = effective_globals(&options).unwrap();
    assert_eq!(globals["window"].mode, GlobalMode::Off);
    assert_eq!(globals["custom"].mode, GlobalMode::Writable);

    let invalid: LintOptions = serde_json::from_value(serde_json::json!({
        "environments": ["deno"]
    }))
    .unwrap();
    assert!(effective_globals(&invalid).is_err());
}
