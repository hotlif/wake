use std::any::type_name;

use wake_compiler::{
    AutomaticJsxOptions, BrowserTarget, Diagnostic, DiagnosticLabel, Language, ModuleOutput,
    ModuleRequest, ModuleRequestKind, ModuleRequestOrigin, Severity, SourceMapArtifact,
    SourceMapMode, SourceText, Target, TextRange, TranspileError, TranspileErrorKind,
    TranspileOptions, TranspileOutput, transpile_module,
};

type TranspileFn = for<'source> fn(
    SourceText<'source>,
    &TranspileOptions,
) -> Result<TranspileOutput, TranspileError>;

#[test]
fn public_api_snapshot_is_an_owned_facade() {
    let _: TranspileFn = transpile_module;
    let options = TranspileOptions::new(Language::TypeScript)
        .with_target(Target::Browsers(vec![BrowserTarget::new("chrome", "120")]))
        .with_jsx(AutomaticJsxOptions::development().with_import_source("react"))
        .with_module_output(ModuleOutput::CommonJs)
        .with_source_map(SourceMapMode::Detached);
    assert_eq!(options.language(), Language::TypeScript);
    assert!(matches!(options.target(), Target::Browsers(_)));
    assert!(
        options
            .jsx()
            .is_some_and(AutomaticJsxOptions::is_development)
    );
    assert_eq!(options.module_output(), ModuleOutput::CommonJs);
    assert_eq!(options.source_map(), SourceMapMode::Detached);

    let mut actual = [
        type_name::<SourceText<'static>>(),
        type_name::<Language>(),
        type_name::<ModuleOutput>(),
        type_name::<SourceMapMode>(),
        type_name::<BrowserTarget>(),
        type_name::<Target>(),
        type_name::<AutomaticJsxOptions>(),
        type_name::<TranspileOptions>(),
        type_name::<Severity>(),
        type_name::<TextRange>(),
        type_name::<DiagnosticLabel>(),
        type_name::<Diagnostic>(),
        type_name::<ModuleRequestKind>(),
        type_name::<ModuleRequestOrigin>(),
        type_name::<ModuleRequest>(),
        type_name::<SourceMapArtifact>(),
        type_name::<TranspileOutput>(),
        type_name::<TranspileErrorKind>(),
        type_name::<TranspileError>(),
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    actual.extend([
        format!(
            "Language::{:?}",
            [Language::JavaScript, Language::TypeScript]
        ),
        format!(
            "ModuleOutput::{:?}",
            [ModuleOutput::PreserveEsm, ModuleOutput::CommonJs]
        ),
        format!(
            "SourceMapMode::{:?}",
            [SourceMapMode::None, SourceMapMode::Detached]
        ),
        format!(
            "Severity::{:?}",
            [
                Severity::Error,
                Severity::Warning,
                Severity::Note,
                Severity::Help,
            ]
        ),
        format!(
            "ModuleRequestKind::{:?}",
            [
                ModuleRequestKind::Import,
                ModuleRequestKind::ExportFrom,
                ModuleRequestKind::DynamicImport,
                ModuleRequestKind::Require,
            ]
        ),
        format!(
            "TranspileErrorKind::{:?}",
            [
                TranspileErrorKind::InvalidOptions,
                TranspileErrorKind::SourceTooLarge,
                TranspileErrorKind::Syntax,
                TranspileErrorKind::UnsupportedTransform,
                TranspileErrorKind::Internal,
            ]
        ),
    ]);
    let actual = actual.join("\n");
    let snapshot = include_str!("fixtures/public-api-v1.txt").replace("\r\n", "\n");
    assert_eq!(actual, snapshot.trim_end());
}

#[test]
fn default_configuration_and_output_match_the_golden() {
    let options = TranspileOptions::new(Language::JavaScript);
    assert!(matches!(options.target(), Target::ModernBaseline));
    assert!(options.jsx().is_none());
    assert_eq!(options.module_output(), ModuleOutput::PreserveEsm);
    assert_eq!(options.source_map(), SourceMapMode::None);

    let output = transpile_module(
        SourceText::new("answer.js", "export const answer = 42;\n"),
        &options,
    )
    .expect("default JavaScript module should transpile");
    let golden = include_str!("fixtures/default-output.js");
    let golden = golden
        .strip_suffix("\r\n")
        .or_else(|| golden.strip_suffix('\n'))
        .unwrap_or(golden);
    assert_eq!(output.code(), golden);
    assert!(output.source_map().is_none());
    assert!(output.module_requests().is_empty());
    assert!(output.diagnostics().is_empty());
}

#[test]
fn clean_external_consumer_needs_only_wake_compiler_types() {
    fn compile(source: &str) -> Result<String, TranspileError> {
        let output = transpile_module(
            SourceText::new("consumer.tsx", source),
            &TranspileOptions::new(Language::TypeScript)
                .with_jsx(AutomaticJsxOptions::production()),
        )?;
        Ok(output.code().to_owned())
    }

    let code = compile("export const App = (): JSX.Element => <main />;")
        .expect("an external crate can consume only the facade");
    assert!(code.contains("react/jsx-runtime"), "{code}");
}
