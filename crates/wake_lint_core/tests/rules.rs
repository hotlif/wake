use wake_lint_core::{LintOptions, RuleLevel, SourceType, lint_text};

fn rules(source: &str) -> Vec<String> {
    lint_text(source, SourceType::Tsx, &LintOptions::default())
        .unwrap()
        .diagnostics
        .into_iter()
        .map(|d| d.rule_id)
        .collect()
}

#[test]
fn recommended_rules_report_in_source_order_with_exact_levels() {
    let source = "debugger; if (value == null) {} if (true) work();";
    let result = lint_text(source, SourceType::Module, &LintOptions::default()).unwrap();
    assert_eq!(
        result
            .diagnostics
            .iter()
            .map(|d| d.rule_id.as_str())
            .collect::<Vec<_>>(),
        [
            "js/no-debugger",
            "js/eqeqeq",
            "js/no-empty",
            "js/no-constant-condition"
        ]
    );
    assert_eq!(result.diagnostics[0].level, RuleLevel::Error);
    assert_eq!(result.diagnostics[1].level, RuleLevel::Warn);
    assert_eq!(
        &source[result.diagnostics[1].start as usize..result.diagnostics[1].end as usize],
        "value == null"
    );
}

#[test]
fn configuration_is_validated_even_when_rule_is_off() {
    let mut options = LintOptions::default();
    options
        .rules
        .insert("js/typo".into(), RuleLevel::Off.into());
    assert!(lint_text("", SourceType::Module, &options).is_err());
    options.rules.clear();
    options
        .rules
        .insert("js/no-debugger".into(), RuleLevel::Off.into());
    options
        .rules
        .insert("js/eqeqeq".into(), RuleLevel::Error.into());
    let result = lint_text("debugger; a != b;", SourceType::Module, &options).unwrap();
    assert_eq!(result.diagnostics.len(), 1);
    assert_eq!(result.diagnostics[0].level, RuleLevel::Error);
}

#[test]
fn syntax_errors_fail_closed_and_cannot_be_suppressed() {
    let result = lint_text(
        "/* wake-lint-disable */ const = ; debugger;",
        SourceType::Module,
        &LintOptions::default(),
    )
    .unwrap();
    assert!(result.has_errors());
    assert!(result.diagnostics.is_empty());
    assert!(!result.parse_diagnostics.is_empty());
}

#[test]
fn static_duplicate_keys_allow_accessor_pairs_and_exclude_generated_jsx_props() {
    assert_eq!(
        rules("const x = {a: 1, 'a': 2, [\"a\"]: 3};"),
        ["js/no-dupe-keys", "js/no-dupe-keys"]
    );
    assert!(rules("const x = {get a(){return 1}, set a(value){}, b: 2};").is_empty());
    assert_eq!(
        rules("const x = {get a(){return 1}, get a(){return 2}};"),
        ["js/no-dupe-keys"]
    );
    assert_eq!(
        rules("const x = {1: 1, '1': 2, 0x1: 3};"),
        ["js/no-dupe-keys", "js/no-dupe-keys"]
    );
    assert!(rules("const el = <div a='1' a='2' />;").is_empty());
    assert_eq!(
        rules("const el = <div a={{x: 1, x: 2}} />;"),
        ["js/no-dupe-keys"]
    );
}

#[test]
fn duplicate_cases_use_cooked_literals_and_do_not_guess_dynamic_values() {
    assert_eq!(
        rules(
            "switch (x) {case 'a': break; case '\\u0061': break; case 1: break; case 0x1: break;}"
        ),
        ["js/no-duplicate-case", "js/no-duplicate-case"]
    );
    assert!(rules("switch (x) {case a: break; case b: break;}").is_empty());
}

#[test]
fn empty_blocks_with_comments_and_empty_functions_are_allowed() {
    assert!(
        rules("if (ok) { /* intentional */ } function noop() {} const f = () => {};").is_empty()
    );
    assert_eq!(rules("if (ok) {}"), ["js/no-empty"]);
}

#[test]
fn constant_conditions_are_semantic_and_nested_rules_still_run() {
    assert_eq!(
        rules("if (!false) { debugger; } while (0) work(); const x = {} ? 1 : 2;"),
        [
            "js/no-constant-condition",
            "js/no-debugger",
            "js/no-constant-condition",
            "js/no-constant-condition"
        ]
    );
    assert!(rules("if (x) work(); while (x && y) work(); const z = x ? 1 : 2;").is_empty());
}

#[test]
fn directives_are_real_comments_and_rule_specific() {
    assert_eq!(
        rules("'// wake-lint-disable'; debugger;"),
        ["js/no-debugger"]
    );
    assert_eq!(
        rules("const el = <div>// wake-lint-disable</div>; debugger;"),
        ["js/no-debugger"]
    );
    assert!(
        rules("// wake-lint-disable-next-line js/no-debugger -- expected\ndebugger;").is_empty()
    );
    assert_eq!(
        rules(
            "/* wake-lint-disable js/no-debugger */ debugger; a == b; /* wake-lint-enable js/no-debugger */ debugger;"
        ),
        ["js/eqeqeq", "js/no-debugger"]
    );
    assert!(rules("debugger; // wake-lint-disable-line js/no-debugger").is_empty());
    assert_eq!(
        rules("// wake-lint-disable-next-line js/no-debugger\u{2028}debugger;\u{2029}debugger;"),
        ["js/no-debugger"]
    );
}

#[test]
fn owned_results_outlive_input_and_keep_unicode_byte_offsets() {
    let source = String::from("// 😀\r\ndebugger;");
    let output = lint_text(&source, SourceType::Module, &LintOptions::default()).unwrap();
    drop(source);
    assert_eq!(output.diagnostics[0].start, 9);
    assert_eq!(output.diagnostics[0].rule_id, "js/no-debugger");
}

#[test]
fn unused_suppressions_track_each_rule_and_idempotent_disable() {
    let result = rules("// wake-lint-disable js/no-debugger, js/eqeqeq\ndebugger;");
    assert_eq!(result, ["wake/unused-disable"]);
    assert_eq!(
        rules("/* wake-lint-disable */ /* wake-lint-disable */ debugger;"),
        ["wake/unused-disable"]
    );
    assert_eq!(
        rules("/* wake-lint-disable js/no-debugger */ /* wake-lint-enable */ debugger;"),
        ["wake/unused-disable", "js/no-debugger"]
    );
    assert!(rules("/* wake-lint-disable js/no-debugger */ debugger; /* wake-lint-enable */ debugger; // wake-lint-disable-line js/no-debugger").is_empty());
}

#[test]
fn multiline_next_line_uses_comment_end_and_rejects_multiline_disable_line() {
    assert!(
        rules("/* wake-lint-disable-next-line js/no-debugger\n -- reason */\ndebugger;").is_empty()
    );
    assert_eq!(
        rules("/* wake-lint-disable-line\n */ debugger;"),
        ["wake/invalid-directive", "js/no-debugger"]
    );
}
