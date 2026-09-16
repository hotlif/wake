use wake_lint_core::{LintOptions, SourceType, fix_text, lint_text};

fn options(parameters: serde_json::Value) -> LintOptions {
    serde_json::from_value(serde_json::json!({"recommended":false,"rules":{"js/prefer-const":{"level":"warn","options":parameters}}})).unwrap()
}

fn names(source: &str, options: &LintOptions) -> Vec<String> {
    let result = lint_text(source, SourceType::TypeScript, options).unwrap();
    assert!(
        result.parse_diagnostics.is_empty(),
        "{:?}",
        result.parse_diagnostics
    );
    result
        .diagnostics
        .iter()
        .map(|d| source[d.start as usize..d.end as usize].into())
        .collect()
}

#[test]
fn const_candidates_follow_binding_writes_and_can_fix_whole_declarations() {
    let source = "let stable=1; let changed=1; changed++; let object={}; object.field=1; let captured=0; function mutate(){captured=2;} let a=1,b=2; use(stable, changed, object, a, b);";
    let options = options(serde_json::json!({}));
    assert_eq!(names(source, &options), ["stable", "object", "a", "b"]);
    let fixed = fix_text(source, SourceType::TypeScript, &options).unwrap();
    assert_eq!(
        fixed.output,
        source
            .replacen("let stable", "const stable", 1)
            .replacen("let object", "const object", 1)
            .replacen("let a=", "const a=", 1)
    );
    assert!(fixed.result.diagnostics.is_empty());
}

#[test]
fn destructuring_and_mixed_declarations_only_fix_when_every_binding_is_safe() {
    let source = "let { a, nested: [b], ...rest }=source; b=2; let fixed=1, changed=2; changed++;";
    let any = options(serde_json::json!({}));
    assert_eq!(names(source, &any), ["a", "rest", "fixed"]);
    assert_eq!(
        fix_text(source, SourceType::TypeScript, &any)
            .unwrap()
            .output,
        source
    );
    assert_eq!(
        names(source, &options(serde_json::json!({"destructuring":"all"}))),
        ["fixed"]
    );
    let safe = "let /* keep */ { a, nested: [b = fallback] } = input;";
    assert_eq!(
        fix_text(safe, SourceType::TypeScript, &any).unwrap().output,
        "const /* keep */ { a, nested: [b = fallback] } = input;"
    );
}

#[test]
fn loop_bindings_are_per_iteration_and_for_initializers_cannot_be_split() {
    let source = "for(let item of list){use(item);} for(let key in object){use(key);} for(let i=0,end=10;i<end;i++){} for(let changed of list){changed++;} for(let [a,b] of list){use(a,b);}";
    let options = options(serde_json::json!({}));
    assert_eq!(names(source, &options), ["item", "key", "a", "b"]);
    assert_eq!(
        fix_text(source, SourceType::TypeScript, &options)
            .unwrap()
            .output,
        source
            .replacen("let item", "const item", 1)
            .replacen("let key", "const key", 1)
            .replacen("let [a,b]", "const [a,b]", 1)
    );
}

#[test]
fn delayed_initialization_requires_a_declaration_position_and_the_same_scope() {
    let source = "let delayed; delayed=1; let before; use(before); before=2; let never; let twice; twice=1; twice=2; let branch; if(flag){branch=1;} let bare; if(flag)bare=1; let closure; function f(){closure=1;}";
    let options = options(serde_json::json!({}));
    assert_eq!(names(source, &options), ["delayed", "before"]);
    assert_eq!(
        names(
            source,
            &crate::options(serde_json::json!({"ignore_read_before_assign":true}))
        ),
        ["delayed"]
    );
    assert_eq!(
        fix_text(source, SourceType::TypeScript, &options)
            .unwrap()
            .output,
        source
    );
}

#[test]
fn dynamic_name_access_and_erased_bindings_do_not_support_const_proofs() {
    let options = options(serde_json::json!({}));
    assert!(names("let value=1; eval('value=2');", &options).is_empty());
    let result = lint_text(
        "let value=1; with (scope) { value=2; }",
        SourceType::Script,
        &options,
    )
    .unwrap();
    assert!(result.diagnostics.is_empty());
    assert_eq!(
        names("function f(eval){let value=1; eval('value=2');}", &options),
        ["value"]
    );
    assert!(names("declare let ambient:number; ambient;", &options).is_empty());
    assert!(
        names(
            "// wake-lint-disable-next-line js/prefer-const\nlet value=1;",
            &options
        )
        .is_empty()
    );
    assert_eq!(
        names(
            "declare module 'ambient' { const value:number; } let value=1; use(value);",
            &options
        ),
        ["value"]
    );
}

#[test]
fn delayed_destructuring_honors_the_assignment_group_and_cannot_declare_members() {
    assert_eq!(
        names(
            "let a,b; [a,b]=input; b++;",
            &options(serde_json::json!({}))
        ),
        ["a"]
    );
    assert!(
        names(
            "let a,b; [a,b]=input; b++;",
            &options(serde_json::json!({"destructuring":"all"}))
        )
        .is_empty()
    );
    assert_eq!(
        names(
            "let a,b; ({a,b}=input);",
            &options(serde_json::json!({"destructuring":"all"}))
        ),
        ["a", "b"]
    );
    assert!(
        names(
            "let a; [a,object.field]=input;",
            &options(serde_json::json!({}))
        )
        .is_empty()
    );
    assert!(names("let a; label: a=1;", &options(serde_json::json!({}))).is_empty());
}
