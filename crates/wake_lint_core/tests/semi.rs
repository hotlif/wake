use wake_lint_core::{LintOptions, SourceType, fix_text, lint_text};

fn options(mode: &str) -> LintOptions {
    serde_json::from_value(serde_json::json!({"recommended":false,"rules":{
        "style/semi":{"level":"warn","options":{"mode":mode}}
    }}))
    .unwrap()
}

fn fixed(source: &str, mode: &str) -> String {
    let result = fix_text(source, SourceType::Tsx, &options(mode)).unwrap();
    assert!(
        result.result.parse_diagnostics.is_empty(),
        "{:?}",
        result.result.parse_diagnostics
    );
    assert!(
        result.result.diagnostics.is_empty(),
        "{:?}",
        result.result.diagnostics
    );
    result.output
}

#[test]
fn always_inserts_at_original_grammar_ends_and_preserves_comments_and_unicode() {
    let source = "import type {T} from 'm'\r\ntype Alias=T\r\nconst 名=<View/> // keep\r\nfunction f(){return 名}\r\nclass C { field: Alias\r\n value=1 }\r\ndo {} while(false) next()";
    assert_eq!(
        fixed(source, "always"),
        "import type {T} from 'm';\r\ntype Alias=T;\r\nconst 名=<View/>; // keep\r\nfunction f(){return 名;}\r\nclass C { field: Alias;\r\n value=1; }\r\ndo {} while(false); next();"
    );
}

#[test]
fn never_removes_only_optional_terminators_and_keeps_empty_and_for_statements() {
    let source = ";\nfor(let i=0;i<2;i++){run(i);}\nconst a=1; const b=2;\nfunction f(){return a;}\nclass C {field=1;}\ndo{}while(false); next();\n";
    assert_eq!(
        fixed(source, "never"),
        ";\nfor(let i=0;i<2;i++){run(i)}\nconst a=1; const b=2\nfunction f(){return a}\nclass C {field=1}\ndo{}while(false) next()\n"
    );
}

#[test]
fn never_preserves_expression_continuation_barriers() {
    for source in [
        "value;\n(call)()",
        "value;\n[index].run()",
        "value;\n`template`",
        "value;\n`template${part}`",
        "value;\n/regex/.test(input)",
        "value;\n+other",
        "value;\n-other",
        "class C { field=1;\n*method(){} }",
    ] {
        assert_eq!(fixed(source, "never"), source, "{source}");
    }
}

#[test]
fn type_member_separators_and_empty_bodies_are_not_statement_terminators() {
    let source = "interface I {a:T; b:U;} type Shape={a:T; b:U;};\nwhile(flag);\nfor(;;);";
    assert_eq!(
        fixed(source, "never"),
        "interface I {a:T; b:U;} type Shape={a:T; b:U;}\nwhile(flag);\nfor(;;);"
    );
    let source = "interface I {a:T; b:U;}\nwhile(flag);\nfor(;;);";
    assert_eq!(fixed(source, "always"), source);
}

#[test]
fn suppression_and_invalid_configuration_use_shared_engine_contracts() {
    let source = "// wake-lint-disable-next-line style/semi\nconst value=1\n";
    assert_eq!(fixed(source, "always"), source);
    assert!(lint_text("", SourceType::Module, &options("sometimes")).is_err());
}

#[test]
fn never_does_not_turn_class_fields_into_member_modifiers() {
    for name in [
        "static",
        "get",
        "set",
        "async",
        "accessor",
        "public",
        "private",
        "protected",
        "readonly",
        "override",
        "abstract",
        "declare",
    ] {
        let source = format!("class C {{ {name};\n next(){{}} }}");
        assert_eq!(fixed(&source, "never"), source, "{name}");
    }
}
