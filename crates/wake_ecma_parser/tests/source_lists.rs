use wake_common::Interner;
use wake_ecma_ast::SourceListKind;
use wake_ecma_parser::{ParseOptions, SourceType, parse, parse_source};

#[test]
fn lists_retain_original_delimiters_and_commas_without_changing_compilation() {
    let source = "import {A,} from 'm'; const [a,]=[1,]; const {b,}={b:2,}; function f(x:T,){call(x,)} type Tuple=[T,]; enum E {A,} type G<T,>=T;";
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
    assert_eq!(parsed.lists.len(), 10);
    assert!(
        parsed
            .lists
            .iter()
            .all(|list| list.comma.is_some() && list.last.is_some())
    );
    for list in &parsed.lists {
        assert!(source.is_char_boundary(list.open.lo as usize));
        assert_eq!(
            &source[list.comma.unwrap().lo as usize..list.comma.unwrap().hi as usize],
            ","
        );
    }
    let ordinary = parse(source, &interner, SourceType::TypeScript);
    assert_eq!(
        ordinary.module.structure_hash(),
        parsed.parsed.module.structure_hash()
    );
    assert_eq!(
        format!("{:?}", ordinary.dependencies),
        format!("{:?}", parsed.parsed.dependencies)
    );
}

#[test]
fn arrow_speculation_rest_and_elisions_have_committed_list_identity() {
    let source = "const f=<T,>(x = function(a:T){return a})=>x; const b=[1,,]; const g=(...rest:T[])=>rest; const seq=(a,b);";
    let interner = Interner::new();
    let parsed = parse_source(source, &interner, SourceType::Tsx, ParseOptions::default());
    assert!(
        !parsed.parsed.has_errors(),
        "{:?}",
        parsed.parsed.diagnostics
    );
    assert_eq!(parsed.lists.len(), 6);
    assert_eq!(
        parsed
            .lists
            .iter()
            .filter(|list| list.kind == SourceListKind::Parameters)
            .count(),
        3
    );
    assert_eq!(
        parsed.lists.iter().filter(|list| list.must_trail).count(),
        1
    );
    let array = parsed
        .lists
        .iter()
        .find(|list| list.kind == SourceListKind::Array)
        .unwrap();
    assert!(
        array.last.is_none(),
        "elision is not an optional trailing comma"
    );
    assert!(
        parsed
            .lists
            .iter()
            .any(|list| list.kind == SourceListKind::Parameters && !list.can_trail)
    );
}
