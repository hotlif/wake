use wake_common::Interner;
use wake_ecma_parser::{ParseOptions, SourceType, parse, parse_source};

#[test]
fn source_arrays_keep_elements_before_spread_lowering_and_exclude_jsx_children_arrays() {
    let source = "const view=<><A/><B/></>; const list=[<A/>,, ...rest, (ok ? <B/> : <C/>)]; const [item=<D/>]=data;";
    let interner = Interner::new();
    let parsed = parse_source(source, &interner, SourceType::Tsx, ParseOptions::default());
    assert!(
        !parsed.parsed.has_errors(),
        "{:?}",
        parsed.parsed.diagnostics
    );
    let array = parsed
        .arrays
        .iter()
        .find(|array| source[array.span.lo as usize..array.span.hi as usize].starts_with("[<A/>"))
        .unwrap();
    assert_eq!(
        parsed.arrays.len(),
        1,
        "binding patterns and synthetic JSX children are not array expressions"
    );
    assert_eq!(array.elements.len(), 4);
    assert!(array.elements[1].is_none());
    assert!(array.elements[2].as_ref().unwrap().spread);
    let expressions = array
        .elements
        .iter()
        .flatten()
        .map(|element| &source[element.expression.lo as usize..element.expression.hi as usize])
        .collect::<Vec<_>>();
    assert_eq!(expressions, ["<A/>", "rest", "ok ? <B/> : <C/>"]);
    assert!(
        parsed
            .arrays
            .iter()
            .all(|array| source.as_bytes()[array.span.lo as usize] == b'[')
    );
    let ordinary = parse(source, &interner, SourceType::Tsx);
    assert_eq!(
        ordinary.module.structure_hash(),
        parsed.parsed.module.structure_hash()
    );
    assert_eq!(
        format!("{:?}", ordinary.dependencies),
        format!("{:?}", parsed.parsed.dependencies)
    );
    let mut features = wake_ecma_transform::FeatureSet::default();
    features.insert(wake_ecma_transform::EcmaFeature::Spread);
    let options = ParseOptions {
        transform_features: features,
        ..Default::default()
    };
    let lowered = parse_source(source, &interner, SourceType::Tsx, options);
    let ordinary_lowered =
        wake_ecma_parser::parse_with(source, &interner, SourceType::Tsx, options);
    assert_eq!(
        parsed.arrays, lowered.arrays,
        "source facts survive actual spread lowering"
    );
    assert_eq!(
        ordinary_lowered.module.structure_hash(),
        lowered.parsed.module.structure_hash()
    );
    assert!(
        lowered
            .parsed
            .module
            .with_ast(|program| program.spread_helper.is_some())
    );
}
