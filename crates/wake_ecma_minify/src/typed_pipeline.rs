//! Unified Closure-style scheduler for the owned typed optimization IR.
//!
//! One-time trusted edits and helper materialization run before a single ordered fixed point.
//! Binding-sensitive stages share a [`TypedAnalysis`] while the program revision is unchanged and
//! rebuild it after structural mutations. The final name pass writes only owned name occurrences.

use std::error::Error;
use std::fmt;

use crate::typed_analysis::TypedAnalysis;
use crate::typed_decorators::{
    DecoratorLoweringError, DecoratorLoweringReport, materialize_decorators,
};
use crate::typed_edits::{
    TypedEditDependency, TypedEditError, TypedEditInput, apply_typed_edits,
    retain_typed_dependencies,
};
use crate::typed_inline::{
    TypedInlineError, eliminate_dead_code, inline_closed_functions, inline_single_use_and_dce,
};
use crate::typed_ir::{IrNodeData, TypedIrError, TypedProgram};
use crate::typed_lowering::{RuntimeHelperReport, materialize_runtime_helpers};
use crate::typed_mangle::{TypedMangleError, TypedMangleStats, mangle_typed_program};
use crate::typed_modules::{
    TypedModuleError, TypedModuleMode, TypedModuleOptions, TypedModulePlan,
    plan_owned_typed_modules, seal_typed_module_plan, try_plan_owned_trivial_bundled_module,
};
use crate::typed_passes::{TypedPassKind, TypedPassOptions, run_typed_pass};
use crate::{OptimizationPass, OptimizeStats};

/// The global cap applies to the complete ordered round, not to independent per-pass loops.
pub const MAX_TYPED_PIPELINE_ITERATIONS: usize = 100;

/// Policy inputs which affect typed optimization or final names.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TypedPipelineOptions {
    pub minify: bool,
    pub drop_debugger: bool,
    pub drop_console: bool,
    pub reserved_names: Vec<String>,
}

/// Successful scheduler output. `iterations` includes the final no-change proof round.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypedPipelineReport {
    pub stats: OptimizeStats,
    pub retained_dependencies: Vec<TypedEditDependency>,
    pub runtime_helpers: RuntimeHelperReport,
    pub decorators: DecoratorLoweringReport,
    pub mangling: TypedMangleStats,
    /// The sealed bundled program contains only binding-free global effects, so final link facts
    /// with no ESM marker and no requests cannot mutate it before emission.
    pub(crate) trivial_effect_module: bool,
}

/// Typed optimization never falls back to source or an older emitter.
#[derive(Debug)]
pub struct TypedPipelineError {
    pub pass: OptimizationPass,
    pub iterations: usize,
    pub message: String,
    source: Option<Box<dyn Error + Send + Sync>>,
}

impl TypedPipelineError {
    fn at(pass: OptimizationPass, iterations: usize, message: impl Into<String>) -> Self {
        Self {
            pass,
            iterations,
            message: message.into(),
            source: None,
        }
    }

    fn caused_by<E>(
        pass: OptimizationPass,
        iterations: usize,
        message: impl Into<String>,
        source: E,
    ) -> Self
    where
        E: Error + Send + Sync + 'static,
    {
        Self {
            pass,
            iterations,
            message: message.into(),
            source: Some(Box::new(source)),
        }
    }
}

impl fmt::Display for TypedPipelineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "typed optimizer pass {} failed after {} rounds: {}",
            self.pass.name(),
            self.iterations,
            self.message
        )
    }
}

impl Error for TypedPipelineError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source
            .as_deref()
            .map(|source| source as &(dyn Error + 'static))
    }
}

/// Apply one-time inputs, reach a real global fixed point, then commit final deterministic names.
#[cfg(test)]
pub(crate) fn run_typed_pipeline(
    program: &mut TypedProgram,
    edits: &TypedEditInput,
    options: &TypedPipelineOptions,
) -> Result<TypedPipelineReport, TypedPipelineError> {
    program.validate().map_err(|error| {
        ir_error(
            OptimizationPass::ApplyTrustedEdits,
            0,
            "input IR is invalid",
            error,
        )
    })?;
    let (next, report, plan, _) = run_typed_pipeline_impl(program.clone(), edits, options, None)?;
    *program = next;
    debug_assert!(plan.is_none());
    Ok(report)
}

/// Production scheduler variant which owns module planning before optimization and seals the
/// resulting request graph only after fixed-point rewrites and final names have committed.
pub(crate) fn run_typed_pipeline_with_modules(
    program: TypedProgram,
    edits: &TypedEditInput,
    options: &TypedPipelineOptions,
    module_options: &TypedModuleOptions,
) -> Result<(TypedProgram, TypedPipelineReport, TypedModulePlan, bool), TypedPipelineError> {
    let (program, report, plan, dynamic_scope_hazard) =
        run_typed_pipeline_impl(program, edits, options, Some(module_options))?;
    Ok((
        program,
        report,
        plan.expect("module-aware scheduler always creates one owned plan"),
        dynamic_scope_hazard,
    ))
}

