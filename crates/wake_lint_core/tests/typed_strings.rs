use std::sync::Arc;
use wake_common::JsString;
use wake_lint_core::{
    LintOptions, RuleLevel, SourceType, TypeId, TypeKind, TypeLiteral, TypeNode, TypeSource,
    TypedSource,
};

#[test]
fn string_type_values_decode_exact_source_without_accepting_type_names_or_expressions() {
    for (source, units) in [
        (r#""\ud800""#, vec![0xd800]),
        (r#""\udfff""#, vec![0xdfff]),
        (r#""\ud83d\udc4d""#, vec![0xd83d, 0xdc4d]),
        (r#""\\ud800""#, r"\ud800".encode_utf16().collect()),
        (r#""\ufffd\ufffd\ufffd""#, vec![0xfffd; 3]),
        (r#""\0\n\t\x41""#, vec![0, 10, 9, 65]),
        ("'👍'", vec![0xd83d, 0xdc4d]),
    ] {
        assert_eq!(
            TypeLiteral::from_string_source(source).unwrap(),
            TypeLiteral::String(JsString::from_utf16(&units))
        );
    }
    for source in [
        "",
        "E.Member",
        "string",
        r#""a" | "b""#,
        r#""a"; "b""#,
        r#""a"+"b""#,
        r#""\u{}""#,
        r#""\x0""#,
        r#""a";"#,
        r#" "a""#,
        r#"("a")"#,
        r#"/* comment */"a""#,
        r#""truncated"#,
    ] {
        assert!(TypeLiteral::from_string_source(source).is_err(), "{source}");
    }
}

#[test]
fn exhaustive_switch_matches_source_code_units_without_case_type_fallback() {
    let node = |units: &[u16]| TypeNode {
        kind: TypeKind::String,
        literal: Some(TypeLiteral::String(JsString::from_utf16(units))),
        ..Default::default()
    };
    for (source, expected) in [
        (
            r#"switch(value){case "\ud800":break;case "\udfff":break;case "\ufffd\ufffd\ufffd":break;}"#,
            0,
        ),
        (
            r#"switch(value){case "\ud800":break;case "\ufffd\ufffd\ufffd":break;}"#,
            1,
        ),
    ] {
        let input = TypeSource::new("a.ts", Arc::from(source), SourceType::TypeScript).unwrap();
        let typed = TypedSource::new(
            input,
            vec![
                node(&[0xd800]),
                node(&[0xdfff]),
                node(&[0xfffd; 3]),
                TypeNode {
                    kind: TypeKind::Union,
                    parts: vec![TypeId(0), TypeId(1), TypeId(2)],
                    ..Default::default()
                },
            ],
            vec![],
        )
        .unwrap()
        .with_switch_types(vec![TypeId(3)])
        .unwrap();
        let result = typed
            .lint(&LintOptions {
                recommended: false,
                rules: [(
                    "ts/switch-exhaustiveness-check".into(),
                    RuleLevel::Error.into(),
                )]
                .into(),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(result.diagnostics.len(), expected, "{source}");
    }
}
