//! Pure ECMA compiler backend.
//!
//! This crate owns the process-local parser/interner identity and presents one staged compiler
//! seam without taking ownership of filesystems, module graphs, task engines, caches, sessions, or
//! product policy. Finalization remains an implementation detail of [`CompilerBackend::emit_module`]: a
//! finalized typed arena must never become an independently cached artifact.

#![forbid(unsafe_code)]

use std::collections::HashSet;
use std::error::Error;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use wake_common::{Diagnostic as WakeDiagnostic, Interner, Severity as WakeSeverity, SourceFile};
use wake_ecma_ast::{
    Expression, MemberProperty, ModuleAst, Program, Statement, UnaryOperator, Visit,
    walk_expression, walk_statement,
};
use wake_ecma_codegen::{
    CodegenError, GeneratedModuleRequest as CodegenModuleRequest,
    GeneratedModuleRequestRole as CodegenModuleRequestRole,
    GeneratedModuleRuntimeNames as CodegenRuntimeNames, Mapping as CodegenMapping, ModuleLinker,
    ModuleMappings as CodegenMappings, ModuleSpecifierKind, ModuleSpecifierRewriter,
    PreserveModuleFormat, SourceMap as CodegenSourceMap, codegen_optimized_with_map_and_requests,
    try_codegen_preserved_optimized, try_codegen_preserved_optimized_with_map,
};
use wake_ecma_minify::codegen_bridge::TypedModuleError;
#[cfg(test)]
use wake_ecma_minify::codegen_bridge::TypedModulePhase;
use wake_ecma_minify::{
    ConstVal, LinkerExportLiveness, LinkerExportStar, MinifyDiagnosticKind, NodeOrigin,
    OptimizeDependency, OptimizeInput, OptimizedProgram, SyntheticReason, TrustedExpression,
    TrustedExpressionEdit, ValidatedDefine, optimize, optimize_one_shot,
};
use wake_ecma_parser::{ParseOptions, parse, parse_with};

pub use wake_common::Span;
pub use wake_ecma_ast::SourceType;
pub use wake_ecma_transform::{BrowserTarget, TargetEnv};

/// Stable optimizer implementation identity for caller-owned cache keys.
pub const OPTIMIZER_PIPELINE_VERSION: &str = wake_ecma_minify::PIPELINE_VERSION;
/// Stable parser implementation identity for caller-owned cache keys.
pub const PARSE_PIPELINE_VERSION: &str = wake_ecma_parser::PIPELINE_VERSION;
/// Stable emitter implementation identity for caller-owned cache keys.
pub const EMIT_PIPELINE_VERSION: &str = wake_ecma_codegen::PIPELINE_VERSION;

/// Whether an optimizer result will be retained across generations.
///
/// This changes only fingerprint work. It must not change diagnostics, retained requests, typed
/// finalization, or emitted bytes.
#[derive(Clone, Copy, Debug, Default, Hash, PartialEq, Eq)]
pub enum LifetimeMode {
    #[default]
    Retained,
    OneShot,
}

/// Mapping collection policy for a single module emission.
#[derive(Clone, Copy, Debug, Default, Hash, PartialEq, Eq)]
pub enum MapMode {
    #[default]
    None,
    SourceMap,
}

/// Optimizer-owned module syntax contract.
#[derive(Clone, Copy, Debug, Default, Hash, PartialEq, Eq)]
pub enum ModuleMode {
    #[default]
    PreserveEsm,
    PreserveCommonJs,
    BundledCommonJs,
}

/// Stable request condition profile. Equal specifier bytes with different kinds remain distinct.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub enum ModuleRequestKind {
    StaticImport,
    DynamicImport,
    Require,
}

impl ModuleRequestKind {
    const fn into_minify(self) -> wake_ecma_minify::ModuleRequestKind {
        match self {
            Self::StaticImport => wake_ecma_minify::ModuleRequestKind::StaticImport,
            Self::DynamicImport => wake_ecma_minify::ModuleRequestKind::DynamicImport,
            Self::Require => wake_ecma_minify::ModuleRequestKind::Require,
        }
    }

    const fn from_minify(kind: wake_ecma_minify::ModuleRequestKind) -> Self {
        match kind {
            wake_ecma_minify::ModuleRequestKind::StaticImport => Self::StaticImport,
            wake_ecma_minify::ModuleRequestKind::DynamicImport => Self::DynamicImport,
            wake_ecma_minify::ModuleRequestKind::Require => Self::Require,
        }
    }
}

/// Stable compiler request identity.
#[derive(Clone, Debug, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub struct ModuleRequest {
    pub specifier: String,
    pub kind: ModuleRequestKind,
}

impl ModuleRequest {
    pub fn new(specifier: impl Into<String>, kind: ModuleRequestKind) -> Self {
        Self {
            specifier: specifier.into(),
            kind,
        }
    }
}

/// Parser-level dependency distinction retained for graph construction.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub enum ParsedDependencyKind {
    Import,
    ExportFrom,
    DynamicImport,
    Require,
}

impl ParsedDependencyKind {
    pub const fn request_kind(self) -> ModuleRequestKind {
        match self {
            Self::Import | Self::ExportFrom => ModuleRequestKind::StaticImport,
            Self::DynamicImport => ModuleRequestKind::DynamicImport,
            Self::Require => ModuleRequestKind::Require,
        }
    }
}

impl From<wake_ecma_ast::DependencyKind> for ParsedDependencyKind {
    fn from(kind: wake_ecma_ast::DependencyKind) -> Self {
        match kind {
            wake_ecma_ast::DependencyKind::Import => Self::Import,
            wake_ecma_ast::DependencyKind::ExportFrom => Self::ExportFrom,
            wake_ecma_ast::DependencyKind::DynamicImport => Self::DynamicImport,
            wake_ecma_ast::DependencyKind::Require => Self::Require,
        }
    }
}

/// One parser-owned dependency with stable string and source coordinates.
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct ParsedDependency {
    specifier: String,
    kind: ParsedDependencyKind,
    span: Span,
}

impl ParsedDependency {
    pub fn specifier(&self) -> &str {
        &self.specifier
    }

    pub const fn kind(&self) -> ParsedDependencyKind {
        self.kind
    }

    pub const fn span(&self) -> Span {
        self.span
    }
}

/// Stable diagnostic severity owned by the compiler façade.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub enum DiagnosticSeverity {
    Error,
    Warning,
    Note,
    Help,
}

impl From<WakeSeverity> for DiagnosticSeverity {
    fn from(severity: WakeSeverity) -> Self {
        match severity {
            WakeSeverity::Error => Self::Error,
            WakeSeverity::Warning => Self::Warning,
            WakeSeverity::Note => Self::Note,
            WakeSeverity::Help => Self::Help,
        }
    }
}

/// One source annotation attached to an owned compiler diagnostic.
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct CompilerDiagnosticLabel {
    span: Span,
    message: Option<String>,
    primary: bool,
}

impl CompilerDiagnosticLabel {
    pub const fn span(&self) -> Span {
        self.span
    }

    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }

    pub const fn is_primary(&self) -> bool {
        self.primary
    }
}

/// Parser diagnostic detached from lower-layer diagnostic and interner types.
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct CompilerDiagnostic {
    severity: DiagnosticSeverity,
    code: Option<String>,
    message: String,
    path: Option<String>,
    labels: Vec<CompilerDiagnosticLabel>,
    notes: Vec<String>,
}

