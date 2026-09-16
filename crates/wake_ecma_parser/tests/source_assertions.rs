use wake_common::{Interner, Span};
use wake_ecma_ast::SourceAssertionKind;
use wake_ecma_parser::{ParseOptions, SourceType, parse, parse_source};

fn text(source: &str, span: Span) -> &str {
    &source[span.lo as usize..span.hi as usize]
}

#[test]
fn assertions_keep_original_operands_types_precedence_and_constructor_boundaries() {
    let source = "const a = left + right as Num; const b = ((value as A)!) satisfies B; const c = <Readonly<Box<T>>>factory<T>(); new Factory!(); const tuple = [a,b] as const; /* 😀 */ const u = 值 as 类型;";
    let interner = Interner::new();
    let parsed = parse_source(
        source,
        &interner,
        SourceType::TypeScript,
        ParseOptions::default(),
    );
    assert!(
        !parsed.parsed.has_errors(),
        "{:?}",
        parsed.parsed.diagnostics
    );
    let records: Vec<_> = parsed
        .assertions
        .iter()
        .map(|assertion| {
            (
                assertion.kind,
                text(source, assertion.span),
                text(source, assertion.operand),
                assertion.type_span.map(|span| text(source, span)),
            )
        })
        .collect();
    assert_eq!(
        records,
        [
            (
                SourceAssertionKind::As,
                "left + right as Num",
                "left + right",
                Some("Num")
            ),
            (SourceAssertionKind::As, "value as A", "value", Some("A")),
            (
                SourceAssertionKind::NonNull,
                "(value as A)!",
                "(value as A)",
                None
            ),
            (
                SourceAssertionKind::Satisfies,
                "((value as A)!) satisfies B",
                "((value as A)!)",
                Some("B")
            ),
            (
                SourceAssertionKind::Angle,
                "<Readonly<Box<T>>>factory<T>()",
                "factory<T>()",
                Some("Readonly<Box<T>>")
            ),
            (SourceAssertionKind::NonNull, "Factory!", "Factory", None),
            (
                SourceAssertionKind::As,
                "[a,b] as const",
                "[a,b]",
                Some("const")
            ),
            (SourceAssertionKind::As, "值 as 类型", "值", Some("类型")),
        ]
    );
    let ordinary = parse(source, &interner, SourceType::TypeScript);
    assert!(!ordinary.has_errors());
    assert_eq!(
        parsed.parsed.module.structure_hash(),
        ordinary.module.structure_hash()
    );
}

#[test]
fn const_assertions_are_grammar_facts_distinct_from_named_types() {
    let source = "/* 😀 */ [1] as const; ({value: 1}) as /* note */ const; < /* note */ const /* note */ >[1]; value as Constant; value as { const: string }; value satisfies Constant; value!;";
    let interner = Interner::new();
    let parsed = parse_source(
        source,
        &interner,
        SourceType::TypeScript,
        ParseOptions::default(),
    );
    assert!(
        !parsed.parsed.has_errors(),
        "{:?}",
        parsed.parsed.diagnostics
    );
    assert_eq!(
        parsed
            .assertions
            .iter()
            .map(|site| site.is_const)
            .collect::<Vec<_>>(),
        [true, true, true, false, false, false, false]
    );
    let ordinary = parse(source, &interner, SourceType::TypeScript);
    assert_eq!(
        parsed.parsed.module.structure_hash(),
        ordinary.module.structure_hash()
    );
}

#[test]
fn assertion_facts_follow_relational_precedence_and_non_null_chains() {
    let source =
        "a || b as T; a + b as T; a < b as T; a as T + b; obj!.method()?.field!; value\n!other;";
    let interner = Interner::new();
    let parsed = parse_source(
        source,
        &interner,
        SourceType::TypeScript,
        ParseOptions::default(),
    );
    assert!(
        !parsed.parsed.has_errors(),
        "{:?}",
        parsed.parsed.diagnostics
    );
    assert_eq!(
        parsed
            .assertions
            .iter()
            .map(|assertion| text(source, assertion.span))
            .collect::<Vec<_>>(),
        [
            "b as T",
            "a + b as T",
            "a < b as T",
            "a as T",
            "obj!",
            "obj!.method()?.field!"
        ]
    );
}

#[test]
fn jsx_and_speculative_arrows_record_each_original_assertion_once() {
    let source = "const id = <T,>(arg: T = seed as T) => arg!; const f = (arg: T = seed as T) => arg!; const el = <div value={(obj as Model)!.name}> as text </div>;";
    let interner = Interner::new();
    let parsed = parse_source(source, &interner, SourceType::Tsx, ParseOptions::default());
    assert!(
        !parsed.parsed.has_errors(),
        "{:?}",
        parsed.parsed.diagnostics
    );
    assert_eq!(
        parsed
            .assertions
            .iter()
            .map(|assertion| text(source, assertion.span))
            .collect::<Vec<_>>(),
        [
            "seed as T",
            "arg!",
            "seed as T",
            "arg!",
            "obj as Model",
            "(obj as Model)!"
        ]
    );
    let ordinary = parse(source, &interner, SourceType::Tsx);
    assert!(!ordinary.has_errors());
    assert_eq!(
        parsed.parsed.module.structure_hash(),
        ordinary.module.structure_hash()
    );
}
