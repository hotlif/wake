//! Owned, single-module JavaScript/TypeScript/React JSX compiler facade.

use std::fmt;

use wake_compiler_core::{
    BrowserTarget as CoreBrowserTarget, CompilerBackend, CompilerDiagnostic,
    CompilerDiagnosticLabel, CompilerErrorKind, DiagnosticSeverity as CoreDiagnosticSeverity,
    LifetimeMode, MapMode, ModuleFinalizeFacts, OptimizeLinkFacts, OptimizeOptions, ParseInput,
    ParsedDependencyKind, SourceType, TargetEnv, TransformEdits,
};

/// Borrowed input for one module compilation. All returned artifacts are owned.
#[derive(Clone, Copy, Debug)]
pub struct SourceText<'source> {
    name: &'source str,
    code: &'source str,
}

impl<'source> SourceText<'source> {
    pub const fn new(name: &'source str, code: &'source str) -> Self {
        Self { name, code }
    }

    pub const fn name(&self) -> &'source str {
        self.name
    }

    pub const fn code(&self) -> &'source str {
        self.code
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Language {
    JavaScript,
    TypeScript,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum ModuleOutput {
    #[default]
    PreserveEsm,
    CommonJs,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum SourceMapMode {
    #[default]
    None,
    Detached,
}

/// One normalized browser target. Query parsing remains outside the compiler facade.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BrowserTarget {
    name: String,
    version: String,
}

impl BrowserTarget {
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn version(&self) -> &str {
        &self.version
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum Target {
    #[default]
    ModernBaseline,
    EsNext,
    Browsers(Vec<BrowserTarget>),
}

/// React-compatible automatic JSX runtime configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AutomaticJsxOptions {
    development: bool,
    import_source: String,
}

impl AutomaticJsxOptions {
    pub fn production() -> Self {
        Self {
            development: false,
            import_source: "react".into(),
        }
    }

    pub fn development() -> Self {
        Self {
            development: true,
            import_source: "react".into(),
        }
    }

    pub fn with_import_source(mut self, import_source: impl Into<String>) -> Self {
        self.import_source = import_source.into();
        self
    }

    pub const fn is_development(&self) -> bool {
        self.development
    }

    pub fn import_source(&self) -> &str {
        &self.import_source
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranspileOptions {
    language: Language,
    target: Target,
    jsx: Option<AutomaticJsxOptions>,
    module_output: ModuleOutput,
    source_map: SourceMapMode,
}

impl TranspileOptions {
    pub fn new(language: Language) -> Self {
        Self {
            language,
            target: Target::default(),
            jsx: None,
            module_output: ModuleOutput::default(),
            source_map: SourceMapMode::default(),
        }
    }

    pub fn with_target(mut self, target: Target) -> Self {
        self.target = target;
        self
    }

    pub fn with_jsx(mut self, jsx: AutomaticJsxOptions) -> Self {
        self.jsx = Some(jsx);
        self
    }

    pub fn with_module_output(mut self, output: ModuleOutput) -> Self {
        self.module_output = output;
        self
    }

    pub fn with_source_map(mut self, source_map: SourceMapMode) -> Self {
        self.source_map = source_map;
        self
    }

    pub const fn language(&self) -> Language {
        self.language
    }

    pub const fn target(&self) -> &Target {
        &self.target
    }

    pub const fn jsx(&self) -> Option<&AutomaticJsxOptions> {
        self.jsx.as_ref()
    }

    pub const fn module_output(&self) -> ModuleOutput {
        self.module_output
    }

    pub const fn source_map(&self) -> SourceMapMode {
        self.source_map
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Severity {
    Error,
    Warning,
    Note,
    Help,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextRange {
    start: u32,
    end: u32,
}

impl TextRange {
    pub const fn new(start: u32, end: u32) -> Self {
        Self { start, end }
    }

    pub const fn start(&self) -> u32 {
        self.start
    }

    pub const fn end(&self) -> u32 {
        self.end
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiagnosticLabel {
    range: TextRange,
    message: Option<String>,
    primary: bool,
}

impl DiagnosticLabel {
    pub const fn range(&self) -> TextRange {
        self.range
    }

    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }

    pub const fn is_primary(&self) -> bool {
        self.primary
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    severity: Severity,
    code: Option<String>,
    message: String,
    path: Option<String>,
    labels: Vec<DiagnosticLabel>,
    notes: Vec<String>,
}

impl Diagnostic {
    pub const fn severity(&self) -> Severity {
        self.severity
    }

    pub fn code(&self) -> Option<&str> {
        self.code.as_deref()
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn path(&self) -> Option<&str> {
        self.path.as_deref()
    }

    pub fn labels(&self) -> &[DiagnosticLabel] {
        &self.labels
    }

    pub fn notes(&self) -> &[String] {
        &self.notes
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ModuleRequestKind {
    Import,
    ExportFrom,
    DynamicImport,
    Require,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ModuleRequestOrigin {
    Source(TextRange),
    Synthetic,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModuleRequest {
    specifier: String,
    kind: ModuleRequestKind,
    origin: ModuleRequestOrigin,
}

impl ModuleRequest {
    pub fn specifier(&self) -> &str {
        &self.specifier
    }

    pub const fn kind(&self) -> ModuleRequestKind {
        self.kind
    }

    pub const fn origin(&self) -> ModuleRequestOrigin {
        self.origin
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceMapArtifact {
    json: String,
}

impl SourceMapArtifact {
    pub fn json(&self) -> &str {
        &self.json
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranspileOutput {
    code: String,
    source_map: Option<SourceMapArtifact>,
    module_requests: Vec<ModuleRequest>,
    diagnostics: Vec<Diagnostic>,
}

impl TranspileOutput {
    pub fn code(&self) -> &str {
        &self.code
    }

    pub const fn source_map(&self) -> Option<&SourceMapArtifact> {
        self.source_map.as_ref()
    }

    pub fn module_requests(&self) -> &[ModuleRequest] {
        &self.module_requests
    }

    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum TranspileErrorKind {
    InvalidOptions,
    SourceTooLarge,
    Syntax,
    UnsupportedTransform,
    Internal,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranspileError {
    kind: TranspileErrorKind,
    message: String,
    diagnostics: Vec<Diagnostic>,
}

impl TranspileError {
    pub const fn kind(&self) -> TranspileErrorKind {
        self.kind
    }

    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }
}

impl fmt::Display for TranspileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for TranspileError {}

/// Transpile one JavaScript or TypeScript module through Wake's sole owned compiler pipeline.
///
/// The public surface deliberately returns only owned values. Parsing arenas, interner atoms,
/// optimizer IR, finalization facts and source-map implementation types are dropped before this
/// function returns.
pub fn transpile_module(
    source: SourceText<'_>,
    options: &TranspileOptions,
) -> Result<TranspileOutput, TranspileError> {
    validate_options(options)?;

    let backend = CompilerBackend::new();
    let source_type = match (options.language, options.jsx.is_some()) {
        (Language::JavaScript, false) => SourceType::Module,
        (Language::JavaScript, true) => SourceType::Jsx,
        (Language::TypeScript, false) => SourceType::TypeScript,
        (Language::TypeScript, true) => SourceType::Tsx,
    };
    let mut parse_input =
        ParseInput::new(source.code, source_type).with_target(core_target(&options.target));
    if let Some(jsx) = &options.jsx {
        parse_input = parse_input.with_jsx(jsx.import_source(), jsx.development, source.name);
    }
    let parsed = backend
        .parse_module(parse_input)
        .map_err(map_compiler_error)?;
    let diagnostics = parsed
        .diagnostics()
        .iter()
        .map(|diagnostic| convert_diagnostic(diagnostic, source.name))
        .collect::<Vec<_>>();
    if parsed.has_errors() {
        return Err(TranspileError {
            kind: TranspileErrorKind::Syntax,
            message: format!("{} could not be parsed", source.name),
            diagnostics,
        });
    }

    if options.module_output == ModuleOutput::CommonJs {
        reject_unsupported_commonjs(&parsed, source.name)?;
    }

    let mut optimize_options = match options.module_output {
        ModuleOutput::PreserveEsm => OptimizeOptions::preserve_esm(),
        ModuleOutput::CommonJs => OptimizeOptions::preserve_commonjs(),
    };
    // Transpilation performs syntax/module lowering only. It never opts into the minifying fixed
    // point, even if lower-layer defaults change in the future.
    optimize_options.minify = false;
    optimize_options.module_name = Some(source.name.to_owned());
    let optimized = backend
        .optimize_module(
            &parsed,
            &optimize_options,
            &OptimizeLinkFacts::default(),
            &TransformEdits::default(),
            LifetimeMode::OneShot,
        )
        .map_err(map_compiler_error)?;
    let map_mode = match options.source_map {
        SourceMapMode::None => MapMode::None,
        SourceMapMode::Detached => MapMode::SourceMap,
    };
    let emission = backend
        .emit_module(&optimized, &ModuleFinalizeFacts::default(), map_mode)
        .map_err(map_compiler_error)?;
    let source_map = emission
        .source_map_json(source.name, source.code)
        .map(|json| SourceMapArtifact { json });
    let module_requests = parsed
        .dependencies()
        .iter()
        .map(|dependency| {
            let span = dependency.span();
            ModuleRequest {
                specifier: dependency.specifier().to_owned(),
                kind: match dependency.kind() {
                    ParsedDependencyKind::Import => ModuleRequestKind::Import,
                    ParsedDependencyKind::ExportFrom => ModuleRequestKind::ExportFrom,
                    ParsedDependencyKind::DynamicImport => ModuleRequestKind::DynamicImport,
                    ParsedDependencyKind::Require => ModuleRequestKind::Require,
                },
                origin: if span.is_dummy() {
                    ModuleRequestOrigin::Synthetic
                } else {
                    ModuleRequestOrigin::Source(TextRange::new(span.lo, span.hi))
                },
            }
        })
        .collect();

    Ok(TranspileOutput {
        code: emission.into_code(),
        source_map,
        module_requests,
        diagnostics,
    })
}

fn validate_options(options: &TranspileOptions) -> Result<(), TranspileError> {
    if let Some(jsx) = &options.jsx
        && (jsx.import_source.trim().is_empty() || jsx.import_source.trim() != jsx.import_source)
    {
        return Err(TranspileError::new(
            TranspileErrorKind::InvalidOptions,
            "automatic JSX import source must be non-empty and have no surrounding whitespace",
        ));
    }
    if let Target::Browsers(targets) = &options.target {
        for target in targets {
            if target.name.trim().is_empty()
                || target.version.trim().is_empty()
                || target.name.trim() != target.name
                || target.version.trim() != target.version
            {
                return Err(TranspileError::new(
                    TranspileErrorKind::InvalidOptions,
                    "browser target names and versions must be non-empty and normalized",
                ));
            }
        }
    }
    Ok(())
}

fn core_target(target: &Target) -> TargetEnv {
    match target {
        Target::ModernBaseline => TargetEnv::baseline(),
        Target::EsNext => TargetEnv::esnext(),
        Target::Browsers(targets) => TargetEnv::new(
            targets
                .iter()
                .map(|target| CoreBrowserTarget::new(&target.name, &target.version))
                .collect(),
        ),
    }
}

fn reject_unsupported_commonjs(
    parsed: &wake_compiler_core::ParsedModule,
    source_name: &str,
) -> Result<(), TranspileError> {
    let syntax = parsed.syntax();
    let unsupported = if syntax.has_top_level_await() {
        Some((
            "WAKE_CJS_TOP_LEVEL_AWAIT",
            "top-level await requires a graph/runtime-owned async module wrapper",
        ))
    } else if syntax.has_import_attributes() {
        Some((
            "WAKE_CJS_IMPORT_ATTRIBUTES",
            "import attributes do not have a reliable single-module CommonJS lowering",
        ))
    } else if syntax.has_import_meta() {
        Some((
            "WAKE_CJS_IMPORT_META",
            "import.meta requires an embedding-runtime policy in CommonJS output",
        ))
    } else if syntax.has_export_star() {
        Some((
            "WAKE_CJS_EXPORT_STAR",
            "export * requires graph-owned export resolution before CommonJS lowering",
        ))
    } else {
        None
    };
    let Some((code, message)) = unsupported else {
        return Ok(());
    };
    Err(TranspileError {
        kind: TranspileErrorKind::UnsupportedTransform,
        message: message.to_owned(),
        diagnostics: vec![Diagnostic {
            severity: Severity::Error,
            code: Some(code.to_owned()),
            message: message.to_owned(),
            path: Some(source_name.to_owned()),
            labels: Vec::new(),
            notes: Vec::new(),
        }],
    })
}

fn convert_diagnostic(diagnostic: &CompilerDiagnostic, source_name: &str) -> Diagnostic {
    Diagnostic {
        severity: match diagnostic.severity() {
            CoreDiagnosticSeverity::Error => Severity::Error,
            CoreDiagnosticSeverity::Warning => Severity::Warning,
            CoreDiagnosticSeverity::Note => Severity::Note,
            CoreDiagnosticSeverity::Help => Severity::Help,
        },
        code: diagnostic.code().map(str::to_owned),
        message: diagnostic.message().to_owned(),
        path: Some(diagnostic.path().unwrap_or(source_name).to_owned()),
        labels: diagnostic.labels().iter().map(convert_label).collect(),
        notes: diagnostic.notes().to_vec(),
    }
}

fn convert_label(label: &CompilerDiagnosticLabel) -> DiagnosticLabel {
    let span = label.span();
    DiagnosticLabel {
        range: TextRange::new(span.lo, span.hi),
        message: label.message().map(str::to_owned),
        primary: label.is_primary(),
    }
}

fn map_compiler_error(error: wake_compiler_core::CompilerError) -> TranspileError {
    let kind = match error.kind() {
        CompilerErrorKind::InvalidConfiguration => TranspileErrorKind::InvalidOptions,
        CompilerErrorKind::SourceTooLarge => TranspileErrorKind::SourceTooLarge,
        CompilerErrorKind::UnsupportedTransform => TranspileErrorKind::UnsupportedTransform,
        CompilerErrorKind::Internal => TranspileErrorKind::Internal,
    };
    TranspileError::new(kind, error.message())
}

impl TranspileError {
    fn new(kind: TranspileErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            diagnostics: Vec::new(),
        }
    }
}