impl CompilerDiagnostic {
    pub const fn severity(&self) -> DiagnosticSeverity {
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

    pub fn labels(&self) -> &[CompilerDiagnosticLabel] {
        &self.labels
    }

    pub fn notes(&self) -> &[String] {
        &self.notes
    }

    pub const fn is_error(&self) -> bool {
        matches!(self.severity, DiagnosticSeverity::Error)
    }
}

fn convert_diagnostic(diagnostic: WakeDiagnostic) -> CompilerDiagnostic {
    CompilerDiagnostic {
        severity: diagnostic.severity.into(),
        code: diagnostic.code.map(|code| code.into_owned()),
        message: diagnostic.message,
        path: diagnostic.path,
        labels: diagnostic
            .labels
            .into_iter()
            .map(|label| CompilerDiagnosticLabel {
                span: label.span,
                message: label.message,
                primary: label.primary,
            })
            .collect(),
        notes: diagnostic.notes,
    }
}

/// Module syntax which a single-module façade must handle explicitly before CommonJS lowering.
#[derive(Clone, Copy, Debug, Default, Hash, PartialEq, Eq)]
pub struct ModuleSyntaxFacts {
    has_top_level_await: bool,
    has_import_meta: bool,
    has_import_attributes: bool,
    has_export_star: bool,
}

impl ModuleSyntaxFacts {
    pub const fn has_top_level_await(&self) -> bool {
        self.has_top_level_await
    }

    pub const fn has_import_meta(&self) -> bool {
        self.has_import_meta
    }

    pub const fn has_import_attributes(&self) -> bool {
        self.has_import_attributes
    }

    pub const fn has_export_star(&self) -> bool {
        self.has_export_star
    }
}

struct ModuleSyntaxScanner<'interner> {
    interner: &'interner Interner,
    has_import_meta: bool,
    has_import_attributes: bool,
    has_export_star: bool,
}

impl<'ast> Visit<'ast> for ModuleSyntaxScanner<'_> {
    fn visit_statement(&mut self, statement: &Statement<'ast>) {
        self.has_export_star |= matches!(statement, Statement::ExportAll(_));
        self.has_import_attributes |= match statement {
            Statement::Import(declaration) => declaration.attributes.is_some(),
            Statement::ExportNamed(declaration) => declaration.attributes.is_some(),
            Statement::ExportAll(declaration) => declaration.attributes.is_some(),
            _ => false,
        };
        walk_statement(self, statement);
    }

    fn visit_expression(&mut self, expression: &Expression<'ast>) {
        if let Expression::Import(import) = expression {
            self.has_import_attributes |= import.options.is_some();
        }
        if let Expression::MetaProperty(meta) = expression {
            self.has_import_meta |= self.interner.resolve(meta.meta) == "import"
                && self.interner.resolve(meta.property) == "meta";
        }
        walk_expression(self, expression);
    }
}

/// Borrowed parse request. [`ParsedModule`] owns every source-backed result after parsing. The
/// default target is ESNext, matching the parser's direct entry point.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseInput<'source> {
    source: &'source str,
    source_type: SourceType,
    target: TargetEnv,
    jsx_import_source: &'source str,
    jsx_development: bool,
    file_name: &'source str,
}

impl<'source> ParseInput<'source> {
    pub fn new(source: &'source str, source_type: SourceType) -> Self {
        Self {
            source,
            source_type,
            target: TargetEnv::esnext(),
            jsx_import_source: "react",
            jsx_development: false,
            file_name: "",
        }
    }

    pub fn with_target(mut self, target: TargetEnv) -> Self {
        self.target = target;
        self
    }

    pub fn with_jsx(
        mut self,
        import_source: &'source str,
        development: bool,
        file_name: &'source str,
    ) -> Self {
        self.jsx_import_source = import_source;
        self.jsx_development = development;
        self.file_name = file_name;
        self
    }

    pub fn source(&self) -> &str {
        self.source
    }

    pub const fn source_type(&self) -> SourceType {
        self.source_type
    }

    pub fn target(&self) -> &TargetEnv {
        &self.target
    }
}

impl Hash for ParseInput<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.source.hash(state);
        source_type_tag(self.source_type).hash(state);
        self.target.hash(state);
        self.jsx_import_source.hash(state);
        self.jsx_development.hash(state);
        self.file_name.hash(state);
    }
}

const fn source_type_tag(source_type: SourceType) -> u8 {
    match source_type {
        SourceType::Module => 0,
        SourceType::Script => 1,
        SourceType::TypeScript => 2,
        SourceType::Tsx => 3,
        SourceType::Jsx => 4,
    }
}

/// Parser output tied to one backend's interner identity.
#[derive(Clone)]
pub struct ParsedModule {
    ast: Arc<ModuleAst>,
    dependencies: Vec<ParsedDependency>,
    diagnostics: Vec<CompilerDiagnostic>,
    syntax: ModuleSyntaxFacts,
}

impl ParsedModule {
    pub fn ast(&self) -> &Arc<ModuleAst> {
        &self.ast
    }

    pub fn ast_owner(&self) -> Arc<ModuleAst> {
        self.ast.clone()
    }

    pub fn source(&self) -> &str {
        self.ast
            .source()
            .expect("CompilerBackend only constructs parser-owned modules")
    }

    pub fn dependencies(&self) -> &[ParsedDependency] {
        &self.dependencies
    }

    pub fn diagnostics(&self) -> &[CompilerDiagnostic] {
        &self.diagnostics
    }

    pub fn has_errors(&self) -> bool {
        self.diagnostics.iter().any(CompilerDiagnostic::is_error)
    }

    pub const fn has_top_level_await(&self) -> bool {
        self.syntax.has_top_level_await
    }

    pub const fn syntax(&self) -> ModuleSyntaxFacts {
        self.syntax
    }
}

impl Hash for ParsedModule {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.ast.hash(state);
        self.dependencies.hash(state);
    }
}

impl fmt::Debug for ParsedModule {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ParsedModule")
            .field("source_len", &self.source().len())
            .field("dependencies", &self.dependencies)
            .field("diagnostics", &self.diagnostics)
            .field("syntax", &self.syntax)
            .finish_non_exhaustive()
    }
}

/// Stable options which affect optimizer semantics or diagnostics.
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct OptimizeOptions {
    pub mode: ModuleMode,
    pub minify: bool,
    pub defines: Vec<(String, String)>,
    pub drop_debugger: bool,
    pub drop_console: bool,
    pub reserved_names: Vec<String>,
    pub module_name: Option<String>,
}

impl Default for OptimizeOptions {
    fn default() -> Self {
        Self::preserve_esm()
    }
}

impl OptimizeOptions {
    pub fn preserve_esm() -> Self {
        Self {
            mode: ModuleMode::PreserveEsm,
            minify: false,
            defines: Vec::new(),
            drop_debugger: false,
            drop_console: false,
            reserved_names: Vec::new(),
            module_name: None,
        }
    }

    pub fn preserve_commonjs() -> Self {
        Self {
            mode: ModuleMode::PreserveCommonJs,
            ..Self::preserve_esm()
        }
    }

    pub fn bundled_commonjs() -> Self {
        Self {
            mode: ModuleMode::BundledCommonJs,
            ..Self::preserve_esm()
        }
    }
}

