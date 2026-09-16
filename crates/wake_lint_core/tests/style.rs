use wake_lint_core::{LintOptions, SourceType, fix_text, lint_text};

fn options() -> LintOptions {
    serde_json::from_value(serde_json::json!({"recommended":false,"rules":{
        "style/quotes":"error", "style/no-trailing-spaces":"error"
    }}))
    .unwrap()
}

#[test]
fn quote_fixes_preserve_values_escapes_jsx_attributes_and_comments() {
    let source = r#"const a = "don't"; const b = "say \"hi\""; const c = "\\\"\n\x27"; const x = <X title="same" value={"yes"} />; // "comment"
"#;
    let fixed = fix_text(source, SourceType::Tsx, &options()).unwrap();
    assert_eq!(
        fixed.output,
        r#"const a = 'don\'t'; const b = 'say "hi"'; const c = '\\"\n\x27'; const x = <X title="same" value={'yes'} />; // "comment"
"#
    );
    assert!(fixed.result.diagnostics.is_empty());
    assert_eq!(
        fix_text(&fixed.output, SourceType::Tsx, &options())
            .unwrap()
            .passes,
        0
    );
    let mut double = options();
    double.rules.insert(
        "style/quotes".into(),
        serde_json::from_value(serde_json::json!({"level":"error","options":{"quote":"double"}}))
            .unwrap(),
    );
    assert_eq!(
        fix_text(
            "'use strict'; const s = 'say \"hi\"';",
            SourceType::Module,
            &double
        )
        .unwrap()
        .output,
        "\"use strict\"; const s = \"say \\\"hi\\\"\";"
    );
}

#[test]
fn trailing_space_fixes_preserve_literal_contents_and_all_line_terminators() {
    let source = "const a = 1; \t\r\n \t\u{2028}const b = `text  \n`; // comment \t\rconst el = <X>text  \n</X>;  ";
    let result = lint_text(source, SourceType::Tsx, &options()).unwrap();
    assert!(result.parse_diagnostics.is_empty());
    assert_eq!(
        result
            .diagnostics
            .iter()
            .filter(|d| d.rule_id == "style/no-trailing-spaces")
            .count(),
        6
    );
    assert_eq!(
        result
            .diagnostics
            .iter()
            .filter(|d| d.fix.is_some())
            .count(),
        4
    );
    let fixed = fix_text(source, SourceType::Tsx, &options()).unwrap();
    assert_eq!(
        fixed.output,
        "const a = 1;\r\n\u{2028}const b = `text  \n`; // comment\rconst el = <X>text  \n</X>;"
    );
    assert_eq!(fixed.result.diagnostics.len(), 2);
    assert!(fixed.result.diagnostics.iter().all(|d| d.fix.is_none()));
}
