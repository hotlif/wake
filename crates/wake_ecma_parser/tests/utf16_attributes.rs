use wake_common::{Interner, JsString};
use wake_ecma_parser::{ParseOptions, SourceType, parse, parse_source};

#[test]
fn attribute_strings_keep_code_units_across_source_forms() {
    let source = r#"
import value from 'first' with { "\ud800": "\udfff", type: "\ufffd", "👍": "\0" };
export { value } from 'second' with { "\ud801": "\udffe" };
export * from 'third' with { "\ud802": "\udffd" };
import type Type from 'fourth' with { "\ud803": "\udffc" };
type T = import('fifth', {with:{"\ud804":"\udffb"}}).T;
"#;
    let interner = Interner::new();
    let parsed = parse_source(
        source,
        &interner,
        SourceType::TypeScript,
        ParseOptions::default(),
    );
    assert!(
        !parsed.parsed.has_errors(),
        "{:?}",
        parsed.parsed.diagnostics
    );
    let attributes = [
        &parsed.imports[0].attributes.as_ref().unwrap().entries,
        &parsed.exports[0].attributes.as_ref().unwrap().entries,
        &parsed.exports[1].attributes.as_ref().unwrap().entries,
        &parsed.imports[1].attributes.as_ref().unwrap().entries,
        &parsed.type_imports[0].attributes,
    ];
    for (index, entries) in attributes.into_iter().enumerate() {
        let key: &JsString = &entries[0].key;
        let value: &JsString = &entries[0].value;
        assert_eq!(
            key.code_units().collect::<Vec<_>>(),
            [0xd800 + index as u16]
        );
        assert_eq!(
            value.code_units().collect::<Vec<_>>(),
            [0xdfff - index as u16]
        );
    }
    let ordinary = parse(source, &interner, SourceType::TypeScript);
    assert!(!ordinary.has_errors(), "{:?}", ordinary.diagnostics);
    assert_eq!(
        parsed.parsed.module.with_ast(wake_ecma_ast::structure_hash),
        ordinary.module.with_ast(wake_ecma_ast::structure_hash)
    );
}

#[test]
fn string_attribute_keys_do_not_relax_module_export_names() {
    for source in [
        r#"import {"\ud800" as value} from 'm';"#,
        r#"export {value as "\ud800"} from 'm';"#,
    ] {
        assert!(parse(source, &Interner::new(), SourceType::Module).has_errors());
    }
}

#[test]
fn static_duplicate_attribute_keys_compare_decoded_code_units() {
    for attributes in [
        r#"type:'a',"type":'b'"#,
        r#""\ud800":'a',"\u{d800}":'b'"#,
        r#""👍":'a',"\ud83d\udc4d":'b'"#,
    ] {
        for prefix in ["import 'm'", "export * from 'm'", "export {value} from 'm'"] {
            let source = format!("{prefix} with {{{attributes}}};");
            let output = parse(&source, &Interner::new(), SourceType::Module);
            assert!(output.has_errors(), "duplicate key accepted: {source}");
        }
    }
    let output = parse(
        r#"import 'm' with {"\ud800":'a',"\ud801":'b',"\ufffd":'c'};"#,
        &Interner::new(),
        SourceType::Module,
    );
    assert!(!output.has_errors(), "{:?}", output.diagnostics);
}