/// Backend-bound, parser-validated define payload. Callers may prepare it once and reuse it for
/// every module with byte-identical raw define options; lower-layer expression owners stay opaque.
#[derive(Clone, Debug)]
pub struct PreparedDefines {
    interner_identity: u64,
    raw: Arc<[(String, String)]>,
    validated: Arc<Vec<ValidatedDefine>>,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct ExportLivenessFacts {
    module_id: u32,
    live_export_names: Vec<String>,
    observed_export_names: Vec<String>,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
enum ExportStarFacts {
    Exact {
        specifier: String,
        names: Vec<String>,
    },
    Runtime {
        specifier: String,
        excluded: Vec<String>,
    },
}

/// Graph/link facts which are stable before optimization and retained-edge convergence.
#[derive(Clone, Debug, Default, Hash, PartialEq, Eq)]
pub struct OptimizeLinkFacts {
    export_liveness: Option<ExportLivenessFacts>,
    export_stars: Vec<ExportStarFacts>,
    internal_esm_requests: Vec<ModuleRequest>,
}

impl OptimizeLinkFacts {
    pub fn set_export_liveness(
        &mut self,
        module_id: u32,
        live_export_names: impl IntoIterator<Item = String>,
        observed_export_names: impl IntoIterator<Item = String>,
    ) {
        self.export_liveness = Some(ExportLivenessFacts {
            module_id,
            live_export_names: canonical_strings(live_export_names),
            observed_export_names: canonical_strings(observed_export_names),
        });
    }

    pub fn clear_export_liveness(&mut self) {
        self.export_liveness = None;
    }

    /// Append one source-ordered exact `export *` plan.
    pub fn add_exact_export_star(
        &mut self,
        specifier: impl Into<String>,
        names: impl IntoIterator<Item = String>,
    ) {
        self.export_stars.push(ExportStarFacts::Exact {
            specifier: specifier.into(),
            names: canonical_strings(names),
        });
    }

    /// Append one source-ordered opaque `export *` fallback plan.
    pub fn add_runtime_export_star(
        &mut self,
        specifier: impl Into<String>,
        excluded: impl IntoIterator<Item = String>,
    ) {
        self.export_stars.push(ExportStarFacts::Runtime {
            specifier: specifier.into(),
            excluded: canonical_strings(excluded),
        });
    }

    pub fn add_internal_esm_request(
        &mut self,
        specifier: impl Into<String>,
        kind: ModuleRequestKind,
    ) {
        let request = ModuleRequest::new(specifier, kind);
        if !self.internal_esm_requests.contains(&request) {
            self.internal_esm_requests.push(request);
            self.internal_esm_requests.sort_unstable();
        }
    }
}

fn canonical_strings(values: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut values: Vec<_> = values.into_iter().collect();
    values.sort_unstable();
    values.dedup();
    values
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct ExpressionReplacement {
    target: Span,
    source: String,
}

/// Owned structural edits produced by an upper-layer transform such as CSS-in-JS.
///
/// Replacement strings are parsed with the backend's own interner before they enter the optimizer;
/// callers cannot manufacture a lower-layer `TrustedExpression`.
#[derive(Clone, Debug, Default, Hash, PartialEq, Eq)]
pub struct TransformEdits {
    expression_replacements: Vec<ExpressionReplacement>,
    statement_removals: Vec<Span>,
    binding_removals: Vec<Span>,
}

impl TransformEdits {
    pub fn replace_expression(&mut self, target: Span, source: impl Into<String>) {
        self.expression_replacements.push(ExpressionReplacement {
            target,
            source: source.into(),
        });
        self.expression_replacements
            .sort_unstable_by_key(|edit| (edit.target.lo, edit.target.hi, edit.source.clone()));
    }

    pub fn remove_statement(&mut self, target: Span) {
        insert_span(&mut self.statement_removals, target);
    }

    pub fn remove_binding(&mut self, target: Span) {
        insert_span(&mut self.binding_removals, target);
    }

    pub fn is_empty(&self) -> bool {
        self.expression_replacements.is_empty()
            && self.statement_removals.is_empty()
            && self.binding_removals.is_empty()
    }
}

fn insert_span(spans: &mut Vec<Span>, target: Span) {
    if spans
        .iter()
        .any(|span| span.lo == target.lo && span.hi == target.hi)
    {
        return;
    }
    spans.push(target);
    spans.sort_unstable_by_key(|span| (span.lo, span.hi));
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
enum FinalModuleTarget {
    External,
    Internal {
        module_id: u32,
        async_dependency: bool,
        dynamic_chunk: Option<u32>,
    },
    RuntimeDynamic {
        request: String,
        expose: Option<String>,
    },
    RuntimeShared {
        request: String,
        scope: String,
    },
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct FinalModuleResolution {
    request: ModuleRequest,
    target: FinalModuleTarget,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct RequestRewrite {
    request: ModuleRequest,
    rewritten_specifier: String,
}

/// Module-local facts available only after retained edges and chunk ownership converge.
///
/// This value is hashable for a caller-owned body cache. The process-local finalized IR produced
/// from it is deliberately private and exists only during [`CompilerBackend::emit_module`].
#[derive(Clone, Debug, Default, Hash, PartialEq, Eq)]
pub struct ModuleFinalizeFacts {
    resolutions: Vec<FinalModuleResolution>,
    rewrites: Vec<RequestRewrite>,
    lower_external_dynamic_to_require: bool,
    no_esmodule: bool,
}

impl ModuleFinalizeFacts {
    pub fn resolve_external(&mut self, specifier: impl Into<String>, kind: ModuleRequestKind) {
        self.set_resolution(
            ModuleRequest::new(specifier, kind),
            FinalModuleTarget::External,
        );
    }

    pub fn resolve_internal(
        &mut self,
        specifier: impl Into<String>,
        kind: ModuleRequestKind,
        module_id: u32,
        async_dependency: bool,
        dynamic_chunk: Option<u32>,
    ) {
        self.set_resolution(
            ModuleRequest::new(specifier, kind),
            FinalModuleTarget::Internal {
                module_id,
                async_dependency,
                dynamic_chunk,
            },
        );
    }

    pub fn resolve_runtime_dynamic(
        &mut self,
        specifier: impl Into<String>,
        request: impl Into<String>,
        expose: Option<String>,
    ) {
        self.set_resolution(
            ModuleRequest::new(specifier, ModuleRequestKind::DynamicImport),
            FinalModuleTarget::RuntimeDynamic {
                request: request.into(),
                expose,
            },
        );
    }

    pub fn resolve_runtime_shared(
        &mut self,
        specifier: impl Into<String>,
        kind: ModuleRequestKind,
        request: impl Into<String>,
        scope: impl Into<String>,
    ) {
        self.set_resolution(
            ModuleRequest::new(specifier, kind),
            FinalModuleTarget::RuntimeShared {
                request: request.into(),
                scope: scope.into(),
            },
        );
    }

    pub fn rewrite_request(
        &mut self,
        specifier: impl Into<String>,
        kind: ModuleRequestKind,
        rewritten_specifier: impl Into<String>,
    ) {
        let rewrite = RequestRewrite {
            request: ModuleRequest::new(specifier, kind),
            rewritten_specifier: rewritten_specifier.into(),
        };
        match self
            .rewrites
            .binary_search_by(|candidate| candidate.request.cmp(&rewrite.request))
        {
            Ok(index) => self.rewrites[index] = rewrite,
            Err(index) => self.rewrites.insert(index, rewrite),
        }
    }

    pub fn set_lower_external_dynamic_to_require(&mut self, enabled: bool) {
        self.lower_external_dynamic_to_require = enabled;
    }

    pub fn set_no_esmodule(&mut self, enabled: bool) {
        self.no_esmodule = enabled;
    }

    pub const fn no_esmodule(&self) -> bool {
        self.no_esmodule
    }

    fn set_resolution(&mut self, request: ModuleRequest, target: FinalModuleTarget) {
        let resolution = FinalModuleResolution { request, target };
        match self
            .resolutions
            .binary_search_by(|candidate| candidate.request.cmp(&resolution.request))
        {
            Ok(index) => self.resolutions[index] = resolution,
            Err(index) => self.resolutions.insert(index, resolution),
        }
    }

    fn resolution(&self, specifier: &str, kind: ModuleRequestKind) -> Option<&FinalModuleTarget> {
        self.resolutions
            .iter()
            .find(|resolution| {
                resolution.request.specifier == specifier && resolution.request.kind == kind
            })
            .map(|resolution| &resolution.target)
    }

    fn rewrite(&self, specifier: &str, kind: ModuleRequestKind) -> Option<&str> {
        self.rewrites
            .iter()
            .find(|rewrite| rewrite.request.specifier == specifier && rewrite.request.kind == kind)
            .map(|rewrite| rewrite.rewritten_specifier.as_str())
    }

    fn validate(&self) -> Result<(), CompilerError> {
        for resolution in &self.resolutions {
            if resolution.request.specifier.is_empty() {
                return Err(CompilerError::new(
                    CompilerErrorKind::InvalidConfiguration,
                    CompilerStage::Finalize,
                    "final module specifier must not be empty",
                ));
            }
            match &resolution.target {
                FinalModuleTarget::RuntimeDynamic { request, expose } => {
                    if resolution.request.kind != ModuleRequestKind::DynamicImport {
                        return Err(CompilerError::new(
                            CompilerErrorKind::InvalidConfiguration,
                            CompilerStage::Finalize,
                            "runtime dynamic target must belong to a dynamic import",
                        ));
                    }
                    if request.is_empty() || expose.as_ref().is_some_and(String::is_empty) {
                        return Err(CompilerError::new(
                            CompilerErrorKind::InvalidConfiguration,
                            CompilerStage::Finalize,
                            "runtime dynamic request and optional expose must be non-empty",
                        ));
                    }
                }
                FinalModuleTarget::RuntimeShared { request, scope }
                    if request.is_empty() || scope.is_empty() =>
                {
                    return Err(CompilerError::new(
                        CompilerErrorKind::InvalidConfiguration,
                        CompilerStage::Finalize,
                        "runtime shared request and scope must be non-empty",
                    ));
                }
                FinalModuleTarget::External
                | FinalModuleTarget::Internal { .. }
                | FinalModuleTarget::RuntimeShared { .. } => {}
            }
        }
        for rewrite in &self.rewrites {
            if rewrite.request.specifier.is_empty() || rewrite.rewritten_specifier.is_empty() {
                return Err(CompilerError::new(
                    CompilerErrorKind::InvalidConfiguration,
                    CompilerStage::Finalize,
                    "request rewrites must use non-empty source and target specifiers",
                ));
            }
        }
        for left in &self.rewrites {
            if !matches!(
                left.request.kind,
                ModuleRequestKind::StaticImport | ModuleRequestKind::DynamicImport
            ) {
                continue;
            }
            if let Some(right) = self.rewrites.iter().find(|right| {
                right.request.specifier == left.request.specifier
                    && right.request.kind != left.request.kind
                    && matches!(
                        right.request.kind,
                        ModuleRequestKind::StaticImport | ModuleRequestKind::DynamicImport
                    )
            }) && right.rewritten_specifier != left.rewritten_specifier
            {
                return Err(CompilerError::new(
                    CompilerErrorKind::InvalidConfiguration,
                    CompilerStage::Finalize,
                    format!(
                        "preserved static and dynamic imports of `{}` cannot use different rewrites",
                        left.request.specifier
                    ),
                ));
            }
        }
        Ok(())
    }
}

/// One module-local source mapping. Generated coordinates are zero-based UTF-16 positions;
/// `source_offset` remains a source byte offset until the upper layer merges bundle maps.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub struct CompilerMapping {
    pub generated_line: u32,
    pub generated_column: u32,
    pub source_index: u32,
    pub source_offset: u32,
    pub name_index: Option<u32>,
    pub is_unmapped: bool,
}

/// Module-local mapping facts emitted in the same token walk as [`CompilerEmission::code`].
#[derive(Clone, Debug, Default, Hash, PartialEq, Eq)]
pub struct CompilerMappings {
    pub mappings: Vec<CompilerMapping>,
    pub names: Vec<String>,
}

impl CompilerMappings {
    pub fn is_empty(&self) -> bool {
        self.mappings.is_empty()
    }

    pub fn len(&self) -> usize {
        self.mappings.len()
    }
}

/// Semantic role of an emitted internal request target.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub enum GeneratedModuleRequestRole {
    Value,
    DiscardedStatic,
}

/// Exact generated byte range of one compiler-owned numeric request target.
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct GeneratedModuleRequest {
    pub start: u32,
    pub end: u32,
    pub target_module_id: u32,
    pub role: GeneratedModuleRequestRole,
    pub request: ModuleRequest,
}

/// Closed set of runtime services referenced by a generated module body.
#[derive(Clone, Debug, Default, Hash, PartialEq, Eq)]
pub struct RuntimeCapabilities {
    pub meta_url: bool,
    pub external_require: bool,
    pub promise_resolve: bool,
    pub object_assign: bool,
    pub object_keys: bool,
    pub object_define_property: bool,
    pub runtime_import: bool,
    pub shared: bool,
}

/// Collision-free runtime binding names paired with the exact emitted body.
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct RuntimeNames {
    pub module: String,
    pub exports: String,
    pub require: String,
    pub capabilities: RuntimeCapabilities,
}

/// Atomic per-module compiler output. Upper layers may wrap or place it, but must keep request
/// ranges and runtime names paired with these exact code bytes.
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct CompilerEmission {
    code: String,
    mappings: Option<CompilerMappings>,
    generated_module_requests: Vec<GeneratedModuleRequest>,
    runtime_names: Option<RuntimeNames>,
}

impl CompilerEmission {
    pub fn code(&self) -> &str {
        &self.code
    }

    pub fn mappings(&self) -> Option<&CompilerMappings> {
        self.mappings.as_ref()
    }

    pub fn generated_module_requests(&self) -> &[GeneratedModuleRequest] {
        &self.generated_module_requests
    }

    pub fn runtime_names(&self) -> Option<&RuntimeNames> {
        self.runtime_names.as_ref()
    }

    /// Serialize this module's detached mappings as a Source Map V3 JSON document.
    ///
    /// Mapping coordinates and lower-layer source-map types stay private to this crate. The
    /// supplied source is used both for `sourcesContent` and byte-offset to UTF-16 conversion.
    pub fn source_map_json(&self, source_name: &str, source_code: &str) -> Option<String> {
        let mappings = self.mappings.as_ref()?;
        let source_file = SourceFile::new(source_name, source_code);
        let mut source_map = CodegenSourceMap::new();
        let source_index = source_map.add_source(source_name, Some(source_code.to_owned()));
        source_map.names = mappings.names.clone();
        source_map.mappings = mappings
            .mappings
            .iter()
            .map(|mapping| CodegenMapping {
                gen_line: mapping.generated_line,
                gen_col: mapping.generated_column,
                src_index: source_index,
                src_offset: mapping.source_offset,
                name_index: mapping.name_index,
                is_unmapped: mapping.is_unmapped,
            })
            .collect();
        Some(source_map.to_json(|index, offset| {
            debug_assert_eq!(index, source_index);
            source_file.location0_utf16(offset)
        }))
    }

    pub fn into_code(self) -> String {
        self.code
    }

    /// Transfer the atomic emission payload without cloning code or detached metadata.
    pub fn into_parts(
        self,
    ) -> (
        String,
        Option<CompilerMappings>,
        Vec<GeneratedModuleRequest>,
        Option<RuntimeNames>,
    ) {
        (
            self.code,
            self.mappings,
            self.generated_module_requests,
            self.runtime_names,
        )
    }
}

/// Compiler stage attached to a fallible façade error.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub enum CompilerStage {
    Parse,
    Configuration,
    Transform,
    Optimize,
    Finalize,
}

/// Stable failure category independent from diagnostic wording and lower-layer implementation
/// details.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub enum CompilerErrorKind {
    InvalidConfiguration,
    SourceTooLarge,
    UnsupportedTransform,
    Internal,
}

/// Owned compiler error. Parser recovery diagnostics remain on [`ParsedModule`].
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct CompilerError {
    kind: CompilerErrorKind,
    stage: CompilerStage,
    message: String,
}

impl CompilerError {
    fn new(kind: CompilerErrorKind, stage: CompilerStage, message: impl Into<String>) -> Self {
        Self {
            kind,
            stage,
            message: message.into(),
        }
    }

    pub const fn kind(&self) -> CompilerErrorKind {
        self.kind
    }

    pub const fn stage(&self) -> CompilerStage {
        self.stage
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for CompilerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{:?}/{:?}: {}",
            self.stage, self.kind, self.message
        )
    }
}

impl Error for CompilerError {}

/// Detached optimizer observability. This value never participates in compiler fingerprints.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OptimizeStatistics {
    iterations: usize,
    passes: Vec<OptimizePassStatistics>,
}

