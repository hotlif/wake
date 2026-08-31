//! Adapter from the build-facing optimizer input to the owned typed pipeline.
//!
//! Build-facing values are converted once to typed edits, module options, and typed provenance.

use std::collections::BTreeSet;
use std::error::Error as _;

use wake_common::{Interner, Span};
use wake_ecma_ast::{ExportDefaultKind, ModuleExportName, Pattern, Program, Statement};
use wake_ecma_semantic::{SemanticModel, SymbolId, analyze};

use crate::typed_edits::{
    TypedDefineValue, TypedEditDependency, TypedEditInput, TypedRemovalKind,
    TypedTrustedExpressionEdit, TypedTrustedRemoval, TypedValidatedDefine,
};
use crate::typed_ir::{
    DerivedOriginKind, IrNodeData, IrOrigin, SyntheticOriginKind, TypedLoweringPlan, TypedProgram,
    is_presemantic_inert_export_const,
};
use crate::typed_modules::{
    TypedLinkerLiveness, TypedModuleError, TypedModuleId, TypedModuleMode, TypedModuleOptions,
    TypedModulePlan,
};
use crate::typed_pipeline::{
    TypedPipelineOptions, TypedPipelineReport, run_typed_pipeline_with_modules,
};
use crate::{
    MinifyDiagnostic, MinifyDiagnosticKind, NodeOrigin, OptimizationPass, OptimizeDependency,
    OptimizeInput, SyntheticReason, ValidatedDefineValue,
};

