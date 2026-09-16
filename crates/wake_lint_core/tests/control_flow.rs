use wake_lint_core::{LintOptions, SourceType, lint_text};

fn check(source: &str, rule: &str) -> Vec<String> {
    let mut options = LintOptions {
        recommended: false,
        ..LintOptions::default()
    };
    options
        .rules
        .insert(rule.into(), wake_lint_core::RuleLevel::Error.into());
    let result = lint_text(source, SourceType::TypeScript, &options).unwrap();
    assert!(
        result.parse_diagnostics.is_empty(),
        "{:?}",
        result.parse_diagnostics
    );
    result
        .diagnostics
        .iter()
        .map(|diagnostic| source[diagnostic.start as usize..diagnostic.end as usize].to_owned())
        .collect()
}

#[test]
fn unreachable_paths_respect_branch_termination_and_loop_targets() {
    let source = "function f(x) { if (x) return 1; else throw x; dead(); function nested() { return; inner(); } var hoisted; } function g() { outer: while (true) { continue outer; } after(); } function h(x) { while (true) { if (x) break; } alive(); }";
    assert_eq!(
        check(source, "js/no-unreachable"),
        ["dead();", "inner();", "after();"]
    );
}

#[test]
fn return_consistency_excludes_synthetic_namespaces_and_nested_functions() {
    let source = "function f(x) { if (x) return 1; } const g = (x) => { if (x) return 1; return; }; function fine(x) { if (x) return 1; throw x; } function nesting() { function nested() { return 1; } } namespace N { export const x = 1; }";
    assert_eq!(check(source, "js/consistent-return").len(), 2);
    assert!(check("function infinite() { while (true) {} } function f(x) { if(x) return; return void run(); }", "js/consistent-return").is_empty());
}

#[test]
fn switch_fallthrough_uses_completions_and_real_comment_markers() {
    assert_eq!(check("switch(x) { case 0: case 1: run(); case 2: break; case 3: done(); /* falls through */ default: break; }", "js/no-fallthrough").len(), 1);
    assert!(
        check(
            "function f(x) { switch(x) { case 0: if (x) return; else throw x; default: break; } }",
            "js/no-fallthrough"
        )
        .is_empty()
    );
}

#[test]
fn finally_only_reports_escaping_abrupt_completions() {
    assert_eq!(
        check(
            "function f() { try { return 1; } finally { if (x) return 2; throw x; } }",
            "js/no-unsafe-finally"
        ),
        ["return 2;", "throw x;"]
    );
    assert!(check("function f() { try { run(); } finally { label: { break label; } while (x) { continue; } try { throw x; } catch {} function nested() { return 1; } } }", "js/no-unsafe-finally").is_empty());
    assert_eq!(
        check(
            "outer: while (x) { try { run(); } finally { break outer; } }",
            "js/no-unsafe-finally"
        ),
        ["break outer;"]
    );
}

#[test]
fn loop_entry_and_finally_override_determine_actual_function_completions() {
    let valid = [
        "function f() { while (false) { return 1; } }",
        "function f() { for (; false;) { return 1; } }",
        "function f() { do { return 1; } while (false); }",
        "function f() { try { return 1; } finally { return; } }",
        "function f(x) { switch (x) { case 0: return 1; default: return 2; } }",
        "function f(x) { while (true) { if(x) return 1; } }",
    ];
    for source in valid {
        assert!(check(source, "js/consistent-return").is_empty(), "{source}");
    }
    assert_eq!(
        check(
            "function f(x) { switch(x) { case 0: return 1; } }",
            "js/consistent-return"
        )
        .len(),
        1
    );
}