impl OptimizeStatistics {
    pub const fn iterations(&self) -> usize {
        self.iterations
    }

    pub fn passes(&self) -> &[OptimizePassStatistics] {
        &self.passes
    }
}

/// Detached statistics for one optimizer pass. Names are copied out of the lower-layer enum so
/// consumers never depend on optimizer implementation types.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OptimizePassStatistics {
    name: String,
    runs: usize,
    changes: usize,
}

impl OptimizePassStatistics {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub const fn runs(&self) -> usize {
        self.runs
    }

    pub const fn changes(&self) -> usize {
        self.changes
    }
}

/// Opaque optimizer result. The typed arena and finalizer plan never cross this façade.
pub struct OptimizedModule {
    optimized: OptimizedProgram,
    mode: ModuleMode,
    retained_requests: Vec<ModuleRequest>,
    /// Observability only: deliberately excluded from [`Self::fingerprint`] and caller cache keys.
    statistics: OptimizeStatistics,
}

impl OptimizedModule {
    pub const fn mode(&self) -> ModuleMode {
        self.mode
    }

    pub fn retained_requests(&self) -> &[ModuleRequest] {
        &self.retained_requests
    }

    pub const fn statistics(&self) -> &OptimizeStatistics {
        &self.statistics
    }

    pub const fn fingerprint(&self) -> u64 {
        self.optimized.fingerprint()
    }

