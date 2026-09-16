use wake_lint_core::{LintOptions, SourceType, lint_text};

fn options(parameters: serde_json::Value, jsx: bool) -> LintOptions {
    serde_json::from_value(serde_json::json!({"recommended":false,"rules":{
        "js/no-unused-vars":{"level":"warn","options":parameters},
        "react/jsx-uses-vars":if jsx {"warn"} else {"off"}
    }}))
    .unwrap()
}

fn names(source: &str, language: SourceType, options: &LintOptions) -> Vec<String> {
    let result = lint_text(source, language, options).unwrap();
    assert!(
        result.parse_diagnostics.is_empty(),
        "{:?}",
        result.parse_diagnostics
    );
    assert!(result.diagnostics.iter().all(|d| d.fix.is_none()));
    result
        .diagnostics
        .iter()
        .map(|d| source[d.start as usize..d.end as usize].into())
        .collect()
}

#[test]
fn unused_values_distinguish_reads_writes_exports_and_self_recursion() {
    let source = "const unused=1; let written; written=2; let incremented=0; incremented++; const read=1; use(read); function recurse(){return recurse();} const recur=()=>recur(); export const exposed=1; const named=2; export {named};";
    assert_eq!(
        names(
            source,
            SourceType::Module,
            &options(serde_json::json!({}), false)
        ),
        ["unused", "written", "incremented", "recurse", "recur"]
    );
    assert!(
        names(
            "let n=0; consume(n++);",
            SourceType::Module,
            &options(serde_json::json!({}), false)
        )
        .is_empty()
    );
}

#[test]
fn unused_parameters_respect_positions_catch_settings_and_local_bindings() {
    let source = "export function f(first,used,last){use(used); let local=1;} try{work();}catch(error){work();} const top=1;";
    assert_eq!(
        names(
            source,
            SourceType::Module,
            &options(serde_json::json!({}), false)
        ),
        ["last", "local", "error", "top"]
    );
    assert_eq!(
        names(
            source,
            SourceType::Module,
            &options(serde_json::json!({"args":"all"}), false)
        ),
        ["first", "last", "local", "error", "top"]
    );
    assert_eq!(
        names(
            source,
            SourceType::Module,
            &options(
                serde_json::json!({"args":"none","caught_errors":"none","vars":"local"}),
                false
            )
        ),
        ["local"]
    );
}

#[test]
fn patterns_can_ignore_bindings_and_report_ignored_names_that_are_used() {
    let source = "const _unused=1; const _used=2; use(_used); export function f(_arg){return _arg;} try{work();}catch(_error){}";
    let parameters = serde_json::json!({"vars_ignore_pattern":"^_","args_ignore_pattern":"^_","caught_errors_ignore_pattern":"^_"});
    assert!(
        names(
            source,
            SourceType::Module,
            &options(parameters.clone(), false)
        )
        .is_empty()
    );
    let mut strict = parameters;
    strict["report_used_ignore_pattern"] = true.into();
    assert_eq!(
        names(source, SourceType::Module, &options(strict, false)),
        ["_used", "_arg"]
    );
}

#[test]
fn rest_siblings_and_lexical_shadowing_are_resolved_by_declaration_identity() {
    let source = "const { omitted, kept, ...rest }=input; use(kept,rest); const outer=1; function f(){const outer=2; use(outer);} use(f);";
    assert_eq!(
        names(
            source,
            SourceType::Module,
            &options(serde_json::json!({}), false)
        ),
        ["omitted", "outer"]
    );
    assert_eq!(
        names(
            source,
            SourceType::Module,
            &options(serde_json::json!({"ignore_rest_siblings":true}), false)
        ),
        ["outer"]
    );
}

