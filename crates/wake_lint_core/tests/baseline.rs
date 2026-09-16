use wake_lint_core::{
    LintOptions, RuleLevel, SourceType, apply_baseline, baseline_candidates,
    fix_text_with_baseline, lint_text, validate_baseline,
};

#[test]
fn baseline_follows_unique_unicode_context_but_not_changed_or_duplicate_lines() {
    let options = LintOptions::default();
    let source = "const 名 = '😀'; debugger;\r\n";
    let original = lint_text(source, SourceType::Module, &options).unwrap();
    let baseline = baseline_candidates(source, &original.diagnostics).unwrap();
    assert_eq!(baseline.entries.len(), 1);
    for source in [format!("// unrelated\n{source}"), format!("  {source}")] {
        let mut result = lint_text(&source, SourceType::Module, &options).unwrap();
        let matched = apply_baseline(&source, &mut result.diagnostics, &baseline.entries).unwrap();
        assert_eq!(matched.suppressed, 1);
        assert!(result.diagnostics.is_empty());
    }
    let changed = source.replace("名", "name");
    let mut result = lint_text(&changed, SourceType::Module, &options).unwrap();
    assert_eq!(
        apply_baseline(&changed, &mut result.diagnostics, &baseline.entries)
            .unwrap()
            .suppressed,
        0
    );
    let repeated = "debugger;\ndebugger;\n";
    let one = lint_text("debugger;\n", SourceType::Module, &options).unwrap();
    let entries = baseline_candidates("debugger;\n", &one.diagnostics)
        .unwrap()
        .entries;
    let mut result = lint_text(repeated, SourceType::Module, &options).unwrap();
    let matched = apply_baseline(repeated, &mut result.diagnostics, &entries).unwrap();
    assert_eq!(matched.suppressed, 0);
    assert_eq!(matched.ambiguous, 2);
    assert_eq!(result.diagnostics.len(), 2);
}

#[test]
fn baseline_never_suppresses_directives_or_accepts_invalid_identity() {
    let source =
        "// wake-lint-disable-next-line js/no-debugger\nkeep();\n// wake-lint-disable unknown\n";
    let result = lint_text(source, SourceType::Module, &LintOptions::default()).unwrap();
    assert!(!result.diagnostics.is_empty());
    assert!(
        baseline_candidates(source, &result.diagnostics)
            .unwrap()
            .entries
            .is_empty()
    );
    let invalid = serde_json::from_value(serde_json::json!({"ruleId":"wake/invalid-directive","messageId":"invalid","fingerprint":"0".repeat(64)})).unwrap();
    assert!(validate_baseline(&[invalid]).is_err());
    let result = lint_text("debugger;", SourceType::Module, &LintOptions::default()).unwrap();
    let mut entries = baseline_candidates("debugger;", &result.diagnostics)
        .unwrap()
        .entries;
    entries.push(entries[0].clone());
    assert!(validate_baseline(&entries).is_err());
}

#[test]
fn fix_iterations_exclude_baselined_fixes_and_return_final_match_counts() {
    let options = LintOptions {
        recommended: false,
        rules: [
            ("style/quotes".into(), RuleLevel::Error.into()),
            ("style/eol-last".into(), RuleLevel::Error.into()),
        ]
        .into(),
        ..Default::default()
    };
    let source = "const 名 = \"😀\";";
    let original = lint_text(source, SourceType::Module, &options).unwrap();
    let entries: Vec<_> = baseline_candidates(source, &original.diagnostics)
        .unwrap()
        .entries
        .into_iter()
        .filter(|entry| entry.rule_id == "style/quotes")
        .collect();
    let (fixed, matched) =
        fix_text_with_baseline(source, SourceType::Module, &options, &entries).unwrap();
    assert_eq!(fixed.output, format!("{source}\n"));
    assert_eq!(fixed.passes, 1);
    assert_eq!(matched.suppressed, 1);
    assert!(fixed.result.diagnostics.is_empty());
}
