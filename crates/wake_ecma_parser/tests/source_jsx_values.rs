use wake_common::Interner;
use wake_ecma_ast::{SourceNodeKind as Kind, SourcePrimitiveValue as Value};
use wake_ecma_parser::{ParseOptions, SourceType, parse, parse_source};

#[test]
fn jsx_attribute_strings_keep_raw_newlines_backslashes_and_decoded_entities() {
    let source = r#"const view=<div title="first
 second &amp; 名" path="C:\bad\" />;"#;
    let interner = Interner::new();
    let parsed = parse_source(source, &interner, SourceType::Tsx, ParseOptions::default());
    assert!(
        !parsed.parsed.has_errors(),
        "{:?}",
        parsed.parsed.diagnostics
    );
    assert_eq!(
        parsed
            .jsx_values
            .iter()
            .map(|value| value.value.clone())
            .collect::<Vec<_>>(),
        [
            Value::String("first\n second & 名".into()),
            Value::String("C:\\bad\\".into())
        ]
    );
    let ordinary = parse(source, &interner, SourceType::Tsx);
    assert_eq!(
        ordinary.module.structure_hash(),
        parsed.parsed.module.structure_hash()
    );
    assert!(parsed.comments.is_empty());
}

#[test]
fn jsx_values_preserve_decoding_primitives_and_dynamic_expressions() {
    let source = r#"const el = <input disabled aria-label="a&amp;b" tabIndex={-1} hidden={false} title={`plain`} alt={undefined} value={null} other={void run()} {...props} />;"#;
    let interner = Interner::new();
    let parsed = parse_source(source, &interner, SourceType::Tsx, ParseOptions::default());
    assert!(
        !parsed.parsed.has_errors(),
        "{:?}",
        parsed.parsed.diagnostics
    );
    let values = parsed
        .jsx_values
        .iter()
        .map(|value| value.value.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        values,
        [
            Value::Boolean(true),
            Value::String("a&b".into()),
            Value::Number(-1.0),
            Value::Boolean(false),
            Value::String("plain".into()),
            Value::Unknown,
            Value::Null,
            Value::Undefined,
            Value::Unknown
        ]
    );
    assert!(parsed.jsx_values[0].expression.is_none());
    for value in &parsed.jsx_values[1..] {
        assert!(value.expression.is_some());
        let node = &parsed.syntax[value.node];
        assert!(matches!(
            node.kind,
            Kind::JsxAttribute | Kind::JsxSpreadAttribute
        ));
        let expression = value.expression.unwrap();
        assert!(expression.lo >= node.span.lo && expression.hi <= node.span.hi);
    }
    assert_eq!(
        &source[parsed.jsx_values[2].expression.unwrap().lo as usize
            ..parsed.jsx_values[2].expression.unwrap().hi as usize],
        "-1"
    );
    let ordinary = parse(source, &interner, SourceType::Tsx);
    assert_eq!(
        ordinary.module.structure_hash(),
        parsed.parsed.module.structure_hash()
    );
    assert_eq!(
        format!("{:?}", ordinary.dependencies),
        format!("{:?}", parsed.parsed.dependencies)
    );
}

#[test]
fn jsx_child_values_distinguish_empty_containers_normalized_text_and_unknowns() {
    let source = "const el=<div>\n  Hello &amp;\n  world\n  {/* comment */}{null}{false}{0}{'text'}{value}<span />\n</div>; const arrow=<T,>(value:T)=>value;";
    let interner = Interner::new();
    let parsed = parse_source(source, &interner, SourceType::Tsx, ParseOptions::default());
    assert!(
        !parsed.parsed.has_errors(),
        "{:?}",
        parsed.parsed.diagnostics
    );
    assert_eq!(
        parsed
            .jsx_values
            .iter()
            .map(|value| value.value.clone())
            .collect::<Vec<_>>(),
        [
            Value::String("Hello & world".into()),
            Value::Empty,
            Value::Null,
            Value::Boolean(false),
            Value::Number(0.0),
            Value::String("text".into()),
            Value::Unknown,
            Value::Empty
        ]
    );
    for value in &parsed.jsx_values {
        let kind = parsed.syntax[value.node].kind;
        assert!(matches!(kind, Kind::JsxText | Kind::JsxExpressionContainer));
        if kind == Kind::JsxText || value.value == Value::Empty {
            assert!(value.expression.is_none());
        }
    }
    let ordinary = parse(source, &interner, SourceType::Tsx);
    assert_eq!(
        ordinary.module.structure_hash(),
        parsed.parsed.module.structure_hash()
    );
    assert_eq!(
        format!("{:?}", ordinary.dependencies),
        format!("{:?}", parsed.parsed.dependencies)
    );
}
