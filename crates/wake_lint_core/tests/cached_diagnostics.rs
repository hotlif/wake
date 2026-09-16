use wake_lint_core::{LintOptions, SourceType, lint_text, validate_cached_diagnostics};

#[test]
fn cached_findings_require_current_rules_messages_levels_and_utf8_edit_ranges() {
    let source = "// 😀\ndebugger;";
    let options = LintOptions::default();
    let diagnostics = lint_text(source, SourceType::Module, &options)
        .unwrap()
        .diagnostics;
    assert!(validate_cached_diagnostics(source, &options, &diagnostics).is_ok());
    let mut invalid = diagnostics.clone();
    invalid[0].rule_id = "js/unknown".into();
    assert!(validate_cached_diagnostics(source, &options, &invalid).is_err());
    invalid = diagnostics.clone();
    invalid[0].message_id = "unknown".into();
    assert!(validate_cached_diagnostics(source, &options, &invalid).is_err());
    invalid = diagnostics.clone();
    invalid[0].level = wake_lint_core::RuleLevel::Warn;
    assert!(validate_cached_diagnostics(source, &options, &invalid).is_err());
    invalid = diagnostics.clone();
    invalid[0].start = 4;
    assert!(validate_cached_diagnostics(source, &options, &invalid).is_err());
    invalid = diagnostics.clone();
    invalid[0].end = 500;
    assert!(validate_cached_diagnostics(source, &options, &invalid).is_err());
    invalid = diagnostics.clone();
    invalid.push(diagnostics[0].clone());
    assert!(validate_cached_diagnostics(source, &options, &invalid).is_err());
    let mut off = options.clone();
    off.rules.insert(
        "js/no-debugger".into(),
        wake_lint_core::RuleLevel::Off.into(),
    );
    assert!(validate_cached_diagnostics(source, &off, &diagnostics).is_err());

    let options: LintOptions = serde_json::from_value(
        serde_json::json!({"recommended":false,"rules":{"style/eol-last":"error"}}),
    )
    .unwrap();
    let mut fixed = lint_text(source, SourceType::Module, &options)
        .unwrap()
        .diagnostics;
    assert!(validate_cached_diagnostics(source, &options, &fixed).is_ok());
    fixed[0].fix.as_mut().unwrap().edits[0].start = 4;
    assert!(validate_cached_diagnostics(source, &options, &fixed).is_err());
}

#[test]
fn cached_unused_directives_preserve_distinct_targets_at_one_source_range() {
    let source = "// wake-lint-disable js/no-debugger, js/eqeqeq\nrun();";
    let options = LintOptions::default();
    let result = lint_text(source, SourceType::Module, &options).unwrap();
    assert_eq!(result.diagnostics.len(), 2);
    assert!(validate_cached_diagnostics(source, &options, &result.diagnostics).is_ok());
}