pub(crate) struct OwnedOptimizationResult {
    pub program: TypedProgram,
    pub module_plan: TypedModulePlan,
    pub report: TypedPipelineReport,
    pub retained_dependencies: Vec<OptimizeDependency>,
    pub dynamic_scope_hazard: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ExportSymbolBinding {
    export_name: String,
    local_name: String,
    symbol_id: SymbolId,
}

fn module_symbol(semantic: &SemanticModel, name: wake_common::Atom) -> Option<SymbolId> {
    semantic
        .scopes
        .first()
        .and_then(|scope| scope.bindings.get(&name))
        .copied()
}

/// Resolve stable public export names while the parser AST and its one semantic model are both
/// alive. The resulting SymbolIds are consumed immediately by typed lowering/module planning and
/// never cross an optimizer task or persistence boundary.
fn collect_export_symbol_bindings(
    program: &Program<'_>,
    interner: &Interner,
    semantic: &SemanticModel,
) -> Vec<ExportSymbolBinding> {
    fn pattern_bindings(
        pattern: &Pattern<'_>,
        interner: &Interner,
        semantic: &SemanticModel,
        bindings: &mut Vec<ExportSymbolBinding>,
    ) {
        match pattern {
            Pattern::Ident(identifier) => {
                if let Some(symbol_id) = module_symbol(semantic, identifier.name) {
                    let name = interner.resolve(identifier.name);
                    bindings.push(ExportSymbolBinding {
                        export_name: name.clone(),
                        local_name: name,
                        symbol_id,
                    });
                }
            }
            Pattern::Array(array) => {
                for element in array.elements.iter().flatten() {
                    pattern_bindings(element, interner, semantic, bindings);
                }
            }
            Pattern::Object(object) => {
                for property in object.properties.iter() {
                    pattern_bindings(&property.value, interner, semantic, bindings);
                }
                if let Some(rest) = &object.rest {
                    pattern_bindings(&rest.argument, interner, semantic, bindings);
                }
            }
            Pattern::Assignment(assignment) => {
                pattern_bindings(&assignment.left, interner, semantic, bindings);
            }
            Pattern::Rest(rest) => {
                pattern_bindings(&rest.argument, interner, semantic, bindings);
            }
        }
    }

    let mut bindings = Vec::new();
    for statement in program.body.iter() {
        match statement {
            Statement::ExportNamed(export) => {
                if let Some(declaration) = &export.declaration {
                    match declaration {
                        Statement::VariableDeclaration(declaration) => {
                            for declarator in declaration.declarations.iter() {
                                pattern_bindings(&declarator.id, interner, semantic, &mut bindings);
                            }
                        }
                        Statement::FunctionDeclaration(function) => {
                            if let Some(identifier) = function.id
                                && let Some(symbol_id) = module_symbol(semantic, identifier.name)
                            {
                                let name = interner.resolve(identifier.name);
                                bindings.push(ExportSymbolBinding {
                                    export_name: name.clone(),
                                    local_name: name,
                                    symbol_id,
                                });
                            }
                        }
                        Statement::ClassDeclaration(class) => {
                            if let Some(identifier) = class.id
                                && let Some(symbol_id) = module_symbol(semantic, identifier.name)
                            {
                                let name = interner.resolve(identifier.name);
                                bindings.push(ExportSymbolBinding {
                                    export_name: name.clone(),
                                    local_name: name,
                                    symbol_id,
                                });
                            }
                        }
                        _ => {}
                    }
                }
                for specifier in export.specifiers.iter().filter(|_| export.source.is_none()) {
                    let export_name = match &specifier.exported {
                        ModuleExportName::Ident(identifier) => interner.resolve(identifier.name),
                        ModuleExportName::String(value) => interner.resolve(*value),
                    };
                    let ModuleExportName::Ident(local) = &specifier.local else {
                        continue;
                    };
                    if let Some(symbol_id) = module_symbol(semantic, local.name) {
                        bindings.push(ExportSymbolBinding {
                            export_name,
                            local_name: interner.resolve(local.name),
                            symbol_id,
                        });
                    }
                }
            }
            Statement::ExportDefault(export) => {
                let identifier = match &export.declaration {
                    ExportDefaultKind::Function(function) => function.id,
                    ExportDefaultKind::Class(class) => class.id,
                    ExportDefaultKind::Expression(_) => None,
                };
                if let Some(identifier) = identifier
                    && let Some(symbol_id) = module_symbol(semantic, identifier.name)
                {
                    bindings.push(ExportSymbolBinding {
                        export_name: "default".to_owned(),
                        local_name: interner.resolve(identifier.name),
                        symbol_id,
                    });
                }
            }
            _ => {}
        }
    }
    bindings
}

/// Build the parser-only half of the whole-module trivial fast path.
///
/// `Some(empty)` linker liveness is an authoritative wake_graph proof that this module has no
/// externally or locally rooted export. We still accept only a deliberately tiny statement
/// grammar here: direct export functions and strictly inert `export const` declarations may be
/// elided, while empty exports/statements and expression statements may remain. Aliases,
/// re-exports, imports, local declarations and every other statement shape fall back to the
/// ordinary semantic path.
fn pre_semantic_trivial_module_elision(
    program: &Program<'_>,
    input: &OptimizeInput<'_>,
) -> Option<TypedLoweringPlan> {
    if !input.bundled_commonjs() {
        return None;
    }
    let liveness = input.linker_liveness.as_ref()?;
    if !liveness.live_export_names().is_empty() {
        return None;
    }

    let mut plan = TypedLoweringPlan::default();
    let mut elided_any = false;
    for (ordinal, statement) in program.body.iter().enumerate() {
        match statement {
            Statement::ExportNamed(export) => match export.declaration {
                Some(Statement::FunctionDeclaration(function))
                    if export.specifiers.is_empty()
                        && export.source.is_none()
                        && export.attributes.is_none()
                        && function
                            .id
                            .is_some_and(|identifier| !identifier.span.is_dummy())
                        && !export.span.is_dummy()
                        && !function.span.is_dummy()
                        && !overlaps_trusted_input(input, export.span) =>
                {
                    plan.elide_top_level_export_function(ordinal);
                    elided_any = true;
                }
                Some(Statement::VariableDeclaration(declaration))
                    if export.specifiers.is_empty()
                        && export.source.is_none()
                        && export.attributes.is_none()
                        && !export.span.is_dummy()
                        && is_presemantic_inert_export_const(declaration)
                        && !overlaps_trusted_input(input, export.span) =>
                {
                    plan.elide_top_level_export_const(ordinal);
                    elided_any = true;
                }
                None if export.specifiers.is_empty()
                    && export.source.is_none()
                    && export.attributes.is_none() => {}
                _ => return None,
            },
            Statement::ExportDefault(export) => {
                let ExportDefaultKind::Function(function) = export.declaration else {
                    return None;
                };
                if export.span.is_dummy()
                    || function.span.is_dummy()
                    || function
                        .id
                        .is_some_and(|identifier| identifier.span.is_dummy())
                    || overlaps_trusted_input(input, export.span)
                {
                    return None;
                }
                plan.elide_top_level_export_function(ordinal);
                elided_any = true;
            }
            Statement::Empty(_) | Statement::Expression(_) => {}
            _ => return None,
        }
    }
    elided_any.then_some(plan)
}

/// Try the whole-module semantic-free owner. Rejection is not an optimizer error: it merely asks
/// the caller to run the existing parser semantic + typed lowering path.
fn lower_exact_trivial_without_semantic(
    program: &Program<'_>,
    interner: &Interner,
    input: &OptimizeInput<'_>,
) -> Option<TypedProgram> {
    let plan = pre_semantic_trivial_module_elision(program, input)?;
    let owned = TypedProgram::lower_with_plan(program, interner, None, &plan).ok()?;
    is_semantic_free_trivial_owner(&owned).then_some(owned)
}

/// Validate the residual owner with an allow-list. This is intentionally stricter than proving
/// individual expressions pure: residual expressions are kept, but none may conceal syntax which
/// needs binding analysis or dynamic-scope/module planning. New IR variants therefore fall back
/// until they are reviewed and explicitly admitted here.
fn is_semantic_free_trivial_owner(program: &TypedProgram) -> bool {
    let Some(IrNodeData::Program {
        spread_helper,
        object_spread_helper,
        for_of_helper,
        body,
        ..
    }) = program.node(program.root()).map(|node| node.data())
    else {
        return false;
    };
    if spread_helper.is_some() || object_spread_helper.is_some() || for_of_helper.is_some() {
        return false;
    }
    let Some(body) = program.list(*body) else {
        return false;
    };
    if body.items().iter().any(
        |statement| match program.node(*statement).map(|node| node.data()) {
            Some(IrNodeData::EmptyStatement | IrNodeData::ExpressionStatement { .. }) => false,
            Some(IrNodeData::ExportNamedDeclaration {
                declaration: None,
                specifiers,
                source: None,
                attributes: None,
            }) => program
                .list(*specifiers)
                .is_none_or(|specifiers| !specifiers.items().is_empty()),
            _ => true,
        },
    ) {
        return false;
    }

    program.nodes().iter().all(|node| {
        if node.is_tombstone() {
            return false;
        }
        match node.data() {
            IrNodeData::Program { .. }
            | IrNodeData::EmptyStatement
            | IrNodeData::ExpressionStatement { .. }
            | IrNodeData::NumberLiteral { .. }
            | IrNodeData::StringLiteral { .. }
            | IrNodeData::BooleanLiteral { .. }
            | IrNodeData::NullLiteral
            | IrNodeData::BigIntLiteral { .. }
            | IrNodeData::RegExpLiteral { .. }
            | IrNodeData::TemplateLiteral { .. }
            | IrNodeData::TemplateElement { .. }
            | IrNodeData::Name { .. }
            | IrNodeData::Identifier { .. }
            | IrNodeData::ArrayExpression { .. }
            | IrNodeData::Elision
            | IrNodeData::ObjectExpression { .. }
            | IrNodeData::UnaryExpression { .. }
            | IrNodeData::UpdateExpression { .. }
            | IrNodeData::BinaryExpression { .. }
            | IrNodeData::LogicalExpression { .. }
            | IrNodeData::AssignmentExpression { .. }
            | IrNodeData::ConditionalExpression { .. }
            | IrNodeData::MemberExpression { .. }
            | IrNodeData::SequenceExpression { .. }
            | IrNodeData::SpreadElement { .. } => true,
            IrNodeData::ObjectProperty { method, .. } => !method,
            IrNodeData::ExportNamedDeclaration {
                declaration: None,
                specifiers,
                source: None,
                attributes: None,
            } => program
                .list(*specifiers)
                .is_some_and(|specifiers| specifiers.items().is_empty()),
            // Function/arrow/class, call/new/tagged, await/yield/import/meta/this/super/with and
            // every declaration or unreviewed syntax form all land here.
            _ => false,
        }
    })
}

/// Prove which dead export function declarations never need to enter owned IR.
///
/// This deliberately recognizes only the narrow case needed by bundled CommonJS lowering with
/// exact linker liveness. A candidate is rejected when another export alias names its symbol,
/// another source occurrence references it, a direct-eval-like unresolved `eval` reference makes
/// module bindings observable, or a trusted structural edit overlaps its source. The only symbol
/// reference accepted by this first slice is one contained by the declaration itself (for example
/// direct self recursion).
fn pre_lower_export_function_elision(
    program: &Program<'_>,
    interner: &Interner,
    semantic: &SemanticModel,
    export_bindings: &[ExportSymbolBinding],
    input: &OptimizeInput<'_>,
) -> TypedLoweringPlan {
    if !input.bundled_commonjs() {
        return TypedLoweringPlan::default();
    }
    let Some(liveness) = input.linker_liveness.as_ref() else {
        return TypedLoweringPlan::default();
    };

    // The parser semantic model records reference identity but not call parents. Treating every
    // unresolved `eval` occurrence as potentially direct is intentionally conservative: it
    // prevents elision whenever source-level dynamic lookup might observe a hoisted function.
    let eval = interner.intern("eval");
    if semantic
        .references
        .iter()
        .any(|reference| reference.name == eval && reference.resolved.is_none())
    {
        return TypedLoweringPlan::default();
    }

    let mut plan = TypedLoweringPlan::default();
    for (ordinal, statement) in program.body.iter().enumerate() {
        let (function, direct_export_name) = match statement {
            Statement::ExportNamed(export) => {
                let Some(Statement::FunctionDeclaration(function)) = export.declaration else {
                    continue;
                };
                let Some(identifier) = function.id else {
                    continue;
                };
                (function, interner.resolve(identifier.name))
            }
            Statement::ExportDefault(export) => {
                let ExportDefaultKind::Function(function) = export.declaration else {
                    continue;
                };
                (function, "default".to_owned())
            }
            _ => continue,
        };
        let declaration_span = statement.span();
        if declaration_span.is_dummy()
            || function.span.is_dummy()
            || overlaps_trusted_input(input, declaration_span)
            || liveness.live_export_names().contains(&direct_export_name)
        {
            continue;
        }

        let symbol = match function.id {
            Some(identifier) => {
                if identifier.span.is_dummy() {
                    continue;
                }
                let Some(symbol) = module_symbol(semantic, identifier.name) else {
                    continue;
                };
                // Multiple declarations or a separate local export alias need a declaration to
                // remain in the pre-module semantic model. They are outside this first slice even
                // when all aliases happen to be dead.
                if semantic
                    .binding_occurrences
                    .iter()
                    .filter(|occurrence| occurrence.symbol == symbol)
                    .count()
                    != 1
                    || export_bindings
                        .iter()
                        .filter(|binding| binding.symbol_id == symbol)
                        .count()
                        != 1
                    || export_bindings.iter().any(|binding| {
                        binding.symbol_id == symbol
                            && liveness.live_export_names().contains(&binding.export_name)
                    })
                    || semantic.references.iter().any(|reference| {
                        reference.resolved == Some(symbol)
                            && (reference.span.is_dummy()
                                || !declaration_span.contains(reference.span))
                    })
                {
                    continue;
                }
                Some(symbol)
            }
            None => None,
        };

        debug_assert!(symbol.is_some() || direct_export_name == "default");
        plan.elide_top_level_export_function(ordinal);
    }
    plan
}

fn overlaps_trusted_input(input: &OptimizeInput<'_>, candidate: Span) -> bool {
    input
        .expression_edits()
        .iter()
        .map(|edit| edit.target())
        .chain(input.statement_removals().iter().copied())
        .chain(input.binding_removals().iter().copied())
        .any(|target| spans_overlap(candidate, target))
}

fn spans_overlap(left: Span, right: Span) -> bool {
    left.is_dummy()
        || right.is_dummy()
        || left.contains(right)
        || right.contains(left)
        || (left.lo < right.hi && right.lo < left.hi)
}

pub(crate) fn optimize_owned_program(
    program: &Program<'_>,
    interner: &Interner,
    input: &OptimizeInput<'_>,
) -> Result<OwnedOptimizationResult, MinifyDiagnostic> {
    let semantic_free = lower_exact_trivial_without_semantic(program, interner, input);
    let (owned, export_bindings) = if let Some(owned) = semantic_free {
        (owned, Vec::new())
    } else {
        let semantic = analyze(program);
        let export_bindings = collect_export_symbol_bindings(program, interner, &semantic);
        let lowering_plan = pre_lower_export_function_elision(
            program,
            interner,
            &semantic,
            &export_bindings,
            input,
        );
        let owned =
            TypedProgram::lower_with_plan(program, interner, Some(&semantic), &lowering_plan)
                .map_err(|error| {
                    diagnostic(
                        input,
                        MinifyDiagnosticKind::InvalidIr,
                        OptimizationPass::ApplyTrustedEdits,
                        0,
                        format!("owned typed IR lowering failed: {error}"),
                    )
                })?;
        (owned, export_bindings)
    };
    let edits = typed_edit_input(input)?;
    let mut reserved_names = input.reserved_names.clone();
    reserved_names.extend(
        export_bindings
            .iter()
            .map(|binding| binding.local_name.clone()),
    );
    let pipeline_options = TypedPipelineOptions {
        minify: input.minify,
        drop_debugger: input.drop_debugger,
        drop_console: input.drop_console,
        reserved_names,
    };
    let module_options = typed_module_options(input, &export_bindings);
    let (owned, report, module_plan, dynamic_scope_hazard) =
        run_typed_pipeline_with_modules(owned, &edits, &pipeline_options, &module_options)
            .map_err(|error| {
                let invalid_linker_liveness = error
                    .source()
                    .and_then(|source| source.downcast_ref::<TypedModuleError>())
                    .is_some_and(|source| {
                        matches!(
                            source,
                            TypedModuleError::InvalidInput { message, .. }
                                if message.contains("linker liveness references unknown symbol")
                        )
                    });
                let kind = if error.iterations
                    == crate::typed_pipeline::MAX_TYPED_PIPELINE_ITERATIONS
                    && error.message.contains("converge")
                {
                    MinifyDiagnosticKind::DidNotConverge
                } else if error.pass == OptimizationPass::ApplyTrustedEdits
                    || invalid_linker_liveness
                {
                    MinifyDiagnosticKind::InvalidTrustedEdit
                } else {
                    MinifyDiagnosticKind::InvalidIr
                };
                diagnostic(
                    input,
                    kind,
                    error.pass,
                    error.iterations,
                    pipeline_error_message(&error),
                )
            })?;
    let retained_dependencies = retained_dependencies(input, &report, &module_plan);

    Ok(OwnedOptimizationResult {
        program: owned,
        module_plan,
        report,
        retained_dependencies,
        dynamic_scope_hazard,
    })
}

fn pipeline_error_message(error: &crate::typed_pipeline::TypedPipelineError) -> String {
    let mut message = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    message
}

fn retained_dependencies(
    input: &OptimizeInput<'_>,
    report: &TypedPipelineReport,
    module_plan: &TypedModulePlan,
) -> Vec<OptimizeDependency> {
    input
        .dependencies
        .iter()
        .filter(|dependency| {
            let origin = typed_origin(dependency.origin);
            report.retained_dependencies.iter().any(|retained| {
                retained.specifier == dependency.specifier && retained.origin == origin
            }) || module_plan.requests().iter().any(|request| {
                request.specifier == dependency.specifier
                    && origins_share_source_lineage(request.origin, origin)
            })
        })
        .cloned()
        .collect()
}

fn origins_share_source_lineage(live: IrOrigin, dependency: IrOrigin) -> bool {
    let Some(dependency_anchor) = origin_anchor(dependency) else {
        return true;
    };
    let Some(live_anchor) = origin_anchor(live) else {
        return false;
    };
    live_anchor.contains(dependency_anchor) || dependency_anchor.contains(live_anchor)
}

const fn origin_anchor(origin: IrOrigin) -> Option<wake_common::Span> {
    match origin {
        IrOrigin::Source(span) => Some(span),
        IrOrigin::Derived { anchor, .. } | IrOrigin::Synthetic { anchor, .. } => anchor,
    }
}

fn typed_edit_input(input: &OptimizeInput<'_>) -> Result<TypedEditInput, MinifyDiagnostic> {
    let mut defines = Vec::with_capacity(input.defines.len());
    for define in &input.defines {
        let value = match &define.value {
            ValidatedDefineValue::Primitive(value) => TypedDefineValue::Primitive(value.clone()),
            ValidatedDefineValue::Expression(expression) => TypedDefineValue::Expression(
                required_owner(input, expression.owner(), "define expression")?,
            ),
        };
        defines.push(TypedValidatedDefine {
            key: define.key.clone(),
            value,
        });
    }

    let mut expression_edits = Vec::with_capacity(input.expression_edits().len());
    for edit in input.expression_edits() {
        expression_edits.push(TypedTrustedExpressionEdit::new(
            edit.target(),
            required_owner(input, edit.expression().owner(), "trusted expression edit")?,
        ));
    }
    let mut removals =
        Vec::with_capacity(input.statement_removals().len() + input.binding_removals().len());
    removals.extend(
        input
            .statement_removals()
            .iter()
            .copied()
            .map(|target| TypedTrustedRemoval {
                target,
                kind: TypedRemovalKind::Statement,
            }),
    );
    removals.extend(
        input
            .binding_removals()
            .iter()
            .copied()
            .map(|target| TypedTrustedRemoval {
                target,
                kind: TypedRemovalKind::Binding,
            }),
    );
    let dependencies = input
        .dependencies
        .iter()
        .map(|dependency| TypedEditDependency {
            specifier: dependency.specifier.clone(),
            origin: typed_origin(dependency.origin),
        })
        .collect();

    Ok(TypedEditInput {
        defines,
        expression_edits,
        removals,
        dependencies,
    })
}

fn required_owner(
    input: &OptimizeInput<'_>,
    owner: Option<&crate::typed_ir::TypedExpressionOwner>,
    description: &str,
) -> Result<crate::typed_ir::TypedExpressionOwner, MinifyDiagnostic> {
    owner.cloned().ok_or_else(|| {
        diagnostic(
            input,
            MinifyDiagnosticKind::InvalidTrustedEdit,
            OptimizationPass::ApplyTrustedEdits,
            0,
            format!("{description} is not backed by parser-validated owned IR"),
        )
    })
}

fn typed_module_options(
    input: &OptimizeInput<'_>,
    export_bindings: &[ExportSymbolBinding],
) -> TypedModuleOptions {
    let mode = if input.bundled_commonjs() {
        TypedModuleMode::BundledCommonJs
    } else if input.preserve_commonjs() {
        TypedModuleMode::PreserveCommonJs
    } else {
        TypedModuleMode::PreserveEsm
    };
    let module_id = TypedModuleId(
        input
            .linker_liveness
            .as_ref()
            .map_or(0, |liveness| liveness.module_id()),
    );
    let mut linker_liveness = TypedLinkerLiveness::default();
    let observed_export_names = input
        .linker_liveness
        .as_ref()
        .map(|liveness| {
            liveness
                .observed_export_names()
                .iter()
                .cloned()
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();
    if let Some(liveness) = &input.linker_liveness {
        for binding in export_bindings {
            if liveness.live_export_names().contains(&binding.export_name) {
                linker_liveness.insert(module_id, binding.symbol_id);
            }
        }
    }
    TypedModuleOptions {
        mode,
        module_id,
        preserve_all_exports: input.linker_liveness.is_none(),
        preserve_export_star: input
            .linker_liveness
            .as_ref()
            .is_none_or(|liveness| liveness.preserve_export_star()),
        observed_export_names,
        linker_liveness,
    }
}

fn typed_origin(origin: NodeOrigin) -> IrOrigin {
    match origin {
        NodeOrigin::Source(span) => IrOrigin::Source(span),
        NodeOrigin::Synthetic { anchor, reason } => match reason {
            SyntheticReason::LoweringGenerated => IrOrigin::Derived {
                anchor,
                kind: DerivedOriginKind::ParserLowering,
            },
            SyntheticReason::TrustedReplacement => IrOrigin::Synthetic {
                anchor,
                kind: SyntheticOriginKind::TrustedEdit,
            },
            SyntheticReason::OptimizerGenerated => IrOrigin::Synthetic {
                anchor,
                kind: SyntheticOriginKind::Optimization,
            },
        },
    }
}

fn diagnostic(
    input: &OptimizeInput<'_>,
    kind: MinifyDiagnosticKind,
    pass: OptimizationPass,
    iterations: usize,
    message: impl Into<String>,
) -> MinifyDiagnostic {
    MinifyDiagnostic {
        kind,
        module_name: input.module_name.clone(),
        pass: Some(pass),
        iterations,
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::typed_ir::IrNodeData;
    use crate::{ConstVal, LinkerExportLiveness, ValidatedDefine};
    use wake_ecma_ast::SourceType;
    use wake_ecma_parser::parse;

    fn lowering_plan(
        source: &str,
        configure: impl FnOnce(&mut OptimizeInput<'_>),
    ) -> (TypedLoweringPlan, TypedProgram) {
        let interner = Interner::new();
        let parsed = parse(source, &interner, SourceType::Module);
        assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);
        let mut input = OptimizeInput::new(source);
        configure(&mut input);
        parsed.module.with_ast(|program| {
            let semantic = analyze(program);
            let bindings = collect_export_symbol_bindings(program, &interner, &semantic);
            let plan =
                pre_lower_export_function_elision(program, &interner, &semantic, &bindings, &input);
            let typed =
                TypedProgram::lower_with_plan(program, &interner, Some(&semantic), &plan).unwrap();
            (plan, typed)
        })
    }

    fn semantic_free_owner(source: &str, live_export_names: &[&str]) -> Option<TypedProgram> {
        let interner = Interner::new();
        let parsed = parse(source, &interner, SourceType::Module);
        assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);
        let mut input = OptimizeInput::new(source);
        input.set_bundled_commonjs(true);
        input.linker_liveness = Some(LinkerExportLiveness::new(
            9,
            live_export_names.iter().copied(),
        ));
        parsed
            .module
            .with_ast(|program| lower_exact_trivial_without_semantic(program, &interner, &input))
    }

    fn assert_first_statement_is_empty_export(program: &TypedProgram) {
        let IrNodeData::Program { body, .. } = program
            .node(program.root())
            .expect("typed program root")
            .data()
        else {
            panic!("typed root is not a program");
        };
        let statement = program.list(*body).expect("program body").items()[0];
        let IrNodeData::ExportNamedDeclaration {
            declaration: None,
            specifiers,
            source: None,
            attributes: None,
        } = program.node(statement).expect("first statement").data()
        else {
            panic!("elided declaration did not retain an empty ESM export marker");
        };
        assert!(
            program
                .list(*specifiers)
                .expect("export specifiers")
                .items()
                .is_empty()
        );
        assert!(
            program
                .nodes()
                .iter()
                .all(|node| !matches!(node.data(), IrNodeData::Function { .. })),
            "the dead function entered owned IR"
        );
    }

    #[test]
    fn exact_trivial_module_uses_semantic_free_owner_for_global_registration() {
        let source = "export function dead(){return dead()}globalThis.__wake_registry.dead=1;";
        let owned = semantic_free_owner(source, &[]).expect("semantic-free trivial owner");

        assert!(
            owned.symbols().is_empty(),
            "parser semantic symbols leaked in"
        );
        assert_first_statement_is_empty_export(&owned);
        assert!(
            owned
                .nodes()
                .iter()
                .any(|node| { matches!(node.data(), IrNodeData::AssignmentExpression { .. }) })
        );
    }

    #[test]
    fn exact_json_like_export_consts_never_enter_semantic_or_owned_ir() {
        let source = r#"
            globalThis.__reg||(globalThis.__reg={});
            export function dead(){return dead()}
            export const CONFIG={name:'Table',nested:{active:true},sizes:[1,,2,-3],tag:`plain`},
                         DATA=[null,false,4n,/ok/gi];
            globalThis.__reg.table=1674;
        "#;
        let interner = Interner::new();
        let parsed = parse(source, &interner, SourceType::Module);
        assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);
        let mut input = OptimizeInput::new(source);
        input.set_bundled_commonjs(true);
        input.linker_liveness = Some(LinkerExportLiveness::new(9, Vec::<String>::new()));
        // Real production builds carry NODE_ENV even when this module does not read it. Defines
        // must not disable the parser-semantic-free proof.
        input.defines = vec![ValidatedDefine::primitive(
            "process.env.NODE_ENV",
            ConstVal::Str("production".into()),
        )];

        let owned = parsed
            .module
            .with_ast(|program| {
                let plan = pre_semantic_trivial_module_elision(program, &input)
                    .expect("pre-semantic lowering plan");
                assert_eq!(plan.elided_top_level_export_functions(), &[1]);
                assert_eq!(plan.elided_top_level_export_consts(), &[2]);
                lower_exact_trivial_without_semantic(program, &interner, &input)
            })
            .expect("JSON-like dead exports should use the semantic-free owner");

        assert!(owned.symbols().is_empty());
        assert!(owned.nodes().iter().all(|node| {
            !matches!(
                node.data(),
                IrNodeData::Function { .. } | IrNodeData::VariableDeclaration { .. }
            )
        }));
        let IrNodeData::Program { body, .. } = owned.node(owned.root()).expect("typed root").data()
        else {
            panic!("typed root is not a program");
        };
        assert_eq!(
            owned
                .list(*body)
                .expect("program body")
                .items()
                .iter()
                .filter(|statement| matches!(
                    owned.node(**statement).map(|node| node.data()),
                    Some(IrNodeData::ExportNamedDeclaration {
                        declaration: None,
                        ..
                    })
                ))
                .count(),
            2,
            "the function and whole const declaration each retain one ESM marker"
        );
    }

    #[test]
    fn observable_or_binding_sensitive_export_consts_fall_back() {
        for source in [
            "export const value=sideEffect();",
            "export const value={[sideEffect()]:1};",
            "export const value={...source};",
            "export const value=[...source];",
            "export const value={method(){}};",
            "export const value={get field(){return 1}};",
            "const field=1;export const value={field};",
            "export const {value}={value:1};",
            "export let value=1;",
            "export const value=1n+1;",
        ] {
            assert!(
                semantic_free_owner(source, &[]).is_none(),
                "unexpected semantic-free owner for {source}"
            );
        }
    }

    #[test]
    fn live_or_trusted_edited_export_const_falls_back() {
        assert!(semantic_free_owner("export const rooted={};", &["rooted"]).is_none());

        let source = "export const edited={nested:[1,2,3]};globalThis.done=true;";
        let interner = Interner::new();
        let parsed = parse(source, &interner, SourceType::Module);
        assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);
        parsed.module.with_ast(|program| {
            let Statement::ExportNamed(export) = program.body[0] else {
                panic!("expected named export");
            };
            let Some(Statement::VariableDeclaration(declaration)) = export.declaration else {
                panic!("expected export const");
            };
            let Pattern::Ident(identifier) = declaration.declarations[0].id else {
                panic!("expected identifier binding");
            };
            let mut input = OptimizeInput::new(source);
            input.set_bundled_commonjs(true);
            input.linker_liveness = Some(LinkerExportLiveness::new(9, Vec::<String>::new()));
            input.add_binding_removal(identifier.span);

            assert!(
                lower_exact_trivial_without_semantic(program, &interner, &input).is_none(),
                "trusted structural edits must own the overlapping declaration"
            );
        });
    }

    #[test]
    fn exact_live_root_or_local_reference_prevents_semantic_free_path() {
        let rooted = "export function rooted(){return 1}";
        assert!(semantic_free_owner(rooted, &["rooted"]).is_none());

        let locally_referenced =
            "export function retained(){return 1}globalThis.__wake_entry=retained;";
        assert!(semantic_free_owner(locally_referenced, &["retained"]).is_none());

        let locally_referenced_const = "export const retained={};globalThis.__wake_entry=retained;";
        assert!(
            semantic_free_owner(locally_referenced_const, &["retained"]).is_none(),
            "the exact graph keep set roots locally referenced const exports"
        );
    }

    #[test]
    fn residual_direct_eval_falls_back_to_full_semantic_path() {
        let source = "export function observed(){return 1}eval('observed');";
        assert!(semantic_free_owner(source, &[]).is_none());

        let interner = Interner::new();
        let parsed = parse(source, &interner, SourceType::Module);
        assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);
        parsed.module.with_ast(|program| {
            let mut input = OptimizeInput::new(source);
            input.set_bundled_commonjs(true);
            input.linker_liveness = Some(LinkerExportLiveness::new(9, Vec::<String>::new()));
            let optimized = optimize_owned_program(program, &interner, &input).unwrap();
            assert!(optimized.dynamic_scope_hazard);
        });
    }