    pub const fn has_dynamic_scope_hazard(&self) -> bool {
        self.optimized.has_dynamic_scope_hazard()
    }

    pub fn can_emit_sealed_without_finalization(&self, no_esmodule: bool) -> bool {
        self.optimized
            .can_emit_sealed_without_finalization(no_esmodule)
    }
}

impl fmt::Debug for OptimizedModule {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OptimizedModule")
            .field("mode", &self.mode)
            .field("retained_requests", &self.retained_requests)
            .field("fingerprint", &format_args!("{:016x}", self.fingerprint()))
            .finish_non_exhaustive()
    }
}

/// Pure compiler service. Clones share only the string interner; there is no hidden filesystem,
/// task graph, cache, or build-session state.
#[derive(Clone)]
pub struct CompilerBackend {
    interner: Arc<Interner>,
}

impl Default for CompilerBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for CompilerBackend {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CompilerBackend")
            .finish_non_exhaustive()
    }
}

impl CompilerBackend {
    pub fn new() -> Self {
        Self {
            interner: Arc::new(Interner::new()),
        }
    }

    /// Read-only shared interner for upper-layer AST consumers such as CSS analysis.
    pub fn interner(&self) -> &Interner {
        &self.interner
    }

    pub fn interner_owner(&self) -> Arc<Interner> {
        self.interner.clone()
    }

    pub fn parse_module(&self, input: ParseInput<'_>) -> Result<ParsedModule, CompilerError> {
        if input.source.len() > u32::MAX as usize {
            return Err(CompilerError::new(
                CompilerErrorKind::SourceTooLarge,
                CompilerStage::Parse,
                format!(
                    "module contains {} bytes; Wake spans support at most {} bytes",
                    input.source.len(),
                    u32::MAX
                ),
            ));
        }
        let output = parse_with(
            input.source,
            &self.interner,
            input.source_type,
            ParseOptions {
                jsx_import_source: input.jsx_import_source,
                jsx_dev: input.jsx_development,
                file_name: input.file_name,
                transform_features: input.target.required_features(),
            },
        );
        let dependencies = output
            .dependencies
            .iter()
            .map(|dependency| ParsedDependency {
                specifier: self.interner.resolve(dependency.specifier),
                kind: dependency.kind.into(),
                span: dependency.span,
            })
            .collect();
        let syntax = output.module.with_ast(|program| {
            let mut scanner = ModuleSyntaxScanner {
                interner: &self.interner,
                has_import_meta: false,
                has_import_attributes: false,
                has_export_star: false,
            };
            scanner.visit_program(program);
            ModuleSyntaxFacts {
                has_top_level_await: output.has_top_level_await,
                has_import_meta: scanner.has_import_meta,
                has_import_attributes: scanner.has_import_attributes,
                has_export_star: scanner.has_export_star,
            }
        });
        Ok(ParsedModule {
            ast: output.module,
            dependencies,
            diagnostics: output
                .diagnostics
                .into_iter()
                .map(convert_diagnostic)
                .collect(),
            syntax,
        })
    }

    pub fn optimize_module(
        &self,
        parsed: &ParsedModule,
        options: &OptimizeOptions,
        link_facts: &OptimizeLinkFacts,
        edits: &TransformEdits,
        lifetime: LifetimeMode,
    ) -> Result<Arc<OptimizedModule>, CompilerError> {
        let prepared = self.prepare_defines(&options.defines)?;
        self.optimize_module_with_prepared_defines(
            parsed, options, &prepared, link_facts, edits, lifetime,
        )
    }

    /// Parse and validate define configuration once for reuse across module optimizer calls.
    pub fn prepare_defines(
        &self,
        definitions: &[(String, String)],
    ) -> Result<PreparedDefines, CompilerError> {
        Ok(PreparedDefines {
            interner_identity: self.interner.identity(),
            raw: Arc::from(definitions),
            validated: Arc::new(self.validate_defines(definitions)?),
        })
    }

