use wake_lint_core::{LintOptions, SourceType, lint_text};

fn options(parameters: serde_json::Value) -> LintOptions {
    serde_json::from_value(serde_json::json!({"recommended":false,"rules":{
        "js/no-shadow":{"level":"error","options":parameters}
    }}))
    .unwrap()
}

#[test]
fn shadows_follow_scope_parents_and_do_not_count_implicit_name_copies() {
    let source = "let outer; {let outer;} function f(outer){} {let sibling;} {let sibling;} {let later;} let later; function copy(arg=1){var arg;} class Box{static{let outer;}} function implicit(){arguments;}";
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
        ["outer", "outer", "later", "outer"]
    );
    assert!(
        result
            .diagnostics
            .iter()
            .all(|d| d.message_id == "shadow" && d.fix.is_none())
    );
    let result = lint_text(
        source,
        SourceType::Module,
        &options(serde_json::json!({"hoist":false})),
    )
    .unwrap();
    assert_eq!(result.diagnostics.len(), 3);
}

#[test]
fn named_expressions_and_typescript_unknown_names_have_explicit_policy() {
    let source = "const fn = function fn(){}; const Class = class Class {}; type T = string; {let T;} let unknown; function f(){unknown; declare const unknown:number;}";
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
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    let result = lint_text(
        source,
        SourceType::TypeScript,
        &options(serde_json::json!({"ignore_named_expressions":false})),
    )
    .unwrap();
    assert_eq!(
        result
            .diagnostics
            .iter()
            .map(|d| &source[d.start as usize..d.end as usize])
            .collect::<Vec<_>>(),
        ["fn", "Class"]
    );
    let legacy = "var f; {function f(){}}";
    assert!(
        lint_text(legacy, SourceType::Script, &options(serde_json::json!({})))
            .unwrap()
            .diagnostics
            .is_empty()
    );
    assert!(
        lint_text(
            "",
            SourceType::Module,
            &options(serde_json::json!({"hoist":"all"}))
        )
        .is_err()
    );
    let ambient_same_name = "declare module 'ambient' { const value:number; } let value=1; function f(){ let value=2; return value; }";
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
}