    #[test]
    fn aliases_reexports_imports_and_local_declarations_reject_semantic_free_path() {
        for source in [
            "export function dead(){}export {dead as alias};",
            "export function dead(){}export {other} from 'dep';",
            "import 'dep';export function dead(){}",
            "const local=1;export function dead(){}",
        ] {
            assert!(
                semantic_free_owner(source, &[]).is_none(),
                "unexpected semantic-free owner for {source}"
            );
        }
    }

    #[test]
    fn exact_bundled_liveness_elides_dead_export_function_before_lowering() {
        let source = "export function dead(){return dead()}";
        let (plan, typed) = lowering_plan(source, |input| {
            input.set_bundled_commonjs(true);
            input.linker_liveness = Some(LinkerExportLiveness::new(9, Vec::<String>::new()));
        });

        assert_eq!(plan.elided_top_level_export_functions(), &[0]);
        assert_first_statement_is_empty_export(&typed);
    }

    #[test]
    fn elided_export_still_materializes_bundled_esmodule_identity() {
        let source = "export function dead(){return dead()}";
        let interner = Interner::new();
        let parsed = parse(source, &interner, SourceType::Module);
        assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);
        parsed.module.with_ast(|program| {
            let mut input = OptimizeInput::new(source);
            input.minify = true;
            input.set_bundled_commonjs(true);
            input.linker_liveness = Some(LinkerExportLiveness::new(9, Vec::<String>::new()));
            let optimized = optimize_owned_program(program, &interner, &input).unwrap();
            assert!(optimized.report.trivial_effect_module);
            assert_eq!(
                optimized
                    .report
                    .stats
                    .pass(OptimizationPass::BuildSemanticModel)
                    .runs,
                0,
                "binding-free exact liveness must not build an unused typed semantic model"
            );
            let (finalized, _) = crate::typed_modules::finalize_owned_typed_modules(
                optimized.program,
                optimized.module_plan,
                &crate::typed_modules::TypedFinalModuleFacts::default(),
            )
            .unwrap();
            let program = finalized.program();

            assert!(program.nodes().iter().any(|node| {
                matches!(
                    node.data(),
                    IrNodeData::StringLiteral { value } if value == "__esModule"
                )
            }));
            assert!(program.nodes().iter().all(|node| {
                !matches!(
                    node.data(),
                    IrNodeData::Function { name: Some(name), .. }
                        if program
                            .node(*name)
                            .and_then(|node| match node.data() {
                                IrNodeData::Identifier { name } => program.node(*name),
                                _ => None,
                            })
                            .and_then(|node| match node.data() {
                                IrNodeData::Name { name } => program.name(*name),
                                _ => None,
                            })
                            .is_some_and(|name| name.original() == "dead")
                )
            }));
        });
    }

    #[test]
    fn pre_lower_elision_requires_bundled_commonjs_and_exact_liveness() {
        let source = "export function retained(){return 1}";
        let (without_exact_liveness, _) = lowering_plan(source, |input| {
            input.set_bundled_commonjs(true);
        });
        let (preserved_esm, _) = lowering_plan(source, |input| {
            input.linker_liveness = Some(LinkerExportLiveness::new(9, Vec::<String>::new()));
        });
        let (live_export, _) = lowering_plan(source, |input| {
            input.set_bundled_commonjs(true);
            input.linker_liveness = Some(LinkerExportLiveness::new(9, ["retained"]));
        });

        assert!(
            without_exact_liveness
                .elided_top_level_export_functions()
                .is_empty()
        );
        assert!(preserved_esm.elided_top_level_export_functions().is_empty());
        assert!(live_export.elided_top_level_export_functions().is_empty());
    }

    #[test]
    fn live_alias_and_external_local_reference_keep_export_function() {
        let alias_source = "export function retained(){return 1}export {retained as publicName};";
        let (live_alias, _) = lowering_plan(alias_source, |input| {
            input.set_bundled_commonjs(true);
            input.linker_liveness = Some(LinkerExportLiveness::new(9, ["publicName"]));
        });
        let reference_source = "export function retained(){return 1}globalThis.consume(retained);";
        let (external_reference, _) = lowering_plan(reference_source, |input| {
            input.set_bundled_commonjs(true);
            input.linker_liveness = Some(LinkerExportLiveness::new(9, Vec::<String>::new()));
        });

        assert!(live_alias.elided_top_level_export_functions().is_empty());
        assert!(
            external_reference
                .elided_top_level_export_functions()
                .is_empty()
        );
    }

    #[test]
    fn unresolved_eval_and_overlapping_trusted_removal_disable_elision() {
        let eval_source = "export function observed(){return 1}eval('observed');";
        let (direct_eval, _) = lowering_plan(eval_source, |input| {
            input.set_bundled_commonjs(true);
            input.linker_liveness = Some(LinkerExportLiveness::new(9, Vec::<String>::new()));
        });
        assert!(direct_eval.elided_top_level_export_functions().is_empty());

        let source = "export function edited(){return 1}";
        let interner = Interner::new();
        let parsed = parse(source, &interner, SourceType::Module);
        assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);
        parsed.module.with_ast(|program| {
            let semantic = analyze(program);
            let bindings = collect_export_symbol_bindings(program, &interner, &semantic);
            let mut input = OptimizeInput::new(source);
            input.set_bundled_commonjs(true);
            input.linker_liveness = Some(LinkerExportLiveness::new(9, Vec::<String>::new()));
            let Statement::ExportNamed(export) = &program.body[0] else {
                panic!("expected named export");
            };
            let Some(Statement::FunctionDeclaration(function)) = export.declaration else {
                panic!("expected export function");
            };
            input.add_binding_removal(function.id.expect("function name").span);

            let plan =
                pre_lower_export_function_elision(program, &interner, &semantic, &bindings, &input);
            assert!(plan.elided_top_level_export_functions().is_empty());
        });
    }

    #[test]
    fn dead_named_default_function_is_replaced_by_esm_marker() {
        let source = "export default function dead(){return dead()}";
        let (plan, typed) = lowering_plan(source, |input| {
            input.set_bundled_commonjs(true);
            input.linker_liveness = Some(LinkerExportLiveness::new(9, Vec::<String>::new()));
        });

        assert_eq!(plan.elided_top_level_export_functions(), &[0]);
        assert_first_statement_is_empty_export(&typed);
    }

    #[test]
    fn typed_module_options_preserve_none_vs_some_empty_liveness() {
        let export_bindings = [ExportSymbolBinding {
            export_name: "kept".into(),
            local_name: "local".into(),
            symbol_id: 3,
        }];
        let mut input = OptimizeInput::new("");
        let absent = typed_module_options(&input, &export_bindings);
        assert!(absent.preserve_all_exports);
        assert!(absent.preserve_export_star);
        assert!(absent.observed_export_names.is_empty());
        assert!(absent.linker_liveness.roots.is_empty());

        input.linker_liveness = Some(LinkerExportLiveness::new(7, Vec::<String>::new()));
        let empty = typed_module_options(&input, &export_bindings);
        assert!(!empty.preserve_all_exports);
        assert!(!empty.preserve_export_star);
        assert!(empty.observed_export_names.is_empty());
        assert_eq!(empty.module_id, TypedModuleId(7));
        assert!(empty.linker_liveness.roots.is_empty());

        input.linker_liveness = Some(LinkerExportLiveness::new(7, ["kept"]));
        let exact = typed_module_options(&input, &export_bindings);
        assert!(!exact.preserve_all_exports);
        assert!(exact.preserve_export_star);
        assert_eq!(
            exact.observed_export_names,
            BTreeSet::from(["kept".to_owned()])
        );
        assert!(exact.linker_liveness.contains(TypedModuleId(7), 3));
    }

    #[test]
    fn public_aliases_resolve_to_module_symbols_not_shadowed_names() {
        let source =
            "const value=1;{const value=2;void value}export {value as answer};export const dead=3;";
        let interner = Interner::new();
        let parsed = parse(source, &interner, SourceType::Module);
        assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);

        parsed.module.with_ast(|program| {
            let semantic = analyze(program);
            let bindings = collect_export_symbol_bindings(program, &interner, &semantic);
            let answer = bindings
                .iter()
                .find(|binding| binding.export_name == "answer")
                .expect("aliased export binding");
            let module_value = semantic.scopes[0].bindings[&interner.intern("value")];
            assert_eq!(answer.symbol_id, module_value);
            assert_eq!(semantic.symbols[answer.symbol_id as usize].scope, 0);
            assert_eq!(answer.local_name, "value");
            let shadow = semantic
                .symbols
                .iter()
                .enumerate()
                .find(|(_, symbol)| symbol.name == interner.intern("value") && symbol.scope != 0)
                .map(|(symbol, _)| symbol as SymbolId)
                .expect("shadow binding");
            assert!(bindings.iter().all(|binding| binding.symbol_id != shadow));
        });
    }

    #[test]
    fn retained_module_dependencies_use_sealed_request_origin_not_only_specifier() {
        let source = "FLAG?import('same'):import('same');";
        let interner = Interner::new();
        let parsed = parse(source, &interner, SourceType::Module);
        assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);
        let dependencies = parsed
            .dependencies
            .iter()
            .map(|dependency| OptimizeDependency {
                specifier: interner.resolve(dependency.specifier),
                origin: NodeOrigin::Source(dependency.span),
            })
            .collect::<Vec<_>>();
        assert_eq!(dependencies.len(), 2);
        let expected = dependencies[0].clone();

        parsed.module.with_ast(|program| {
            let mut input = OptimizeInput::new(source);
            input.minify = true;
            input.defines = vec![ValidatedDefine::primitive("FLAG", ConstVal::Bool(true))];
            input.dependencies = dependencies;
            let result = optimize_owned_program(program, &interner, &input).unwrap();
            assert_eq!(
                result.retained_dependencies,
                vec![expected],
                "sealed requests: {:?}; report: {:?}",
                result.module_plan.requests(),
                result.report.retained_dependencies,
            );
        });
    }
}
