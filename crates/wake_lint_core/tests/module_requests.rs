use wake_lint_core::{ModuleRequestKind as Kind, SourceType, inspect_module};

#[test]
fn surrogate_module_literals_are_known_requests_with_distinct_values() {
    let result = inspect_module(
        r#"type T=import('\ud800').T; import('\ud801'); require('\ufffd');"#,
        SourceType::TypeScript,
    );
    assert!(
        result.parse_diagnostics.is_empty(),
        "{:?}",
        result.parse_diagnostics
    );
    assert_eq!(result.requests.len(), 3);
    for (request, unit) in result.requests.iter().zip([0xd800, 0xd801, 0xfffd]) {
        let value: wake_common::JsString = request.specifier.as_ref().unwrap().into();
        assert_eq!(value.code_units().collect::<Vec<_>>(), [unit]);
    }
}

#[test]
fn literal_dynamic_attributes_preserve_code_units_and_last_property_wins() {
    let result = inspect_module(
        r#"import('m', {with:{"\ud800":"old","\ud801":"\ufffd","\u{d800}":"\udfff"}});"#,
        SourceType::Module,
    );
    assert!(result.parse_diagnostics.is_empty());
    assert!(result.requests[0].attributes_known);
    let entries = &result.requests[0].attributes;
    assert_eq!(entries.len(), 2);
    let key = &entries[0].0;
    let value = &entries[0].1;
    assert_eq!(key.code_units().collect::<Vec<_>>(), [0xd800]);
    assert_eq!(value.code_units().collect::<Vec<_>>(), [0xdfff]);
}

#[test]
fn original_static_imports_exports_and_equals_preserve_erased_type_edges() {
    let source = r#"import type { A } from './type'; import {type B} from './inline';
import {value, type C} from './mixed' with {type:'json'};
export type {D} from './export-type'; export {type E} from './export-inline'; export * from './export-value';
import type T = require('./equals-type'); import V = require('./equals-value');
const view = <div/>;
"#;
    let result = inspect_module(source, SourceType::Tsx);
    assert!(
        result.parse_diagnostics.is_empty(),
        "{:?}",
        result.parse_diagnostics
    );
    let requests = result.requests;
    assert_eq!(requests.len(), 8);
    assert_eq!(
        requests.iter().map(|r| r.type_only).collect::<Vec<_>>(),
        [true, true, false, true, true, false, true, false]
    );
    assert_eq!(requests[0].kind, Kind::Import);
    assert_eq!(requests[3].kind, Kind::Export);
    assert_eq!(requests[6].kind, Kind::ImportEquals);
    assert_eq!(requests[7].kind, Kind::ImportEquals);
    assert_eq!(requests[2].attributes, [("type".into(), "json".into())]);
    for request in requests {
        assert!(request.attributes_known);
        let raw = &source[request.specifier_span.lo as usize..request.specifier_span.hi as usize];
        assert_eq!(request.specifier.unwrap(), &raw[1..raw.len() - 1]);
    }
}

#[test]
fn require_uses_original_resolution_and_dynamic_requests_stay_explicit() {
    let source = r#"const first = require('./one'); function f(require) { require('./shadow'); }
function nested(){ return require('./two'); } const other = object.require('./object');
import('./dynamic', {with:{type:'json'}}); import(target); require(target);
import('./opaque', options);
"#;
    let result = inspect_module(source, SourceType::Module);
    assert!(result.parse_diagnostics.is_empty());
    assert_eq!(
        result
            .requests
            .iter()
            .map(|r| (
                r.kind,
                r.specifier.as_ref().and_then(wake_common::JsString::as_str)
            ))
            .collect::<Vec<_>>(),
        [
            (Kind::Require, Some("./one")),
            (Kind::Require, Some("./two")),
            (Kind::DynamicImport, Some("./dynamic")),
            (Kind::DynamicImport, None),
            (Kind::Require, None),
            (Kind::DynamicImport, Some("./opaque")),
        ]
    );
    assert_eq!(
        result.requests[2].attributes,
        [("type".into(), "json".into())]
    );
    assert!(!result.requests[5].attributes_known);
}

#[test]
fn dynamic_or_incomplete_scopes_do_not_prove_require_identity() {
    let with = inspect_module("with (scope) { require('./unknown'); }", SourceType::Script);
    assert!(with.parse_diagnostics.is_empty());
    assert!(with.requests.is_empty());
    assert!(with.incomplete);
    let eval = inspect_module("eval(code); require('./unknown');", SourceType::Script);
    assert!(eval.requests.is_empty());
    assert!(eval.incomplete);
    let ambient = inspect_module(
        "declare const require: any; require('./unknown');",
        SourceType::TypeScript,
    );
    assert!(ambient.requests.is_empty());
    assert!(ambient.incomplete);
    let ordinary = inspect_module(
        "function f(eval) { eval(code); } require('./known');",
        SourceType::Module,
    );
    assert_eq!(ordinary.requests.len(), 1);
    assert!(!ordinary.incomplete);
    let local = inspect_module(
        "declare module 'ambient' { const require: any; } function f(require) { require('./shadow'); }",
        SourceType::TypeScript,
    );
    assert!(local.requests.is_empty());
    assert!(!local.incomplete);
}

#[test]
fn parse_errors_never_publish_partial_module_requests_and_literals_are_decoded() {
    let bad = inspect_module("import './valid'; const broken = ;", SourceType::Module);
    assert!(!bad.parse_diagnostics.is_empty());
    assert!(bad.requests.is_empty());
    let source = "import './caf\\u00e9'; const text=\"require('fake')\"; // import('fake')\n";
    let result = inspect_module(source, SourceType::Module);
    assert_eq!(result.requests.len(), 1);
    assert_eq!(
        result.requests[0]
            .specifier
            .as_ref()
            .and_then(wake_common::JsString::as_str),
        Some("./café")
    );
}

#[test]
fn erased_import_type_expressions_are_separate_from_dynamic_runtime_requests() {
    let source = "type T = import('./types', {with:{'resolution-mode':'require'}}).T; const value = import('./runtime');";
    let result = inspect_module(source, SourceType::TypeScript);
    assert!(result.parse_diagnostics.is_empty());
    assert_eq!(result.requests.len(), 2);
    assert_eq!(result.requests[0].kind, Kind::TypeImport);
    assert!(result.requests[0].type_only);
    assert_eq!(
        result.requests[0].attributes,
        [("resolution-mode".into(), "require".into())]
    );
    assert_eq!(result.requests[1].kind, Kind::DynamicImport);
    assert!(!result.requests[1].type_only);
}
