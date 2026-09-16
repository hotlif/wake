use wake_lint_core::{LintOptions, SourceType, fix_text, lint_text};

fn options(parameters: serde_json::Value) -> LintOptions {
    serde_json::from_value(serde_json::json!({"recommended":false,"rules":{
        "style/indent":{"level":"warn","options":parameters}
    }}))
    .unwrap()
}

fn fixed(source: &str, parameters: serde_json::Value) -> String {
    let result = fix_text(source, SourceType::Tsx, &options(parameters)).unwrap();
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
fn blocks_lists_and_closing_groups_have_structural_indentation() {
    let source = "function f(\nvalue:T\n) {\nconst list = [\nvalue,\n{\nname: '名'\n}\n];\nreturn list;\n}\n";
    let expected = "function f(\n  value:T\n) {\n  const list = [\n    value,\n    {\n      name: '名'\n    }\n  ];\n  return list;\n}\n";
    assert_eq!(fixed(source, serde_json::json!({})), expected);
}

#[test]
fn bare_control_flow_and_case_bodies_are_indented_without_reindenting_else() {
    let source = "if (ready)\nrun();\nelse if (other)\nstop();\nelse\nfinish();\nfor(;;)\nif(ready)\nbreak;\nswitch(value){\ncase 1:\nrun();\nbreak;\ndefault:\nstop();\n}";
    let expected = "if (ready)\n  run();\nelse if (other)\n  stop();\nelse\n  finish();\nfor(;;)\n  if(ready)\n    break;\nswitch(value){\n  case 1:\n    run();\n    break;\n  default:\n    stop();\n}";
    assert_eq!(fixed(source, serde_json::json!({})), expected);
}

#[test]
fn multiline_expressions_declarations_and_member_chains_get_one_continuation_level() {
    let source = "const value = left +\nright;\nconst a=1,\nb=2;\nservice\n.run()\n.finish();\nfunction f(){\nreturn ready\n? left\n: right;\n}";
    let expected = "const value = left +\n  right;\nconst a=1,\n  b=2;\nservice\n  .run()\n  .finish();\nfunction f(){\n  return ready\n    ? left\n    : right;\n}";
    assert_eq!(fixed(source, serde_json::json!({})), expected);
}

#[test]
fn tabs_width_and_every_ecmascript_newline_are_preserved() {
    let source = "function f(){\r\n    if(ok){\u{2028} run();\u{2029} }\r}\n";
    assert_eq!(
        fixed(source, serde_json::json!({"style":"tabs","width":4})),
        "function f(){\r\n\tif(ok){\u{2028}\t\trun();\u{2029}\t}\r}\n"
    );
    assert_eq!(
        fixed("function f(){\nx();\n}", serde_json::json!({"width":4})),
        "function f(){\n    x();\n}"
    );
    for width in [
        serde_json::json!(0),
        serde_json::json!(9),
        serde_json::json!(2.5),
        serde_json::json!("2"),
    ] {
        assert!(
            lint_text(
                "",
                SourceType::Module,
                &options(serde_json::json!({"width":width}))
            )
            .is_err()
        );
    }
}

#[test]
fn types_and_jsx_use_original_grammar_without_touching_literal_content() {
    let source = "type Obj<T> = {\nvalue: T;\n};\nconst node=(\n<Panel\ntitle='text'\n>\n<Child />\n</Panel>\n);";
    let expected = "type Obj<T> = {\n  value: T;\n};\nconst node=(\n  <Panel\n    title='text'\n  >\n    <Child />\n  </Panel>\n);";
    assert_eq!(fixed(source, serde_json::json!({})), expected);
    let source = "function f(){\nconst text=`first\n raw template\n`;\n/* comment\n * raw body\n */\nconst jsx=<div>first\n raw JSX text\n</div>;\n}\n";
    let expected = "function f(){\n  const text=`first\n raw template\n`;\n  /* comment\n * raw body\n */\n  const jsx=<div>first\n raw JSX text\n  </div>;\n}\n";
    assert_eq!(fixed(source, serde_json::json!({})), expected);
}

#[test]
fn suppression_blank_lines_and_inline_nested_closers_keep_stable_fixes() {
    let source = "function f(){\n// wake-lint-disable-next-line style/indent\n unaligned();\n  \ncall({\nvalue: 1\n});\n}\n";
    let expected = "function f(){\n  // wake-lint-disable-next-line style/indent\n unaligned();\n  \n  call({\n      value: 1\n  });\n}\n";
    assert_eq!(fixed(source, serde_json::json!({})), expected);
}

#[test]
fn fixes_preserve_literal_values_including_non_lf_jsx_text_boundaries() {
    use wake_common::Interner;
    use wake_ecma_ast::{Expression, Visit, walk_expression};
    use wake_ecma_parser::parse;
    struct Literals<'a> {
        interner: &'a Interner,
        values: Vec<wake_common::JsString>,
    }
    impl<'a> Visit<'a> for Literals<'_> {
        fn visit_expression(&mut self, expression: &Expression<'a>) {
            match expression {
                Expression::StringLiteral(value) => {
                    self.values.push(self.interner.resolve_js(value.value))
                }
                Expression::TemplateLiteral(value) => {
                    for quasi in &value.quasis {
                        self.values
                            .push(self.interner.with_resolved(quasi.raw, str::to_owned).into());
                    }
                }
                Expression::RegExpLiteral(value) => self.values.push(
                    self.interner
                        .with_resolved(value.pattern, str::to_owned)
                        .into(),
                ),
                _ => {}
            }
            walk_expression(self, expression);
        }
    }
    for source in [
        "function f(){\nconst text=tag`first\n raw ${\nvalue\n}\n tail`;\n}",
        "function f(){\nconst text='first\\\n raw';\nconst pattern=/[({]/;\n}",
        "function f(){\nconst view=<div title='first'>text\n <Child/>\n</div>;\n}",
        "function f(){\nconst view=<div title='first\n raw'>text\n <Child/>\n</div>;\n}",
        "function f(){\nconst view=<div>text\u{2028} <Child/>\u{2029} </div>;\n}",
        "function f(){\nconst view=<div>text\r <Child/>\r </div>;\n}",
    ] {
        let output = fixed(source, serde_json::json!({}));
        let interner = Interner::new();
        let before = parse(source, &interner, SourceType::Tsx);
        let after = parse(&output, &interner, SourceType::Tsx);
        let mut before_values = Literals {
            interner: &interner,
            values: Vec::new(),
        };
        let mut after_values = Literals {
            interner: &interner,
            values: Vec::new(),
        };
        before
            .module
            .with_ast(|program| before_values.visit_program(program));
        after
            .module
            .with_ast(|program| after_values.visit_program(program));
        assert_eq!(
            before_values.values, after_values.values,
            "{source:?}\n{output:?}"
        );
    }
}
