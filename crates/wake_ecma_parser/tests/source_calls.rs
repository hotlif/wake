use wake_common::{Interner, Span};
use wake_ecma_ast::SourceCallKind;
use wake_ecma_parser::{ParseOptions, SourceType, parse, parse_source};

fn text(source: &str, span: Span) -> &str {
    &source[span.lo as usize..span.hi as usize]
}

#[test]
fn original_calls_and_templates_keep_erased_arguments_and_exclude_generated_calls() {
    let source = "const result = obj?.method?.<T>(input as T, ...rest); new Factory<T>(seed!); new Factory; const tagged = tagger<T>`x ${value as T} ${other!}`; const plain = `${foo()} ${x as T}`; const view = <div onClick={() => handler()}>{render()}</div>; namespace N { export const result = compute(); } enum E { A = init() } const lazy = import('x', {with:{type:'json'}}); type X = import('x').X;";
    let interner = Interner::new();
    let parsed = parse_source(source, &interner, SourceType::Tsx, ParseOptions::default());
    assert!(
        !parsed.parsed.has_errors(),
        "{:?}",
        parsed.parsed.diagnostics
    );
    let calls: Vec<_> = parsed
        .calls
        .iter()
        .map(|call| {
            (
                call.kind,
                text(source, call.span),
                text(source, call.head),
                call.optional,
                call.arguments
                    .iter()
                    .map(|span| text(source, *span))
                    .collect::<Vec<_>>(),
            )
        })
        .collect();
    assert_eq!(
        calls,
        [
            (
                SourceCallKind::Call,
                "obj?.method?.<T>(input as T, ...rest)",
                "obj?.method?.<T>",
                true,
                vec!["input as T", "...rest"]
            ),
            (
                SourceCallKind::Construct,
                "new Factory<T>(seed!)",
                "Factory<T>",
                false,
                vec!["seed!"]
            ),
            (
                SourceCallKind::Construct,
                "new Factory",
                "Factory",
                false,
                vec![]
            ),
            (
                SourceCallKind::TaggedTemplate,
                "tagger<T>`x ${value as T} ${other!}`",
                "tagger<T>",
                false,
                vec!["value as T", "other!"]
            ),
            (SourceCallKind::Call, "foo()", "foo", false, vec![]),
            (SourceCallKind::Call, "handler()", "handler", false, vec![]),
            (SourceCallKind::Call, "render()", "render", false, vec![]),
            (SourceCallKind::Call, "compute()", "compute", false, vec![]),
            (SourceCallKind::Call, "init()", "init", false, vec![]),
            (
                SourceCallKind::DynamicImport,
                "import('x', {with:{type:'json'}})",
                "import",
                false,
                vec!["'x'", "{with:{type:'json'}}"]
            ),
        ]
    );
    let templates: Vec<_> = parsed
        .templates
        .iter()
        .map(|template| {
            (
                text(source, template.span),
                template.tagged,
                template
                    .expressions
                    .iter()
                    .map(|span| text(source, *span))
                    .collect::<Vec<_>>(),
            )
        })
        .collect();
    assert_eq!(
        templates,
        [
            (
                "`x ${value as T} ${other!}`",
                true,
                vec!["value as T", "other!"]
            ),
            ("`${foo()} ${x as T}`", false, vec!["foo()", "x as T"])
        ]
    );
    let ordinary = parse(source, &interner, SourceType::Tsx);
    assert!(!ordinary.has_errors());
    assert_eq!(
        parsed.parsed.module.structure_hash(),
        ordinary.module.structure_hash()
    );
}

#[test]
fn dynamic_import_arguments_keep_trailing_commas_and_in_expressions_in_for_headers() {
    let source = "for (import('x', key in options,);;){break;} import('x',);";
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
            .calls
            .iter()
            .map(|call| text(source, call.span))
            .collect::<Vec<_>>(),
        ["import('x', key in options,)", "import('x',)"]
    );
    assert_eq!(
        parsed.calls[0]
            .arguments
            .iter()
            .map(|span| text(source, *span))
            .collect::<Vec<_>>(),
        ["'x'", "key in options"]
    );
    let ordinary = parse(source, &interner, SourceType::TypeScript);
    assert!(!ordinary.has_errors());
    assert_eq!(
        parsed.parsed.module.structure_hash(),
        ordinary.module.structure_hash()
    );
}

#[test]
fn constructor_tags_and_speculative_parameters_preserve_each_committed_call_once() {
    let source = "const f = <T,>(x: T = make<T>()) => x; const g = (x: T = make<T>()) => x; new tag<T>`x ${make()}`(); new new Factory()();";
    let interner = Interner::new();
    let parsed = parse_source(source, &interner, SourceType::Tsx, ParseOptions::default());
    assert!(
        !parsed.parsed.has_errors(),
        "{:?}",
        parsed.parsed.diagnostics
    );
    assert_eq!(
        parsed
            .calls
            .iter()
            .map(|call| text(source, call.span))
            .collect::<Vec<_>>(),
        [
            "make<T>()",
            "make<T>()",
            "make()",
            "tag<T>`x ${make()}`",
            "new tag<T>`x ${make()}`()",
            "new Factory()",
            "new new Factory()()"
        ]
    );
    assert_eq!(
        parsed.calls[3].head.lo,
        source.find("tag<T>").unwrap() as u32
    );
    assert_eq!(parsed.templates.len(), 1);
}
