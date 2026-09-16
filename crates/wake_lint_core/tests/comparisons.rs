use wake_lint_core::{LintOptions, SourceType, lint_text};

fn check(source: &str, rule: &str) -> Vec<String> {
    let mut options = LintOptions {
        recommended: false,
        ..LintOptions::default()
    };
    options
        .rules
        .insert(rule.into(), wake_lint_core::RuleLevel::Error.into());
    let result = lint_text(source, SourceType::Module, &options).unwrap();
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
fn self_assignment_pairs_destructuring_without_assuming_spread_or_getter_values() {
    assert_eq!(
        check(
            "a = a; [a, b] = [a, b]; ({x: a} = {x: a}); a += a; a = b; obj.x = obj.x; [a] = [...a];",
            "js/no-self-assign"
        ),
        ["a", "a", "b", "a"]
    );
    assert!(
        check(
            "[a, b] = [b, a]; ({x:a} = {y:a}); ({x:a} = {...other,x:a});",
            "js/no-self-assign"
        )
        .is_empty()
    );
}

#[test]
fn self_comparisons_are_limited_to_repeatable_primitive_syntax() {
    assert_eq!(check("a === a; (a) < a; 'x' != \"x\"; 1 >= 1; false == false; null === null; a + a; call() == call(); obj.x == obj.x;", "js/no-self-compare").len(), 6);
    assert!(
        check(
            "a === b; 'a' == 'b'; 1 == 2; ({}) === ({});",
            "js/no-self-compare"
        )
        .is_empty()
    );
}

#[test]
fn nan_rules_resolve_globals_and_cover_switches() {
    assert_eq!(check("x === NaN; Number.NaN < x; x != Number['NaN']; switch (x) { case NaN: break; } switch (NaN) { case 1: break; }", "js/use-isnan").len(), 5);
    assert!(
        check(
            "function f(NaN, Number) { return x === NaN || x === Number.NaN; } Number.isNaN(x);",
            "js/use-isnan"
        )
        .is_empty()
    );
}

#[test]
fn constant_binary_expressions_report_short_circuit_nullish_and_comparison_mistakes() {
    let source = "false && run(); [] || fallback; null ?? fallback; (x < limit) ?? 10; (x + y) ?? fallback; (ok ? 1 : 2) ?? fallback; (void call()) ?? fallback; typeof x === null; !x === 1; 1n == 1; 'a' < 'b'; obj === []; ({}) != ({}); (() => x) === obj; /x/ === value;";
    assert_eq!(
        check(source, "js/no-constant-binary-expression"),
        [
            "false && run()",
            "[] || fallback",
            "null ?? fallback",
            "(x < limit) ?? 10",
            "(x + y) ?? fallback",
            "(ok ? 1 : 2) ?? fallback",
            "(void call()) ?? fallback",
            "typeof x === null",
            "!x === 1",
            "1n == 1",
            "'a' < 'b'",
            "obj === []",
            "({}) != ({})",
            "(() => x) === obj",
            "/x/ === value",
        ]
    );
}

#[test]
fn constant_binary_analysis_preserves_coercion_aliasing_unknown_globals_and_class_escape() {
    assert!(check("x && call(); x || false; x ?? fallback; (ok ? null : 1) ?? fallback; typeof x === 'string'; !x === true; [] == false; ({valueOf(){return value;}}) == 1; new Factory() === old; (x = []) === x; (class { static { exposed = this; } }) === exposed; function f(undefined, NaN) { return undefined === x || NaN === y; }", "js/no-constant-binary-expression").is_empty());
}

#[test]
fn constant_binary_rules_use_typescript_expression_structure_and_real_directives() {
    let source = "// wake-lint-disable-next-line js/no-constant-binary-expression\nconst x = (value as number) + 1 ?? fallback;\nconst y = (value as number) === []; const element = <div>{1 === 2}</div>;";
    let options = LintOptions {
        recommended: false,
        rules: [(
            "js/no-constant-binary-expression".into(),
            wake_lint_core::RuleLevel::Error.into(),
        )]
        .into(),
        ..Default::default()
    };
    let result = lint_text(source, SourceType::Tsx, &options).unwrap();
    assert!(
        result.parse_diagnostics.is_empty(),
        "{:?}",
        result.parse_diagnostics
    );
    assert_eq!(result.diagnostics.len(), 2, "{:?}", result.diagnostics);
    assert!(
        result
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.fix.is_none())
    );
}

#[test]
fn constant_binary_nullish_facts_do_not_assume_primitive_object_coercions() {
    assert!(check("(-object) === 1n; (object + 1) === 'x'; new Factory() || backup; (ok ? null : object) ?? backup;", "js/no-constant-binary-expression").is_empty());
    assert_eq!(check("0x0n || backup; 0b1n && run(); `text${value}` || backup; (x ?? 1) ?? backup; (x || 1) ?? backup; (x && false) && run(); null == void call(); void call() === 1; typeof [] === 'object';", "js/no-constant-binary-expression").len(), 9);
}
