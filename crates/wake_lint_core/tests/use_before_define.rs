use wake_lint_core::{LintOptions, SourceType, lint_text};

fn options(parameters: serde_json::Value) -> LintOptions {
    serde_json::from_value(serde_json::json!({"recommended":false,"rules":{
        "js/no-use-before-define":{"level":"error","options":parameters}
    }}))
    .unwrap()
}

#[test]
fn forward_value_uses_follow_exact_scope_and_declaration_identity() {
    let source = "call(); function call(){} new Later(); class Later {} read; let read; const closure=()=>future; let future; let same; {same; let same;} function params(a=b,b=1){} missing; export { exported }; const exported=1;";
    let result = lint_text(source, SourceType::Module, &options(serde_json::json!({}))).unwrap();
    assert!(
        result.parse_diagnostics.is_empty(),
        "{:?}",
        result.parse_diagnostics
    );
    assert_eq!(
        result
            .diagnostics
            .iter()
            .map(|d| &source[d.start as usize..d.end as usize])
            .collect::<Vec<_>>(),
        ["call", "Later", "read", "future", "same", "b"]
    );
    assert!(
        result
            .diagnostics
            .iter()
            .all(|d| d.message_id == "before" && d.fix.is_none())
    );
}

#[test]
fn enum_member_forward_references_follow_the_enum_source_scope() {
    let source = "enum State { A = B, B = 1 }";
    let result = lint_text(
        source,
        SourceType::TypeScript,
        &options(serde_json::json!({})),
    )
    .unwrap();
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
        ["B"]
    );
    assert_eq!(result.diagnostics[0].message_id, "before");
}

#[test]
fn categories_are_configurable_and_erased_value_names_and_type_uses_are_not_guessed() {
    let source = "call(); function call(){} new Later(); class Later {} read; let read; type Query=typeof read; let typed:Shape; interface Shape{} declared; declare const declared:number;";
    let result = lint_text(
        source,
        SourceType::TypeScript,
        &options(serde_json::json!({"functions":false,"classes":false})),
    )
    .unwrap();
    assert!(
        result.parse_diagnostics.is_empty(),
        "{:?}",
        result.parse_diagnostics
    );
    assert_eq!(
        result
            .diagnostics
            .iter()
            .map(|d| &source[d.start as usize..d.end as usize])
            .collect::<Vec<_>>(),
        ["read"]
    );
    assert!(
        lint_text(
            source,
            SourceType::TypeScript,
            &options(serde_json::json!({"functions":false,"classes":false,"variables":false}))
        )
        .unwrap()
        .diagnostics
        .is_empty()
    );
    assert!(
        lint_text(
            "",
            SourceType::Module,
            &options(serde_json::json!({"variables":"false"}))
        )
        .is_err()
    );
    let suppressed = "// wake-lint-disable-next-line js/no-use-before-define\nx; let x;";
    assert!(
        lint_text(
            suppressed,
            SourceType::Module,
            &options(serde_json::json!({}))
        )
        .unwrap()
        .diagnostics
        .is_empty()
    );
    let ambient_same_name = "declare module 'ambient' { const value:number; } value; let value=1;";
    let result = lint_text(
        ambient_same_name,
        SourceType::TypeScript,
        &options(serde_json::json!({})),
    )
    .unwrap();
    assert_eq!(
        result
            .diagnostics
            .iter()
            .map(|d| &ambient_same_name[d.start as usize..d.end as usize])
            .collect::<Vec<_>>(),
        ["value"]
    );

    let global_same_name = "declare global { const value:number; } value; let value=1;";
    let result = lint_text(
        global_same_name,
        SourceType::TypeScript,
        &options(serde_json::json!({})),
    )
    .unwrap();
    assert_eq!(
        result
            .diagnostics
            .iter()
            .map(|d| &global_same_name[d.start as usize..d.end as usize])
            .collect::<Vec<_>>(),
        ["value"]
    );
}
