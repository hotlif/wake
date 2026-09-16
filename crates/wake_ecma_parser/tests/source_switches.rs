use wake_common::Interner;
use wake_ecma_ast::SourcePrimitiveValue;
use wake_ecma_parser::{ParseOptions, SourceType, parse_source};

#[test]
fn switch_source_facts_keep_literal_values_and_case_clause_ranges() {
    let interner = Interner::new();
    let source = "switch (state) { case 'a': break; case -1: break; case 0x1_0n: break; case -0b1_01n: break; case value: break; default: break; }";
    let parsed = parse_source(
        source,
        &interner,
        SourceType::TypeScript,
        ParseOptions::default(),
    );
    assert!(!parsed.parsed.has_errors());
    let switch = &parsed.switches[0];
    assert!(switch.has_default);
    assert_eq!(
        switch.cases,
        vec![
            SourcePrimitiveValue::String("a".into()),
            SourcePrimitiveValue::Number(-1.0),
            SourcePrimitiveValue::BigInt("16".into()),
            SourcePrimitiveValue::BigInt("-5".into()),
            SourcePrimitiveValue::Unknown,
        ]
    );
    assert_eq!(switch.case_spans.len(), 5);
    assert_eq!(switch.case_clause_spans.len(), 5);
    for (case, clause) in switch.case_spans.iter().zip(&switch.case_clause_spans) {
        assert!(clause.lo <= case.lo && case.hi <= clause.hi);
    }
}
