use wake_lint_core::{LintOptions, SourceType, lint_text};

fn check(rule: &str, source: &str) -> Vec<wake_lint_core::LintDiagnostic> {
    let options = serde_json::from_value::<LintOptions>(
        serde_json::json!({"recommended":false,"rules":{rule:"error"}}),
    )
    .unwrap();
    let result = lint_text(source, SourceType::Tsx, &options).unwrap();
    assert!(
        result.parse_diagnostics.is_empty(),
        "{source}: {:?}",
        result.parse_diagnostics
    );
    result.diagnostics
}

#[test]
fn sparse_arrays_do_not_confuse_destructuring_or_jsx_lowering() {
    assert_eq!(
        check(
            "js/no-sparse-arrays",
            "const a = [1,,3]; const [,x] = values; const el = <X><Y/><Z/></X>;"
        )
        .len(),
        1
    );
    assert_eq!(
        check(
            "js/no-sparse-arrays",
            "const a = [,]; const b = [,,]; const c = [1,];"
        )
        .len(),
        2
    );
}

#[test]
fn typeof_comparisons_only_accept_standard_result_strings() {
    assert_eq!(
        check(
            "js/valid-typeof",
            "typeof x === 'str'; 'functon' != typeof y; typeof x === 'string'; typeof y === name;"
        )
        .len(),
        2
    );
    for value in [
        "undefined",
        "object",
        "boolean",
        "number",
        "string",
        "function",
        "symbol",
        "bigint",
    ] {
        assert!(check("js/valid-typeof", &format!("typeof x === '{value}';")).is_empty());
    }
    assert!(
        check(
            "js/valid-typeof",
            "obj.typeof === 'wrong'; 'typeof' === 'wrong';"
        )
        .is_empty()
    );
}

#[test]
fn condition_assignments_walk_operands_but_do_not_enter_function_bodies() {
    assert_eq!(check("js/no-cond-assign", "if ((x = y)) run(); while (ready && (x += 1)) run(); do run(); while (x = read()); for (; x = y;) run(); (x = y) ? a : b;").len(), 5);
    assert!(
        check(
            "js/no-cond-assign",
            "if (() => x = 1) run(); if (function f() { x = 1; }) run(); if (x === y) run();"
        )
        .is_empty()
    );
    assert_eq!(
        check("js/no-cond-assign", "if ((x = y) ? a : b) run();").len(),
        1
    );
}

#[test]
fn var_reports_every_declaration_form_without_scope_changing_fixes() {
    let diagnostics = check(
        "js/no-var",
        "var x; for (var i = 0; i < 2; i++) {} for (var key in obj) {} for (var item of items) {} let y; const z = 1;",
    );
    assert_eq!(diagnostics.len(), 4);
    assert!(diagnostics.iter().all(|d| d.fix.is_none()));
}
