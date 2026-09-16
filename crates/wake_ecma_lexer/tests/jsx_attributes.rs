use wake_ecma_lexer::{Lexer, TokenKind, tokenize};

#[test]
fn jsx_attribute_mode_keeps_raw_text_without_javascript_escape_or_newline_rules() {
    let source = "\n\"raw\n // not comment \\x &amp; 名\\\"";
    let mut lexer = Lexer::new_with_comments(source);
    let checkpoint = lexer.checkpoint();
    let token = lexer.next_jsx_attribute_token(0);
    assert_eq!(token.kind, TokenKind::Str);
    assert!(token.newline_before);
    assert_eq!(token.span.hi as usize, source.len());
    assert!(lexer.diagnostics().is_empty());
    assert!(lexer.comments().is_empty());
    lexer.rewind(checkpoint);
    assert_eq!(lexer.next_jsx_attribute_token(0), token);
    assert!(
        !tokenize(source).1.is_empty(),
        "ordinary JS string grammar stays strict"
    );
}

#[test]
fn jsx_attribute_expression_tokens_and_unterminated_strings_preserve_lexer_state() {
    let mut lexer = Lexer::new("\r\n{value}");
    assert_eq!(lexer.next_jsx_attribute_token(0).kind, TokenKind::LBrace);
    assert_eq!(lexer.next(false).kind, TokenKind::Ident);
    assert_eq!(lexer.next(false).kind, TokenKind::RBrace);
    let mut lexer = Lexer::new("\"unterminated\n名");
    assert_eq!(lexer.next_jsx_attribute_token(0).kind, TokenKind::Error);
    assert!(!lexer.diagnostics().is_empty());
    assert_eq!(lexer.next(false).kind, TokenKind::Eof);
}
