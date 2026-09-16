use wake_ecma_lexer::decode_escaped_value;

#[test]
fn template_lexing_leaves_escape_validity_to_the_tagged_parser_context() {
    let source = r"tag`\u${value}\8${other}\x`";
    let (tokens, diagnostics) = wake_ecma_lexer::tokenize(source);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    use wake_ecma_lexer::TokenKind::*;
    assert_eq!(
        tokens.iter().map(|token| token.kind).collect::<Vec<_>>(),
        [
            Ident,
            TemplateHead,
            Ident,
            TemplateMiddle,
            Ident,
            TemplateTail,
            Eof
        ]
    );
}

#[test]
fn template_values_distinguish_valid_identity_escapes_from_invalid_cooked_text() {
    use wake_ecma_lexer::decode_template_value;
    for (raw, expected) in [
        (r"\\x", r"\x"),
        (r"\\u{}", r"\u{}"),
        (r"\0", "\0"),
        (r"\x41", "A"),
        (r"\u0041", "A"),
        (r"\u{00000000000000000000000041}", "A"),
        (r"\q", "q"),
        (r"\👍", "👍"),
        (r"\`", "`"),
        (r"\${literal}", "${literal}"),
    ] {
        assert_eq!(decode_template_value(raw).unwrap(), expected, "{raw}");
    }
    assert_eq!(
        decode_template_value(r"\ud800\u{dfff}")
            .unwrap()
            .code_units()
            .collect::<Vec<_>>(),
        [0xd800, 0xdfff]
    );
    for raw in [r"\", r"\1", r"\08", r"\x0", r"\u{}", r"\\\u{z}"] {
        assert!(decode_template_value(raw).is_none(), "{raw}");
    }
}

fn units(raw: &str) -> Vec<u16> {
    decode_escaped_value(raw).code_units().collect()
}

#[test]
fn isolated_surrogates_are_code_units_not_missing_or_replacement_characters() {
    for (raw, expected) in [
        (r"\ud800", vec![0xd800]),
        (r"\udfff", vec![0xdfff]),
        (r"a\u{d800}b\u{dfff}c", vec![97, 0xd800, 98, 0xdfff, 99]),
        (r"\udc00\ud800", vec![0xdc00, 0xd800]),
        (r"\ud800\ud800\udc00", vec![0xd800, 0xd800, 0xdc00]),
        (r"\ud800\u0000\ufffd", vec![0xd800, 0, 0xfffd]),
    ] {
        assert_eq!(units(raw), expected, "{raw}");
    }
}

#[test]
fn equivalent_spellings_preserve_identical_utf16_sequences() {
    for raw in ["👍", r"\u{1f44d}", r"\ud83d\udc4d", r"\u{d83d}\udc4d"] {
        assert_eq!(units(raw), vec![0xd83d, 0xdc4d], "{raw}");
    }
}

#[test]
fn escaped_controls_and_line_continuations_preserve_code_units() {
    assert_eq!(
        units(r#"\n\r\t\b\f\v\0\x41\\\'\""#),
        vec![10, 13, 9, 8, 12, 11, 0, 65, 92, 39, 34]
    );
    for ending in ["\n", "\r", "\r\n", "\u{2028}", "\u{2029}"] {
        assert_eq!(units(&format!("a\\{ending}b")), vec![97, 98]);
    }
}

#[test]
fn scanner_accepts_string_code_units_but_rejects_invalid_escapes_and_identifiers() {
    for raw in [r"\ud800", r"\udfff", r"\u{d800}", r"\udc00\ud800"] {
        let source = format!("\"{raw}\"");
        let mut lexer = wake_ecma_lexer::Lexer::new(&source);
        let token = lexer.next(true);
        assert!(lexer.take_diagnostics().is_empty(), "{source}");
        assert_eq!(lexer.string_value(token.span), decode_escaped_value(raw));
    }
    for source in [
        r#""\u{z}""#,
        r#""\u{d800z}""#,
        r#""\u{}""#,
        r#""\u{110000}""#,
        r#""\u123z""#,
        r"const \ud800 = 1",
        r"const a\u{dc00} = 1",
    ] {
        assert!(!wake_ecma_lexer::tokenize(source).1.is_empty(), "{source}");
    }
}
