use wake_common::Interner;
use wake_ecma_parser::{ParseOptions, SourceNodeKind, SourceType, parse_source, parse_with};

#[test]
fn rejects_mismatched_jsx_closing_tags_in_both_parse_paths() {
    for source in [
        "<div></span>",
        "<UI.A></UI.B>",
        "<x:a></x:b>",
        "<></div>",
        "<div></>",
        "<a><b></a></b>",
    ] {
        let interner = Interner::new();
        let captured = parse_source(source, &interner, SourceType::Tsx, ParseOptions::default());
        let ordinary = parse_with(source, &interner, SourceType::Tsx, ParseOptions::default());
        assert!(captured.parsed.has_errors(), "{source}");
        assert_eq!(
            format!("{:?}", captured.parsed.diagnostics),
            format!("{:?}", ordinary.diagnostics)
        );
    }
    for source in ["<div></div>", "<UI.A></UI.A>", "<x:a></x:a>", "<><b /></>"] {
        assert!(
            !parse_source(
                source,
                &Interner::new(),
                SourceType::Tsx,
                ParseOptions::default()
            )
            .parsed
            .has_errors(),
            "{source}"
        );
    }
}

#[test]
fn retains_original_jsx_structure_duplicates_and_raw_values() {
    let source = "const el = <UI.Button<T> key='a' key={id} disabled {...props}>\n  &amp; 😀 {/* comment */}<span />{items.map(x => <b>{x}</b>)}\n</UI.Button>;";
    let output = parse_source(
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
    let nodes = &output.syntax;
    let text = |index: usize| &source[nodes[index].span.lo as usize..nodes[index].span.hi as usize];
    let elements: Vec<_> = nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| n.kind == SourceNodeKind::JsxElement)
        .map(|(i, _)| text(i))
        .collect();
    assert_eq!(
        elements,
        [
            "<UI.Button<T> key='a' key={id} disabled {...props}>\n  &amp; 😀 {/* comment */}<span />{items.map(x => <b>{x}</b>)}\n</UI.Button>",
            "<span />",
            "<b>{x}</b>",
        ]
    );
    let attrs: Vec<_> = nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| n.kind == SourceNodeKind::JsxAttribute)
        .map(|(i, _)| text(i))
        .collect();
    assert_eq!(attrs, ["key='a'", "key={id}", "disabled"]);
    assert!(
        nodes
            .iter()
            .enumerate()
            .any(|(i, n)| n.kind == SourceNodeKind::JsxText && text(i) == "\n  &amp; 😀 ")
    );
    assert!(
        nodes
            .iter()
            .enumerate()
            .any(|(i, n)| n.kind == SourceNodeKind::JsxExpressionContainer
                && text(i) == "{/* comment */}")
    );
    assert!(
        nodes
            .iter()
            .enumerate()
            .any(|(i, n)| n.kind == SourceNodeKind::JsxSpreadAttribute && text(i) == "{...props}")
    );
    assert!(
        nodes
            .iter()
            .enumerate()
            .any(|(i, n)| n.kind == SourceNodeKind::JsxAttributeValue && text(i) == "'a'")
    );
    for (i, node) in nodes.iter().enumerate() {
        if let Some(parent) = node.parent {
            assert!(parent < i);
            assert!(
                nodes[parent].span.lo <= node.span.lo && nodes[parent].span.hi >= node.span.hi,
                "{node:?} outside {:?}",
                nodes[parent]
            );
        }
    }
}

#[test]
fn fragment_and_namespaced_names_retain_exact_ranges() {
    let source = "const el = <><x:tag data-value='&lt;' /> text </>;";
    let output = parse_source(
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
    let names: Vec<_> = output
        .syntax
        .iter()
        .filter(|n| n.kind == SourceNodeKind::JsxName)
        .map(|n| &source[n.span.lo as usize..n.span.hi as usize])
        .collect();
    assert_eq!(names, ["x:tag", "data-value"]);
    assert!(
        output
            .syntax
            .iter()
            .any(|n| n.kind == SourceNodeKind::JsxFragment
                && &source[n.span.lo as usize..n.span.hi as usize]
                    == "<><x:tag data-value='&lt;' /> text </>")
    );
}

#[test]
fn source_capture_preserves_runtime_ast_and_comments() {
    let interner = Interner::new();
    let source = "// head\nconst fn = <T,>(x: T) => <div a={x}>{x}</div>;";
    let output = parse_source(source, &interner, SourceType::Tsx, ParseOptions::default());
    let ordinary = parse_with(source, &interner, SourceType::Tsx, ParseOptions::default());
    assert_eq!(
        ordinary.module.with_ast(|p| format!("{p:?}")),
        output.parsed.module.with_ast(|p| format!("{p:?}"))
    );
    assert_eq!(
        format!("{:?}", ordinary.diagnostics),
        format!("{:?}", output.parsed.diagnostics)
    );
    assert_eq!(
        format!("{:?}", ordinary.dependencies),
        format!("{:?}", output.parsed.dependencies)
    );
    assert_eq!(output.comments.len(), 1);
    assert_eq!(
        output
            .syntax
            .iter()
            .filter(|n| n.kind == SourceNodeKind::JsxElement)
            .count(),
        1
    );
}
