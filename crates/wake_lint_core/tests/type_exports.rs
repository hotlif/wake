use wake_lint_core::{LintOptions, SourceType, lint_text};

fn options() -> LintOptions {
    serde_json::from_value(serde_json::json!({"recommended":false,"rules":{
        "ts/consistent-type-exports":"error"
    }}))
    .unwrap()
}

#[test]
fn local_type_exports_distinguish_dual_bindings_and_import_kinds() {
    let source = r#"interface Shape {} type Alias = Shape;
    import type { Imported } from 'types'; import { Unknown } from 'unknown';
    const Dual = 1; type Dual = string;
    declare const Ambient: number; type Ambient = number;
    class Class {} enum Enum { A } namespace Namespace { export interface Member {} }
    export { Shape, Alias as Renamed, Imported, Unknown, Dual, Ambient, Class, Enum, Namespace };
    export type { Shape as TypeShape }; export { type Alias as TypeAlias };
    export { External } from 'external'; export * from 'star';"#;
    let result = lint_text(source, SourceType::TypeScript, &options()).unwrap();
    assert!(
        result.parse_diagnostics.is_empty(),
        "{:?}",
        result.parse_diagnostics
    );
    assert_eq!(
        result
            .diagnostics
            .iter()
            .map(|d| &source[d.start as usize..d.end as usize])
            .collect::<Vec<_>>(),
        ["Shape", "Alias", "Imported"]
    );
    assert!(
        result
            .diagnostics
            .iter()
            .all(|d| d.message_id == "type" && d.fix.is_none())
    );
}

#[test]
fn export_rule_defaults_suppression_and_closed_options_are_shared() {
    let source = "type T = string; export { T };";
    assert!(
        lint_text(source, SourceType::TypeScript, &LintOptions::default())
            .unwrap()
            .diagnostics
            .is_empty()
    );
    let suppressed = "type T = string;\n// wake-lint-disable-next-line ts/consistent-type-exports\nexport { T };";
    assert!(
        lint_text(suppressed, SourceType::TypeScript, &options())
            .unwrap()
            .diagnostics
            .is_empty()
    );
    let invalid: LintOptions = serde_json::from_value(serde_json::json!({"rules":{
        "ts/consistent-type-exports":{"level":"off","options":{"fixMixedExportsWithInlineTypeSpecifier":true}}
    }})).unwrap();
    assert!(lint_text("", SourceType::TypeScript, &invalid).is_err());
}

#[test]
fn incomplete_ambient_value_names_do_not_hide_a_local_type_export() {
    let source =
        "declare module 'ambient' { const value: number; } type value = string; export { value };";
    let result = lint_text(source, SourceType::TypeScript, &options()).unwrap();
    assert!(
        result.parse_diagnostics.is_empty(),
        "{:?}",
        result.parse_diagnostics
    );
    assert_eq!(
        result
            .diagnostics
            .iter()
            .map(|diagnostic| &source[diagnostic.start as usize..diagnostic.end as usize])
            .collect::<Vec<_>>(),
        ["value"]
    );
}