#[test]
fn jsx_usage_marks_only_resolved_component_roots_without_own_diagnostics() {
    let source = "import { Component, Other } from 'ui'; const UI={}; export const el=<><Component/><UI.Child/></>;";
    assert_eq!(
        names(
            source,
            SourceType::Tsx,
            &options(serde_json::json!({}), false)
        ),
        ["Component", "Other", "UI"]
    );
    assert_eq!(
        names(
            source,
            SourceType::Tsx,
            &options(serde_json::json!({}), true)
        ),
        ["Other"]
    );
    let only: LintOptions = serde_json::from_value(
        serde_json::json!({"recommended":false,"rules":{"react/jsx-uses-vars":"warn"}}),
    )
    .unwrap();
    assert!(
        names(
            "const unused=1; const el=<Missing/>;",
            SourceType::Tsx,
            &only
        )
        .is_empty()
    );
}

#[test]
fn type_uses_exports_and_queries_respect_separate_namespaces_and_self_references() {
    let source = "import type { Used, Unused } from 'types'; export type Public=Used; type Private={next:Private}; const Dual=1; type Dual=string; export const value:Dual='x'; const queried=1; export type Query=typeof queried; export function f<T,U>(x:T):T{return x;}";
    assert_eq!(
        names(
            source,
            SourceType::TypeScript,
            &options(serde_json::json!({}), false)
        ),
        ["Unused", "Private", "Dual", "U"]
    );
}

#[test]
fn ambient_and_implicit_bindings_are_not_invented_and_suppression_remains_exact() {
    let source = "declare const injected:number; declare namespace External { interface Public {} } export function f(){return arguments.length;} // wake-lint-disable-next-line js/no-unused-vars\nconst suppressed=1;";
    assert!(
        names(
            source,
            SourceType::TypeScript,
            &options(serde_json::json!({}), false)
        )
        .is_empty()
    );
}

#[test]
fn string_ambient_names_do_not_hide_a_local_unused_binding() {
    let source = "declare module 'ambient' { const value:number; } const value=1;";
    assert_eq!(
        names(
            source,
            SourceType::TypeScript,
            &options(serde_json::json!({}), false)
        ),
        ["value"]
    );
    assert_eq!(
        names(
            "declare module 'ambient' { const value:number; } type value = string;",
            SourceType::TypeScript,
            &options(serde_json::json!({}), false),
        ),
        ["value"]
    );
}

#[test]
fn global_ambient_names_do_not_mark_a_shadowing_local_binding_as_ambient() {
    let source = "declare global { const value: number; } const value = 1;";
    assert_eq!(
        names(
            source,
            SourceType::TypeScript,
            &options(serde_json::json!({}), false)
        ),
        ["value"]
    );
}

#[test]
fn default_exports_ambient_type_declarations_and_copy_environments_preserve_uses() {
    let options = options(serde_json::json!({}), true);
    assert!(
        names(
            "export default function Named(arg){return arg;}",
            SourceType::Module,
            &options
        )
        .is_empty()
    );
    assert!(
        names(
            "export default class Named {}",
            SourceType::Module,
            &options
        )
        .is_empty()
    );
    assert!(
        names(
            "declare interface Ambient {} declare type Other=string;",
            SourceType::TypeScript,
            &options
        )
        .is_empty()
    );
    assert!(
        names(
            "export function f(a=1){var a; return a;}",
            SourceType::Module,
            &options
        )
        .is_empty()
    );
    assert!(
        names(
            "{function legacy(){}} use(legacy);",
            SourceType::Script,
            &options
        )
        .is_empty()
    );
}

#[test]
fn dynamic_reads_are_not_proof_of_unused_bindings_but_shadowed_eval_is_ordinary() {
    let options = options(serde_json::json!({}), false);
    assert!(
        names(
            "const value=1; eval('use(value)');",
            SourceType::Script,
            &options
        )
        .is_empty()
    );
    assert!(
        names(
            "const value=1; with(object){use(value);}",
            SourceType::Script,
            &options
        )
        .is_empty()
    );
    assert_eq!(
        names(
            "function f(eval){const value=1; eval('use(value)');} use(f);",
            SourceType::Script,
            &options
        ),
        ["value"]
    );
}
