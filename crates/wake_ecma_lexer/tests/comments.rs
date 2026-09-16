use wake_ecma_lexer::{CommentKind, Lexer, TokenKind};

fn finish(lexer: &mut Lexer<'_>) {
    while !lexer.next(false).is_eof() {}
}

#[test]
fn collects_exact_comment_ranges_without_whitespace_or_literal_impostors() {
    let source = "#!/usr/bin/env node\r\n// 注释😀\r\n'/* string */'; `// template`; /* block\r\n😀 */ x // tail";
    let mut lexer = Lexer::new_with_comments(source);
    finish(&mut lexer);
    let comments = lexer.comments();
    assert_eq!(comments.len(), 4);
    assert_eq!(comments[0].kind, CommentKind::Hashbang);
    assert_eq!(comments[1].kind, CommentKind::Line);
    assert_eq!(comments[2].kind, CommentKind::Block);
    let texts: Vec<_> = comments
        .iter()
        .map(|c| &source[c.span.lo as usize..c.span.hi as usize])
        .collect();
    assert_eq!(
        texts,
        [
            "#!/usr/bin/env node",
            "// 注释😀",
            "/* block\r\n😀 */",
            "// tail"
        ]
    );
    assert!(lexer.diagnostics().is_empty());
}

#[test]
fn ordinary_lexer_does_not_collect_comments() {
    let mut lexer = Lexer::new("// skipped\nx /* skipped */");
    finish(&mut lexer);
    assert!(lexer.comments().is_empty());
}

#[test]
fn unicode_line_separators_end_line_comments() {
    for separator in ['\u{2028}', '\u{2029}'] {
        let source = format!("// comment{separator}value");
        let mut lexer = Lexer::new_with_comments(&source);
        let token = lexer.next(false);
        assert_eq!(token.kind, TokenKind::Ident);
        assert!(token.newline_before);
        assert_eq!(
            &source[token.span.lo as usize..token.span.hi as usize],
            "value"
        );
        assert_eq!(lexer.comments()[0].span.hi, 10);
    }
}

#[test]
fn checkpoint_discards_speculation_and_rescanning_does_not_duplicate_comments() {
    let mut lexer = Lexer::new_with_comments("x /* actual */ y // tail");
    lexer.next(false);
    let checkpoint = lexer.checkpoint();
    finish(&mut lexer);
    assert_eq!(lexer.comments().len(), 2);
    lexer.rewind(checkpoint);
    assert!(lexer.comments().is_empty());
    finish(&mut lexer);
    assert_eq!(lexer.comments().len(), 2);
    lexer.next_at(1, false);
    finish(&mut lexer);
    assert_eq!(lexer.comments().len(), 2);
}

#[test]
fn jsx_relex_removes_false_comments_and_checkpoint_restores_previous_observation() {
    let source = "// JSX text <x/>";
    let mut lexer = Lexer::new_with_comments(source);
    lexer.next(false);
    let checkpoint = lexer.checkpoint();
    assert_eq!(lexer.comments().len(), 1);
    let token = lexer.next_jsx_child_token(0);
    assert_eq!(token.kind, TokenKind::JsxText);
    assert!(lexer.comments().is_empty());
    lexer.rewind(checkpoint);
    assert_eq!(lexer.comments().len(), 1);
}

#[test]
fn regex_comment_impostors_are_not_collected() {
    let mut lexer = Lexer::new_with_comments(r"/[/][*]not-comment/ /* real */");
    assert_eq!(lexer.next(true).kind, TokenKind::Regex);
    finish(&mut lexer);
    assert_eq!(lexer.comments().len(), 1);
}

#[test]
fn unterminated_block_comment_is_retained_through_eof() {
    let source = "x /* incomplete 😀";
    let mut lexer = Lexer::new_with_comments(source);
    finish(&mut lexer);
    assert_eq!(lexer.comments().len(), 1);
    assert_eq!(lexer.comments()[0].span.hi as usize, source.len());
    assert!(!lexer.diagnostics().is_empty());
}
