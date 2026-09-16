use wake_common::Interner;
use wake_ecma_parser::{ParseOptions, SourceType, parse_with, parse_with_comments};

#[test]
fn parser_context_excludes_jsx_text_and_literal_impostors() {
    let source = "// first\nconst el = <div title='/* attr */'>// text\n<span />{/* real */}{`// template`}{/[/][*]/}</div>; // last";
    let output = parse_with_comments(
        source,
        &Interner::new(),
        SourceType::Tsx,
        ParseOptions::default(),
    );
    assert!(
        !output.parsed.has_errors(),
        "{:?}",
        output.parsed.diagnostics
    );
    let comments: Vec<_> = output
        .comments
        .iter()
        .map(|c| &source[c.span.lo as usize..c.span.hi as usize])
        .collect();
    assert_eq!(comments, ["// first", "/* real */", "// last"]);
}

#[test]
fn comments_survive_successful_and_failed_typescript_speculation_once() {
    for source in [
        "f<A<B> /* type */ >(value); // tail",
        "const result = left < /* compare */ right > other; // tail",
        "const f = <T, /* generic */>(value: T): T => value;",
    ] {
        let interner = Interner::new();
        let output =
            parse_with_comments(source, &interner, SourceType::Tsx, ParseOptions::default());
        assert!(
            !output.parsed.has_errors(),
            "{source}: {:?}",
            output.parsed.diagnostics
        );
        let comments: Vec<_> = output
            .comments
            .iter()
            .map(|c| &source[c.span.lo as usize..c.span.hi as usize])
            .collect();
        let expected = if source.starts_with("f<") {
            vec!["/* type */", "// tail"]
        } else if source.starts_with("const result") {
            vec!["/* compare */", "// tail"]
        } else {
            vec!["/* generic */"]
        };
        assert_eq!(comments, expected, "{source}");
    }
}

#[test]
fn collecting_comments_preserves_compilation_outputs_and_source_ownership() {
    let interner = Interner::new();
    let source = String::from(
        "\u{feff}// 😀\r\nexport const el: unknown = <div {...props}>hi</div>; /* tail */",
    );
    let options = ParseOptions::default();
    let ordinary = parse_with(&source, &interner, SourceType::Tsx, options);
    let output = parse_with_comments(&source, &interner, SourceType::Tsx, options);
    assert_eq!(
        ordinary.module.structure_hash(),
        output.parsed.module.structure_hash()
    );
    assert_eq!(
        ordinary.module.with_ast(|p| format!("{p:?}")),
        output.parsed.module.with_ast(|p| format!("{p:?}"))
    );
    assert_eq!(
        format!("{:?}", ordinary.dependencies),
        format!("{:?}", output.parsed.dependencies)
    );
    assert_eq!(
        format!("{:?}", ordinary.diagnostics),
        format!("{:?}", output.parsed.diagnostics)
    );
    assert_eq!(
        ordinary.has_top_level_await,
        output.parsed.has_top_level_await
    );
    drop(source);
    let snapshot = output.parsed.module.source().unwrap();
    assert_eq!(
        &snapshot[output.comments[0].span.lo as usize..output.comments[0].span.hi as usize],
        "// 😀"
    );
}

#[test]
fn retains_unterminated_comment_with_parser_diagnostic() {
    let source = "const value = 1; /* incomplete 😀";
    let output = parse_with_comments(
        source,
        &Interner::new(),
        SourceType::Module,
        ParseOptions::default(),
    );
    assert!(output.parsed.has_errors());
    assert_eq!(output.comments.len(), 1);
    assert_eq!(output.comments[0].span.hi as usize, source.len());
}
