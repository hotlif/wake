use wake_common::Interner;
use wake_ecma_parser::{ParseOptions, SourceType, parse_source};

#[test]
fn committed_tokens_cover_js_ts_jsx_without_lookahead_or_lowering_artifacts() {
    for (source, ty, expected) in [
        (
            "const x: Box<Box<T>> = value >> 1;",
            SourceType::TypeScript,
            vec![
                "const", "x", ":", "Box", "<", "Box", "<", "T", ">", ">", "=", "value", ">>", "1",
                ";",
            ],
        ),
        (
            "const el = <foo-bar data-id=\"中文\"> // text\n{value /* real */}<C /></foo-bar>;",
            SourceType::Jsx,
            vec![
                "const",
                "el",
                "=",
                "<",
                "foo-bar",
                "data-id",
                "=",
                "\"中文\"",
                ">",
                " // text\n",
                "{",
                "value",
                "}",
                "<",
                "C",
                "/",
                ">",
                "<",
                "/",
                "foo-bar",
                ">",
                ";",
            ],
        ),
        (
            "const x = <>abc{/* ok */}<UI.Item /></>;",
            SourceType::Jsx,
            vec![
                "const", "x", "=", "<", ">", "abc", "{", "}", "<", "UI", ".", "Item", "/", ">",
                "<", "/", ">", ";",
            ],
        ),
        (
            "fn<T> / 2; a < b > c;",
            SourceType::TypeScript,
            vec![
                "fn", "<", "T", ">", "/", "2", ";", "a", "<", "b", ">", "c", ";",
            ],
        ),
        (
            "const x = `a${/x/.test(v)}z`;",
            SourceType::Module,
            vec![
                "const", "x", "=", "`a${", "/x/", ".", "test", "(", "v", ")", "}z`", ";",
            ],
        ),
    ] {
        let interner = Interner::new();
        let parsed = parse_source(source, &interner, ty, ParseOptions::default());
        assert!(
            !parsed.parsed.has_errors(),
            "{source}: {:?}",
            parsed.parsed.diagnostics
        );
        let tokens: Vec<_> = parsed
            .tokens
            .iter()
            .map(|token| &source[token.span.lo as usize..token.span.hi as usize])
            .collect();
        assert_eq!(tokens, expected, "{source}");
        assert!(
            parsed
                .tokens
                .windows(2)
                .all(|pair| pair[0].span.hi <= pair[1].span.lo)
        );
        for comment in &parsed.comments {
            assert!(
                parsed
                    .tokens
                    .iter()
                    .all(|token| token.span.hi <= comment.span.lo
                        || token.span.lo >= comment.span.hi)
            );
        }
    }
}

#[test]
fn token_context_and_unicode_newlines_preserve_the_original_snapshot() {
    use wake_ecma_parser::SourceTokenContext;
    let source = "// prefix\r\nconst\u{2028}变量 = <X title=\"y\"> z </X>;";
    let interner = Interner::new();
    let parsed = parse_source(source, &interner, SourceType::Tsx, ParseOptions::default());
    assert!(!parsed.parsed.has_errors());
    let variable = parsed
        .tokens
        .iter()
        .find(|token| &source[token.span.lo as usize..token.span.hi as usize] == "变量")
        .unwrap();
    assert!(variable.newline_before);
    assert_eq!(variable.context, SourceTokenContext::JavaScript);
    let title = parsed
        .tokens
        .iter()
        .find(|token| &source[token.span.lo as usize..token.span.hi as usize] == "\"y\"")
        .unwrap();
    assert_eq!(title.context, SourceTokenContext::JsxTag);
    assert!(
        parsed
            .tokens
            .iter()
            .any(|token| token.context == SourceTokenContext::JsxText)
    );
}

