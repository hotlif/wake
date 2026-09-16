use wake_common::Interner;
use wake_ecma_parser::{ParseOptions, SourceType, parse, parse_source};

#[test]
fn import_type_requests_keep_original_attributes_and_do_not_change_compilation() {
    let source = r#"type A = import('./t\u0079pe', {with:{'resolution-mode':'require'}}).A;
type B = typeof import('./value');
function f(value: import('./parameter').P): import('./return').R { return value; }
const view = <div/>;"#;
    let interner = Interner::new();
    let parsed = parse_source(source, &interner, SourceType::Tsx, ParseOptions::default());
    assert!(
        !parsed.parsed.has_errors(),
        "{:?}",
        parsed.parsed.diagnostics
    );
    assert_eq!(
        parsed
            .type_imports
            .iter()
            .map(|import| import.source.value.as_str().unwrap())
            .collect::<Vec<_>>(),
        ["./type", "./value", "./parameter", "./return"]
    );
    assert!(
        parsed
            .type_imports
            .iter()
            .all(|import| import.attributes_known)
    );
    assert_eq!(parsed.type_imports[0].attributes[0].key, "resolution-mode");
    assert_eq!(parsed.type_imports[0].attributes[0].value, "require");
    assert_eq!(
        &source[parsed.type_imports[0].source.span.lo as usize
            ..parsed.type_imports[0].source.span.hi as usize],
        "'./t\\u0079pe'"
    );
    let ordinary = parse(source, &interner, SourceType::Tsx);
    assert!(!ordinary.has_errors());
    assert_eq!(
        parsed.parsed.module.structure_hash(),
        ordinary.module.structure_hash()
    );
}

#[test]
fn speculative_type_imports_are_not_duplicated_or_confused_with_runtime_imports() {
    let source = r#"const arrow = <T extends import('./constraint').T,>(value: T) => value;
const callback = (value: import('./parameter').P) => value;
const result = factory<import('./argument').A>(value);
const comparison = lhs < import('./runtime');
type Unknown = import('./unknown', {other:{mode:'custom'}}).T;
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
    assert_eq!(
        parsed
            .type_imports
            .iter()
            .map(|import| import.source.value.as_str().unwrap())
            .collect::<Vec<_>>(),
        ["./constraint", "./parameter", "./argument", "./unknown"]
    );
    assert!(!parsed.type_imports[3].attributes_known);
}

#[test]
fn source_type_imports_require_literal_modules_and_valid_attribute_values() {
    for source in [
        "type T = import();",
        "type T = import(path);",
        "type T = import;",
        "type T = import('./x', {with:{type: 1}});",
    ] {
        let interner = Interner::new();
        let parsed = parse_source(
            source,
            &interner,
            SourceType::TypeScript,
            ParseOptions::default(),
        );
        assert!(parsed.parsed.has_errors(), "{source}");
    }
}
