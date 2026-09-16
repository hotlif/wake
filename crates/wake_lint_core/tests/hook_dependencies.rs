use wake_lint_core::{LintDiagnostic, LintOptions, RuleLevel, SourceType, lint_text};
const RULE: &str = "react-hooks/exhaustive-deps";
fn check_with(source: &str, custom: &str) -> Vec<LintDiagnostic> {
    let mut options = LintOptions {
        recommended: false,
        ..Default::default()
    };
    options.rules.insert(
        RULE.into(),
        serde_json::from_value(
            serde_json::json!({"level":"error", "options":{"additional_effect_hooks":custom}}),
        )
        .unwrap(),
    );
    let result = lint_text(source, SourceType::Tsx, &options).unwrap();
    assert!(
        result.parse_diagnostics.is_empty(),
        "{:?}",
        result.parse_diagnostics
    );
    assert!(result.diagnostics.iter().all(|d| d.fix.is_none()));
    result.diagnostics
}
fn check(source: &str) -> Vec<LintDiagnostic> {
    check_with(source, "")
}
fn messages(source: &str) -> Vec<String> {
    check(source).into_iter().map(|d| d.message).collect()
}

#[test]
fn captures_use_original_symbols_static_paths_and_nested_callbacks() {
    let source = r#"import {useEffect as effect} from 'react'; const external = 1;
function App(props, key) { const local = props.value;
 effect(() => { consume(props.user.name, local, external); const inside = 1; consume(inside); queue(() => consume(props.extra)); }, [props.user]);
 effect(() => { consume(props.user.name); }, [props]);
 effect(() => { consume(props[key]); }, [props]);
}"#;
    let diagnostics = messages(source);
    assert_eq!(diagnostics.len(), 3, "{diagnostics:?}");
    assert!(diagnostics.iter().any(|d| d.contains("'local'")));
    assert!(diagnostics.iter().any(|d| d.contains("'props.extra'")));
    assert!(diagnostics.iter().any(|d| d.contains("'key'")));
}

#[test]
fn string_ambient_names_do_not_hide_local_hook_captures() {
    let source = r#"declare module 'ambient' { const value:number; }
import {useEffect} from 'react';
function App() { const value = 1; useEffect(() => consume(value), [value]); }"#;
    assert!(check(source).is_empty());
}

#[test]
fn stable_react_outputs_literals_and_capture_free_function_groups_are_exempt() {
    let source = r#"import {useEffect as effect, useState as state, useReducer, useRef, useTransition} from 'react';
function App(p) { const [value, set] = state(0); const [s, dispatch] = useReducer(reduce, 0);
 const ref = useRef(null); const [pending, start] = useTransition(); const literal = 1;
 function first() { return second(); } function second() { return first(); }
 const helper = () => external();
 effect(() => { set(1); dispatch({}); start(() => {}); consume(ref.current, literal, first, helper); }, []);
 effect(() => consume(value, s, pending, p), []);
}"#;
    let diagnostics = messages(source);
    assert_eq!(diagnostics.len(), 4, "{diagnostics:?}");
    for name in ["value", "s", "pending", "p"] {
        assert!(diagnostics.iter().any(|d| d.contains(&format!("'{name}'"))));
    }
}

#[test]
fn dependency_entries_distinguish_effect_extras_duplicates_mutable_and_memo_extras() {
    let source = r#"import {useEffect, useMemo, useRef} from 'react'; const moduleValue = 1;
function App(p, extra) { const ref = useRef(null);
 useEffect(() => consume(p), [p, extra]);
 useMemo(() => p.value, [p, extra]);
 useEffect(() => consume(p), [p, p, moduleValue, ref.current]);
}"#;
    let result = check(source);
    assert_eq!(
        result
            .iter()
            .map(|d| d.message_id.as_str())
            .collect::<Vec<_>>(),
        ["unnecessary", "duplicate", "external", "mutable"]
    );
}

#[test]
fn dynamic_arrays_unknown_callbacks_missing_memo_arrays_and_async_effects_report() {
    let source = r#"import {useEffect, useMemo, useCallback} from 'react';
function App(p, callback, deps) {
 useEffect(() => consume(p)); useMemo(() => p); useCallback(() => p);
 useEffect(callback, []); useEffect(() => p, deps); useEffect(() => p, [...deps]);
 useEffect(async () => { await consume(p); }, [p]);
}"#;
    let result = check(source);
    assert_eq!(
        result
            .iter()
            .map(|d| d.message_id.as_str())
            .collect::<Vec<_>>(),
        [
            "missing-array",
            "missing-array",
            "unknown-callback",
            "dynamic",
            "dynamic",
            "async"
        ]
    );
}