#[test]
fn instantiation_expression_followed_by_division_matches_ordinary_parse() {
    for source in [
        "fn<T> / 2;",
        "fn<Box<T>> / 2;",
        "fn<T> /= 2;",
        "a > /x/.test(b);",
        "fn<T>();",
    ] {
        let interner = Interner::new();
        let plain = wake_ecma_parser::parse_with(
            source,
            &interner,
            SourceType::TypeScript,
            ParseOptions::default(),
        );
        let captured = parse_source(
            source,
            &interner,
            SourceType::TypeScript,
            ParseOptions::default(),
        );
        assert!(!plain.has_errors(), "{source}: {:?}", plain.diagnostics);
        assert!(
            !captured.parsed.has_errors(),
            "{source}: {:?}",
            captured.parsed.diagnostics
        );
        assert_eq!(
            plain.module.with_ast(|p| format!("{p:?}")),
            captured.parsed.module.with_ast(|p| format!("{p:?}"))
        );
    }
}

#[test]
fn expression_start_relexes_regex_after_control_heads_without_changing_division() {
    for source in [
        "if (x) /test/.exec(x);",
        "while (x) /test/.exec(x);",
        "for (;;) /x/.test(y);",
        "if (x) /=/.test(x);",
        "call() / 2; (value) / other; value /= 2;",
    ] {
        let interner = Interner::new();
        let parsed = wake_ecma_parser::parse_with(
            source,
            &interner,
            SourceType::Module,
            ParseOptions::default(),
        );
        assert!(!parsed.has_errors(), "{source}: {:?}", parsed.diagnostics);
        let captured = parse_source(
            source,
            &interner,
            SourceType::Module,
            ParseOptions::default(),
        );
        assert!(!captured.parsed.has_errors(), "{source}");
        assert_eq!(
            parsed.module.with_ast(|p| format!("{p:?}")),
            captured.parsed.module.with_ast(|p| format!("{p:?}"))
        );
    }
}

#[test]
fn source_tokens_and_comments_cover_every_nontrivia_byte_and_preserve_compilation() {
    let sources = [
        "#!/usr/bin/env wake\nconst π = /x/.test('x'); // 😀\n",
        "const x = <A {...props} key={id} title={<B/>}>{...items}{/* c */}<svg:path /></A>;",
        "const f = <T extends object,>(x: T): T => x; f<A<B>>(value); left < right > other;",
        "type A<T> = { [P in keyof T]?: T[P] }; interface B { (a: string): void }; const b = value! satisfies B;",
        "import type { T } from 'pkg'; export type { T }; export const x = (value as T);",
        "class C<T> { readonly x?: T; m<U>(v: U) { return v; } }",
        "const x = `a${`${v}x`}b`; const y = <><C /> tail <UI.X.Y /></>;",
        "if (x) /test/.exec(x); const y = a < b / c > d;",
        "declare module 'ambient' { import { T } from 'pkg'; export interface X { x: T } } import { value } from 'real';",
        "declare function f<T>(x: T): void; declare class C { value: any; method(): void; } declare const x: C;",
    ];
    for source in sources {
        let interner = Interner::new();
        let captured = parse_source(source, &interner, SourceType::Tsx, ParseOptions::default());
        let plain = wake_ecma_parser::parse_with(
            source,
            &interner,
            SourceType::Tsx,
            ParseOptions::default(),
        );
        assert!(
            !captured.parsed.has_errors(),
            "{source}: {:?}",
            captured.parsed.diagnostics
        );
        assert_eq!(
            plain.module.with_ast(|p| format!("{p:?}")),
            captured.parsed.module.with_ast(|p| format!("{p:?}")),
            "{source}"
        );
        assert_eq!(plain.dependencies, captured.parsed.dependencies);
        let mut spans: Vec<_> = captured
            .tokens
            .iter()
            .map(|token| token.span)
            .chain(captured.comments.iter().map(|comment| comment.span))
            .collect();
        spans.sort_by_key(|span| span.lo);
        let mut end = 0;
        for span in spans {
            assert!(span.lo >= end, "overlapping token at {span:?}: {source}");
            assert!(
                source[end as usize..span.lo as usize]
                    .chars()
                    .all(char::is_whitespace),
                "unrecorded token before {span:?}: {source}"
            );
            assert!(
                source.is_char_boundary(span.lo as usize)
                    && source.is_char_boundary(span.hi as usize)
            );
            end = span.hi;
        }
        assert!(
            source[end as usize..].chars().all(char::is_whitespace),
            "unrecorded end: {source}"
        );
    }
}