    /// Optimize with backend-bound definitions prepared by [`Self::prepare_defines`].
    pub fn optimize_module_with_prepared_defines(
        &self,
        parsed: &ParsedModule,
        options: &OptimizeOptions,
        prepared: &PreparedDefines,
        link_facts: &OptimizeLinkFacts,
        edits: &TransformEdits,
        lifetime: LifetimeMode,
    ) -> Result<Arc<OptimizedModule>, CompilerError> {
        if prepared.interner_identity != self.interner.identity() {
            return Err(CompilerError::new(
                CompilerErrorKind::InvalidConfiguration,
                CompilerStage::Configuration,
                "prepared defines belong to a different compiler backend",
            ));
        }
        if prepared.raw.as_ref() != options.defines.as_slice() {
            return Err(CompilerError::new(
                CompilerErrorKind::InvalidConfiguration,
                CompilerStage::Configuration,
                "prepared defines do not match the raw optimize options",
            ));
        }
        let mut input = OptimizeInput::new(parsed.source());
        input.minify = options.minify;
        input.drop_console = options.drop_console;
        input.drop_debugger = options.drop_debugger;
        input.module_name = options.module_name.clone();
        input.defines = prepared.validated.as_ref().clone();
        input.reserved_names = options.reserved_names.clone();
        if options.mode == ModuleMode::BundledCommonJs {
            for reserved in ["module", "exports", "__wake_require__"] {
                if !input.reserved_names.iter().any(|name| name == reserved) {
                    input.reserved_names.push(reserved.into());
                }
            }
            input.set_bundled_commonjs(true);
        } else if options.mode == ModuleMode::PreserveCommonJs {
            input.set_preserve_commonjs(true);
        }
        input.set_bundled_internal_esm_dependencies(
            link_facts
                .internal_esm_requests
                .iter()
                .map(|request| (request.specifier.clone(), request.kind.into_minify())),
        );
        input.set_linker_export_stars(link_facts.export_stars.iter().map(|star| match star {
            ExportStarFacts::Exact { specifier, names } => {
                LinkerExportStar::exact(specifier.clone(), names.iter().cloned())
            }
            ExportStarFacts::Runtime {
                specifier,
                excluded,
            } => LinkerExportStar::runtime(specifier.clone(), excluded.iter().cloned()),
        }));
        input.linker_liveness = link_facts.export_liveness.as_ref().map(|liveness| {
            LinkerExportLiveness::from_parts(
                liveness.module_id,
                liveness.live_export_names.iter().cloned(),
                liveness.observed_export_names.iter().cloned(),
            )
        });
        input.dependencies = parsed
            .dependencies
            .iter()
            .map(|dependency| OptimizeDependency {
                specifier: dependency.specifier.clone(),
                kind: dependency.kind.request_kind().into_minify(),
                origin: if dependency.span.is_dummy() {
                    NodeOrigin::Synthetic {
                        anchor: None,
                        reason: SyntheticReason::LoweringGenerated,
                    }
                } else {
                    NodeOrigin::Source(dependency.span)
                },
            })
            .collect();
        for edit in &edits.expression_replacements {
            let parsed_replacement = parse(&edit.source, &self.interner, SourceType::Module);
            if parsed_replacement.has_errors() {
                return Err(CompilerError::new(
                    CompilerErrorKind::InvalidConfiguration,
                    CompilerStage::Transform,
                    format!(
                        "replacement at {}..{} is not valid JavaScript{}",
                        edit.target.lo,
                        edit.target.hi,
                        parse_error_suffix(&parsed_replacement.diagnostics)
                    ),
                ));
            }
            let trusted = TrustedExpressionEdit::from_parsed_program(
                edit.target,
                &parsed_replacement.module,
                &self.interner,
            );
            if !trusted.expression().is_valid() {
                return Err(CompilerError::new(
                    CompilerErrorKind::InvalidConfiguration,
                    CompilerStage::Transform,
                    format!(
                        "replacement at {}..{} must be exactly one lowerable expression",
                        edit.target.lo, edit.target.hi
                    ),
                ));
            }
            input.add_expression_edit(trusted);
        }
        input.extend_statement_removals(edits.statement_removals.iter().copied());
        input.extend_binding_removals(edits.binding_removals.iter().copied());

        let optimized = match lifetime {
            LifetimeMode::Retained => optimize(parsed.ast.clone(), &self.interner, &input),
            LifetimeMode::OneShot => optimize_one_shot(parsed.ast.clone(), &self.interner, &input),
        }
        .map_err(|error| {
            let kind = match error.kind {
                MinifyDiagnosticKind::OptimizerInputMismatch
                | MinifyDiagnosticKind::InvalidTrustedEdit => {
                    CompilerErrorKind::InvalidConfiguration
                }
                MinifyDiagnosticKind::UnsupportedTransform => {
                    CompilerErrorKind::UnsupportedTransform
                }
                MinifyDiagnosticKind::InvalidIr | MinifyDiagnosticKind::DidNotConverge => {
                    CompilerErrorKind::Internal
                }
            };
            CompilerError::new(kind, CompilerStage::Optimize, error.to_string())
        })?;

        let mut seen = HashSet::new();
        let retained_requests = optimized
            .typed_module_plan()
            .requests()
            .iter()
            .filter_map(|request| {
                let request = ModuleRequest::new(
                    request.specifier.clone(),
                    ModuleRequestKind::from_minify(request.kind),
                );
                seen.insert(request.clone()).then_some(request)
            })
            .collect();
        let statistics = OptimizeStatistics {
            iterations: optimized.stats().iterations,
            passes: optimized
                .stats()
                .passes
                .iter()
                .map(|pass| OptimizePassStatistics {
                    name: pass.pass.name().to_owned(),
                    runs: pass.runs,
                    changes: pass.changes,
                })
                .collect(),
        };
        Ok(Arc::new(OptimizedModule {
            optimized,
            mode: options.mode,
            retained_requests,
            statistics,
        }))
    }

    /// Finalize and emit one optimized module. There is intentionally no public standalone
    /// finalization API or final-IR value.
    pub fn emit_module(
        &self,
        optimized: &OptimizedModule,
        facts: &ModuleFinalizeFacts,
        map_mode: MapMode,
    ) -> Result<CompilerEmission, CompilerError> {
        facts.validate()?;
        match optimized.mode {
            ModuleMode::PreserveEsm => {
                self.emit_preserved(optimized, facts, PreserveModuleFormat::EsModule, map_mode)
            }
            ModuleMode::PreserveCommonJs => {
                self.emit_preserved(optimized, facts, PreserveModuleFormat::CommonJs, map_mode)
            }
            ModuleMode::BundledCommonJs => {
                let linker = FinalLinker { facts };
                // Bundled callers need request ranges and runtime names even when they do not
                // consume a source map. The lower layer produces all three in one token walk.
                let (code, mappings, requests, runtime_names) =
                    codegen_optimized_with_map_and_requests(
                        &optimized.optimized,
                        &self.interner,
                        &linker,
                        facts.no_esmodule,
                    );
                Ok(CompilerEmission {
                    code,
                    mappings: (map_mode == MapMode::SourceMap).then(|| convert_mappings(mappings)),
                    generated_module_requests: requests
                        .into_iter()
                        .map(convert_generated_request)
                        .collect(),
                    runtime_names: Some(convert_runtime_names(runtime_names)),
                })
            }
        }
    }

    fn emit_preserved(
        &self,
        optimized: &OptimizedModule,
        facts: &ModuleFinalizeFacts,
        format: PreserveModuleFormat,
        map_mode: MapMode,
    ) -> Result<CompilerEmission, CompilerError> {
        let rewriter = FinalSpecifierRewriter { facts };
        let (code, mappings) = match map_mode {
            MapMode::None => (
                try_codegen_preserved_optimized(
                    &optimized.optimized,
                    &self.interner,
                    format,
                    &rewriter,
                )
                .map_err(compiler_error_from_codegen)?,
                None,
            ),
            MapMode::SourceMap => {
                let (code, mappings) = try_codegen_preserved_optimized_with_map(
                    &optimized.optimized,
                    &self.interner,
                    format,
                    &rewriter,
                )
                .map_err(compiler_error_from_codegen)?;
                (code, Some(convert_mappings(mappings)))
            }
        };
        Ok(CompilerEmission {
            code,
            mappings,
            generated_module_requests: Vec::new(),
            runtime_names: None,
        })
    }

    fn validate_defines(
        &self,
        definitions: &[(String, String)],
    ) -> Result<Vec<ValidatedDefine>, CompilerError> {
        let mut seen = HashSet::new();
        let mut validated = Vec::with_capacity(definitions.len());
        for (key, value) in definitions {
            if key.is_empty() || key.trim() != key || !seen.insert(key.clone()) {
                return Err(CompilerError::new(
                    CompilerErrorKind::InvalidConfiguration,
                    CompilerStage::Configuration,
                    format!("define key `{key}` must be a unique, non-empty static member chain"),
                ));
            }
            let key_source = format!("const __wake_define_key__=({key});");
            let parsed_key = parse(&key_source, &self.interner, SourceType::Module);
            if parsed_key.has_errors()
                || !parsed_key.module.with_ast(|program| {
                    define_initializer(program).is_some_and(is_static_define_key)
                })
            {
                return Err(CompilerError::new(
                    CompilerErrorKind::InvalidConfiguration,
                    CompilerStage::Configuration,
                    format!(
                        "define key `{key}` must be an identifier, meta-property, or dot member chain{}",
                        parse_error_suffix(&parsed_key.diagnostics)
                    ),
                ));
            }
            if value.trim().is_empty() {
                return Err(CompilerError::new(
                    CompilerErrorKind::InvalidConfiguration,
                    CompilerStage::Configuration,
                    format!("define `{key}` has an empty expression"),
                ));
            }
            let value_source = format!("const __wake_define_value__=({value});");
            let parsed_value = parse(&value_source, &self.interner, SourceType::Script);
            if parsed_value.has_errors() {
                return Err(CompilerError::new(
                    CompilerErrorKind::InvalidConfiguration,
                    CompilerStage::Configuration,
                    format!(
                        "define `{key}` is not one valid JavaScript expression{}",
                        parse_error_suffix(&parsed_value.diagnostics)
                    ),
                ));
            }
            let Some(primitive) = parsed_value.module.with_ast(|program| {
                define_initializer(program)
                    .map(|expression| primitive_define_value(expression, &self.interner))
            }) else {
                return Err(CompilerError::new(
                    CompilerErrorKind::InvalidConfiguration,
                    CompilerStage::Configuration,
                    format!("define `{key}` is not one valid JavaScript expression"),
                ));
            };
            if let Some(primitive) = primitive {
                validated.push(ValidatedDefine::primitive(key.clone(), primitive));
                continue;
            }
            let expression_source = format!("({value})");
            let parsed_expression = parse(&expression_source, &self.interner, SourceType::Script);
            let expression =
                TrustedExpression::from_parsed_program(&parsed_expression.module, &self.interner);
            if !expression.is_valid() {
                return Err(CompilerError::new(
                    CompilerErrorKind::InvalidConfiguration,
                    CompilerStage::Configuration,
                    format!("define `{key}` could not be lowered into one owned expression"),
                ));
            }
            validated.push(ValidatedDefine::expression(key.clone(), expression));
        }
        Ok(validated)
    }
}

