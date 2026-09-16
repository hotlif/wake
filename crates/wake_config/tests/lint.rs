use wake_config::Config;

#[test]
fn lint_type_projects_are_closed_data_with_an_explicit_compiler_default() {
    let config: Config = toml::from_str("[lint.types]\nprojects=['tsconfig.json']").unwrap();
    let types = config.lint.types.unwrap();
    assert_eq!(types.compiler, "typescript");
    assert_eq!(types.projects, ["tsconfig.json"]);
    for source in [
        "[lint.types]\nprojects=['tsconfig.json']\nnode=true",
        "[lint.types]\nprojects='tsconfig.json'",
        "[lint.types]\ncompiler=7",
    ] {
        assert!(toml::from_str::<Config>(source).is_err());
    }
}

#[test]
fn lint_config_accepts_levels_and_ordered_overrides() {
    let config: Config = toml::from_str(
        r#"
[lint]
recommended = false
files = ["src/**/*.ts"]
ignore = ["**/generated/**"]
[lint.rules]
"js/no-debugger" = "error"
[[lint.overrides]]
files = ["**/*.test.ts"]
rules = { "js/no-debugger" = "off" }
"#,
    )
    .unwrap();
    assert!(!config.lint.recommended);
    assert_eq!(config.lint.files, ["src/**/*.ts"]);
    assert_eq!(config.lint.rules["js/no-debugger"].as_str(), "error");
    assert_eq!(
        config.lint.overrides[0].rules["js/no-debugger"].as_str(),
        "off"
    );
}

#[test]
fn lint_rejects_unknown_configuration_fields_and_levels() {
    for source in [
        "[lint]\nfix = true",
        "[lint.rules]\n'js/no-debugger' = 'fatal'",
        "[[lint.overrides]]\nfiles=['*.js']\nunknown=true",
    ] {
        assert!(toml::from_str::<Config>(source).is_err(), "{source}");
    }
}

#[test]
fn lint_accepts_parameter_objects_and_versioned_presets() {
    let config: Config = toml::from_str(
        r#"
[lint]
presets = ["react@1", "style"]
[lint.rules]
"js/eqeqeq" = { level = "warn", options = { allow_null = true } }
[[lint.overrides]]
files = ["**/*.test.ts"]
rules = { "js/eqeqeq" = "error" }
"#,
    )
    .expect("native rule objects and presets must deserialize");
    assert_eq!(config.lint.overrides.len(), 1);
    for source in [
        "[lint.rules]\n'js/eqeqeq' = { options = {} }",
        "[lint.rules]\n'js/eqeqeq' = { level = 'off', unknown = true }",
        "[lint.rules]\n'js/eqeqeq' = { level = 'off', options = [] }",
        "[lint.rules]\n'js/eqeqeq' = { level = 'off', options = false }",
    ] {
        assert!(toml::from_str::<Config>(source).is_err(), "{source}");
    }
}

#[test]
fn lint_accepts_closed_processor_mappings() {
    let config: Config = toml::from_str(
        r#"
[lint.processors]
"docs/**/*.md" = "markdown"
"#,
    )
    .unwrap();
    assert_eq!(
        config
            .lint
            .processors
            .get("docs/**/*.md")
            .map(String::as_str),
        Some("markdown")
    );
    assert!(toml::from_str::<Config>("[lint.processors]\n\"**/*.md\" = 7").is_err());
}
