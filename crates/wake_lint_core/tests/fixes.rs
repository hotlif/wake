use wake_lint_core::{
    LintFix, LintOptions, RuleLevel, SourceType, TextEdit, apply_fixes, fix_text, lint_text,
};

fn edit(start: u32, end: u32, text: &str) -> TextEdit {
    TextEdit {
        start,
        end,
        text: text.into(),
    }
}
fn fix(edits: Vec<TextEdit>) -> LintFix {
    LintFix { edits }
}

#[test]
fn edits_are_atomic_utf8_checked_and_prioritized_by_candidate_order() {
    let source = "a😀bc";
    let result = apply_fixes(
        source,
        &[
            fix(vec![edit(1, 5, "X"), edit(6, 7, "C")]),
            fix(vec![edit(1, 5, "Y"), edit(0, 1, "A")]),
            fix(vec![edit(5, 6, "B")]),
        ],
    )
    .unwrap();
    assert_eq!(result.output, "aXBC");
    assert_eq!((result.applied, result.skipped), (2, 1));
    for edits in [
        vec![edit(2, 5, "")],
        vec![edit(9, 9, "")],
        vec![edit(5, 1, "")],
        vec![edit(1, 5, ""), edit(2, 6, "")],
        vec![edit(1, 1, "x"), edit(1, 1, "y")],
        vec![],
    ] {
        assert!(apply_fixes(source, &[fix(edits)]).is_err());
    }
    let insertions = apply_fixes(
        "ab",
        &[fix(vec![edit(1, 1, "X")]), fix(vec![edit(1, 2, "Y")])],
    )
    .unwrap();
    assert_eq!(insertions.output, "aXb");
    assert_eq!(insertions.skipped, 1);
}

#[test]
fn safe_fixes_reparse_and_keep_jsx_attributes_comments_and_newline_style() {
    let mut options = LintOptions::default();
    options
        .rules
        .insert("react/self-closing-comp".into(), RuleLevel::Error.into());
    options
        .rules
        .insert("style/eol-last".into(), RuleLevel::Warn.into());
    let source = "// 😀\r\nconst x = <><C<T> a={'>'}></C><b> </b><i>{/* keep */}</i></>;";
    let result = fix_text(source, SourceType::Tsx, &options).unwrap();
    assert_eq!(
        result.output,
        "// 😀\r\nconst x = <><C<T> a={'>'}/><b> </b><i>{/* keep */}</i></>;\n"
    );
    assert_eq!(result.passes, 1);
    assert!(result.result.diagnostics.is_empty());
    assert_eq!(
        fix_text(&result.output, SourceType::Tsx, &options)
            .unwrap()
            .passes,
        0
    );
    let adjacent = fix_text("<C></C>", SourceType::Tsx, &options).unwrap();
    assert_eq!(adjacent.output, "<C/>\n");
    assert_eq!(adjacent.passes, 2);
    assert_eq!(
        fix_text("<C prop={<D></D>}></C>\n", SourceType::Tsx, &options)
            .unwrap()
            .output,
        "<C prop={<D/>}/>\n"
    );
    let suppressed = "// wake-lint-disable-next-line react/self-closing-comp\n<C></C>\n";
    assert_eq!(
        fix_text(suppressed, SourceType::Tsx, &options)
            .unwrap()
            .output,
        suppressed
    );
    let broken = "<C></D>";
    let broken_result = fix_text(broken, SourceType::Tsx, &options).unwrap();
    assert_eq!(broken_result.output, broken);
    assert!(broken_result.result.has_errors());
    for source in [
        "",
        "// comment\r",
        "// comment\r\n",
        "// comment\u{2028}",
        "// comment\u{2029}",
    ] {
        assert!(
            lint_text(source, SourceType::Tsx, &options)
                .unwrap()
                .diagnostics
                .is_empty(),
            "{source:?}"
        );
    }
}

#[test]
fn self_closing_fix_never_discards_comments_in_a_closing_tag() {
    let mut options = LintOptions::default();
    options
        .rules
        .insert("react/self-closing-comp".into(), RuleLevel::Error.into());
    let source = "<C></C /* retain this comment */>";
    let fixed = fix_text(source, SourceType::Tsx, &options).unwrap();
    assert_eq!(fixed.output, source);
    assert_eq!(fixed.passes, 0);
}