#[test]
fn local_callbacks_shadowing_and_custom_effect_aliases_keep_real_identities() {
    let source = r#"import {useEffect as effect} from 'react'; import {useObserve as observe} from 'store';
function App(props) {
 const callback = () => consume(props.name);
 effect(callback, []); observe(() => consume(props.age), []);
 function shadow(effect) { effect(() => consume(props), []); }
 const fake = {useEffect(){}}; fake.useEffect(() => consume(props), []);
}"#;
    assert_eq!(check(source).len(), 1);
    let custom = check_with(source, "^useObserve$");
    assert_eq!(custom.len(), 2);
    assert!(custom[0].message.contains("'props.name'"));
    assert!(custom[1].message.contains("'props.age'"));
    let options = LintOptions {
        recommended: false,
        rules: [(
            RULE.into(),
            serde_json::from_value(
                serde_json::json!({"level":"off", "options":{"additional_effect_hooks":"["}}),
            )
            .unwrap(),
        )]
        .into(),
        ..Default::default()
    };
    assert!(lint_text("", SourceType::Module, &options).is_err());
}

#[test]
fn reactive_function_captures_reassignments_and_stale_writes_are_not_stable() {
    let source = r#"import {useEffect} from 'react';
function App(p) { let value = 0; const callback = () => consume(p);
 function first() { second(); } function second() { first(); consume(p); }
 useEffect(() => { callback(); first(); value = 1; }, []);
 let changed = () => consume(p); changed = external; useEffect(changed, []);
}"#;
    let result = check(source);
    assert_eq!(result.len(), 4, "{result:?}");
    assert_eq!(
        result.iter().filter(|d| d.message_id == "missing").count(),
        2
    );
    assert_eq!(
        result
            .iter()
            .filter(|d| d.message_id == "stale-write")
            .count(),
        1
    );
    assert_eq!(
        result
            .iter()
            .filter(|d| d.message_id == "unknown-callback")
            .count(),
        1
    );
}

#[test]
fn optional_paths_receiver_calls_imperative_handles_and_tsx_captures_are_original() {
    let source = r#"import R, {useEffect, useImperativeHandle} from 'react';
function App<T>(props: T, ref) {
 useEffect(() => { props.service?.start(); consume(props.user?.name); }, [props.service, props.user]);
 useImperativeHandle(ref, () => ({focus() { consume(props.id); }}), []);
 R.useMemo(() => <span>{props.title}</span>, []);
}"#;
    let result = messages(source);
    assert_eq!(result.len(), 2, "{result:?}");
    assert!(result[0].contains("'props.id'"));
    assert!(result[1].contains("'props.title'"));
}

#[test]
fn nested_ambient_value_captures_report_missing_when_scope_is_represented() {
    let source = "import {useEffect} from 'react'; function App() { { declare const ambient: number; useEffect(() => consume(ambient), []); } }";
    assert_eq!(
        check(source)
            .iter()
            .map(|d| d.message_id.as_str())
            .collect::<Vec<_>>(),
        ["missing"]
    );
    let options = LintOptions {
        recommended: false,
        rules: [(RULE.into(), RuleLevel::Off.into())].into(),
        ..Default::default()
    };
    assert!(
        lint_text(source, SourceType::TypeScript, &options)
            .unwrap()
            .diagnostics
            .is_empty()
    );
}

#[test]
fn ordinary_current_properties_callback_identity_and_parent_captures_are_precise() {
    let source = r#"import {useEffect, useMemo} from 'react';
function App(props) {
 useEffect(() => consume(props.current), [props.current]);
 const callback = () => props.value;
 useEffect(callback, [callback]); useMemo(callback, [callback]);
 useEffect(() => consume(props, props.value), []);
}"#;
    let result = messages(source);
    assert_eq!(result, ["Missing dependency 'props'."]);
}

#[test]
fn effects_without_arrays_still_check_lost_writes_and_incomplete_captures() {
    let source = r#"import {useEffect} from 'react'; declare const ambient: number;
function App() { let value = 0; useEffect(() => { value = 1; }); useEffect(() => consume(ambient)); }"#;
    assert_eq!(
        check(source)
            .iter()
            .map(|d| d.message_id.as_str())
            .collect::<Vec<_>>(),
        ["stale-write"]
    );
}
