use std::fs;
use wake_app::{CancellationToken, LintProjectOptions, lint_project};

#[test]
fn globals_merge_per_name_and_explain_standard_root_override_and_request() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("wake.config.toml"),
        r#"
[lint.globals]
console = "readonly"
Promise = "off"
rootOnly = "writable"
[[lint.overrides]]
files = ["**/*.ts"]
globals = { console = "writable", local = "readonly" }
[[lint.overrides]]
files = ["src/**"]
globals = { local = "off" }
"#,
    )
    .unwrap();
    let options = LintProjectOptions {
        root: dir.path().into(),
        print_config: Some("src/a.ts".into()),
        globals: [
            ("requested".into(), serde_json::json!("readonly")),
            ("console".into(), serde_json::json!("off")),
        ]
        .into(),
        ..Default::default()
    };
    let result = lint_project(options, &CancellationToken::default()).unwrap();
    let json = serde_json::to_value(result).unwrap();
    let globals = &json["config"]["globals"];
    assert_eq!(
        globals["Math"],
        serde_json::json!({"mode":"readonly","source":"standard:es2024@1"})
    );
    assert_eq!(
        globals["Promise"],
        serde_json::json!({"mode":"off","source":"config:globals"})
    );
    assert_eq!(globals["rootOnly"]["mode"], "writable");
    assert_eq!(
        globals["console"],
        serde_json::json!({"mode":"off","source":"request"})
    );
    assert_eq!(globals["requested"]["source"], "request");
    assert_eq!(
        globals["local"],
        serde_json::json!({"mode":"off","source":"override:1"})
    );
}

#[test]
fn request_globals_are_validated_by_the_core_and_conflict_with_catalog() {
    let dir = tempfile::tempdir().unwrap();
    for (name, mode) in [
        ("x.y", serde_json::json!("off")),
        ("valid", serde_json::json!(false)),
    ] {
        let error = lint_project(
            LintProjectOptions {
                root: dir.path().into(),
                globals: [(name.into(), mode)].into(),
                ..Default::default()
            },
            &CancellationToken::default(),
        )
        .unwrap_err();
        assert_eq!(error.code, "WAKE_LINT_CONFIG");
    }
    assert!(
        lint_project(
            LintProjectOptions {
                list_rules: true,
                globals: [("valid".into(), serde_json::json!("readonly"))].into(),
                ..Default::default()
            },
            &CancellationToken::default()
        )
        .is_err()
    );
}

#[test]
fn invalid_global_names_and_modes_fail_even_in_unmatched_overrides() {
    let dir = tempfile::tempdir().unwrap();
    for value in [
        "{ 'bad.name' = 'off' }",
        "{ valid = true }",
        "{ valid = 'readwrite' }",
    ] {
        fs::write(
            dir.path().join("wake.config.toml"),
            format!("[[lint.overrides]]\nfiles=['never/**']\nglobals={value}"),
        )
        .unwrap();
        let error = lint_project(
            LintProjectOptions {
                root: dir.path().into(),
                ..Default::default()
            },
            &CancellationToken::default(),
        )
        .unwrap_err();
        assert_eq!(error.code, "WAKE_LINT_CONFIG");
    }
}
