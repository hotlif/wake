use wake_lint_core::{LintOptions, SourceType, lint_text};

fn count(source: &str, rule: &str) -> usize {
    let options: LintOptions = serde_json::from_value(serde_json::json!({
        "recommended": false, "rules": {rule: "error"}
    }))
    .unwrap();
    let result = lint_text(source, SourceType::Tsx, &options).unwrap();
    assert!(
        result.parse_diagnostics.is_empty(),
        "{:?}",
        result.parse_diagnostics
    );
    result.diagnostics.len()
}

#[test]
fn duplicate_keys_and_cases_compare_exact_code_units() {
    let source = r#"
const object={"\ud800":1,"\ufffd":2,"\u{d800}":3,"👍":4,"\ud83d\udc4d":5};
switch(value){case "\ud800":break;case "\ufffd":break;case "\u{d800}":break;}
"#;
    assert_eq!(count(source, "js/no-dupe-keys"), 2);
    assert_eq!(count(source, "js/no-duplicate-case"), 1);
}

#[test]
fn lone_surrogates_are_truthy_and_never_valid_typeof_keywords() {
    let source = r#"if("\ud800"){work()}typeof value==="\ud800";typeof value==="string";"#;
    assert_eq!(count(source, "js/no-constant-condition"), 1);
    assert_eq!(count(source, "js/valid-typeof"), 1);
}

#[test]
fn accessibility_keeps_nonempty_text_and_ascii_fallback_tokens() {
    let source = r#"
const view=<>
  <a href={"\ud800"} aria-label={"\ud800"}/>
  <div role={"\ud800 button"} tabIndex={0}/>
  <div role={"\ud800"}/>
  <div aria-live={"\ud800"}/>
  <div aria-valuenow={"\ud800"}/>
</>;
"#;
    assert_eq!(count(source, "a11y/anchor-has-content"), 0);
    assert_eq!(count(source, "a11y/anchor-is-valid"), 0);
    assert_eq!(count(source, "a11y/aria-role"), 1);
    assert_eq!(count(source, "a11y/aria-proptypes"), 2);
}

#[test]
fn tagged_invalid_escapes_are_accepted_without_inventing_a_constant_value() {
    assert_eq!(
        count(
            r"if (tag`\x${value}\8`) { work(); }",
            "js/no-constant-condition"
        ),
        0
    );
    let result = lint_text(
        r"if (`\8`) { work(); }",
        SourceType::TypeScript,
        &LintOptions::default(),
    )
    .unwrap();
    assert!(!result.parse_diagnostics.is_empty());
}

#[test]
fn duplicate_imports_compare_attribute_code_units_without_replacement() {
    let source = r#"
import 'm' with {"\ud800":"\udfff",type:"json"};
import 'm' with {"\ud801":"\udfff",type:"json"};
import 'm' with {"\ufffd":"\udfff",type:"json"};
import 'm' with {"\ud800":"\ufffd",type:"json"};
import 'm' with {"type":"json","\u{d800}":"\udfff"};
"#;
    assert_eq!(count(source, "js/no-duplicate-imports"), 1);
}

#[test]
fn duplicate_erased_imports_compare_module_code_units() {
    let source = r#"import type A from '\ud800'; import type B from '\ud801'; import type C from '\ufffd'; import type D from '\u{d800}';"#;
    assert_eq!(count(source, "js/no-duplicate-imports"), 1);
}
