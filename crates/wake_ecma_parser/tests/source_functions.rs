use wake_common::{Interner, Span};
use wake_ecma_parser::{ParseOptions, SourceType, parse, parse_source};

#[test]
fn function_regions_exclude_computed_keys_and_keep_erased_headers_and_arrow_bodies() {
    let source = r#"const obj = { [key as typeof outside]<T>(p: typeof input): typeof result { return p; } };
const arrow = <T,>(p: T): T => (p as T);
function overload(p: typeof arg): typeof result;
function overload(p) { return p; }
const comparison = fn < other > / 2;
"#;
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
        parsed.functions.len(),
        4,
        "speculation must not invent functions"
    );
    let text = |span: Span| &source[span.lo as usize..span.hi as usize];
    let method = &parsed.functions[0];
    assert!(text(method.span).starts_with("[key"));
    assert!(text(method.scope).starts_with("<T>(p:"));
    assert_eq!(text(method.body.unwrap()), "{ return p; }");
    assert_eq!(text(parsed.functions[1].body.unwrap()), "(p as T)");
    assert!(parsed.functions[2].body.is_none());
    assert_eq!(text(parsed.functions[3].body.unwrap()), "{ return p; }");
    assert_eq!(
        parsed
            .functions
            .iter()
            .map(|f| f.is_arrow)
            .collect::<Vec<_>>(),
        [false, true, false, false]
    );
    let ordinary = parse(source, &interner, SourceType::TypeScript);
    assert_eq!(
        parsed.parsed.module.structure_hash(),
        ordinary.module.structure_hash()
    );
    assert_eq!(
        format!("{:?}", parsed.parsed.dependencies),
        format!("{:?}", ordinary.dependencies)
    );
    let mut options = ParseOptions::default();
    options
        .transform_features
        .insert(wake_ecma_transform::EcmaFeature::ArrowFunction);
    let lowered = parse_source(source, &interner, SourceType::TypeScript, options);
    assert_eq!(parsed.functions, lowered.functions);
    let ordinary_lowered =
        wake_ecma_parser::parse_with(source, &interner, SourceType::TypeScript, options);
    assert_eq!(
        lowered.parsed.module.structure_hash(),
        ordinary_lowered.module.structure_hash()
    );
}
