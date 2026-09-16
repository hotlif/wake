use wake_lint_core::{LintOptions, SourceType, lint_text};

fn options() -> LintOptions {
    serde_json::from_value(serde_json::json!({"recommended":false,"rules":{
        "js/no-redeclare":"error"
    }}))
    .unwrap()
}

#[test]
fn redeclarations_use_lexical_scope_and_original_declaration_occurrences() {
    let source = "var repeated; var repeated; function f(param){var param; var local; var local;} { let scoped; } { let scoped; } function g(arg=1){var arg;} function same(){} function same(){}";
    let result = lint_text(source, SourceType::Module, &options()).unwrap();
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
        ["repeated", "param", "local", "same"]
    );
    assert!(
        result
            .diagnostics
            .iter()
            .all(|d| d.message_id == "duplicate" && d.fix.is_none())
    );
}

#[test]
fn type_declaration_merging_and_compiler_helpers_do_not_create_redeclarations() {
    let source = r#"class C {} namespace C { export interface Member {} }
    function F() {} namespace F { export const x = 1 }
    enum E { A } enum E { B = 1 } namespace E { export type Member = string }
    namespace N { export const a = 1 } namespace N { export const b = 2 }
    interface Shape {} interface Shape {} type ShapeAlias = Shape;
    function overload(x: string): string; function overload(x: string) { return x }
    const jsx = <><Widget /><Widget /></>;
    var real; var real;"#;
    let result = lint_text(source, SourceType::Tsx, &options()).unwrap();
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
        ["real"]
    );
}

#[test]
fn erased_namespace_merges_with_erased_class_function_and_enum() {
    let source = r#"declare class AmbientClass {}
    declare namespace AmbientClass { export interface Member {} }
    declare function AmbientFunction(): void;
    declare namespace AmbientFunction { export interface Member {} }
    declare enum AmbientEnum { A }
    declare namespace AmbientEnum { export interface Member {} }"#;
    let result = lint_text(source, SourceType::TypeScript, &options()).unwrap();
    assert!(
        result.parse_diagnostics.is_empty(),
        "{:?}",
        result.parse_diagnostics
    );
    assert!(
        result.diagnostics.is_empty(),
        "unexpected diagnostics: {:?}",
        result.diagnostics
    );
}

#[test]
fn duplicate_erased_value_declarations_keep_both_occurrences() {
    let source = "declare const erased: number;\ndeclare const erased: number;";
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
        ["erased"]
    );
}

#[test]
fn implicit_bindings_and_sloppy_block_functions_are_distinct_from_explicit_duplicates() {
    let source = "function f(){var arguments;} {function legacy(){}} {function legacy(){}} var legacy; function repeated(a,a){}";
    let result = lint_text(source, SourceType::Script, &options()).unwrap();
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
        ["a"]
    );
    let suppressed = "var x;\n// wake-lint-disable-next-line js/no-redeclare\nvar x;";
    assert!(
        lint_text(suppressed, SourceType::Module, &options())
            .unwrap()
            .diagnostics
            .is_empty()
    );
}