fn run_typed_pipeline_impl(
    mut program: TypedProgram,
    edits: &TypedEditInput,
    options: &TypedPipelineOptions,
    module_options: Option<&TypedModuleOptions>,
) -> Result<
    (
        TypedProgram,
        TypedPipelineReport,
        Option<TypedModulePlan>,
        bool,
    ),
    TypedPipelineError,
> {
    // The production caller transfers a freshly lowered, validated owner. Test-only borrowed
    // entry points validate before cloning. From here to the next explicit boundary, every pass
    // preserves the typed-IR invariants or returns an error without exposing this owned value.
    debug_assert!(program.validate().is_ok());

    let edit_report = apply_typed_edits(&mut program, edits).map_err(|error| {
        edit_error(
            OptimizationPass::ApplyTrustedEdits,
            0,
            "trusted edits could not be committed",
            error,
        )
    })?;
    let runtime_helpers = materialize_runtime_helpers(&mut program).map_err(|error| {
        ir_error(
            OptimizationPass::ApplyTrustedEdits,
            0,
            "runtime helpers could not be materialized",
            error,
        )
    })?;
    let decorators = materialize_decorators(&mut program).map_err(|error| {
        decorator_error(
            OptimizationPass::ApplyTrustedEdits,
            0,
            "decorators could not be materialized",
            error,
        )
    })?;

    let mut stats = OptimizeStats::default();
    let helper_changes = usize::from(runtime_helpers.spread_name.is_some())
        + usize::from(runtime_helpers.object_spread_name.is_some())
        + usize::from(runtime_helpers.for_of_name.is_some())
        + decorators.decorated_classes
        + usize::from(decorators.es_decorate_name.is_some())
        + usize::from(decorators.run_initializers_name.is_some());
    stats.record(
        OptimizationPass::ApplyTrustedEdits,
        edit_report.change_count + helper_changes,
    );

    // Exact graph liveness plus the parser/lowering allow-list can leave a module with only
    // binding-free global effects. Once trusted edits and helper/decorator materialization are
    // proven inert, the general module planner's semantic snapshot has no consumer: there are no
    // declarations, requests, calls, scopes or names to resolve. Let the strict module-side proof
    // consume this owner and seal the ordinary plan directly; rejection preserves the owner and
    // falls through to the complete analysis-backed pipeline.
    if options.minify
        && !options.drop_console
        && !options.drop_debugger
        && edit_report.change_count == 0
        && helper_changes == 0
        && let Some(module_options) = module_options
    {
        let (planned, plan) = try_plan_owned_trivial_bundled_module(program, module_options)
            .map_err(|error| {
                module_error(
                    OptimizationPass::BuildSemanticModel,
                    0,
                    "binding-free module syntax could not be planned structurally",
                    error,
                )
            })?;
        program = planned;
        if let Some(mut plan) = plan {
            run_trivial_minifying_compaction(&mut program, &mut stats)?;
            seal_typed_module_plan(&program, &mut plan).map_err(|error| {
                module_error(
                    OptimizationPass::MangleIdentifiers,
                    0,
                    "binding-free module plan could not be sealed",
                    error,
                )
            })?;
            let retained_dependencies = retain_typed_dependencies(&program, &edits.dependencies)
                .map_err(|error| {
                    edit_error(
                        OptimizationPass::EliminateDeadCode,
                        0,
                        "binding-free retained dependencies could not be computed",
                        error,
                    )
                })?;
            return Ok((
                program,
                TypedPipelineReport {
                    stats,
                    retained_dependencies,
                    runtime_helpers,
                    decorators,
                    mangling: TypedMangleStats::default(),
                    trivial_effect_module: true,
                },
                Some(plan),
                false,
            ));
        }
    }

    let mut analysis = TypedAnalysis::rebuild_validated(&program).map_err(|error| {
        ir_error(
            OptimizationPass::BuildSemanticModel,
            0,
            "pre-module semantic model could not be rebuilt",
            error,
        )
    })?;
    stats.record(OptimizationPass::BuildSemanticModel, 0);
    let mut module_plan_changed = false;
    let mut module_plan = if let Some(module_options) = module_options {
        let pre_plan_revision = program.revision();
        let (planned, plan) = plan_owned_typed_modules(program, &analysis, module_options)
            .map_err(|error| {
                module_error(
                    OptimizationPass::BuildSemanticModel,
                    0,
                    "module syntax could not be planned structurally",
                    error,
                )
            })?;
        program = planned;
        module_plan_changed = program.revision() != pre_plan_revision;
        Some(plan)
    } else {
        None
    };

    let trivial_effect_module = if let Some(plan) = module_plan.as_ref() {
        can_skip_minifying_fixed_point(&program, plan, edit_report.change_count == 0, options)
            .map_err(|error| {
                ir_error(
                    OptimizationPass::BuildSemanticModel,
                    0,
                    "trivial effect-module proof could not inspect typed IR",
                    error,
                )
            })?
    } else {
        false
    };
    if trivial_effect_module {
        run_trivial_minifying_compaction(&mut program, &mut stats)?;
    }
    // Module planning allocates runtime identities and may lower ESM syntax. Binding-sensitive
    // work normally needs a fresh analysis for that revision, but the strict trivial-effect proof
    // below guarantees there is no live binding, scope, call or dynamic lookup to inspect. Avoid
    // rebuilding a semantic model that neither fixed-point nor mangle will consume.
    if module_plan_changed && !trivial_effect_module {
        analysis = rebuild_for(
            &program,
            OptimizationPass::BuildSemanticModel,
            0,
            &mut stats,
        )?;
    }
    let final_analysis = if options.minify && !trivial_effect_module {
        Some(run_minifying_fixed_point(
            &mut program,
            options,
            &mut stats,
            analysis,
        )?)
    } else {
        if options.minify {
            None
        } else {
            Some(run_readable_fixed_point(
                &mut program,
                options,
                module_options.is_some_and(|options| !options.preserve_all_exports),
                !edits.defines.is_empty(),
                &mut stats,
                analysis,
            )?)
        }
    };
    let dynamic_scope_hazard = final_analysis.as_ref().is_some_and(|analysis| {
        analysis
            .scopes()
            .iter()
            .any(|scope| scope.is_frozen() || scope.contains_direct_eval() || scope.contains_with())
    });

    let mangling = if options.minify && !trivial_effect_module {
        let final_analysis = final_analysis
            .as_ref()
            .expect("non-trivial minify always retains a current semantic model");
        let reserved = options
            .reserved_names
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        let mangling =
            mangle_typed_program(&mut program, final_analysis, &reserved).map_err(|error| {
                mangle_error(
                    OptimizationPass::MangleIdentifiers,
                    stats.iterations,
                    "final names could not be committed",
                    error,
                )
            })?;
        stats.record(
            OptimizationPass::MangleProperties,
            mangling.renamed_private_names() + mangling.renamed_properties(),
        );
        stats.record(
            OptimizationPass::ReuseVariableSlots,
            mangling.reused_slots(),
        );
        stats.record(
            OptimizationPass::MangleIdentifiers,
            mangling.renamed_symbols(),
        );
        mangling
    } else {
        TypedMangleStats::default()
    };

    program.validate().map_err(|error| {
        ir_error(
            OptimizationPass::MangleIdentifiers,
            stats.iterations,
            "final IR invariant failed",
            error,
        )
    })?;
    if let Some(plan) = &mut module_plan {
        seal_typed_module_plan(&program, plan).map_err(|error| {
            module_error(
                OptimizationPass::MangleIdentifiers,
                stats.iterations,
                "post-optimization module plan could not be sealed",
                error,
            )
        })?;
    }
    let retained_dependencies =
        retain_typed_dependencies(&program, &edits.dependencies).map_err(|error| {
            edit_error(
                OptimizationPass::EliminateDeadCode,
                stats.iterations,
                "final retained dependencies could not be computed",
                error,
            )
        })?;

    Ok((
        program,
        TypedPipelineReport {
            stats,
            retained_dependencies,
            runtime_helpers,
            decorators,
            mangling,
            trivial_effect_module,
        },
        module_plan,
        dynamic_scope_hazard,
    ))
}

