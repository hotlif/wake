//! Build-facing entry point for the owned, Closure-style optimization pipeline.
//!
//! The result contains no parser AST, span-indexed rewrite table, or compatibility mangle plan.
//! The frozen parser tree is consumed exactly once and all later state is owned typed IR.

use std::fmt;
use std::sync::Arc;

use wake_common::{FxHashSet, Interner, Span};
use wake_ecma_ast::{ModuleAst, Program, SourceType, Statement};

use crate::{ConstVal, ModuleRequestKind};

impl From<wake_ecma_ast::DependencyKind> for ModuleRequestKind {
    fn from(kind: wake_ecma_ast::DependencyKind) -> Self {
        match kind {
            wake_ecma_ast::DependencyKind::Import | wake_ecma_ast::DependencyKind::ExportFrom => {
                Self::StaticImport
            }
            wake_ecma_ast::DependencyKind::DynamicImport => Self::DynamicImport,
            wake_ecma_ast::DependencyKind::Require => Self::Require,
        }
    }
}

/// Bump whenever pass semantics, ordering, or fingerprint inputs change.
pub const PIPELINE_VERSION: &str = "wake-closure-minifier-v15";
pub const MAX_FIXED_POINT_ITERATIONS: usize = 100;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OptimizationPass {
    ApplyTrustedEdits,
    BuildSemanticModel,
    ConstantPropagationAndFolding,
    SimplifyControlFlow,
    InlineClosedFunctions,
    InlineSingleUseVariables,
    EliminateDeadCode,
    MergeStatements,
    LatePeephole,
    MangleProperties,
    ReuseVariableSlots,
    MangleIdentifiers,
}

impl OptimizationPass {
    pub const fn name(self) -> &'static str {
        match self {
            Self::ApplyTrustedEdits => "apply-trusted-edits",
            Self::BuildSemanticModel => "build-semantic-model",
            Self::ConstantPropagationAndFolding => "constant-propagation-and-folding",
            Self::SimplifyControlFlow => "simplify-control-flow",
            Self::InlineClosedFunctions => "inline-closed-functions",
            Self::InlineSingleUseVariables => "inline-single-use-variables",
            Self::EliminateDeadCode => "eliminate-dead-code",
            Self::MergeStatements => "merge-statements",
            Self::LatePeephole => "late-peephole",
            Self::MangleProperties => "mangle-properties",
            Self::ReuseVariableSlots => "reuse-variable-slots",
            Self::MangleIdentifiers => "mangle-identifiers",
        }
    }
}

pub const ONE_TIME_PASS_ORDER: &[OptimizationPass] = &[
    OptimizationPass::ApplyTrustedEdits,
    OptimizationPass::BuildSemanticModel,
];
pub const FIXED_POINT_PASS_ORDER: &[OptimizationPass] = &[
    OptimizationPass::ConstantPropagationAndFolding,
    OptimizationPass::SimplifyControlFlow,
    OptimizationPass::InlineClosedFunctions,
    OptimizationPass::InlineSingleUseVariables,
    OptimizationPass::EliminateDeadCode,
    OptimizationPass::MergeStatements,
    OptimizationPass::LatePeephole,
];
pub const FINAL_PASS_ORDER: &[OptimizationPass] = &[
    OptimizationPass::MangleProperties,
    OptimizationPass::ReuseVariableSlots,
    OptimizationPass::MangleIdentifiers,
];
const ALL_PASS_ORDER: &[OptimizationPass] = &[
    OptimizationPass::ApplyTrustedEdits,
    OptimizationPass::BuildSemanticModel,
    OptimizationPass::ConstantPropagationAndFolding,
    OptimizationPass::SimplifyControlFlow,
    OptimizationPass::InlineClosedFunctions,
    OptimizationPass::InlineSingleUseVariables,
    OptimizationPass::EliminateDeadCode,
    OptimizationPass::MergeStatements,
    OptimizationPass::LatePeephole,
    OptimizationPass::MangleProperties,
    OptimizationPass::ReuseVariableSlots,
    OptimizationPass::MangleIdentifiers,
];

/// Compatibility provenance at parser/linker boundaries. It is converted immediately to typed
/// provenance and never participates in optimization as a span side table.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SyntheticReason {
    LoweringGenerated,
    TrustedReplacement,
    OptimizerGenerated,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NodeOrigin {
    Source(Span),
    Synthetic {
        anchor: Option<Span>,
        reason: SyntheticReason,
    },
}

