use std::sync::Arc;

use wake_lint_core::{LintOptions, ModuleFile, ModuleGraph, ModuleId, SourceType, lint_text};

fn options(typeof_check: bool) -> LintOptions {
    serde_json::from_value(serde_json::json!({"recommended":false,"rules":{
        "js/no-undef":{"level":"error","options":{"typeof":typeof_check}},
        "react/jsx-no-undef":"warn"
    }}))
    .unwrap()
}

fn names(source: &str, language: SourceType, options: &LintOptions) -> Vec<(String, String)> {
    let result = lint_text(source, language, options).unwrap();
    assert!(
        result.parse_diagnostics.is_empty(),
        "{:?}",
        result.parse_diagnostics
    );
    assert!(
        result
            .diagnostics
            .iter()
            .all(|d| d.fix.is_none() && d.message_id == "undefined")
    );
    result
        .diagnostics
        .iter()
        .map(|d| {
            (
                d.rule_id.clone(),
                source[d.start as usize..d.end as usize].into(),
            )
        })
        .collect()
}

#[test]
fn undefined_values_use_original_reads_writes_scopes_and_explicit_globals() {
    let source = "const bound=1; function f(arg){return ()=>bound+arg+arguments.length;} bound; missing; written=1; incremented++; const object={property:bound, shorthand}; object.member; Promise.resolve(Math.PI); console.log(injected);";
    let mut options = options(false);
    options
        .globals
        .insert("injected".into(), wake_lint_core::GlobalMode::Writable);
    let found = names(source, SourceType::Module, &options);
    assert_eq!(
        found
            .iter()
            .map(|(_, name)| name.as_str())
            .collect::<Vec<_>>(),
        ["missing", "written", "incremented", "shorthand", "console"]
    );
    options
        .globals
        .insert("Promise".into(), wake_lint_core::GlobalMode::Off);
    assert_eq!(
        names("Promise;", SourceType::Module, &options)[0].1,
        "Promise"
    );
    assert!(
        names(
            "// wake-lint-disable-next-line js/no-undef\nmissing;",
            SourceType::Module,
            &options
        )
        .is_empty()
    );
}

#[test]
fn typeof_exemption_is_direct_and_does_not_hide_nested_undefined_values() {
    let source = "typeof missing; typeof (parenthesized); typeof object.member; typeof (first + second); typeof call();";
    assert_eq!(
        names(source, SourceType::Module, &options(false))
            .iter()
            .map(|(_, name)| name.as_str())
            .collect::<Vec<_>>(),
        ["object", "first", "second", "call"]
    );
    assert_eq!(names(source, SourceType::Module, &options(true)).len(), 6);
    let invalid: LintOptions = serde_json::from_value(
        serde_json::json!({"rules":{"js/no-undef":{"level":"off","options":{"typeof":"yes"}}}}),
    )
    .unwrap();
    assert!(lint_text("", SourceType::Module, &invalid).is_err());
}

#[test]
fn jsx_checks_component_roots_once_and_separates_runtime_expressions_from_tags() {
    let source = "import { Known } from 'ui'; const UI={}; const node=<><Known /><UI.Panel /><Missing child={value}></Missing><missing.part /><div title='notReference' /><custom-element /><svg:path /></>;";
    assert_eq!(
        names(source, SourceType::Tsx, &options(false)),
        [
            ("react/jsx-no-undef".into(), "Missing".into()),
            ("js/no-undef".into(), "value".into()),
            ("react/jsx-no-undef".into(), "missing".into()),
        ]
    );
    let mut configured = options(false);
    configured
        .globals
        .insert("External".into(), wake_lint_core::GlobalMode::Readonly);
    assert!(names("const el=<External/>;", SourceType::Tsx, &configured).is_empty());
}

#[test]
fn type_references_queries_erased_values_and_generated_jsx_helpers_do_not_become_undefined() {
    let source = "interface Shape {} type Query=typeof unknownQuery; declare const ambient:Shape; declare namespace API { const value:number; } const use:UnknownType=ambient; const runtime=ambient; const api=API; export {use}; const el=<div/>; realMissing;";
    assert_eq!(
        names(source, SourceType::Tsx, &options(false)),
        [("js/no-undef".into(), "realMissing".into())]
    );
}

#[test]
fn runtime_namespace_erased_values_follow_their_iife_scope() {
    let source =
        "namespace Box { declare const value: number; const copy = value; } const outside = value;";
    assert_eq!(
        names(source, SourceType::TypeScript, &options(false)),
        [("js/no-undef".into(), "value".into())]
    );
}

#[test]
fn nested_ambient_namespace_inside_runtime_namespace_does_not_leak_to_module_root() {
    let source = "namespace Outer { declare namespace Inner { const value: number; } const copy = Inner.value; } const outside = Inner.value;";
    assert_eq!(
        names(source, SourceType::TypeScript, &options(false)),
        [("js/no-undef".into(), "Inner".into())]
    );
}

#[test]
fn global_namespace_does_not_merge_with_same_name_module_namespace() {
    let source = "declare global { namespace Outer { const globalValue: number; } } namespace Outer { const copy = globalValue; }";
    assert_eq!(
        names(source, SourceType::TypeScript, &options(false)),
        [("js/no-undef".into(), "globalValue".into())]
    );
}

#[test]
fn project_graph_projects_global_ambient_values_across_files_without_namespace_leaks() {
    let declarations = ModuleFile::new(
        "declarations".into(),
        "globals.d.ts".into(),
        Arc::from(
            "export {}; declare global { function shared(argument: number): void; const sharedValue: number; namespace Shared { const value: number; } namespace Hidden { const value: number; } }",
        ),
        SourceType::TypeScript,
    )
    .unwrap();
    let consumer = ModuleFile::new(
        "consumer".into(),
        "consumer.ts".into(),
        Arc::from(
            "const result = shared(argument); const copy = sharedValue + Shared.value; value;",
        ),
        SourceType::TypeScript,
    )
    .unwrap();
    let script_declarations = ModuleFile::new(
        "script-declarations".into(),
        "script.d.ts".into(),
        Arc::from("declare const scriptGlobal: number;"),
        SourceType::TypeScript,
    )
    .unwrap();
    let graph = ModuleGraph::new(vec![declarations, script_declarations, consumer]).unwrap();
    let result = graph.lint(ModuleId(2), &options(false)).unwrap();
    assert_eq!(result.diagnostics.len(), 2, "{:?}", result.diagnostics);
    assert!(
        result
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.rule_id == "js/no-undef")
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message == "'argument' is not defined.")
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message == "'value' is not defined.")
    );
}