/// Run the only structural normalization which the binding-free proof may still expose.
///
/// In particular, late peephole canonicalizes `object["property"]` to the shorter
/// `object.property` spelling when the property is identifier-safe, and flattens nested sequences.
/// A reverse-preorder run reaches canonical form in one pass for the trivial grammar, so this
/// deliberately does not restore the semantic fixed point or identifier mangling.
fn run_trivial_minifying_compaction(
    program: &mut TypedProgram,
    stats: &mut OptimizeStats,
) -> Result<(), TypedPipelineError> {
    let changes = run_typed_pass(
        program,
        TypedPassOptions::default(),
        TypedPassKind::LatePeephole,
    )
    .map_err(|error| {
        ir_error(
            OptimizationPass::LatePeephole,
            0,
            "binding-free late peephole failed",
            error,
        )
    })?;
    stats.record(OptimizationPass::LatePeephole, changes);
    Ok(())
}

/// Prove that the module planner left only binding-free global side effects whose syntax can be
/// normalized without semantic analysis. These modules have no legal target for inlining, DCE or
/// identifier/property mangling, so running the complete fixed point can only rediscover zero
/// semantic changes. The proof is deliberately strict: calls and scope-producing syntax fall back
/// to the ordinary pipeline because they may carry direct-eval, helper or local-binding semantics.
fn can_skip_minifying_fixed_point(
    program: &TypedProgram,
    plan: &TypedModulePlan,
    edits_were_inert: bool,
    options: &TypedPipelineOptions,
) -> Result<bool, TypedIrError> {
    if !options.minify
        || options.drop_console
        || options.drop_debugger
        || !edits_were_inert
        || plan.mode() != TypedModuleMode::BundledCommonJs
        || plan.has_top_level_await()
    {
        return Ok(false);
    }
    let IrNodeData::Program { body, .. } = program
        .node(program.root())
        .expect("validated typed program root")
        .data()
    else {
        return Ok(false);
    };
    for &statement in program
        .list(*body)
        .expect("validated typed program body")
        .items()
    {
        if !matches!(
            program
                .node(statement)
                .expect("validated top-level statement")
                .data(),
            IrNodeData::ExpressionStatement { .. } | IrNodeData::EmptyStatement
        ) {
            return Ok(false);
        }
        for node in program.subtree_preorder(statement)? {
            let data = program.node(node).expect("validated effect subtree").data();
            if let IrNodeData::Name { name } = data
                && program
                    .name(*name)
                    .expect("validated name occurrence")
                    .symbol()
                    .is_some()
            {
                return Ok(false);
            }
            if matches!(
                data,
                IrNodeData::Function { .. }
                    | IrNodeData::ArrowFunction { .. }
                    | IrNodeData::Class { .. }
                    | IrNodeData::CallExpression { .. }
                    | IrNodeData::NewExpression { .. }
                    | IrNodeData::TaggedTemplateExpression { .. }
                    | IrNodeData::AwaitExpression { .. }
                    | IrNodeData::YieldExpression { .. }
                    | IrNodeData::ImportExpression { .. }
                    | IrNodeData::MetaProperty { .. }
                    | IrNodeData::ThisExpression
                    | IrNodeData::SuperExpression
                    | IrNodeData::WithStatement { .. }
            ) {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

fn run_minifying_fixed_point(
    program: &mut TypedProgram,
    options: &TypedPipelineOptions,
    stats: &mut OptimizeStats,
    mut analysis: TypedAnalysis,
) -> Result<TypedAnalysis, TypedPipelineError> {
    let pass_options = TypedPassOptions {
        drop_debugger: options.drop_debugger,
        drop_console: options.drop_console,
    };
    let mut last_changed = OptimizationPass::LatePeephole;
    let mut analysis_current = true;
    for iteration in 1..=MAX_TYPED_PIPELINE_ITERATIONS {
        let mut round_changes = 0usize;
        let primitive_changes = run_structural(
            program,
            pass_options,
            TypedPassKind::PrimitiveFolding,
            OptimizationPass::ConstantPropagationAndFolding,
            iteration,
            stats,
            &mut last_changed,
        )?;
        round_changes += primitive_changes;
        let branch_changes = run_structural(
            program,
            pass_options,
            TypedPassKind::BranchSimplification,
            OptimizationPass::SimplifyControlFlow,
            iteration,
            stats,
            &mut last_changed,
        )?;
        round_changes += branch_changes;
        if primitive_changes + branch_changes != 0 {
            analysis_current = false;
        }

        if !analysis_current {
            analysis = rebuild_for(
                program,
                OptimizationPass::InlineClosedFunctions,
                iteration,
                stats,
            )?;
            analysis_current = true;
        }
        let function_stats = inline_closed_functions(program, &analysis).map_err(|error| {
            inline_error(
                OptimizationPass::InlineClosedFunctions,
                iteration,
                "closed-function inline failed",
                error,
            )
        })?;
        let function_changes = function_stats.function_changes();
        stats.record(OptimizationPass::InlineClosedFunctions, function_changes);
        if function_changes != 0 {
            round_changes += function_changes;
            last_changed = OptimizationPass::InlineClosedFunctions;
            analysis_current = false;
        }

        if !analysis_current {
            analysis = rebuild_for(
                program,
                OptimizationPass::InlineSingleUseVariables,
                iteration,
                stats,
            )?;
            analysis_current = true;
        }
        let local_stats = inline_single_use_and_dce(program, &analysis).map_err(|error| {
            inline_error(
                OptimizationPass::InlineSingleUseVariables,
                iteration,
                "single-use inline and DCE failed",
                error,
            )
        })?;
        let inline_changes = local_stats.inline_changes();
        let dce_changes = local_stats.dce_changes();
        stats.record(OptimizationPass::InlineSingleUseVariables, inline_changes);
        stats.record(OptimizationPass::EliminateDeadCode, dce_changes);
        if inline_changes != 0 {
            round_changes += inline_changes;
            last_changed = OptimizationPass::InlineSingleUseVariables;
            analysis_current = false;
        }
        if dce_changes != 0 {
            round_changes += dce_changes;
            last_changed = OptimizationPass::EliminateDeadCode;
            analysis_current = false;
        }

        for (typed, public) in [
            (
                TypedPassKind::ConfiguredDrops,
                OptimizationPass::EliminateDeadCode,
            ),
            (
                TypedPassKind::DeadStatementCleanup,
                OptimizationPass::EliminateDeadCode,
            ),
            (
                TypedPassKind::StatementMerging,
                OptimizationPass::MergeStatements,
            ),
            (TypedPassKind::LatePeephole, OptimizationPass::LatePeephole),
        ] {
            let changes = run_structural(
                program,
                pass_options,
                typed,
                public,
                iteration,
                stats,
                &mut last_changed,
            )?;
            round_changes += changes;
            if changes != 0 {
                analysis_current = false;
            }
        }

        stats.iterations = iteration;
        if round_changes == 0 {
            debug_assert!(analysis_current);
            return Ok(analysis);
        }
    }
    Err(TypedPipelineError::at(
        last_changed,
        MAX_TYPED_PIPELINE_ITERATIONS,
        format!(
            "fixed point did not converge after {MAX_TYPED_PIPELINE_ITERATIONS} ordered rounds"
        ),
    ))
}

fn run_readable_fixed_point(
    program: &mut TypedProgram,
    options: &TypedPipelineOptions,
    eliminate_dead_bindings: bool,
    simplify_defines: bool,
    stats: &mut OptimizeStats,
    mut analysis: TypedAnalysis,
) -> Result<TypedAnalysis, TypedPipelineError> {
    let pass_options = TypedPassOptions {
        drop_debugger: options.drop_debugger,
        drop_console: options.drop_console,
    };
    let mut last_changed = OptimizationPass::SimplifyControlFlow;
    let mut analysis_current = true;
    for iteration in 1..=MAX_TYPED_PIPELINE_ITERATIONS {
        let mut round_changes = 0usize;
        let mut structural_passes = Vec::with_capacity(4);
        if simplify_defines {
            structural_passes.extend([
                (
                    TypedPassKind::PrimitiveFolding,
                    OptimizationPass::ConstantPropagationAndFolding,
                ),
                (
                    TypedPassKind::BranchSimplification,
                    OptimizationPass::SimplifyControlFlow,
                ),
            ]);
        }
        if options.drop_debugger || options.drop_console {
            structural_passes.push((
                TypedPassKind::ConfiguredDrops,
                OptimizationPass::EliminateDeadCode,
            ));
        }
        if simplify_defines || options.drop_debugger || options.drop_console {
            structural_passes.push((
                TypedPassKind::DeadStatementCleanup,
                OptimizationPass::EliminateDeadCode,
            ));
        }
        for (typed, public) in structural_passes {
            let changes = run_structural(
                program,
                pass_options,
                typed,
                public,
                iteration,
                stats,
                &mut last_changed,
            )?;
            round_changes += changes;
            if changes != 0 {
                analysis_current = false;
            }
        }
        if eliminate_dead_bindings {
            if !analysis_current {
                analysis = rebuild_for(
                    program,
                    OptimizationPass::EliminateDeadCode,
                    iteration,
                    stats,
                )?;
                analysis_current = true;
            }
            let dce = eliminate_dead_code(program, &analysis).map_err(|error| {
                inline_error(
                    OptimizationPass::EliminateDeadCode,
                    iteration,
                    "readable tree-shaking DCE failed",
                    error,
                )
            })?;
            let changes = dce.dce_changes();
            stats.record(OptimizationPass::EliminateDeadCode, changes);
            if changes != 0 {
                round_changes += changes;
                last_changed = OptimizationPass::EliminateDeadCode;
                analysis_current = false;
            }
        }
        stats.iterations = iteration;
        if round_changes == 0 {
            if !analysis_current {
                analysis = rebuild_for(
                    program,
                    OptimizationPass::BuildSemanticModel,
                    iteration,
                    stats,
                )?;
            }
            return Ok(analysis);
        }
    }
    Err(TypedPipelineError::at(
        last_changed,
        MAX_TYPED_PIPELINE_ITERATIONS,
        "readable structural pipeline did not converge",
    ))
}

#[allow(clippy::too_many_arguments)]
fn run_structural(
    program: &mut TypedProgram,
    options: TypedPassOptions,
    typed: TypedPassKind,
    public: OptimizationPass,
    iteration: usize,
    stats: &mut OptimizeStats,
    last_changed: &mut OptimizationPass,
) -> Result<usize, TypedPipelineError> {
    let changes = run_typed_pass(program, options, typed).map_err(|error| {
        ir_error(
            public,
            iteration,
            format!("{} structural rewrite failed", typed.name()),
            error,
        )
    })?;
    stats.record(public, changes);
    if changes != 0 {
        *last_changed = public;
    }
    Ok(changes)
}

fn rebuild_for(
    program: &TypedProgram,
    pass: OptimizationPass,
    iteration: usize,
    stats: &mut OptimizeStats,
) -> Result<TypedAnalysis, TypedPipelineError> {
    let analysis = TypedAnalysis::rebuild_validated(program).map_err(|error| {
        ir_error(
            pass,
            iteration,
            "current-tree analysis rebuild failed",
            error,
        )
    })?;
    stats.record(OptimizationPass::BuildSemanticModel, 0);
    Ok(analysis)
}

fn ir_error(
    pass: OptimizationPass,
    iterations: usize,
    message: impl Into<String>,
    error: TypedIrError,
) -> TypedPipelineError {
    TypedPipelineError::caused_by(pass, iterations, message, error)
}

fn edit_error(
    pass: OptimizationPass,
    iterations: usize,
    message: impl Into<String>,
    error: TypedEditError,
) -> TypedPipelineError {
    TypedPipelineError::caused_by(pass, iterations, message, error)
}

fn inline_error(
    pass: OptimizationPass,
    iterations: usize,
    message: impl Into<String>,
    error: TypedInlineError,
) -> TypedPipelineError {
    TypedPipelineError::caused_by(pass, iterations, message, error)
}

fn mangle_error(
    pass: OptimizationPass,
    iterations: usize,
    message: impl Into<String>,
    error: TypedMangleError,
) -> TypedPipelineError {
    TypedPipelineError::caused_by(pass, iterations, message, error)
}

fn decorator_error(
    pass: OptimizationPass,
    iterations: usize,
    message: impl Into<String>,
    error: DecoratorLoweringError,
) -> TypedPipelineError {
    TypedPipelineError::caused_by(pass, iterations, message, error)
}

fn module_error(
    pass: OptimizationPass,
    iterations: usize,
    message: impl Into<String>,
    error: TypedModuleError,
) -> TypedPipelineError {
    TypedPipelineError::caused_by(pass, iterations, message, error)
}

#[cfg(test)]
mod tests {
    use wake_common::{Interner, Span};
    use wake_ecma_ast::SourceType;
    use wake_ecma_parser::parse;

    use super::*;
    use crate::ConstVal;
    use crate::typed_edits::{TypedEditDependency, TypedValidatedDefine};
    use crate::typed_ir::{IrOrigin, PropertyKeyKind};
    use crate::typed_modules::{TypedModuleMode, TypedModuleOptions, TypedModuleRequestKind};

    fn lower(source: &str) -> TypedProgram {
        lower_as(source, SourceType::Script)
    }

    fn lower_as(source: &str, source_type: SourceType) -> TypedProgram {
        let interner = Interner::new();
        let parsed = parse(source, &interner, source_type);
        assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);
        parsed.module.with_ast(|program| {
            let semantic = wake_ecma_semantic::analyze(program);
            TypedProgram::lower(program, &interner, Some(&semantic)).unwrap()
        })
    }

    fn has_identifier_property(program: &TypedProgram, expected: &str) -> bool {
        program.preorder().unwrap().into_iter().any(|node| {
            let Some(IrNodeData::MemberExpression {
                property,
                property_kind: PropertyKeyKind::Identifier,
                ..
            }) = program.node(node).map(|node| node.data())
            else {
                return false;
            };
            let Some(IrNodeData::Name { name }) = program.node(*property).map(|node| node.data())
            else {
                return false;
            };
            program
                .name(*name)
                .is_some_and(|name| name.emitted() == expected)
        })
    }

    #[test]
    fn global_order_reaches_a_real_fixed_point_and_records_each_slot() {
        let mut program =
            lower("function folded(){const longValue=1+2;return longValue}consume(folded());");
        let report = run_typed_pipeline(
            &mut program,
            &TypedEditInput::default(),
            &TypedPipelineOptions {
                minify: true,
                ..TypedPipelineOptions::default()
            },
        )
        .unwrap();
        assert!(report.stats.iterations >= 2);
        assert!(
            report
                .stats
                .pass(OptimizationPass::ConstantPropagationAndFolding)
                .runs
                >= 2
        );
        assert!(
            report
                .stats
                .pass(OptimizationPass::InlineSingleUseVariables)
                .changes
                > 0
        );
        program.validate().unwrap();
    }

    #[test]
    fn unchanged_minifying_round_reuses_the_initial_analysis() {
        let mut program = lower("consume();");
        let report = run_typed_pipeline(
            &mut program,
            &TypedEditInput::default(),
            &TypedPipelineOptions {
                minify: true,
                ..TypedPipelineOptions::default()
            },
        )
        .unwrap();

        assert_eq!(report.stats.iterations, 1);
        assert_eq!(
            report.stats.pass(OptimizationPass::BuildSemanticModel).runs,
            1
        );
    }

    #[test]
    fn structural_changes_rebuild_analysis_before_binding_sensitive_work() {
        let mut program = lower("if(true){consume(1)}");
        let report = run_typed_pipeline(
            &mut program,
            &TypedEditInput::default(),
            &TypedPipelineOptions {
                minify: true,
                ..TypedPipelineOptions::default()
            },
        )
        .unwrap();

        assert!(
            report
                .stats
                .pass(OptimizationPass::SimplifyControlFlow)
                .changes
                > 0
        );
        assert!(report.stats.pass(OptimizationPass::BuildSemanticModel).runs >= 2);
    }

    #[test]
    fn primitive_call_specialization_exposes_branch_and_function_inline_opportunities() {
        let mut program = lower(
            "function choose(flag){if(flag)return 10;return 20}globalThis.result=choose(true);",
        );
        let report = run_typed_pipeline(
            &mut program,
            &TypedEditInput::default(),
            &TypedPipelineOptions {
                minify: true,
                ..TypedPipelineOptions::default()
            },
        )
        .unwrap();

        assert!(report.stats.iterations >= 3);
        assert!(
            report
                .stats
                .pass(OptimizationPass::InlineClosedFunctions)
                .changes
                >= 5
        );
        assert!(
            report
                .stats
                .pass(OptimizationPass::SimplifyControlFlow)
                .changes
                >= 1
        );
        let live = program.preorder().unwrap();
        assert!(!live.iter().any(|node| matches!(
            program.node(*node).unwrap().data(),
            crate::typed_ir::IrNodeData::Function { .. }
                | crate::typed_ir::IrNodeData::IfStatement { .. }
        )));
        assert!(live.iter().any(|node| matches!(
            program.node(*node).unwrap().data(),
            crate::typed_ir::IrNodeData::NumberLiteral { value } if *value == 10.0
        )));
        program.validate().unwrap();
    }

    #[test]
    fn readable_mode_applies_defines_and_drop_flags_without_mangling() {
        let mut program = lower("let descriptive=1;if(FLAG){console.log(descriptive)}debugger;");
        let edits = TypedEditInput {
            defines: vec![TypedValidatedDefine::primitive(
                "FLAG",
                ConstVal::Bool(false),
            )],
            ..TypedEditInput::default()
        };
        let report = run_typed_pipeline(
            &mut program,
            &edits,
            &TypedPipelineOptions {
                minify: false,
                drop_debugger: true,
                drop_console: true,
                ..TypedPipelineOptions::default()
            },
        )
        .unwrap();
        assert!(!report.mangling.changed());
        assert!(
            program.names().iter().any(|name| {
                name.original() == "descriptive" && name.emitted() == "descriptive"
            })
        );
    }

    #[test]
    fn readable_mode_does_not_run_compression_passes_without_policy_edits() {
        let mut program = lower("if(true){consume(1+2)}");
        let report = run_typed_pipeline(
            &mut program,
            &TypedEditInput::default(),
            &TypedPipelineOptions::default(),
        )
        .unwrap();

        assert_eq!(
            report
                .stats
                .pass(OptimizationPass::ConstantPropagationAndFolding)
                .runs,
            0
        );
        assert_eq!(
            report
                .stats
                .pass(OptimizationPass::SimplifyControlFlow)
                .runs,
            0
        );
        let live = program.preorder().unwrap();
        assert!(live.iter().any(|node| matches!(
            program.node(*node).unwrap().data(),
            crate::typed_ir::IrNodeData::IfStatement { .. }
        )));
        assert!(live.iter().any(|node| matches!(
            program.node(*node).unwrap().data(),
            crate::typed_ir::IrNodeData::BinaryExpression { .. }
        )));
    }

    #[test]
    fn final_dependency_scan_observes_fixed_point_deletions() {
        let source = "if(false){load('gone')}load('kept');";
        let gone = source.find("load('gone')").unwrap() as u32;
        let kept = source.rfind("load('kept')").unwrap() as u32;
        let mut program = lower(source);
        let edits = TypedEditInput {
            dependencies: vec![
                TypedEditDependency {
                    specifier: "gone".into(),
                    origin: IrOrigin::Source(Span::new(gone, gone + 12)),
                },
                TypedEditDependency {
                    specifier: "kept".into(),
                    origin: IrOrigin::Source(Span::new(kept, kept + 12)),
                },
            ],
            ..TypedEditInput::default()
        };
        let report = run_typed_pipeline(
            &mut program,
            &edits,
            &TypedPipelineOptions {
                minify: true,
                ..TypedPipelineOptions::default()
            },
        )
        .unwrap();
        assert_eq!(
            report
                .retained_dependencies
                .iter()
                .map(|dependency| dependency.specifier.as_str())
                .collect::<Vec<_>>(),
            vec!["kept"]
        );
    }

    #[test]
    fn decorators_are_materialized_before_the_first_semantic_rebuild() {
        let mut program = lower_as(
            "function dec(value){return value}@dec class Example{@dec field=1}",
            SourceType::TypeScript,
        );
        let report = run_typed_pipeline(
            &mut program,
            &TypedEditInput::default(),
            &TypedPipelineOptions::default(),
        )
        .unwrap();
        assert_eq!(report.decorators.decorated_classes, 1);
        assert!(report.decorators.es_decorate_name.is_some());
        assert!(report.decorators.run_initializers_name.is_some());
        program.validate().unwrap();
        TypedAnalysis::rebuild(&program).unwrap();
    }

    #[test]
    fn trivial_module_runs_late_peephole_without_semantic_or_fixed_point() {
        let program = lower_as(
            r#"globalThis.__reg||(globalThis.__reg={});globalThis.__reg["alpha_key"]=1;"#,
            SourceType::Module,
        );
        let (program, report, plan, dynamic_scope_hazard) = run_typed_pipeline_with_modules(
            program,
            &TypedEditInput::default(),
            &TypedPipelineOptions {
                minify: true,
                ..TypedPipelineOptions::default()
            },
            &TypedModuleOptions {
                mode: TypedModuleMode::BundledCommonJs,
                preserve_all_exports: false,
                ..TypedModuleOptions::default()
            },
        )
        .unwrap();

        assert!(report.trivial_effect_module);
        assert!(!dynamic_scope_hazard);
        assert_eq!(plan.sealed_revision(), Some(program.revision()));
        assert_eq!(report.stats.iterations, 0);
        assert_eq!(
            report.stats.pass(OptimizationPass::BuildSemanticModel).runs,
            0
        );
        assert_eq!(report.stats.pass(OptimizationPass::LatePeephole).runs, 1);
        assert_eq!(report.stats.pass(OptimizationPass::LatePeephole).changes, 1);
        assert_eq!(
            report
                .stats
                .pass(OptimizationPass::ConstantPropagationAndFolding)
                .runs,
            0
        );
        assert_eq!(
            report
                .stats
                .pass(OptimizationPass::InlineSingleUseVariables)
                .runs,
            0
        );
        assert!(!report.mangling.changed());

        assert!(
            has_identifier_property(&program, "alpha_key"),
            "computed registry key was not normalized"
        );
        program.validate().unwrap();
    }

    #[test]
    fn analysis_planned_trivial_module_runs_the_same_late_compaction() {
        let program = lower_as(
            r#"globalThis.__reg||(globalThis.__reg={});globalThis.__reg["secondary_key"]=2;"#,
            SourceType::Module,
        );
        let (program, report, plan, dynamic_scope_hazard) = run_typed_pipeline_with_modules(
            program,
            &TypedEditInput::default(),
            &TypedPipelineOptions {
                minify: true,
                ..TypedPipelineOptions::default()
            },
            &TypedModuleOptions {
                mode: TypedModuleMode::BundledCommonJs,
                // Absent linker liveness keeps the semantic planner as the owner, exercising the
                // second trivial branch instead of the exact empty-liveness early return.
                preserve_all_exports: true,
                ..TypedModuleOptions::default()
            },
        )
        .unwrap();

        assert!(report.trivial_effect_module);
        assert!(!dynamic_scope_hazard);
        assert_eq!(plan.sealed_revision(), Some(program.revision()));
        assert_eq!(report.stats.iterations, 0);
        assert_eq!(
            report.stats.pass(OptimizationPass::BuildSemanticModel).runs,
            1
        );
        assert_eq!(report.stats.pass(OptimizationPass::LatePeephole).runs, 1);
        assert_eq!(report.stats.pass(OptimizationPass::LatePeephole).changes, 1);
        assert_eq!(
            report
                .stats
                .pass(OptimizationPass::ConstantPropagationAndFolding)
                .runs,
            0
        );
        assert!(!report.mangling.changed());
        assert!(
            has_identifier_property(&program, "secondary_key"),
            "semantic-planned trivial key was not normalized"
        );
        program.validate().unwrap();
    }

    #[test]
    fn module_plan_is_built_before_the_fixed_point_and_sealed_after_names() {
        let mut program = lower_as(
            "import 'dep';const descriptive=1;consume(descriptive);",
            SourceType::Module,
        );
        let (next, report, plan, dynamic_scope_hazard) = run_typed_pipeline_with_modules(
            program,
            &TypedEditInput::default(),
            &TypedPipelineOptions {
                minify: true,
                ..TypedPipelineOptions::default()
            },
            &TypedModuleOptions {
                mode: TypedModuleMode::BundledCommonJs,
                ..TypedModuleOptions::default()
            },
        )
        .unwrap();
        program = next;
        assert!(report.stats.iterations >= 1);
        assert!(!dynamic_scope_hazard);
        assert_eq!(plan.sealed_revision(), Some(program.revision()));
        assert_eq!(plan.requests().len(), 1);
        assert_eq!(
            plan.requests()[0].kind,
            TypedModuleRequestKind::StaticImport
        );
        assert_eq!(plan.requests()[0].specifier, "dep");
        program.validate().unwrap();
    }
}
