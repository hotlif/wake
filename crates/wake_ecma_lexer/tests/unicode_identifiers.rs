use wake_ecma_lexer::{TokenKind, tokenize};

#[test]
fn unicode_identifier_properties_accept_combining_connector_and_id_not_xid_characters() {
    for name in [
        "a\u{0301}",
        "a\u{203f}",
        "a\u{200c}",
        "a\u{200d}",
        "a\u{30fb}",
        "a\u{ff65}",
        "\u{2118}",
        "\u{309b}",
        "\u{10400}x",
        "a\\u0301",
        "\\u2118",
        "\\u{10400}x",
    ] {
        let (tokens, diagnostics) = tokenize(name);
        assert!(diagnostics.is_empty(), "{name:?}: {diagnostics:?}");
        assert_eq!(tokens.len(), 2, "{name:?}");
        assert_eq!(tokens[0].kind, TokenKind::Ident, "{name:?}");
        assert_eq!(tokens[0].span.hi as usize, name.len(), "{name:?}");
    }
}

#[test]
fn alphabetic_marks_and_non_decimal_numbers_do_not_create_identifiers() {
    for name in [
        "\u{0345}x",
        "\u{0301}x",
        "\u{200c}x",
        "a\u{00b2}",
        "a\u{00bc}",
        "\\u0345x",
        "a\\u00b2",
        "\\uD800",
    ] {
        let (_, diagnostics) = tokenize(name);
        assert!(!diagnostics.is_empty(), "accepted {name:?}");
    }
}

#[test]
fn private_identifiers_validate_the_whole_name_with_the_same_escape_rules() {
    for name in ["#a\u{0301}", "#a\\u0301", "#\\u0061", "#\u{2118}"] {
        let (tokens, diagnostics) = tokenize(name);
        assert!(diagnostics.is_empty(), "{name:?}: {diagnostics:?}");
        assert_eq!(tokens.len(), 2, "{name:?}");
        assert_eq!(tokens[0].kind, TokenKind::PrivateIdent, "{name:?}");
        assert_eq!(tokens[0].span.hi as usize, name.len(), "{name:?}");
    }
    for name in ["#1", "#\u{0345}", "#\\u0031", "#a\\u00b2"] {
        assert!(!tokenize(name).1.is_empty(), "accepted {name:?}");
    }
}

#[test]
fn numeric_literal_boundaries_distinguish_unicode_trivia_from_identifier_starts() {
    for separator in ['\u{00a0}', '\u{2003}', '\u{2028}', '\u{2029}', '\u{feff}'] {
        let source = format!("1{separator}+2");
        assert!(
            tokenize(&source).1.is_empty(),
            "{source:?}: {:?}",
            tokenize(&source).1
        );
    }
    for source in ["1名", "1\\u0061", "0x1名", "1n名", "1\u{2118}", "1n1"] {
        assert!(!tokenize(source).1.is_empty(), "accepted {source:?}");
    }
}