fn compiler_error_from_codegen(error: CodegenError) -> CompilerError {
    let kind = match &error {
        CodegenError::ModuleFinalization(TypedModuleError::Unsupported { .. }) => {
            CompilerErrorKind::UnsupportedTransform
        }
        CodegenError::ModuleModeMismatch { .. }
        | CodegenError::ModuleFinalization(
            TypedModuleError::InvalidInput { .. }
            | TypedModuleError::StaleAnalysis { .. }
            | TypedModuleError::StalePlan { .. }
            | TypedModuleError::PendingRequests { .. }
            | TypedModuleError::Ir(_),
        ) => CompilerErrorKind::Internal,
    };
    CompilerError::new(kind, CompilerStage::Finalize, error.to_string())
}

struct FinalSpecifierRewriter<'a> {
    facts: &'a ModuleFinalizeFacts,
}

impl ModuleSpecifierRewriter for FinalSpecifierRewriter<'_> {
    fn rewrite(&self, specifier: &str) -> Option<String> {
        self.rewrite_with_kind(specifier, ModuleSpecifierKind::Import)
    }

    fn rewrite_with_kind(&self, specifier: &str, kind: ModuleSpecifierKind) -> Option<String> {
        match kind {
            ModuleSpecifierKind::Import => self
                .facts
                .rewrite(specifier, ModuleRequestKind::StaticImport)
                .or_else(|| {
                    self.facts
                        .rewrite(specifier, ModuleRequestKind::DynamicImport)
                }),
            ModuleSpecifierKind::Require => {
                self.facts.rewrite(specifier, ModuleRequestKind::Require)
            }
        }
        .map(str::to_owned)
    }

    fn lower_dynamic_import_to_require(&self) -> bool {
        self.facts.lower_external_dynamic_to_require
    }
}

struct FinalLinker<'a> {
    facts: &'a ModuleFinalizeFacts,
}

impl ModuleLinker for FinalLinker<'_> {
    fn module_id(
        &self,
        specifier: &str,
        kind: wake_ecma_codegen::ModuleRequestKind,
    ) -> Option<u32> {
        let kind = ModuleRequestKind::from_minify(kind);
        match self.facts.resolution(specifier, kind) {
            Some(FinalModuleTarget::Internal { module_id, .. }) => Some(*module_id),
            Some(
                FinalModuleTarget::External
                | FinalModuleTarget::RuntimeDynamic { .. }
                | FinalModuleTarget::RuntimeShared { .. },
            )
            | None => None,
        }
    }

    fn dynamic_chunk(&self, specifier: &str) -> Option<u32> {
        match self
            .facts
            .resolution(specifier, ModuleRequestKind::DynamicImport)
        {
            Some(FinalModuleTarget::Internal { dynamic_chunk, .. }) => *dynamic_chunk,
            _ => None,
        }
    }

    fn runtime_dynamic_import(&self, specifier: &str) -> Option<String> {
        match self
            .facts
            .resolution(specifier, ModuleRequestKind::DynamicImport)
        {
            Some(FinalModuleTarget::RuntimeDynamic { request, .. }) => Some(request.clone()),
            _ => None,
        }
    }

    fn runtime_dynamic_import_expose(&self, specifier: &str) -> Option<String> {
        match self
            .facts
            .resolution(specifier, ModuleRequestKind::DynamicImport)
        {
            Some(FinalModuleTarget::RuntimeDynamic { expose, .. }) => expose.clone(),
            _ => None,
        }
    }

    fn runtime_shared_module(
        &self,
        specifier: &str,
        kind: wake_ecma_codegen::ModuleRequestKind,
    ) -> Option<(String, String)> {
        match self
            .facts
            .resolution(specifier, ModuleRequestKind::from_minify(kind))
        {
            Some(FinalModuleTarget::RuntimeShared { request, scope }) => {
                Some((request.clone(), scope.clone()))
            }
            _ => None,
        }
    }

    fn is_async_module(&self, id: u32) -> bool {
        self.facts.resolutions.iter().any(|resolution| {
            matches!(
                resolution.target,
                FinalModuleTarget::Internal {
                    module_id,
                    async_dependency: true,
                    ..
                } if module_id == id
            )
        })
    }
}

fn convert_mappings(mappings: CodegenMappings) -> CompilerMappings {
    CompilerMappings {
        mappings: mappings.mappings.into_iter().map(convert_mapping).collect(),
        names: mappings.names,
    }
}

const fn convert_mapping(mapping: CodegenMapping) -> CompilerMapping {
    CompilerMapping {
        generated_line: mapping.gen_line,
        generated_column: mapping.gen_col,
        source_index: mapping.src_index,
        source_offset: mapping.src_offset,
        name_index: mapping.name_index,
        is_unmapped: mapping.is_unmapped,
    }
}

fn convert_generated_request(request: CodegenModuleRequest) -> GeneratedModuleRequest {
    GeneratedModuleRequest {
        start: request.start,
        end: request.end,
        target_module_id: request.target_module_id,
        role: match request.role {
            CodegenModuleRequestRole::Value => GeneratedModuleRequestRole::Value,
            CodegenModuleRequestRole::DiscardedStatic => {
                GeneratedModuleRequestRole::DiscardedStatic
            }
        },
        request: ModuleRequest::new(
            request.specifier,
            ModuleRequestKind::from_minify(request.kind),
        ),
    }
}

fn convert_runtime_names(names: CodegenRuntimeNames) -> RuntimeNames {
    RuntimeNames {
        module: names.module,
        exports: names.exports,
        require: names.require,
        capabilities: RuntimeCapabilities {
            meta_url: names.capabilities.meta_url,
            external_require: names.capabilities.external_require,
            promise_resolve: names.capabilities.promise_resolve,
            object_assign: names.capabilities.object_assign,
            object_keys: names.capabilities.object_keys,
            object_define_property: names.capabilities.object_define_property,
            runtime_import: names.capabilities.runtime_import,
            shared: names.capabilities.shared,
        },
    }
}

fn define_initializer<'ast>(program: &'ast Program<'ast>) -> Option<Expression<'ast>> {
    if program.body.len() != 1 {
        return None;
    }
    let Statement::VariableDeclaration(declaration) = program.body[0] else {
        return None;
    };
    if declaration.declarations.len() != 1 {
        return None;
    }
    declaration.declarations[0].init
}

fn is_static_define_key(expression: Expression<'_>) -> bool {
    match expression {
        Expression::Identifier(_) | Expression::MetaProperty(_) => true,
        Expression::Member(member) if !member.optional => {
            matches!(member.property, MemberProperty::Ident(_))
                && is_static_define_key(member.object)
        }
        _ => false,
    }
}

