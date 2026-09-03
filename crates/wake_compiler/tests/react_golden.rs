use wake_compiler::{
    AutomaticJsxOptions, Language, SourceText, TranspileOptions, transpile_module,
};

const SOURCE: &str = "export const View = (name: string) => <section>{name}</section>;";

#[test]
fn production_react_output_matches_the_owned_facade_golden() {
    assert_react_golden(
        AutomaticJsxOptions::production(),
        include_str!("fixtures/react-production.js"),
    );
}

#[test]
fn development_react_output_matches_the_owned_facade_golden() {
    assert_react_golden(
        AutomaticJsxOptions::development(),
        include_str!("fixtures/react-development.js"),
    );
}

fn assert_react_golden(jsx: AutomaticJsxOptions, expected: &str) {
    let output = transpile_module(
        SourceText::new("src/view.tsx", SOURCE),
        &TranspileOptions::new(Language::TypeScript).with_jsx(jsx),
    )
    .expect("React TSX fixture must transpile");
    let expected = expected.replace("\r\n", "\n");
    let expected = expected
        .strip_suffix("\r\n")
        .or_else(|| expected.strip_suffix('\n'))
        .unwrap_or(&expected);

    assert_eq!(output.code(), expected);
}
