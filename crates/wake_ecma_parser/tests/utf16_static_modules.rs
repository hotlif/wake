use wake_common::{Interner, JsString};
use wake_ecma_ast::Statement;
use wake_ecma_parser::{SourceType, parse};

#[test]
fn static_module_requests_keep_distinct_code_unit_identities() {
    let source = r#"
import '\ud800';
import high from '\ud801';
export { high } from '\ud802';
export * from '\udfff';
import replacement from '\ufffd';
export * from '\ud83d\udc4d';
import '';
"#;
    let interner = Interner::new();
    let parsed = parse(source, &interner, SourceType::Module);
    assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);
    let dependencies: Vec<_> = parsed
        .dependencies
        .iter()
        .map(|dependency| dependency.specifier.clone())
        .collect();
    assert_eq!(
        dependencies,
        [
            JsString::from_utf16(&[0xd800]),
            JsString::from_utf16(&[0xd801]),
            JsString::from_utf16(&[0xd802]),
            JsString::from_utf16(&[0xdfff]),
            JsString::from("\u{fffd}"),
            JsString::from("👍"),
            JsString::from(""),
        ]
    );
}

#[test]
fn static_module_requests_keep_non_scalar_code_units() {
    let source = r#"
import '\ud800';
import value from '\udfff';
export { value } from '\ufffd';
export * from '\ud83d\udc4d';
"#;
    let interner = Interner::new();
    let parsed = parse(source, &interner, SourceType::Module);
    assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);
    assert_eq!(parsed.dependencies.len(), 4);
    let values = parsed.module.with_ast(|program| {
        program
            .body
            .iter()
            .filter_map(|statement| match statement {
                Statement::Import(import) => Some(import.source),
                Statement::ExportNamed(export) => export.source,
                Statement::ExportAll(export) => Some(export.source),
                _ => None,
            })
            .map(|source| interner.resolve_js(source))
            .collect::<Vec<_>>()
    });
    assert_eq!(
        values,
        [
            JsString::from_utf16(&[0xd800]),
            JsString::from_utf16(&[0xdfff]),
            JsString::from("\u{fffd}"),
            JsString::from("👍"),
        ]
    );
}
