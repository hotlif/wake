use wake_lint_core::{LintOptions, SourceType, lint_text};

#[test]
fn array_type_rule_only_reports_unqualified_single_argument_type_references() {
    let options: LintOptions =
        serde_json::from_value(serde_json::json!({"recommended":false,"rules":{
            "ts/array-type":"error"
        }}))
        .unwrap();
    let source = r#"type A=Array<string>; type B=ReadonlyArray<number>; type C=Array<Array<number>>; type D=Arr\u0061y<boolean>;
type E=ns.Array<string>; type F=Array<A,B>; const value=Array<number>(1); class C extends Array<number> {} interface I extends Array<number> {}
const text='Array<T>'; type G=string[]; type H=readonly number[]; type J=typeof Array<number>;"#;
    let result = lint_text(source, SourceType::TypeScript, &options).unwrap();
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
        [
            "Array<string>",
            "ReadonlyArray<number>",
            "Array<Array<number>>",
            "Array<number>",
            "Arr\\u0061y<boolean>"
        ]
    );
    assert!(
        result
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.message_id == "array" && diagnostic.fix.is_none())
    );
}

#[test]
fn generic_array_types_distinguish_array_suffixes_from_keys_and_tuples() {
    let options: LintOptions =
        serde_json::from_value(serde_json::json!({"recommended":false,"rules":{
            "ts/array-type":{"level":"warn","options":{"syntax":"generic"}}
        }}))
        .unwrap();
    let source = "type A=string[]; type B=readonly number[]; type C=Foo[K][]; type D=Foo[Key]; type E=[string,number]; type F=typeof values[]; type G=(X|Y)[][]; const el=<X value={items[index]} />;";
    let result = lint_text(source, SourceType::Tsx, &options).unwrap();
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
        [
            "string[]",
            "number[]",
            "Foo[K][]",
            "typeof values[]",
            "(X|Y)[]",
            "(X|Y)[][]"
        ]
    );
    assert!(
        result
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.message_id == "generic")
    );
    assert!(
        lint_text(
            "const items=[]; items[index]; const text='Array<T>';",
            SourceType::Jsx,
            &options
        )
        .unwrap()
        .diagnostics
        .is_empty()
    );
}

#[test]
fn array_type_options_are_closed_and_rule_uses_real_comment_suppression() {
    let options: LintOptions = serde_json::from_value(
        serde_json::json!({"recommended":false,"rules":{"ts/array-type":"error"}}),
    )
    .unwrap();
    assert!(
        lint_text(
            "// wake-lint-disable-next-line ts/array-type\ntype A=Array<string>;",
            SourceType::TypeScript,
            &options
        )
        .unwrap()
        .diagnostics
        .is_empty()
    );
    for invalid in [serde_json::json!(true), serde_json::json!("automatic")] {
        let options: LintOptions = serde_json::from_value(serde_json::json!({"rules":{"ts/array-type":{"level":"off","options":{"syntax":invalid}}}})).unwrap();
        assert!(lint_text("", SourceType::TypeScript, &options).is_err());
    }
}

#[test]
fn array_type_checks_nested_heritage_arguments_and_keeps_comments_and_tsx_expressions_separate() {
    let options: LintOptions = serde_json::from_value(
        serde_json::json!({"recommended":false,"rules":{"ts/array-type":"error"}}),
    )
    .unwrap();
    let source = "interface I extends Array<ReadonlyArray<T>> {} class C implements Array<Array<T>> {} type X=Array /* note */ <string>; const el=<Widget value={Array<T>(value)}>Array&lt;T&gt;</Widget>; type Qualified=Array.Other<T>;";
    let result = lint_text(source, SourceType::Tsx, &options).unwrap();
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
        ["ReadonlyArray<T>", "Array<T>", "Array /* note */ <string>"]
    );
    assert!(
        result
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.fix.is_none())
    );
}

#[test]
fn typescript_declaration_rules_keep_original_interfaces_and_namespaces() {
    let options: LintOptions =
        serde_json::from_value(serde_json::json!({"recommended":false,"rules":{
            "ts/no-namespace":"error", "ts/no-empty-interface":"error", "ts/no-explicit-any":"error"
        }}))
        .unwrap();
    let source = "namespace A.B { export interface Empty {} } declare namespace C { interface NonEmpty { x: string } } interface Derived extends Base {} declare module 'ambient' { interface Filled { x: any } }";
    let result = lint_text(source, SourceType::TypeScript, &options).unwrap();
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
            "ts/no-namespace",
            "ts/no-empty-interface",
            "ts/no-namespace",
            "ts/no-empty-interface",
            "ts/no-explicit-any"
        ]
    );
    assert!(lint_text("interface X { (): void; } interface Y { [key: string]: unknown } const text = 'namespace A {}';", SourceType::TypeScript, &options).unwrap().diagnostics.is_empty());
}

#[test]
fn ts_comment_directives_require_explanations_without_scanning_literals() {
    let options: LintOptions =
        serde_json::from_value(serde_json::json!({"recommended":false,"rules":{
            "ts/ban-ts-comment":"error"
        }}))
        .unwrap();
    let source = "// @ts-ignore: explanation\nvalue;\n/*\n * @ts-nocheck\n */\n// @ts-expect-error: x\nvalue;\n// @ts-expect-error: verified upstream defect\nvalue;\n// @ts-check\nconst s = '// @ts-ignore'; /* docs mention @ts-ignore */";
    let result = lint_text(source, SourceType::TypeScript, &options).unwrap();
    assert_eq!(result.diagnostics.len(), 3, "{:?}", result);
    assert!(
        result
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.fix.is_none())
    );
}

#[test]
fn typescript_syntax_rules_consume_parser_facts_and_share_suppression() {
    let options: LintOptions =
        serde_json::from_value(serde_json::json!({"recommended":false,"rules":{
            "ts/no-explicit-any":"error", "ts/no-non-null-assertion":"warn"
        }}))
        .unwrap();
    let source = "type A = { any: string; nested: [any, (x: any) => any] }; const any = value; const x: typeof any = any; x!.field; let definite!: A; !ready;\n// wake-lint-disable-next-line ts/no-explicit-any\nconst hidden: any = value;";
    let result = lint_text(source, SourceType::Tsx, &options).unwrap();
    assert!(
        result.parse_diagnostics.is_empty(),
        "{:?}",
        result.parse_diagnostics
    );
    assert_eq!(
        result
            .diagnostics
            .iter()
            .map(|d| d.rule_id.as_str())
            .collect::<Vec<_>>(),
        [
            "ts/no-explicit-any",
            "ts/no-explicit-any",
            "ts/no-explicit-any",
            "ts/no-non-null-assertion"
        ]
    );
    assert!(result.diagnostics.iter().all(|d| d.fix.is_none()));
    assert!(
        lint_text(
            "const any = value; const x = <X any={any}>any!</X>; !ready;",
            SourceType::Jsx,
            &options
        )
        .unwrap()
        .diagnostics
        .is_empty()
    );
}
