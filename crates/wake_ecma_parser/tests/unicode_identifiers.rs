use wake_common::Interner;
use wake_ecma_parser::{SourceType, parse_source};

#[test]
fn raw_and_escaped_names_share_identity_without_unicode_normalization() {
    let source = "const a\u{0301}=1; a\\u0301; const é=2; const e\u{0301}=3; é; e\u{0301}; class C { #a\u{0301}=1; value(){return this.#a\\u0301;} }";
    let interner = Interner::new();
    let parsed = parse_source(source, &interner, SourceType::Module, Default::default());
    assert!(
        !parsed.parsed.has_errors(),
        "{:?}",
        parsed.parsed.diagnostics
    );
    let identifiers: Vec<_> = parsed
        .identifiers
        .iter()
        .map(|id| id.name.as_str())
        .collect();
    assert!(
        identifiers
            .iter()
            .filter(|&&name| name == "a\u{0301}")
            .count()
            >= 2
    );
    assert!(identifiers.contains(&"é"));
    assert!(identifiers.contains(&"e\u{0301}"));
    assert_eq!(
        &source[parsed.identifiers[0].span.lo as usize..parsed.identifiers[0].span.hi as usize],
        "a\u{0301}"
    );
}
