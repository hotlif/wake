use wake_lint_core::{LintOptions, SourceType, lint_text};

#[test]
fn erased_value_declarations_are_not_misclassified_as_host_globals() {
    let options: LintOptions =
        serde_json::from_value(serde_json::json!({"recommended":false,"rules":{
            "js/no-console":"error", "js/no-async-promise-executor":"error",
            "js/no-promise-executor-return":"error", "js/use-isnan":"error"
        }}))
        .unwrap();
    let source = "declare const console: {log(value: string): void}; declare class Promise { constructor(executor: any); } declare const NaN: number; console.log('local'); new Promise(async () => 1); value == NaN;";
    let result = lint_text(source, SourceType::TypeScript, &options).unwrap();
    assert!(
        result.parse_diagnostics.is_empty(),
        "{:?}",
        result.parse_diagnostics
    );
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
}

#[test]
fn strict_blocks_and_class_static_blocks_do_not_hide_outer_globals() {
    let options: LintOptions =
        serde_json::from_value(serde_json::json!({"recommended":false,"rules":{
            "js/no-console":"error", "js/no-async-promise-executor":"error"
        }}))
        .unwrap();
    for source in [
        "{ console.log('local'); function console() {} } console.log('global');",
        "class C { static { console.log('local'); var console; } } console.log('global');",
        "class C { static { new Promise(async () => {}); var Promise; } } new Promise(async () => {});",
    ] {
        let result = lint_text(source, SourceType::Module, &options).unwrap();
        assert!(
            result.parse_diagnostics.is_empty(),
            "{source}: {:?}",
            result.parse_diagnostics
        );
        assert_eq!(
            result.diagnostics.len(),
            1,
            "{source}: {:?}",
            result.diagnostics
        );
        let expected = source
            .rfind("console.log")
            .or_else(|| source.rfind("async"))
            .unwrap();
        assert_eq!(
            result.diagnostics[0].start as usize, expected,
            "only the final global occurrence should be reported: {source}: {:?}",
            result.diagnostics
        );
    }
}

#[test]
fn global_rules_respect_shadowing_and_exclude_nested_executor_functions() {
    let options: LintOptions = serde_json::from_value(serde_json::json!({"recommended":false,"rules":{
        "js/no-console":"error", "js/no-async-promise-executor":"error", "js/no-promise-executor-return":"error"
    }})).unwrap();
    let source = "console.log('x'); new Promise(async (resolve) => { function nested() { return 1; } return resolve(1); }); new Promise(() => 1); new Promise(() => void run()); function local(console, Promise) { console.log('x'); new Promise(async () => 1); }";
    let result = lint_text(source, SourceType::Tsx, &options).unwrap();
    assert!(
        result.parse_diagnostics.is_empty(),
        "{:?}",
        result.parse_diagnostics
    );
    let ids: Vec<_> = result
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.rule_id.as_str())
        .collect();
    assert_eq!(
        ids,
        [
            "js/no-console",
            "js/no-async-promise-executor",
            "js/no-promise-executor-return",
            "js/no-promise-executor-return"
        ]
    );
    assert!(lint_text("import { Promise, console } from 'local'; console.log('x'); new Promise(async () => 1);", SourceType::Module, &options).unwrap().diagnostics.is_empty());
}

#[test]
fn globals_use_lexical_identity_in_classes_catches_and_hoisted_scopes() {
    let options: LintOptions = serde_json::from_value(serde_json::json!({"recommended":false,"rules":{
        "js/no-console":"error", "js/no-async-promise-executor":"error", "js/no-promise-executor-return":"error"
    }})).unwrap();
    let valid = [
        "const C = class Promise { method() { new Promise(async () => 1); } };",
        "{ console.log('x'); let console; }",
        "function f() { new Promise(async () => 1); var Promise; }",
        "try {} catch (console) { console.log('x'); }",
        "new Promise(function () { const nested = () => 1; class C { method() { return 1; } } return; });",
        "new Promise(() => { if (ok) return void run(); });",
    ];
    for source in valid {
        let result = lint_text(source, SourceType::Module, &options).unwrap();
        assert!(
            result.parse_diagnostics.is_empty(),
            "{source}: {:?}",
            result.parse_diagnostics
        );
        assert!(
            result.diagnostics.is_empty(),
            "{source}: {:?}",
            result.diagnostics
        );
    }
    let source = "console?.['log']('x'); new Promise(function () { if (ok) return 1; try { run(); } finally { return 2; } });";
    let result = lint_text(source, SourceType::Module, &options).unwrap();
    assert_eq!(result.diagnostics.len(), 3, "{:?}", result);
    for diagnostic in result.diagnostics {
        assert!(source.is_char_boundary(diagnostic.start as usize));
        assert!(source.is_char_boundary(diagnostic.end as usize));
        assert!(diagnostic.fix.is_none());
    }
}