impl NodeOrigin {
    pub const fn source_span(self) -> Option<Span> {
        match self {
            Self::Source(span) => Some(span),
            Self::Synthetic { .. } => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct OptimizeDependency {
    pub specifier: String,
    pub kind: ModuleRequestKind,
    pub origin: NodeOrigin,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ValidatedDefineValue {
    Primitive(ConstVal),
    Expression(TrustedExpression),
}

#[derive(Clone, Debug, PartialEq)]
pub struct ValidatedDefine {
    pub key: String,
    pub value: ValidatedDefineValue,
}

impl ValidatedDefine {
    pub fn primitive(key: impl Into<String>, value: ConstVal) -> Self {
        Self {
            key: key.into(),
            value: ValidatedDefineValue::Primitive(value),
        }
    }

    pub fn expression(key: impl Into<String>, expression: TrustedExpression) -> Self {
        Self {
            key: key.into(),
            value: ValidatedDefineValue::Expression(expression),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkerExportLiveness {
    module_id: u32,
    /// Public names whose local declaration bindings must remain available to the optimizer.
    live_export_names: FxHashSet<String>,
    /// Exact public keys observed across the module boundary.
    observed_export_names: FxHashSet<String>,
}

/// Stable linker plan for one plain `export *` declaration, in source order.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct LinkerExportStar {
    specifier: String,
    resolution: LinkerExportStarResolution,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum LinkerExportStarResolution {
    Exact(Vec<String>),
    Runtime { excluded: Vec<String> },
}

impl LinkerExportStar {
    pub fn exact(specifier: impl Into<String>, names: impl IntoIterator<Item = String>) -> Self {
        let mut names: Vec<_> = names.into_iter().collect();
        names.sort_unstable();
        names.dedup();
        Self {
            specifier: specifier.into(),
            resolution: LinkerExportStarResolution::Exact(names),
        }
    }

    pub fn runtime(
        specifier: impl Into<String>,
        excluded: impl IntoIterator<Item = String>,
    ) -> Self {
        let mut excluded: Vec<_> = excluded.into_iter().collect();
        excluded.sort_unstable();
        excluded.dedup();
        Self {
            specifier: specifier.into(),
            resolution: LinkerExportStarResolution::Runtime { excluded },
        }
    }

    pub fn specifier(&self) -> &str {
        &self.specifier
    }

    pub const fn resolution(&self) -> &LinkerExportStarResolution {
        &self.resolution
    }
}

impl LinkerExportLiveness {
    pub fn new(
        module_id: u32,
        live_export_names: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        let live_export_names: FxHashSet<String> =
            live_export_names.into_iter().map(Into::into).collect();
        Self {
            module_id,
            observed_export_names: live_export_names.clone(),
            live_export_names,
        }
    }

    pub fn from_parts(
        module_id: u32,
        live_export_names: impl IntoIterator<Item = impl Into<String>>,
        observed_export_names: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            module_id,
            live_export_names: live_export_names.into_iter().map(Into::into).collect(),
            observed_export_names: observed_export_names.into_iter().map(Into::into).collect(),
        }
    }

    pub const fn module_id(&self) -> u32 {
        self.module_id
    }

    pub fn live_export_names(&self) -> &FxHashSet<String> {
        &self.live_export_names
    }

    pub fn observed_export_names(&self) -> &FxHashSet<String> {
        &self.observed_export_names
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TrustedExpressionValidation {
    CompleteExpression,
    MissingSourceIdentity,
    ParserIdentityMismatch,
    ParserReportedErrors,
    WrongSyntaxCategory,
    RequiresLowering,
    InvalidOwnedIr,
}

impl TrustedExpressionValidation {
    const fn diagnostic(self) -> &'static str {
        match self {
            Self::CompleteExpression => "complete expression",
            Self::MissingSourceIdentity => "parser source identity is missing",
            Self::ParserIdentityMismatch => "parser/interner identity does not match",
            Self::ParserReportedErrors => "the parser reported an error",
            Self::WrongSyntaxCategory => "parsed syntax is not exactly one expression",
            Self::RequiresLowering => "replacement must already be lowered JavaScript",
            Self::InvalidOwnedIr => "expression could not be lowered into owned typed IR",
        }
    }
}

/// Parser-proven, lifetime-independent expression syntax.
#[derive(Clone, Debug, PartialEq)]
pub struct TrustedExpression {
    owner: Option<crate::typed_ir::TypedExpressionOwner>,
    canonical_source: String,
    validation: TrustedExpressionValidation,
}

impl TrustedExpression {
    pub fn from_parsed_program(parsed: &ModuleAst, interner: &Interner) -> Self {
        let Some(source) = parsed.source() else {
            return Self::invalid(
                String::new(),
                TrustedExpressionValidation::MissingSourceIdentity,
            );
        };
        if parsed.interner_identity() != Some(interner.identity()) {
            return Self::invalid(
                source.to_owned(),
                TrustedExpressionValidation::ParserIdentityMismatch,
            );
        }
        if parsed.parser_had_errors() != Some(false) {
            return Self::invalid(
                source.to_owned(),
                TrustedExpressionValidation::ParserReportedErrors,
            );
        }
        parsed.with_ast(|program| {
            if !matches!(program.source_type, SourceType::Module | SourceType::Script) {
                return Self::invalid(
                    source.to_owned(),
                    TrustedExpressionValidation::RequiresLowering,
                );
            }
            let [Statement::Expression(statement)] = program.body.as_slice() else {
                return Self::invalid(
                    source.to_owned(),
                    TrustedExpressionValidation::WrongSyntaxCategory,
                );
            };
            let semantic = wake_ecma_semantic::analyze(program);
            match crate::typed_ir::lower_expression_owner(
                &statement.expression,
                interner,
                Some(&semantic),
            ) {
                Ok(owner) => {
                    let span = statement.expression.span();
                    let canonical_source = source
                        .get(span.lo as usize..span.hi as usize)
                        .unwrap_or(source)
                        .to_owned();
                    Self {
                        owner: Some(owner),
                        canonical_source,
                        validation: TrustedExpressionValidation::CompleteExpression,
                    }
                }
                Err(_) => Self::invalid(
                    source.to_owned(),
                    TrustedExpressionValidation::InvalidOwnedIr,
                ),
            }
        })
    }

    fn invalid(source: String, validation: TrustedExpressionValidation) -> Self {
        Self {
            owner: None,
            canonical_source: source,
            validation,
        }
    }

    pub fn owner(&self) -> Option<&crate::typed_ir::TypedExpressionOwner> {
        self.owner.as_ref()
    }

    pub fn source(&self) -> &str {
        &self.canonical_source
    }

    pub const fn is_valid(&self) -> bool {
        matches!(
            self.validation,
            TrustedExpressionValidation::CompleteExpression
        )
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TrustedExpressionEdit {
    target: Span,
    expression: TrustedExpression,
}

impl TrustedExpressionEdit {
    pub fn from_parsed_program(target: Span, parsed: &ModuleAst, interner: &Interner) -> Self {
        Self {
            target,
            expression: TrustedExpression::from_parsed_program(parsed, interner),
        }
    }

    pub const fn target(&self) -> Span {
        self.target
    }

    pub fn source(&self) -> &str {
        self.expression.source()
    }

    pub fn expression(&self) -> &TrustedExpression {
        &self.expression
    }
}

#[derive(Debug, Clone)]
pub struct OptimizeInput<'source> {
    pub source: &'source str,
    pub minify: bool,
    pub defines: Vec<ValidatedDefine>,
    pub drop_debugger: bool,
    pub drop_console: bool,
    pub reserved_names: Vec<String>,
    pub linker_liveness: Option<LinkerExportLiveness>,
    linker_export_stars: Vec<LinkerExportStar>,
    expression_replacements: Vec<TrustedExpressionEdit>,
    preserve_commonjs: bool,
    bundled_commonjs: bool,
    bundled_internal_esm_dependencies: FxHashSet<(String, ModuleRequestKind)>,
    removed_statement_spans: FxHashSet<Span>,
    removed_binding_spans: FxHashSet<Span>,
    pub dependencies: Vec<OptimizeDependency>,
    pub module_name: Option<String>,
}

impl<'source> OptimizeInput<'source> {
    pub fn new(source: &'source str) -> Self {
        Self {
            source,
            minify: true,
            defines: Vec::new(),
            drop_debugger: false,
            drop_console: false,
            reserved_names: Vec::new(),
            linker_liveness: None,
            linker_export_stars: Vec::new(),
            expression_replacements: Vec::new(),
            preserve_commonjs: false,
            bundled_commonjs: false,
            bundled_internal_esm_dependencies: FxHashSet::default(),
            removed_statement_spans: FxHashSet::default(),
            removed_binding_spans: FxHashSet::default(),
            dependencies: Vec::new(),
            module_name: None,
        }
    }

    #[doc(hidden)]
    pub fn set_preserve_commonjs(&mut self, enabled: bool) {
        self.preserve_commonjs = enabled;
    }

    pub(crate) const fn preserve_commonjs(&self) -> bool {
        self.preserve_commonjs
    }

    #[doc(hidden)]
    pub fn set_bundled_commonjs(&mut self, enabled: bool) {
        self.bundled_commonjs = enabled;
    }

    pub(crate) const fn bundled_commonjs(&self) -> bool {
        self.bundled_commonjs
    }

    #[doc(hidden)]
    pub fn set_bundled_internal_esm_dependencies(
        &mut self,
        dependencies: impl IntoIterator<Item = (String, ModuleRequestKind)>,
    ) {
        self.bundled_internal_esm_dependencies = dependencies.into_iter().collect();
    }

    #[doc(hidden)]
    pub fn set_linker_export_stars(&mut self, plans: impl IntoIterator<Item = LinkerExportStar>) {
        self.linker_export_stars = plans.into_iter().collect();
    }

    pub(crate) fn linker_export_stars(&self) -> &[LinkerExportStar] {
        &self.linker_export_stars
    }

    pub fn add_expression_edit(&mut self, edit: TrustedExpressionEdit) {
        self.expression_replacements.push(edit);
    }

    pub fn extend_expression_edits(
        &mut self,
        edits: impl IntoIterator<Item = TrustedExpressionEdit>,
    ) {
        self.expression_replacements.extend(edits);
    }

    pub fn expression_edits(&self) -> &[TrustedExpressionEdit] {
        &self.expression_replacements
    }

    /// Add a parser-identified statement to the trusted structural removal set.
    pub fn add_statement_removal(&mut self, target: Span) {
        self.removed_statement_spans.insert(target);
    }

    pub fn extend_statement_removals(&mut self, targets: impl IntoIterator<Item = Span>) {
        self.removed_statement_spans.extend(targets);
    }

    /// Add a parser-identified binding to the trusted structural removal set.
    pub fn add_binding_removal(&mut self, target: Span) {
        self.removed_binding_spans.insert(target);
    }

    pub fn extend_binding_removals(&mut self, targets: impl IntoIterator<Item = Span>) {
        self.removed_binding_spans.extend(targets);
    }

    pub(crate) fn statement_removals(&self) -> &FxHashSet<Span> {
        &self.removed_statement_spans
    }

    pub(crate) fn binding_removals(&self) -> &FxHashSet<Span> {
        &self.removed_binding_spans
    }
}

impl Default for OptimizeInput<'static> {
    fn default() -> Self {
        let mut input = Self::new("");
        input.minify = false;
        input
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MinifyDiagnosticKind {
    OptimizerInputMismatch,
    InvalidTrustedEdit,
    UnsupportedTransform,
    InvalidIr,
    DidNotConverge,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MinifyDiagnostic {
    pub kind: MinifyDiagnosticKind,
    pub module_name: Option<String>,
    pub pass: Option<OptimizationPass>,
    pub iterations: usize,
    pub message: String,
}

impl fmt::Display for MinifyDiagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(module_name) = &self.module_name {
            write!(formatter, "minification failed for {module_name}: ")?;
        } else {
            formatter.write_str("minification failed: ")?;
        }
        formatter.write_str(&self.message)
    }
}
impl std::error::Error for MinifyDiagnostic {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PassStats {
    pub pass: OptimizationPass,
    pub runs: usize,
    pub changes: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OptimizeStats {
    pub iterations: usize,
    pub passes: Vec<PassStats>,
}

impl Default for OptimizeStats {
    fn default() -> Self {
        Self {
            iterations: 0,
            passes: ALL_PASS_ORDER
                .iter()
                .copied()
                .map(|pass| PassStats {
                    pass,
                    runs: 0,
                    changes: 0,
                })
                .collect(),
        }
    }
}

impl OptimizeStats {
    pub fn pass(&self, pass: OptimizationPass) -> &PassStats {
        self.passes
            .iter()
            .find(|stats| stats.pass == pass)
            .expect("every pipeline pass has a stable statistics slot")
    }

    pub(crate) fn record(&mut self, pass: OptimizationPass, changes: usize) {
        let stats = self
            .passes
            .iter_mut()
            .find(|stats| stats.pass == pass)
            .expect("every pipeline pass has a stable statistics slot");
        stats.runs += 1;
        stats.changes += changes;
    }
}

/// Fully owned output of the typed optimizer. There is no AST lifetime or span-plan payload.
pub struct OptimizedProgram {
    owned_program: crate::typed_ir::TypedProgram,
    typed_module_plan: crate::typed_modules::TypedModulePlan,
    typed_report: crate::typed_pipeline::TypedPipelineReport,
    retained_dependencies: Vec<OptimizeDependency>,
    fingerprint: u64,
    minify: bool,
    dynamic_scope_hazard: bool,
    linker_module_id: Option<u32>,
    preserve_commonjs: bool,
    bundled_commonjs: bool,
    internal_esm_dependencies: FxHashSet<(String, ModuleRequestKind)>,
}

impl fmt::Debug for OptimizedProgram {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OptimizedProgram")
            .field("owned_program", &self.owned_program)
            .field("typed_module_plan", &self.typed_module_plan)
            .field("typed_report", &self.typed_report)
            .field("retained_dependencies", &self.retained_dependencies)
            .field("fingerprint", &format_args!("{:016x}", self.fingerprint))
            .field("minify", &self.minify)
            .field("dynamic_scope_hazard", &self.dynamic_scope_hazard)
            .field("linker_module_id", &self.linker_module_id)
            .field("preserve_commonjs", &self.preserve_commonjs)
            .field("bundled_commonjs", &self.bundled_commonjs)
            .field("internal_esm_dependencies", &self.internal_esm_dependencies)
            .finish()
    }
}

impl OptimizedProgram {
    #[doc(hidden)]
    pub const fn typed_program(&self) -> &crate::codegen_bridge::TypedProgram {
        &self.owned_program
    }

    #[doc(hidden)]
    pub const fn typed_module_plan(&self) -> &crate::codegen_bridge::TypedModulePlan {
        &self.typed_module_plan
    }

    pub(crate) const fn typed_report(&self) -> &crate::typed_pipeline::TypedPipelineReport {
        &self.typed_report
    }

    pub const fn minify(&self) -> bool {
        self.minify
    }

    pub const fn preserve_commonjs(&self) -> bool {
        self.preserve_commonjs
    }

    pub const fn bundled_commonjs(&self) -> bool {
        self.bundled_commonjs
    }

    pub fn dependency_target_is_esm(&self, specifier: &str, kind: ModuleRequestKind) -> bool {
        self.internal_esm_dependencies
            .contains(&(specifier.to_owned(), kind))
    }

    pub fn retained_dependencies(&self) -> &[OptimizeDependency] {
        &self.retained_dependencies
    }

    pub const fn stats(&self) -> &OptimizeStats {
        &self.typed_report().stats
    }

    pub const fn fingerprint(&self) -> u64 {
        self.fingerprint
    }

    pub const fn has_dynamic_scope_hazard(&self) -> bool {
        self.dynamic_scope_hazard
    }

    pub const fn linker_module_id(&self) -> Option<u32> {
        self.linker_module_id
    }

    /// Whether bundled codegen can borrow the sealed program because finalization is a proven
    /// no-op for these final link facts. This remains paired with the optimizer-owned plan; callers
    /// cannot assert the proof for an unrelated typed program.
    #[doc(hidden)]
    pub fn can_emit_sealed_without_finalization(&self, no_esmodule: bool) -> bool {
        self.typed_report.trivial_effect_module
            && self.bundled_commonjs
            && no_esmodule
            && self.typed_module_plan.requests().is_empty()
            && !self.typed_module_plan.has_top_level_await()
            && !self.typed_module_plan.is_finalized()
            && self.typed_module_plan.sealed_revision() == Some(self.owned_program.revision())
    }
}

/// Consume a parser-owned program and execute only the owned typed pipeline.
pub fn optimize(
    program: Arc<ModuleAst>,
    interner: &Interner,
    input: &OptimizeInput<'_>,
) -> Result<OptimizedProgram, MinifyDiagnostic> {
    optimize_impl(program, interner, input, true)
}

/// One-shot build counterpart of [`optimize`]. The returned program is fully identical for
/// optimization and code generation, but its stable cross-generation fingerprint is omitted
/// because the transient caller cannot reuse it before the process-level operation completes.
/// Long-lived build sessions and public optimizer consumers must use [`optimize`].
#[doc(hidden)]
pub fn optimize_one_shot(
    program: Arc<ModuleAst>,
    interner: &Interner,
    input: &OptimizeInput<'_>,
) -> Result<OptimizedProgram, MinifyDiagnostic> {
    optimize_impl(program, interner, input, false)
}

fn optimize_impl(
    program: Arc<ModuleAst>,
    interner: &Interner,
    input: &OptimizeInput<'_>,
    compute_fingerprint: bool,
) -> Result<OptimizedProgram, MinifyDiagnostic> {
    validate_owned_optimizer_input(&program, interner, input)?;
    validate_input(input)?;
    let fingerprint = if compute_fingerprint {
        program.with_ast(|ast| stable_fingerprint(input, interner, ast))
    } else {
        0
    };
    let result = program
        .with_ast(|ast| crate::owned_optimizer::optimize_owned_program(ast, interner, input))?;
    Ok(OptimizedProgram {
        owned_program: result.program,
        typed_module_plan: result.module_plan,
        typed_report: result.report,
        retained_dependencies: result.retained_dependencies,
        fingerprint,
        minify: input.minify,
        dynamic_scope_hazard: result.dynamic_scope_hazard,
        linker_module_id: input
            .linker_liveness
            .as_ref()
            .map(LinkerExportLiveness::module_id),
        preserve_commonjs: input.preserve_commonjs,
        bundled_commonjs: input.bundled_commonjs,
        internal_esm_dependencies: input.bundled_internal_esm_dependencies.clone(),
    })
}

fn validate_owned_optimizer_input(
    program: &ModuleAst,
    interner: &Interner,
    input: &OptimizeInput<'_>,
) -> Result<(), MinifyDiagnostic> {
    let source_matches = program
        .source()
        .is_some_and(|source| source.as_bytes() == input.source.as_bytes());
    let interner_matches = program.interner_identity() == Some(interner.identity());
    if source_matches && interner_matches {
        return Ok(());
    }
    let message = match (program.source().is_some(), source_matches, interner_matches) {
        (false, _, _) => "the AST owner does not carry parser source identity",
        (true, false, _) => "OptimizeInput.source does not match the AST owner's exact source",
        (true, true, false) => "the supplied Interner did not create this AST's Atom values",
        (true, true, true) => unreachable!(),
    };
    Err(input_diagnostic(
        input,
        MinifyDiagnosticKind::OptimizerInputMismatch,
        message,
    ))
}

fn validate_input(input: &OptimizeInput<'_>) -> Result<(), MinifyDiagnostic> {
    if input.preserve_commonjs && input.bundled_commonjs {
        return Err(input_diagnostic(
            input,
            MinifyDiagnosticKind::InvalidTrustedEdit,
            "preserved and bundled CommonJS modes are mutually exclusive",
        ));
    }
    if !input.bundled_commonjs && !input.bundled_internal_esm_dependencies.is_empty() {
        return Err(input_diagnostic(
            input,
            MinifyDiagnosticKind::InvalidTrustedEdit,
            "internal ESM dependency facts require bundled CommonJS mode",
        ));
    }
    let mut define_keys = FxHashSet::default();
    for define in &input.defines {
        let valid_value = match &define.value {
            ValidatedDefineValue::Primitive(_) => true,
            ValidatedDefineValue::Expression(expression) => expression.is_valid(),
        };
        if define.key.trim().is_empty() || !valid_value || !define_keys.insert(define.key.as_str())
        {
            return Err(input_diagnostic(
                input,
                MinifyDiagnosticKind::InvalidTrustedEdit,
                format!(
                    "define `{}` must have a unique non-empty key and parser-owned value",
                    define.key
                ),
            ));
        }
    }
    let mut replacement_spans = Vec::with_capacity(input.expression_replacements.len());
    for edit in &input.expression_replacements {
        let target = edit.target;
        let valid_target = !target.is_dummy()
            && input
                .source
                .get(target.lo as usize..target.hi as usize)
                .is_some();
        if !valid_target || !edit.expression.is_valid() {
            return Err(input_diagnostic(
                input,
                MinifyDiagnosticKind::InvalidTrustedEdit,
                format!(
                    "trusted expression edit at {}..{} is invalid: {}",
                    target.lo,
                    target.hi,
                    edit.expression.validation.diagnostic()
                ),
            ));
        }
        replacement_spans.push(target);
    }
    replacement_spans.sort_unstable_by_key(|span| (span.lo, span.hi));
    if let Some(pair) = replacement_spans
        .windows(2)
        .find(|pair| pair[0].hi > pair[1].lo)
    {
        return Err(input_diagnostic(
            input,
            MinifyDiagnosticKind::InvalidTrustedEdit,
            format!(
                "trusted expression edits overlap at {}..{} and {}..{}",
                pair[0].lo, pair[0].hi, pair[1].lo, pair[1].hi
            ),
        ));
    }
    for dependency in &input.dependencies {
        let valid_origin = match dependency.origin {
            NodeOrigin::Source(span)
            | NodeOrigin::Synthetic {
                anchor: Some(span), ..
            } => {
                !span.is_dummy()
                    && input
                        .source
                        .get(span.lo as usize..span.hi as usize)
                        .is_some()
            }
            NodeOrigin::Synthetic { anchor: None, .. } => true,
        };
        if dependency.specifier.is_empty() || !valid_origin {
            return Err(input_diagnostic(
                input,
                MinifyDiagnosticKind::InvalidTrustedEdit,
                format!(
                    "dependency `{}` has an invalid origin",
                    dependency.specifier
                ),
            ));
        }
    }
    Ok(())
}

fn input_diagnostic(
    input: &OptimizeInput<'_>,
    kind: MinifyDiagnosticKind,
    message: impl Into<String>,
) -> MinifyDiagnostic {
    MinifyDiagnostic {
        kind,
        module_name: input.module_name.clone(),
        pass: Some(OptimizationPass::ApplyTrustedEdits),
        iterations: 0,
        message: message.into(),
    }
}

fn stable_fingerprint(
    input: &OptimizeInput<'_>,
    interner: &Interner,
    program: &Program<'_>,
) -> u64 {
    let mut hash = StableHasher::new();
    hash.write_str(PIPELINE_VERSION);
    hash.write_str(input.source);
    hash.write_str(match program.source_type {
        SourceType::Module => "module",
        SourceType::Script => "script",
        SourceType::TypeScript => "typescript",
        SourceType::Tsx => "tsx",
        SourceType::Jsx => "jsx",
    });
    hash.write_bool(program.strict);
    for helper in [
        program.spread_helper,
        program.object_spread_helper,
        program.for_of_helper,
    ] {
        if let Some(helper) = helper {
            hash.write_bool(true);
            hash.write_str(&interner.resolve(helper));
        } else {
            hash.write_bool(false);
        }
    }
    hash.write_bool(input.minify);
    hash.write_bool(input.drop_debugger);
    hash.write_bool(input.drop_console);
    hash.write_bool(input.preserve_commonjs);
    hash.write_bool(input.bundled_commonjs);
    let mut internal_dependencies: Vec<_> =
        input.bundled_internal_esm_dependencies.iter().collect();
    internal_dependencies.sort_unstable();
    hash.write_u64(internal_dependencies.len() as u64);
    for (specifier, kind) in internal_dependencies {
        hash.write_str(specifier);
        hash.write_u64(*kind as u64);
    }
    let mut reserved = input.reserved_names.clone();
    reserved.sort();
    hash.write_u64(reserved.len() as u64);
    for name in reserved {
        hash.write_str(&name);
    }
    let mut defines = input.defines.clone();
    defines.sort_unstable_by(|left, right| left.key.cmp(&right.key));
    hash.write_u64(defines.len() as u64);
    for define in defines {
        hash.write_str(&define.key);
        match define.value {
            ValidatedDefineValue::Primitive(ConstVal::Bool(value)) => {
                hash.write_u64(0);
                hash.write_bool(value);
            }
            ValidatedDefineValue::Primitive(ConstVal::Str(value)) => {
                hash.write_u64(1);
                hash.write_str(&value);
            }
            ValidatedDefineValue::Primitive(ConstVal::Num(value)) => {
                hash.write_u64(2);
                hash.write_u64(value.to_bits())
            }
            ValidatedDefineValue::Primitive(ConstVal::Null) => hash.write_u64(3),
            ValidatedDefineValue::Primitive(ConstVal::Undefined) => hash.write_u64(4),
            ValidatedDefineValue::Expression(expression) => {
                hash.write_u64(5);
                hash.write_str(expression.source());
                if let Some(owner) = expression.owner() {
                    hash.write_u64(owner.fingerprint());
                }
            }
        }
    }
    hash.write_bool(input.linker_liveness.is_some());
    if let Some(liveness) = &input.linker_liveness {
        hash.write_u64(u64::from(liveness.module_id));
        let mut names: Vec<_> = liveness.live_export_names.iter().collect();
        names.sort_unstable();
        hash.write_u64(names.len() as u64);
        for name in names {
            hash.write_str(name);
        }
        let mut observed_names: Vec<_> = liveness.observed_export_names.iter().collect();
        observed_names.sort_unstable();
        hash.write_u64(observed_names.len() as u64);
        for name in observed_names {
            hash.write_str(name);
        }
    }
    hash.write_u64(input.linker_export_stars.len() as u64);
    for star in &input.linker_export_stars {
        hash.write_str(&star.specifier);
        match &star.resolution {
            LinkerExportStarResolution::Exact(names) => {
                hash.write_u64(0);
                hash.write_u64(names.len() as u64);
                for name in names {
                    hash.write_str(name);
                }
            }
            LinkerExportStarResolution::Runtime { excluded } => {
                hash.write_u64(1);
                hash.write_u64(excluded.len() as u64);
                for name in excluded {
                    hash.write_str(name);
                }
            }
        }
    }
    hash.write_span_set(input.statement_removals());
    hash.write_span_set(input.binding_removals());
    let mut edits: Vec<_> = input.expression_replacements.iter().collect();
    edits.sort_unstable_by_key(|edit| (edit.target.lo, edit.target.hi));
    hash.write_u64(edits.len() as u64);
    for edit in edits {
        hash.write_span(edit.target);
        hash.write_str(edit.source());
        if let Some(owner) = edit.expression.owner() {
            hash.write_u64(owner.fingerprint());
        }
    }
    let mut dependencies = input.dependencies.clone();
    dependencies.sort_unstable_by(|left, right| {
        origin_key(left.origin)
            .cmp(&origin_key(right.origin))
            .then_with(|| left.kind.cmp(&right.kind))
            .then_with(|| left.specifier.cmp(&right.specifier))
    });
    hash.write_u64(dependencies.len() as u64);
    for dependency in dependencies {
        hash.write_str(&dependency.specifier);
        hash.write_u64(dependency.kind as u64);
        let (kind, lo, hi, reason) = origin_key(dependency.origin);
        hash.write_u64(u64::from(kind));
        hash.write_u64(u64::from(lo));
        hash.write_u64(u64::from(hi));
        hash.write_u64(u64::from(reason));
    }
    hash.finish()
}

fn origin_key(origin: NodeOrigin) -> (u8, u32, u32, u8) {
    match origin {
        NodeOrigin::Source(span) => (0, span.lo, span.hi, 0),
        NodeOrigin::Synthetic { anchor, reason } => {
            let (lo, hi) = anchor.map_or((0, 0), |span| (span.lo, span.hi));
            let reason = match reason {
                SyntheticReason::LoweringGenerated => 0,
                SyntheticReason::TrustedReplacement => 1,
                SyntheticReason::OptimizerGenerated => 2,
            };
            (1, lo, hi, reason)
        }
    }
}

struct StableHasher(u64);

impl StableHasher {
    const OFFSET: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x00000100000001b3;
    const fn new() -> Self {
        Self(Self::OFFSET)
    }
    fn write(&mut self, bytes: &[u8]) {
        self.write_u64(bytes.len() as u64);
        for byte in bytes {
            self.0 ^= u64::from(*byte);
            self.0 = self.0.wrapping_mul(Self::PRIME);
        }
    }
    fn write_str(&mut self, value: &str) {
        self.write(value.as_bytes());
    }
    fn write_bool(&mut self, value: bool) {
        self.write(&[u8::from(value)]);
    }
    fn write_u64(&mut self, value: u64) {
        for byte in value.to_le_bytes() {
            self.0 ^= u64::from(byte);
            self.0 = self.0.wrapping_mul(Self::PRIME);
        }
    }
    fn write_span(&mut self, span: Span) {
        self.write_u64(u64::from(span.lo));
        self.write_u64(u64::from(span.hi));
    }
    fn write_span_set(&mut self, spans: &FxHashSet<Span>) {
        let mut spans: Vec<_> = spans.iter().copied().collect();
        spans.sort_unstable_by_key(|span| (span.lo, span.hi));
        self.write_u64(spans.len() as u64);
        for span in spans {
            self.write_span(span);
        }
    }
    const fn finish(self) -> u64 {
        self.0
    }
}
