//! Token 流快照测试（insta）——PLAN §1.2 DoD「各类字面量单测 + snapshot」。
//!
//! 更新快照：`cargo insta review`（或 `INSTA_UPDATE=always cargo test -p wake_ecma_lexer`）。

use wake_ecma_lexer::{TokenKind, tokenize};

/// 把源码 tokenize 成稳定可读的多行文本：`kind  "text"[ ⏎]`。
fn dump(src: &str) -> String {
    let (tokens, diags) = tokenize(src);
    let mut out = String::new();
    for t in &tokens {
        if matches!(t.kind, TokenKind::Eof) {
            continue;
        }
        let text = &src[t.span.lo as usize..t.span.hi as usize];
        let nl = if t.newline_before { " ⏎" } else { "" };
        out.push_str(&format!("{:<18} {:?}{}\n", t.kind.describe(), text, nl));
    }
    if !diags.is_empty() {
        out.push_str("--- diagnostics ---\n");
        for d in &diags {
            out.push_str(&format!("{}: {}\n", d.severity.as_str(), d.message));
        }
    }
    out
}

#[test]
fn snapshot_literals() {
    insta::assert_snapshot!(dump(
        "const n = 42n; let f = 3.14e-2; let h = 0xDE_AD; let b = 0b1010; let o = 0o755;"
    ));
}

#[test]
fn snapshot_strings_templates() {
    insta::assert_snapshot!(dump(
        r#"let s = "a\tb"; let t = `x${a + b}y${c}z`; let e = '\u{1F600}';"#
    ));
}

#[test]
fn snapshot_operators() {
    insta::assert_snapshot!(dump(
        "a ??= b; x **= 2; y >>>= 1; p?.q?.[r]; m = n === o ? p : q; ...spread;"
    ));
}

#[test]
fn snapshot_class_private_regex() {
    insta::assert_snapshot!(dump(
        "class C { #x = 1; get #y() { return this.#x; } } const re = /^\\d+$/gimsu;"
    ));
}

#[test]
fn snapshot_asi_newlines() {
    insta::assert_snapshot!(dump("a\nb\nreturn\n42"));
}