fn primitive_define_value(expression: Expression<'_>, interner: &Interner) -> Option<ConstVal> {
    match expression {
        Expression::NumberLiteral(literal) => Some(ConstVal::Num(literal.value)),
        Expression::StringLiteral(literal) => Some(ConstVal::Str(interner.resolve(literal.value))),
        Expression::BooleanLiteral(literal) => Some(ConstVal::Bool(literal.value)),
        Expression::NullLiteral(_) => Some(ConstVal::Null),
        Expression::Identifier(identifier) => match interner.resolve(identifier.name).as_str() {
            "undefined" => Some(ConstVal::Undefined),
            "NaN" => Some(ConstVal::Num(f64::NAN)),
            "Infinity" => Some(ConstVal::Num(f64::INFINITY)),
            _ => None,
        },
        Expression::Unary(unary) => {
            let argument = primitive_define_value(unary.argument, interner)?;
            match (unary.operator, argument) {
                (UnaryOperator::Minus, ConstVal::Num(value)) => Some(ConstVal::Num(-value)),
                (UnaryOperator::Plus, ConstVal::Num(value)) => Some(ConstVal::Num(value)),
                (UnaryOperator::LogicalNot, value) => Some(ConstVal::Bool(!value.truthy())),
                (UnaryOperator::Void, _) => Some(ConstVal::Undefined),
                _ => None,
            }
        }
        _ => None,
    }
}

fn parse_error_suffix(diagnostics: &[WakeDiagnostic]) -> String {
    diagnostics
        .iter()
        .find(|diagnostic| diagnostic.is_error())
        .map(|diagnostic| format!(": {}", diagnostic.message))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codegen_unsupported_module_failure_keeps_its_structured_kind() {
        let error = compiler_error_from_codegen(CodegenError::ModuleFinalization(
            TypedModuleError::Unsupported {
                phase: TypedModulePhase::Finalize,
                node: None,
                message: "fixture".into(),
            },
        ));

        assert_eq!(error.kind(), CompilerErrorKind::UnsupportedTransform);
        assert_eq!(error.stage(), CompilerStage::Finalize);
    }

    #[test]
    fn codegen_mode_mismatch_is_an_internal_invariant_failure() {
        let error = compiler_error_from_codegen(CodegenError::ModuleModeMismatch {
            expected: PreserveModuleFormat::EsModule,
            actual: "fixture".into(),
        });

        assert_eq!(error.kind(), CompilerErrorKind::Internal);
        assert_eq!(error.stage(), CompilerStage::Finalize);
    }

    #[test]
    fn optimized_module_exposes_owned_pipeline_statistics() {
        let backend = CompilerBackend::new();
        let parsed = backend
            .parse_module(ParseInput::new(
                "const unused=1;export const answer=40+2;",
                SourceType::Module,
            ))
            .expect("parse module");
        let mut options = OptimizeOptions::bundled_commonjs();
        options.minify = true;
        let optimized = backend
            .optimize_module(
                &parsed,
                &options,
                &OptimizeLinkFacts::default(),
                &TransformEdits::default(),
                LifetimeMode::Retained,
            )
            .expect("fixture must optimize");

        let statistics = optimized.statistics();
        assert!(statistics.iterations() > 0);
        assert!(!statistics.passes().is_empty());
        assert!(
            statistics
                .passes()
                .iter()
                .all(|pass| !pass.name().is_empty())
        );
        assert!(statistics.passes().iter().any(|pass| pass.runs() > 0));
    }

    #[test]
    fn prepared_defines_are_bound_to_raw_options_and_backend_identity() {
        let backend = CompilerBackend::new();
        let parsed = backend
            .parse_module(ParseInput::new(
                "export const answer=DEBUG?1:2;",
                SourceType::Module,
            ))
            .expect("parse module");
        let mut options = OptimizeOptions::bundled_commonjs();
        options.defines = vec![("DEBUG".into(), "false".into())];
        let prepared = backend
            .prepare_defines(&options.defines)
            .expect("valid definitions");
        backend
            .optimize_module_with_prepared_defines(
                &parsed,
                &options,
                &prepared,
                &OptimizeLinkFacts::default(),
                &TransformEdits::default(),
                LifetimeMode::Retained,
            )
            .expect("matching prepared definitions");

        let mut changed = options.clone();
        changed.defines[0].1 = "true".into();
        let mismatch = backend
            .optimize_module_with_prepared_defines(
                &parsed,
                &changed,
                &prepared,
                &OptimizeLinkFacts::default(),
                &TransformEdits::default(),
                LifetimeMode::Retained,
            )
            .expect_err("raw options must match prepared definitions");
        assert_eq!(mismatch.stage(), CompilerStage::Configuration);
        assert_eq!(mismatch.kind(), CompilerErrorKind::InvalidConfiguration);

        let other_backend = CompilerBackend::new();
        let other_parsed = other_backend
            .parse_module(ParseInput::new(
                "export const answer=DEBUG?1:2;",
                SourceType::Module,
            ))
            .expect("parse module");
        let mismatch = other_backend
            .optimize_module_with_prepared_defines(
                &other_parsed,
                &options,
                &prepared,
                &OptimizeLinkFacts::default(),
                &TransformEdits::default(),
                LifetimeMode::Retained,
            )
            .expect_err("prepared definitions must not cross backend identities");
        assert_eq!(mismatch.stage(), CompilerStage::Configuration);
        assert_eq!(mismatch.kind(), CompilerErrorKind::InvalidConfiguration);
    }

    #[test]
    fn prepare_defines_owns_validation_and_typed_values() {
        let backend = CompilerBackend::new();
        let prepared = backend
            .prepare_defines(&[
                ("DEBUG".into(), "false".into()),
                ("process.env.MODE".into(), "'production'".into()),
                ("CONFIG".into(), "{answer: 42}".into()),
            ])
            .expect("valid definitions");

        assert!(matches!(
            prepared.validated[0].value,
            wake_ecma_minify::ValidatedDefineValue::Primitive(ConstVal::Bool(false))
        ));
        assert!(matches!(
            &prepared.validated[1].value,
            wake_ecma_minify::ValidatedDefineValue::Primitive(ConstVal::Str(value))
                if value == "production"
        ));
        assert!(matches!(
            &prepared.validated[2].value,
            wake_ecma_minify::ValidatedDefineValue::Expression(value)
                if value.source() == "{answer: 42}"
        ));
    }

    #[test]
    fn prepare_defines_rejects_non_static_keys_and_statement_injection() {
        let backend = CompilerBackend::new();
        assert!(
            backend
                .prepare_defines(&[("config[key]".into(), "1".into())])
                .is_err()
        );
        assert!(
            backend
                .prepare_defines(&[("DEBUG".into(), "false);globalThis.injected=true".into())])
                .is_err()
        );
        assert!(
            backend
                .prepare_defines(&[
                    ("DEBUG".into(), "true".into()),
                    ("DEBUG".into(), "false".into()),
                ])
                .is_err()
        );
    }

    #[test]
    fn compiler_emission_transfers_its_owned_parts() {
        let backend = CompilerBackend::new();
        let parsed = backend
            .parse_module(ParseInput::new(
                "export const answer=42;",
                SourceType::Module,
            ))
            .expect("parse module");
        let optimized = backend
            .optimize_module(
                &parsed,
                &OptimizeOptions::bundled_commonjs(),
                &OptimizeLinkFacts::default(),
                &TransformEdits::default(),
                LifetimeMode::Retained,
            )
            .expect("optimize");
        let emission = backend
            .emit_module(
                &optimized,
                &ModuleFinalizeFacts::default(),
                MapMode::SourceMap,
            )
            .expect("emit");
        let (code, mappings, requests, runtime_names) = emission.into_parts();
        assert!(!code.is_empty());
        assert!(mappings.is_some());
        assert!(requests.is_empty());
        assert!(runtime_names.is_some());
    }
}
