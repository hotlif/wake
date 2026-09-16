use wake_common::{Interner, JsString};
use wake_ecma_parser::{ParseOptions, SourceType, parse, parse_declaration_facts, parse_source};

#[test]
fn erased_modules_and_type_requests_preserve_code_units_without_runtime_dependencies() {
    let source = r#"
import type {A} from '\ud800';
export type {B} from '\ud801';
export type * from '\ud802';
export type T = import('\ud803').T;
declare module '\ud804' { export type Inner = import('\ud805').T; }
export type Keep = A;
"#;
    let interner = Interner::new();
    let output = parse_source(
        source,
        &interner,
        SourceType::TypeScript,
        ParseOptions::default(),
    );
    assert!(
        !output.parsed.has_errors(),
        "{:?}",
        output.parsed.diagnostics
    );
    let values: Vec<JsString> = vec![
        output.imports[0].source.as_ref().unwrap().value.clone(),
        output.exports[0].source.as_ref().unwrap().value.clone(),
        output.exports[1].source.as_ref().unwrap().value.clone(),
        output.type_imports[0].source.value.clone(),
        output.namespaces[0].ambient.as_ref().unwrap().value.clone(),
        output.type_imports[1].source.value.clone(),
    ];
    for (index, value) in values.iter().enumerate() {
        assert_eq!(
            value.code_units().collect::<Vec<_>>(),
            [0xd800 + index as u16]
        );
    }
    assert!(output.parsed.dependencies.is_empty());
    let ordinary = parse(source, &interner, SourceType::TypeScript);
    assert!(!ordinary.has_errors(), "{:?}", ordinary.diagnostics);
    assert!(ordinary.dependencies.is_empty());
    assert_eq!(
        ordinary.module.with_ast(wake_ecma_ast::structure_hash),
        output.parsed.module.with_ast(wake_ecma_ast::structure_hash)
    );
    let declarations = parse_declaration_facts(source, SourceType::TypeScript).unwrap();
    let values: Vec<JsString> = declarations
        .requests()
        .map(|request| request.specifier().into())
        .collect();
    for index in [0, 1, 2, 3, 5] {
        assert!(
            values.contains(&JsString::from_utf16(&[0xd800 + index])),
            "{values:?}"
        );
    }
}
