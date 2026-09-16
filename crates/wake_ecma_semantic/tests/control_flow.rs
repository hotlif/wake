use wake_common::Interner;
use wake_ecma_parser::{SourceType, parse};
use wake_ecma_semantic::analyze_control_flow;

#[test]
fn completions_keep_nested_functions_labels_and_finally_separate() {
    let source = "function mixed(x) { if (x) return 1; } function complete(x) { try { if (x) return 1; throw x; } finally { if (x) return; } } function loop() { outer: while (true) { continue outer; } after(); } function safe() { try { run(); } finally { while (x) { break; } try { throw 1; } catch {} } }";
    let interner = Interner::new();
    let parsed = parse(source, &interner, SourceType::Module);
    assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);
    let facts = parsed.module.with_ast(analyze_control_flow);
    assert_eq!(facts.functions.len(), 4);
    assert_eq!(
        facts
            .functions
            .iter()
            .filter(|function| function.returns_value
                && (function.returns_void || function.end_reachable))
            .count(),
        2
    );
    assert_eq!(
        facts
            .unreachable
            .iter()
            .map(|span| &source[span.lo as usize..span.hi as usize])
            .collect::<Vec<_>>(),
        ["after();"]
    );
    assert_eq!(
        facts
            .finally_exits
            .iter()
            .map(|span| &source[span.lo as usize..span.hi as usize])
            .collect::<Vec<_>>(),
        ["return;"]
    );
}
