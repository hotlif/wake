use wake_lint_core::{LintOptions, RuleLevel, SourceType, lint_text};

fn options() -> LintOptions {
    LintOptions {
        recommended: false,
        rules: [("react-hooks/rules-of-hooks".into(), RuleLevel::Error.into())].into(),
        ..Default::default()
    }
}

fn check(source: &str) -> Vec<(String, String)> {
    let result = lint_text(source, SourceType::Tsx, &options()).unwrap();
    assert!(
        result.parse_diagnostics.is_empty(),
        "{:?}",
        result.parse_diagnostics
    );
    result
        .diagnostics
        .into_iter()
        .map(|d| {
            assert!(d.fix.is_none());
            (
                source[d.start as usize..d.end as usize].into(),
                d.message_id,
            )
        })
        .collect()
}

#[test]
fn hooks_use_original_import_and_function_identities() {
    let source = r#"import React, {useState as state, memo as wrap, forwardRef} from 'react';
import * as R from 'react'; import {useStore as store} from 'state';
function App() { state(0); R.useEffect(() => {}); store(); return <div/>; }
const Other = () => React.useState(1);
const Wrapped = wrap(() => { state(2); return null; });
const Forwarded = forwardRef(function(props, ref) { state(3); return null; });
export default () => { state(4); return null; };
function useMine() { state(5); }
function ordinary(state) { state(6); const React = {useState(){}}; React.useState(7); }
function invalid() { state(8); }
const wrong = function named() { state(9); };
function callbackOwner() { wrap(() => state(10)); }
"#;
    assert_eq!(
        check(source),
        [
            ("state(8)".into(), "context".into()),
            ("state(9)".into(), "context".into())
        ]
    );
}

#[test]
fn hooks_report_paths_early_returns_short_circuit_and_optional_calls() {
    let source = r#"import {useState as h} from 'react';
function A(x) { if (x) h(1); }
function B(x) { if (x) return null; h(2); }
function C(x) { x && h(3); x?.f(h(4)); }
function D(x) { if (x) throw Error(); h(5); }
function E() { if (false) h(6); h(7); }
function F(x) { for (let n = h(8); x;) { h(9); break; } }
function G(x) { switch(x) { case 1: h(10); break; default: break; } }
function H(x) { (x?.f)(h(11)); }
"#;
    assert_eq!(
        check(source),
        [
            ("h(1)".into(), "conditional".into()),
            ("h(2)".into(), "conditional".into()),
            ("h(3)".into(), "conditional".into()),
            ("h(4)".into(), "conditional".into()),
            ("h(9)".into(), "loop".into()),
            ("h(10)".into(), "conditional".into()),
        ]
    );
}

#[test]
fn hooks_reject_callbacks_classes_parameters_async_and_exceptions() {
    let source = r#"import {useState as h} from 'react';
h(0);
function App(x = h(1)) { [1].map(() => h(2)); try { h(3); } catch { h(4); } finally { h(5); } }
async function Async() { h(6); }
function* Generator() { h(7); }
class Thing { App() { h(8); } value = h(9); static { h(10); } }
"#;
    assert_eq!(
        check(source),
        [
            ("h(0)".into(), "context".into()),
            ("h(1)".into(), "parameters".into()),
            ("h(2)".into(), "context".into()),
            ("h(3)".into(), "exception".into()),
            ("h(4)".into(), "exception".into()),
            ("h(5)".into(), "exception".into()),
            ("h(6)".into(), "async".into()),
            ("h(7)".into(), "generator".into()),
            ("h(8)".into(), "context".into()),
            ("h(9)".into(), "context".into()),
            ("h(10)".into(), "context".into()),
        ]
    );
}

#[test]
fn react_use_is_conditional_but_still_requires_a_react_function() {
    let source = r#"import {use as read} from 'react';
function App(x) { if (x) read(x); for (const p of x) read(p); try { read(x); } catch {} }
function helper() { read(promise); }
function use(x) { return x; } function plain() { use(1); }
"#;
    assert_eq!(
        check(source),
        [
            ("read(x)".into(), "exception".into()),
            ("read(promise)".into(), "context".into())
        ]
    );
}

#[test]
fn hook_conventions_and_original_ts_bindings_exclude_synthetic_lowering() {
    let source = r#"function useLocal() {} function use1() {}
function App<T>(p: T) { useLocal(); use1(); const useLocal = () => {}; }
function ordinary() { useLocal(); use1(); }
namespace N { export const value = 1; }
declare function useMissing(): void;
function Incomplete() { if (flag) useMissing(); }
"#;
    assert_eq!(
        check(source),
        [
            ("useLocal()".into(), "context".into()),
            ("use1()".into(), "context".into())
        ]
    );
}

#[test]
fn call_analysis_budget_failure_is_typed_and_disabled_rules_do_not_build_graphs() {
    let source = format!("function App() {{ {} }}", "ordinary();".repeat(60_000));
    let error = lint_text(&source, SourceType::Module, &options()).unwrap_err();
    assert!(
        matches!(error, wake_lint_core::LintError::Analysis(_)),
        "{error}"
    );
    let options = LintOptions {
        recommended: false,
        ..Default::default()
    };
    assert!(
        lint_text(&source, SourceType::Module, &options)
            .unwrap()
            .diagnostics
            .is_empty()
    );
}

#[test]
fn named_react_wrappers_and_assigned_or_for_bound_components_keep_context() {
    let source = r#"import {useState as h, forwardRef, memo} from 'react';
const First = forwardRef(function render(props, ref) { h(0); return null; });
const Second = memo(function render() { h(1); return null; });
let App; App = () => { h(2); };
for (const Other = () => { h(3); }; false;) {}
const Invalid = function plain() { h(4); };
"#;
    assert_eq!(check(source), [("h(4)".into(), "context".into())]);
}
