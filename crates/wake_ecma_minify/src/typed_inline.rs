//! Structural closed-function, single-use local, and analysis-driven DCE passes.
//!
//! Every public stage requires a same-revision [`TypedAnalysis`]. Rewrites clone or move owned IR
//! subtrees and edit the real parent lists; parser ASTs, spans, and emitter side tables are
//! intentionally outside this module's dependency boundary.

use std::collections::BTreeSet;
use std::fmt;

use wake_ecma_ast::{UnaryOperator, VarKind};
use wake_ecma_semantic::{DeclKind, SymbolId};

use crate::typed_analysis::{NameAccess, TypedAnalysis, TypedEffectSummary};
use crate::typed_ir::{
    ChildRole, ClassContext, FunctionContext, IrNodeData, ListId, NameRole, NodeId, TypedIrError,
    TypedProgram,
};

#[cfg(test)]
pub const MAX_TYPED_INLINE_ROUNDS: usize = 100;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TypedInlineStats {
    changes: usize,
    function_changes: usize,
    inline_changes: usize,
    dce_changes: usize,
    #[cfg(test)]
    rounds: usize,
    #[cfg(test)]
    functions_inlined: usize,
    #[cfg(test)]
    calls_inlined: usize,
    #[cfg(test)]
    variables_inlined: usize,
    #[cfg(test)]
    declarations_removed: usize,
    #[cfg(test)]
    pure_statements_removed: usize,
    #[cfg(test)]
    initializers_preserved: usize,
}

impl TypedInlineStats {
    #[cfg(test)]
    pub const fn changes(&self) -> usize {
        self.changes
    }

    /// Structural edits committed by closed-function/call inlining, including definition removal.
    pub const fn function_changes(&self) -> usize {
        self.function_changes
    }

    /// Structural edits committed by single-use-variable inlining, including binding removal.
    pub const fn inline_changes(&self) -> usize {
        self.inline_changes
    }

    /// Structural edits committed only by dead-code elimination.
    pub const fn dce_changes(&self) -> usize {
        self.dce_changes
    }

    #[cfg(test)]
    pub const fn rounds(&self) -> usize {
        self.rounds
    }

    #[cfg(test)]
    pub const fn functions_inlined(&self) -> usize {
        self.functions_inlined
    }

    #[cfg(test)]
    pub const fn calls_inlined(&self) -> usize {
        self.calls_inlined
    }

    #[cfg(test)]
    pub const fn variables_inlined(&self) -> usize {
        self.variables_inlined
    }

    #[cfg(test)]
    pub const fn declarations_removed(&self) -> usize {
        self.declarations_removed
    }

    #[cfg(test)]
    pub const fn pure_statements_removed(&self) -> usize {
        self.pure_statements_removed
    }

    #[cfg(test)]
    pub const fn initializers_preserved(&self) -> usize {
        self.initializers_preserved
    }

    #[cfg(test)]
    pub const fn changed(&self) -> bool {
        self.changes != 0
    }

    #[cfg(test)]
    fn merge(&mut self, other: Self) {
        self.changes += other.changes;
        self.function_changes += other.function_changes;
        self.inline_changes += other.inline_changes;
        self.dce_changes += other.dce_changes;
        self.functions_inlined += other.functions_inlined;
        self.calls_inlined += other.calls_inlined;
        self.variables_inlined += other.variables_inlined;
        self.declarations_removed += other.declarations_removed;
        self.pure_statements_removed += other.pure_statements_removed;
        self.initializers_preserved += other.initializers_preserved;
        debug_assert_eq!(
            self.changes,
            self.function_changes + self.inline_changes + self.dce_changes
        );
    }

    fn record_function_change(&mut self) {
        self.changes += 1;
        self.function_changes += 1;
    }

    fn record_inline_change(&mut self) {
        self.changes += 1;
        self.inline_changes += 1;
    }

    fn record_dce_change(&mut self) {
        self.changes += 1;
        self.dce_changes += 1;
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TypedInlineError {
    StaleAnalysis {
        program_revision: u64,
        analysis_revision: u64,
    },
    #[cfg(test)]
    DidNotConverge {
        limit: usize,
    },
    InvalidIr(TypedIrError),
}

impl fmt::Display for TypedInlineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StaleAnalysis {
                program_revision,
                analysis_revision,
            } => write!(
                formatter,
                "typed inline requires a same-revision analysis (program {program_revision}, analysis {analysis_revision})"
            ),
            #[cfg(test)]
            Self::DidNotConverge { limit } => {
                write!(
                    formatter,
                    "typed inline did not converge after {limit} rounds"
                )
            }
            Self::InvalidIr(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for TypedInlineError {}

impl From<TypedIrError> for TypedInlineError {
    fn from(error: TypedIrError) -> Self {
        Self::InvalidIr(error)
    }
}

/// Run both structural stages until neither commits a rewrite.
#[cfg(test)]
pub fn run_typed_inline_fixed_point(
    program: &mut TypedProgram,
    analysis: &TypedAnalysis,
) -> Result<TypedInlineStats, TypedInlineError> {
    ensure_current(program, analysis)?;
    let mut current = analysis.clone();
    let mut total = TypedInlineStats::default();
    for round in 1..=MAX_TYPED_INLINE_ROUNDS {
        let functions = inline_closed_functions(program, &current)?;
        let function_changed = functions.changed();
        total.merge(functions);
        if function_changed {
            current = TypedAnalysis::rebuild(program)?;
        }

        let locals = inline_single_use_and_dce(program, &current)?;
        let local_changed = locals.changed();
        total.merge(locals);
        total.rounds = round;
        if !function_changed && !local_changed {
            return Ok(total);
        }
        current = TypedAnalysis::rebuild(program)?;
    }
    Err(TypedInlineError::DidNotConverge {
        limit: MAX_TYPED_INLINE_ROUNDS,
    })
}

/// Inline the currently provable closed function declarations and remove their declarations.
pub fn inline_closed_functions(
    program: &mut TypedProgram,
    analysis: &TypedAnalysis,
) -> Result<TypedInlineStats, TypedInlineError> {
    ensure_current(program, analysis)?;
    let plans = plan_closed_functions(program, analysis)?;
    let already_inlined = plans
        .iter()
        .map(|plan| plan.declaration)
        .collect::<BTreeSet<_>>();
    let specializations = plan_primitive_call_specializations(program, analysis, &already_inlined)?;
    let mut stats = TypedInlineStats::default();
    for plan in plans {
        for call in &plan.calls {
            let source = match plan.result {
                ClosedResult::Definition(node) => node,
                ClosedResult::Argument(_) => call.argument.expect("identity call argument"),
            };
            let replacement = program.clone_detached_subtree(source)?;
            program.replace_node(call.call, replacement)?;
            stats.record_function_change();
            #[cfg(test)]
            {
                stats.calls_inlined += 1;
            }
        }
        if remove_attached_list_node(program, plan.declaration)? {
            stats.record_function_change();
            #[cfg(test)]
            {
                stats.declarations_removed += 1;
            }
        }
        #[cfg(test)]
        {
            stats.functions_inlined += 1;
        }
    }
    for plan in specializations {
        let replacement = program.clone_detached_subtree(plan.argument)?;
        program.replace_node(plan.parameter_read, replacement)?;
        stats.record_function_change();
        if !remove_attached_list_node(program, plan.parameter)? {
            return Err(TypedIrError {
                node: Some(plan.parameter),
                message: "planned primitive specialization parameter is no longer attached".into(),
            }
            .into());
        }
        stats.record_function_change();
        if !remove_attached_list_node(program, plan.argument)? {
            return Err(TypedIrError {
                node: Some(plan.argument),
                message: "planned primitive specialization argument is no longer attached".into(),
            }
            .into());
        }
        stats.record_function_change();
    }
    debug_assert_eq!(stats.changes, stats.function_changes);
    Ok(stats)
}

/// Inline current single-use primitive locals and commit conservative DCE.
pub fn inline_single_use_and_dce(
    program: &mut TypedProgram,
    analysis: &TypedAnalysis,
) -> Result<TypedInlineStats, TypedInlineError> {
    ensure_current(program, analysis)?;
    let plan = plan_locals_and_dce(program, analysis, true)?;
    commit_local_plan(program, plan)
}

/// Remove analysis-proven dead declarations and expressions without performing value inlining.
///
/// This is used by readable tree-shaken builds after the module planner removes dead public
/// bindings. It preserves readable source structure while still collecting newly orphaned local
/// declarations and helper chains.
pub fn eliminate_dead_code(
    program: &mut TypedProgram,
    analysis: &TypedAnalysis,
) -> Result<TypedInlineStats, TypedInlineError> {
    ensure_current(program, analysis)?;
    let plan = plan_locals_and_dce(program, analysis, false)?;
    commit_local_plan(program, plan)
}

fn ensure_current(
    program: &TypedProgram,
    analysis: &TypedAnalysis,
) -> Result<(), TypedInlineError> {
    if program.revision() != analysis.revision() {
        return Err(TypedInlineError::StaleAnalysis {
            program_revision: program.revision(),
            analysis_revision: analysis.revision(),
        });
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum ClosedResult {
    Definition(NodeId),
    Argument(usize),
}

#[derive(Clone, Copy)]
struct ClosedCall {
    call: NodeId,
    argument: Option<NodeId>,
}

struct ClosedFunctionPlan {
    declaration: NodeId,
    result: ClosedResult,
    calls: Vec<ClosedCall>,
}

#[derive(Clone, Copy)]
struct PrimitiveCallSpecialization {
    parameter: NodeId,
    parameter_read: NodeId,
    argument: NodeId,
}

fn plan_closed_functions(
    program: &TypedProgram,
    analysis: &TypedAnalysis,
) -> Result<Vec<ClosedFunctionPlan>, TypedIrError> {
    let mut declarations = Vec::new();
    for node in program.preorder_validated()? {
        match program.node(node).expect("validated typed node").data() {
            IrNodeData::Function {
                context: FunctionContext::Declaration,
                ..
            } => declarations.push(node),
            IrNodeData::Program { .. }
            | IrNodeData::VariableDeclaration { .. }
            | IrNodeData::VariableDeclarator { .. }
            | IrNodeData::Function { .. }
            | IrNodeData::FunctionBody { .. }
            | IrNodeData::Class { .. }
            | IrNodeData::Block { .. }
            | IrNodeData::EmptyStatement
            | IrNodeData::DebuggerStatement
            | IrNodeData::ExpressionStatement { .. }
            | IrNodeData::IfStatement { .. }
            | IrNodeData::ForStatement { .. }
            | IrNodeData::ForInStatement { .. }
            | IrNodeData::ForOfStatement { .. }
            | IrNodeData::WhileStatement { .. }
            | IrNodeData::DoWhileStatement { .. }
            | IrNodeData::SwitchStatement { .. }
            | IrNodeData::SwitchCase { .. }
            | IrNodeData::ReturnStatement { .. }
            | IrNodeData::BreakStatement { .. }
            | IrNodeData::ContinueStatement { .. }
            | IrNodeData::ThrowStatement { .. }
            | IrNodeData::TryStatement { .. }
            | IrNodeData::CatchClause { .. }
            | IrNodeData::LabeledStatement { .. }
            | IrNodeData::WithStatement { .. }
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
            | IrNodeData::ThisExpression
            | IrNodeData::SuperExpression
            | IrNodeData::MetaProperty { .. }
            | IrNodeData::ArrayExpression { .. }
            | IrNodeData::Elision
            | IrNodeData::ObjectExpression { .. }
            | IrNodeData::ObjectProperty { .. }
            | IrNodeData::UnaryExpression { .. }
            | IrNodeData::UpdateExpression { .. }
            | IrNodeData::BinaryExpression { .. }
            | IrNodeData::LogicalExpression { .. }
            | IrNodeData::AssignmentExpression { .. }
            | IrNodeData::ConditionalExpression { .. }
            | IrNodeData::CallExpression { .. }
            | IrNodeData::NewExpression { .. }
            | IrNodeData::MemberExpression { .. }
            | IrNodeData::SequenceExpression { .. }
            | IrNodeData::TaggedTemplateExpression { .. }
            | IrNodeData::SpreadElement { .. }
            | IrNodeData::AwaitExpression { .. }
            | IrNodeData::YieldExpression { .. }
            | IrNodeData::ImportExpression { .. }
            | IrNodeData::ArrowFunction { .. }
            | IrNodeData::MethodDefinition { .. }
            | IrNodeData::PropertyDefinition { .. }
            | IrNodeData::StaticBlock { .. }
            | IrNodeData::ArrayPattern { .. }
            | IrNodeData::ObjectPattern { .. }
            | IrNodeData::ObjectPatternProperty { .. }
            | IrNodeData::AssignmentPattern { .. }
            | IrNodeData::RestPattern { .. }
            | IrNodeData::ImportDeclaration { .. }
            | IrNodeData::ImportSpecifier { .. }
            | IrNodeData::ImportAttributes { .. }
            | IrNodeData::ImportAttribute { .. }
            | IrNodeData::ExportNamedDeclaration { .. }
            | IrNodeData::ExportSpecifier { .. }
            | IrNodeData::ExportDefaultDeclaration { .. }
            | IrNodeData::ExportAllDeclaration { .. } => {}
        }
    }

    let mut plans = Vec::new();
    for declaration in declarations {
        let IrNodeData::Function {
            name: Some(name),
            parameters,
            body: Some(body),
            is_async: false,
            is_generator: false,
            ..
        } = program
            .node(declaration)
            .expect("function declaration")
            .data()
        else {
            continue;
        };
        if is_direct_export(program, declaration) {
            continue;
        }
        let Some(symbol) = symbol_of_name_node(program, *name) else {
            continue;
        };
        let Some(facts) = analysis.symbol(symbol) else {
            continue;
        };
        if facts.is_frozen()
            || facts.escape().captured()
            || !facts.writes().is_empty()
            || facts.declarations().len() != 1
            || facts.reads().is_empty()
            || !is_removable_statement(program, declaration)
        {
            continue;
        }
        let IrNodeData::FunctionBody { statements, .. } =
            program.node(*body).expect("function body").data()
        else {
            continue;
        };
        let statements = list_items(program, *statements);
        let [return_statement] = statements.as_slice() else {
            continue;
        };
        let IrNodeData::ReturnStatement {
            argument: Some(result),
        } = program
            .node(*return_statement)
            .expect("function return")
            .data()
        else {
            continue;
        };
        let parameters = list_items(program, *parameters);
        let result_kind = match parameters.as_slice() {
            [] if is_closed_primitive(program, analysis, *result) => {
                ClosedResult::Definition(*result)
            }
            parameters if !parameters.is_empty() => {
                let Some(result_symbol) = symbol_of_identifier(program, *result) else {
                    continue;
                };
                let mut selected = None;
                let mut simple = true;
                for (index, parameter) in parameters.iter().copied().enumerate() {
                    let Some(parameter_symbol) = symbol_of_identifier(program, parameter) else {
                        simple = false;
                        break;
                    };
                    let Some(parameter_facts) = analysis.symbol(parameter_symbol) else {
                        simple = false;
                        break;
                    };
                    if program
                        .symbol(parameter_symbol)
                        .is_none_or(|symbol| symbol.decl_kind() != DeclKind::Param)
                        || parameter_facts.is_frozen()
                        || !parameter_facts.writes().is_empty()
                    {
                        simple = false;
                        break;
                    }
                    if parameter_symbol == result_symbol {
                        selected = Some(index);
                    }
                }
                let Some(selected) = simple.then_some(selected).flatten() else {
                    continue;
                };
                ClosedResult::Argument(selected)
            }
            _ => continue,
        };

        let mut calls = Vec::new();
        let mut valid = true;
        for &read in facts.reads() {
            let Some(name_use) = analysis.name_use(read) else {
                valid = false;
                break;
            };
            let Some(identifier) = parent_node(program, name_use.node()) else {
                valid = false;
                break;
            };
            let Some(call) = parent_node(program, identifier) else {
                valid = false;
                break;
            };
            let Some(IrNodeData::CallExpression {
                callee,
                arguments,
                optional: false,
            }) = program.node(call).map(|node| node.data())
            else {
                valid = false;
                break;
            };
            if *callee != identifier {
                valid = false;
                break;
            }
            let arguments = list_items(program, *arguments);
            let argument = match result_kind {
                ClosedResult::Definition(_) if arguments.is_empty() => None,
                ClosedResult::Argument(selected) if arguments.len() == parameters.len() => {
                    if !arguments
                        .iter()
                        .copied()
                        .all(|argument| is_closed_primitive(program, analysis, argument))
                    {
                        valid = false;
                        break;
                    }
                    Some(arguments[selected])
                }
                ClosedResult::Definition(_) | ClosedResult::Argument(_) => {
                    valid = false;
                    break;
                }
            };
            calls.push(ClosedCall { call, argument });
        }
        if !valid
            || calls.is_empty()
            || !closed_function_cost_does_not_grow(program, symbol, result_kind, &calls)
        {
            continue;
        }
        calls.sort_unstable_by_key(|call| call.call);
        plans.push(ClosedFunctionPlan {
            declaration,
            result: result_kind,
            calls,
        });
    }
    Ok(plans)
}

fn plan_primitive_call_specializations(
    program: &TypedProgram,
    analysis: &TypedAnalysis,
    excluded: &BTreeSet<NodeId>,
) -> Result<Vec<PrimitiveCallSpecialization>, TypedIrError> {
    let mut plans = Vec::new();
    for declaration in program.preorder_validated()? {
        if excluded.contains(&declaration) || is_direct_export(program, declaration) {
            continue;
        }
        let IrNodeData::Function {
            context: FunctionContext::Declaration,
            name: Some(name),
            parameters,
            body: Some(body),
            is_async: false,
            is_generator: false,
        } = program
            .node(declaration)
            .expect("validated typed function")
            .data()
        else {
            continue;
        };
        let Some(function_symbol) = symbol_of_name_node(program, *name) else {
            continue;
        };
        let Some(function_facts) = analysis.symbol(function_symbol) else {
            continue;
        };
        if function_facts.is_frozen()
            || function_facts.escape().captured()
            || !function_facts.writes().is_empty()
            || function_facts.declarations().len() != 1
            || function_facts.reads().len() != 1
            || !is_removable_statement(program, declaration)
            || specialization_body_is_observable(program, *body)?
        {
            continue;
        }

        let parameters = list_items(program, *parameters);
        let [parameter] = parameters.as_slice() else {
            continue;
        };
        let Some(parameter_symbol) = symbol_of_identifier(program, *parameter) else {
            continue;
        };
        let Some(parameter_facts) = analysis.symbol(parameter_symbol) else {
            continue;
        };
        if program
            .symbol(parameter_symbol)
            .is_none_or(|symbol| symbol.decl_kind() != DeclKind::Param)
            || parameter_facts.is_frozen()
            || parameter_facts.escape().captured()
            || parameter_facts.escape().escaped()
            || parameter_facts.escape().aliased()
            || !parameter_facts.writes().is_empty()
            || parameter_facts.declarations().len() != 1
            || parameter_facts.reads().len() != 1
        {
            continue;
        }
        let parameter_read_name = parameter_facts.reads()[0];
        let Some(parameter_read_use) = analysis.name_use(parameter_read_name) else {
            continue;
        };
        let Some(parameter_read) = parent_node(program, parameter_read_use.node()) else {
            continue;
        };
        if parameter_read_use.access() != NameAccess::Read
            || symbol_of_identifier(program, parameter_read) != Some(parameter_symbol)
            || !is_descendant(program, parameter_read, *body)
            || analysis.read_is_definitely_initialized(parameter_read_name) != Some(true)
        {
            continue;
        }

        let Some(function_read_use) = analysis.name_use(function_facts.reads()[0]) else {
            continue;
        };
        let Some(callee) = parent_node(program, function_read_use.node()) else {
            continue;
        };
        let Some(call) = parent_node(program, callee) else {
            continue;
        };
        let Some(IrNodeData::CallExpression {
            callee: direct_callee,
            arguments,
            optional: false,
        }) = program.node(call).map(|node| node.data())
        else {
            continue;
        };
        if *direct_callee != callee || is_descendant(program, call, declaration) {
            continue;
        }
        let arguments = list_items(program, *arguments);
        let [argument] = arguments.as_slice() else {
            continue;
        };
        if !is_closed_primitive(program, analysis, *argument)
            || !primitive_specialization_cost_does_not_grow(program, parameter_symbol, *argument)
        {
            continue;
        }
        plans.push(PrimitiveCallSpecialization {
            parameter: *parameter,
            parameter_read,
            argument: *argument,
        });
    }
    Ok(plans)
}

fn primitive_specialization_cost_does_not_grow(
    program: &TypedProgram,
    parameter: SymbolId,
    argument: NodeId,
) -> bool {
    let Some(argument_cost) = primitive_cost(program, argument) else {
        return false;
    };
    let parameter_cost = program
        .symbol(parameter)
        .map_or(usize::MAX, |symbol| symbol.original_name().len());
    argument_cost <= parameter_cost.saturating_add(argument_cost)
}

fn specialization_body_is_observable(
    program: &TypedProgram,
    body: NodeId,
) -> Result<bool, TypedIrError> {
    for node in program.subtree_preorder(body)? {
        let observable = match program.node(node).expect("validated typed node").data() {
            IrNodeData::Function { .. }
            | IrNodeData::Class { .. }
            | IrNodeData::WithStatement { .. }
            | IrNodeData::ThisExpression
            | IrNodeData::SuperExpression
            | IrNodeData::MetaProperty { .. }
            | IrNodeData::AwaitExpression { .. }
            | IrNodeData::YieldExpression { .. }
            | IrNodeData::ArrowFunction { .. } => true,
            IrNodeData::Name { name } => program.name(*name).is_some_and(|name| {
                name.original() == "arguments"
                    && name.symbol().is_none()
                    && matches!(
                        name.role(),
                        NameRole::Reference | NameRole::AssignmentTarget
                    )
            }),
            IrNodeData::Program { .. }
            | IrNodeData::VariableDeclaration { .. }
            | IrNodeData::VariableDeclarator { .. }
            | IrNodeData::FunctionBody { .. }
            | IrNodeData::Block { .. }
            | IrNodeData::EmptyStatement
            | IrNodeData::DebuggerStatement
            | IrNodeData::ExpressionStatement { .. }
            | IrNodeData::IfStatement { .. }
            | IrNodeData::ForStatement { .. }
            | IrNodeData::ForInStatement { .. }
            | IrNodeData::ForOfStatement { .. }
            | IrNodeData::WhileStatement { .. }
            | IrNodeData::DoWhileStatement { .. }
            | IrNodeData::SwitchStatement { .. }
            | IrNodeData::SwitchCase { .. }
            | IrNodeData::ReturnStatement { .. }
            | IrNodeData::BreakStatement { .. }
            | IrNodeData::ContinueStatement { .. }
            | IrNodeData::ThrowStatement { .. }
            | IrNodeData::TryStatement { .. }
            | IrNodeData::CatchClause { .. }
            | IrNodeData::LabeledStatement { .. }
            | IrNodeData::NumberLiteral { .. }
            | IrNodeData::StringLiteral { .. }
            | IrNodeData::BooleanLiteral { .. }
            | IrNodeData::NullLiteral
            | IrNodeData::BigIntLiteral { .. }
            | IrNodeData::RegExpLiteral { .. }
            | IrNodeData::TemplateLiteral { .. }
            | IrNodeData::TemplateElement { .. }
            | IrNodeData::Identifier { .. }
            | IrNodeData::ArrayExpression { .. }
            | IrNodeData::Elision
            | IrNodeData::ObjectExpression { .. }
            | IrNodeData::ObjectProperty { .. }
            | IrNodeData::UnaryExpression { .. }
            | IrNodeData::UpdateExpression { .. }
            | IrNodeData::BinaryExpression { .. }
            | IrNodeData::LogicalExpression { .. }
            | IrNodeData::AssignmentExpression { .. }
            | IrNodeData::ConditionalExpression { .. }
            | IrNodeData::CallExpression { .. }
            | IrNodeData::NewExpression { .. }
            | IrNodeData::MemberExpression { .. }
            | IrNodeData::SequenceExpression { .. }
            | IrNodeData::TaggedTemplateExpression { .. }
            | IrNodeData::SpreadElement { .. }
            | IrNodeData::ImportExpression { .. }
            | IrNodeData::MethodDefinition { .. }
            | IrNodeData::PropertyDefinition { .. }
            | IrNodeData::StaticBlock { .. }
            | IrNodeData::ArrayPattern { .. }
            | IrNodeData::ObjectPattern { .. }
            | IrNodeData::ObjectPatternProperty { .. }
            | IrNodeData::AssignmentPattern { .. }
            | IrNodeData::RestPattern { .. }
            | IrNodeData::ImportDeclaration { .. }
            | IrNodeData::ImportSpecifier { .. }
            | IrNodeData::ImportAttributes { .. }
            | IrNodeData::ImportAttribute { .. }
            | IrNodeData::ExportNamedDeclaration { .. }
            | IrNodeData::ExportSpecifier { .. }
            | IrNodeData::ExportDefaultDeclaration { .. }
            | IrNodeData::ExportAllDeclaration { .. } => false,
        };
        if observable {
            return Ok(true);
        }
    }
    Ok(false)
}

fn closed_function_cost_does_not_grow(
    program: &TypedProgram,
    symbol: SymbolId,
    result: ClosedResult,
    calls: &[ClosedCall],
) -> bool {
    let name_len = program
        .symbol(symbol)
        .map_or(usize::MAX, |symbol| symbol.original_name().len());
    match result {
        ClosedResult::Argument(_) => true,
        ClosedResult::Definition(result) => {
            let result_cost = primitive_cost(program, result).unwrap_or(usize::MAX);
            let declaration_cost = 8usize
                .saturating_add(1)
                .saturating_add(name_len)
                .saturating_add(3)
                .saturating_add(6)
                .saturating_add(1)
                .saturating_add(result_cost)
                .saturating_add(1);
            let old_calls = calls.len().saturating_mul(name_len.saturating_add(2));
            // A primitive substituted into an arbitrary expression position can require one
            // parenthesis pair (for example as a call target or exponentiation operand). Charge
            // that worst-case cost instead of depending on emitter precedence here.
            let new_calls = calls.len().saturating_mul(result_cost.saturating_add(2));
            new_calls <= declaration_cost.saturating_add(old_calls)
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PrimitiveKind {
    Number,
    String,
    Boolean,
    Null,
    BigInt,
    Undefined,
}

fn primitive_cost(program: &TypedProgram, node: NodeId) -> Option<usize> {
    primitive_profile(program, node).map(|(_, cost)| cost)
}

fn primitive_profile(program: &TypedProgram, node: NodeId) -> Option<(PrimitiveKind, usize)> {
    match program.node(node)?.data() {
        IrNodeData::NumberLiteral { value } => Some((
            PrimitiveKind::Number,
            crate::write_number_minified(*value).len(),
        )),
        IrNodeData::StringLiteral { value } => Some((
            PrimitiveKind::String,
            crate::ConstVal::Str(value.clone()).to_source().len(),
        )),
        IrNodeData::BooleanLiteral { value: _ } => Some((PrimitiveKind::Boolean, 2)),
        IrNodeData::NullLiteral => Some((PrimitiveKind::Null, 4)),
        IrNodeData::BigIntLiteral { raw } => Some((PrimitiveKind::BigInt, raw.len())),
        IrNodeData::UnaryExpression { operator, argument } => {
            let (argument_kind, mut argument_cost) = primitive_profile(program, *argument)?;
            if matches!(
                program.node(*argument).map(|node| node.data()),
                Some(IrNodeData::UnaryExpression { .. })
            ) {
                argument_cost = argument_cost.saturating_add(2);
            }
            let (result_kind, separator) = match operator {
                UnaryOperator::LogicalNot => (PrimitiveKind::Boolean, 0),
                UnaryOperator::Void => (PrimitiveKind::Undefined, 1),
                UnaryOperator::Plus if argument_kind != PrimitiveKind::BigInt => {
                    (PrimitiveKind::Number, 0)
                }
                UnaryOperator::Minus | UnaryOperator::BitwiseNot
                    if argument_kind == PrimitiveKind::BigInt =>
                {
                    (PrimitiveKind::BigInt, 0)
                }
                UnaryOperator::Minus | UnaryOperator::BitwiseNot => (PrimitiveKind::Number, 0),
                UnaryOperator::Delete | UnaryOperator::Typeof | UnaryOperator::Plus => return None,
            };
            Some((
                result_kind,
                operator
                    .as_str()
                    .len()
                    .saturating_add(separator)
                    .saturating_add(argument_cost),
            ))
        }
        IrNodeData::Program { .. }
        | IrNodeData::VariableDeclaration { .. }
        | IrNodeData::VariableDeclarator { .. }
        | IrNodeData::Function { .. }
        | IrNodeData::FunctionBody { .. }
        | IrNodeData::Class { .. }
        | IrNodeData::Block { .. }
        | IrNodeData::EmptyStatement
        | IrNodeData::DebuggerStatement
        | IrNodeData::ExpressionStatement { .. }
        | IrNodeData::IfStatement { .. }
        | IrNodeData::ForStatement { .. }
        | IrNodeData::ForInStatement { .. }
        | IrNodeData::ForOfStatement { .. }
        | IrNodeData::WhileStatement { .. }
        | IrNodeData::DoWhileStatement { .. }
        | IrNodeData::SwitchStatement { .. }
        | IrNodeData::SwitchCase { .. }
        | IrNodeData::ReturnStatement { .. }
        | IrNodeData::BreakStatement { .. }
        | IrNodeData::ContinueStatement { .. }
        | IrNodeData::ThrowStatement { .. }
        | IrNodeData::TryStatement { .. }
        | IrNodeData::CatchClause { .. }
        | IrNodeData::LabeledStatement { .. }
        | IrNodeData::WithStatement { .. }
        | IrNodeData::RegExpLiteral { .. }
        | IrNodeData::TemplateLiteral { .. }
        | IrNodeData::TemplateElement { .. }
        | IrNodeData::Name { .. }
        | IrNodeData::Identifier { .. }
        | IrNodeData::ThisExpression
        | IrNodeData::SuperExpression
        | IrNodeData::MetaProperty { .. }
        | IrNodeData::ArrayExpression { .. }
        | IrNodeData::Elision
        | IrNodeData::ObjectExpression { .. }
        | IrNodeData::ObjectProperty { .. }
        | IrNodeData::UpdateExpression { .. }
        | IrNodeData::BinaryExpression { .. }
        | IrNodeData::LogicalExpression { .. }
        | IrNodeData::AssignmentExpression { .. }
        | IrNodeData::ConditionalExpression { .. }
        | IrNodeData::CallExpression { .. }
        | IrNodeData::NewExpression { .. }
        | IrNodeData::MemberExpression { .. }
        | IrNodeData::SequenceExpression { .. }
        | IrNodeData::TaggedTemplateExpression { .. }
        | IrNodeData::SpreadElement { .. }
        | IrNodeData::AwaitExpression { .. }
        | IrNodeData::YieldExpression { .. }
        | IrNodeData::ImportExpression { .. }
        | IrNodeData::ArrowFunction { .. }
        | IrNodeData::MethodDefinition { .. }
        | IrNodeData::PropertyDefinition { .. }
        | IrNodeData::StaticBlock { .. }
        | IrNodeData::ArrayPattern { .. }
        | IrNodeData::ObjectPattern { .. }
        | IrNodeData::ObjectPatternProperty { .. }
        | IrNodeData::AssignmentPattern { .. }
        | IrNodeData::RestPattern { .. }
        | IrNodeData::ImportDeclaration { .. }
        | IrNodeData::ImportSpecifier { .. }
        | IrNodeData::ImportAttributes { .. }
        | IrNodeData::ImportAttribute { .. }
        | IrNodeData::ExportNamedDeclaration { .. }
        | IrNodeData::ExportSpecifier { .. }
        | IrNodeData::ExportDefaultDeclaration { .. }
        | IrNodeData::ExportAllDeclaration { .. } => None,
    }
}

fn is_closed_primitive(program: &TypedProgram, analysis: &TypedAnalysis, node: NodeId) -> bool {
    let Some(effect) = analysis.effect(node) else {
        return false;
    };
    !effect.may_have_side_effects()
        && !effect.may_throw()
        && matches!(
            program.node(node).map(|node| node.data()),
            Some(
                IrNodeData::NumberLiteral { .. }
                    | IrNodeData::StringLiteral { .. }
                    | IrNodeData::BooleanLiteral { .. }
                    | IrNodeData::NullLiteral
                    | IrNodeData::BigIntLiteral { .. }
                    | IrNodeData::UnaryExpression { .. }
            )
        )
        && primitive_cost(program, node).is_some()
}

#[derive(Clone, Copy)]
struct LocalInline {
    read_identifier: NodeId,
    initializer: NodeId,
}

#[derive(Clone, Copy)]
struct PreservedInitializer {
    declaration: NodeId,
    initializer: NodeId,
}

#[derive(Default)]
struct LocalPlan {
    inlines: Vec<LocalInline>,
    inline_declarators: BTreeSet<NodeId>,
    dce_declarators: BTreeSet<NodeId>,
    preserve_initializers: Vec<PreservedInitializer>,
    remove_declarations: BTreeSet<NodeId>,
    preserve_class_evaluations: Vec<NodeId>,
    remove_expressions: BTreeSet<NodeId>,
}

fn plan_locals_and_dce(
    program: &TypedProgram,
    analysis: &TypedAnalysis,
    allow_inline: bool,
) -> Result<LocalPlan, TypedIrError> {
    let mut variable_declarations = Vec::new();
    let mut function_declarations = Vec::new();
    let mut class_declarations = Vec::new();
    let mut expression_statements = Vec::new();
    for node in program.preorder_validated()? {
        match program.node(node).expect("validated typed node").data() {
            IrNodeData::VariableDeclaration { .. } => variable_declarations.push(node),
            IrNodeData::Function {
                context: FunctionContext::Declaration,
                name: Some(_),
                ..
            } => function_declarations.push(node),
            IrNodeData::Class {
                context: ClassContext::Declaration,
                name: Some(_),
                ..
            } => class_declarations.push(node),
            IrNodeData::ExpressionStatement { .. } => expression_statements.push(node),
            IrNodeData::Program { .. }
            | IrNodeData::VariableDeclarator { .. }
            | IrNodeData::Function { .. }
            | IrNodeData::FunctionBody { .. }
            | IrNodeData::Class { .. }
            | IrNodeData::Block { .. }
            | IrNodeData::EmptyStatement
            | IrNodeData::DebuggerStatement
            | IrNodeData::IfStatement { .. }
            | IrNodeData::ForStatement { .. }
            | IrNodeData::ForInStatement { .. }
            | IrNodeData::ForOfStatement { .. }
            | IrNodeData::WhileStatement { .. }
            | IrNodeData::DoWhileStatement { .. }
            | IrNodeData::SwitchStatement { .. }
            | IrNodeData::SwitchCase { .. }
            | IrNodeData::ReturnStatement { .. }
            | IrNodeData::BreakStatement { .. }
            | IrNodeData::ContinueStatement { .. }
            | IrNodeData::ThrowStatement { .. }
            | IrNodeData::TryStatement { .. }
            | IrNodeData::CatchClause { .. }
            | IrNodeData::LabeledStatement { .. }
            | IrNodeData::WithStatement { .. }
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
            | IrNodeData::ThisExpression
            | IrNodeData::SuperExpression
            | IrNodeData::MetaProperty { .. }
            | IrNodeData::ArrayExpression { .. }
            | IrNodeData::Elision
            | IrNodeData::ObjectExpression { .. }
            | IrNodeData::ObjectProperty { .. }
            | IrNodeData::UnaryExpression { .. }
            | IrNodeData::UpdateExpression { .. }
            | IrNodeData::BinaryExpression { .. }
            | IrNodeData::LogicalExpression { .. }
            | IrNodeData::AssignmentExpression { .. }
            | IrNodeData::ConditionalExpression { .. }
            | IrNodeData::CallExpression { .. }
            | IrNodeData::NewExpression { .. }
            | IrNodeData::MemberExpression { .. }
            | IrNodeData::SequenceExpression { .. }
            | IrNodeData::TaggedTemplateExpression { .. }
            | IrNodeData::SpreadElement { .. }
            | IrNodeData::AwaitExpression { .. }
            | IrNodeData::YieldExpression { .. }
            | IrNodeData::ImportExpression { .. }
            | IrNodeData::ArrowFunction { .. }
            | IrNodeData::MethodDefinition { .. }
            | IrNodeData::PropertyDefinition { .. }
            | IrNodeData::StaticBlock { .. }
            | IrNodeData::ArrayPattern { .. }
            | IrNodeData::ObjectPattern { .. }
            | IrNodeData::ObjectPatternProperty { .. }
            | IrNodeData::AssignmentPattern { .. }
            | IrNodeData::RestPattern { .. }
            | IrNodeData::ImportDeclaration { .. }
            | IrNodeData::ImportSpecifier { .. }
            | IrNodeData::ImportAttributes { .. }
            | IrNodeData::ImportAttribute { .. }
            | IrNodeData::ExportNamedDeclaration { .. }
            | IrNodeData::ExportSpecifier { .. }
            | IrNodeData::ExportDefaultDeclaration { .. }
            | IrNodeData::ExportAllDeclaration { .. } => {}
        }
    }

    let mut plan = LocalPlan::default();
    let symbol_count = program.symbols().len();
    for index in 0..symbol_count {
        if !allow_inline {
            break;
        }
        let symbol = u32::try_from(index).expect("typed symbol arena is u32 indexed");
        let Some(symbol_record) = program.symbol(symbol) else {
            continue;
        };
        if symbol_record.decl_kind() != DeclKind::Const {
            continue;
        }
        let Some(facts) = analysis.symbol(symbol) else {
            continue;
        };
        if facts.declarations().len() != 1
            || facts.reads().len() != 1
            || !facts.writes().is_empty()
            || facts.is_frozen()
            || facts.escape().captured()
        {
            continue;
        }
        let Some(declaration_name) = analysis
            .name_use(facts.declarations()[0])
            .map(|name_use| name_use.node())
        else {
            continue;
        };
        let Some(binding_identifier) = parent_node(program, declaration_name) else {
            continue;
        };
        let Some(declarator) = parent_node(program, binding_identifier) else {
            continue;
        };
        let Some(IrNodeData::VariableDeclarator {
            binding,
            initializer: Some(initializer),
        }) = program.node(declarator).map(|node| node.data())
        else {
            continue;
        };
        if *binding != binding_identifier || symbol_of_identifier(program, *binding) != Some(symbol)
        {
            continue;
        }
        let Some(declaration) = parent_node(program, declarator) else {
            continue;
        };
        let Some(IrNodeData::VariableDeclaration {
            kind: VarKind::Const,
            ..
        }) = program.node(declaration).map(|node| node.data())
        else {
            continue;
        };
        if is_direct_export(program, declaration)
            || !is_removable_statement(program, declaration)
            || !is_closed_primitive(program, analysis, *initializer)
        {
            continue;
        }
        let read = facts.reads()[0];
        if analysis.read_is_definitely_initialized(read) != Some(true) {
            continue;
        }
        let Some(read_use) = analysis.name_use(read) else {
            continue;
        };
        if read_use.access() != NameAccess::Read || read_use.symbol() != Some(symbol) {
            continue;
        }
        let Some(read_identifier) = parent_node(program, read_use.node()) else {
            continue;
        };
        if symbol_of_identifier(program, read_identifier) != Some(symbol)
            || !identifier_is_replaceable_expression(program, read_identifier)
        {
            continue;
        }
        plan.inlines.push(LocalInline {
            read_identifier,
            initializer: *initializer,
        });
        plan.inline_declarators.insert(declarator);
    }

    plan.inlines
        .sort_unstable_by_key(|inline| inline.read_identifier);

    for declaration in variable_declarations {
        if !is_removable_statement(program, declaration) || is_direct_export(program, declaration) {
            continue;
        }
        let Some(IrNodeData::VariableDeclaration { kind, declarations }) =
            program.node(declaration).map(|node| node.data())
        else {
            unreachable!("collected variable declaration changed during planning")
        };
        if kind.is_using() {
            continue;
        }
        let declarators = list_items(program, *declarations);
        let mut removable = Vec::new();
        let mut effectful = Vec::new();
        for declarator in &declarators {
            if plan.inline_declarators.contains(declarator) {
                continue;
            }
            let Some((symbol, initializer)) =
                simple_unused_declarator(program, analysis, *declarator)
            else {
                continue;
            };
            let Some(facts) = analysis.symbol(symbol) else {
                continue;
            };
            if facts.is_frozen()
                || facts.escape().captured()
                || facts.escape().escaped()
                || !facts.writes().is_empty()
            {
                continue;
            }
            match initializer {
                None => removable.push(*declarator),
                Some(initializer)
                    if effect_is_removable(
                        analysis.effect(initializer),
                        program,
                        analysis,
                        initializer,
                    ) =>
                {
                    removable.push(*declarator);
                }
                Some(initializer) => effectful.push((*declarator, initializer)),
            }
        }

        if declarators.len() == 1 {
            let only = declarators[0];
            if removable.contains(&only) {
                plan.dce_declarators.insert(only);
            } else if let Some((_, initializer)) = effectful
                .iter()
                .copied()
                .find(|(declarator, _)| *declarator == only)
            {
                plan.preserve_initializers.push(PreservedInitializer {
                    declaration,
                    initializer,
                });
            }
            continue;
        }

        plan.dce_declarators.extend(removable);
    }

    plan.remove_declarations
        .extend(plan_dead_function_declarations(
            program,
            analysis,
            &function_declarations,
        ));
    for declaration in class_declarations {
        if !declaration_is_unread_and_removable(program, analysis, declaration) {
            continue;
        }
        if class_evaluation_is_removable(program, analysis, declaration) {
            plan.remove_declarations.insert(declaration);
        } else {
            plan.preserve_class_evaluations.push(declaration);
        }
    }

    let inline_reads = plan
        .inlines
        .iter()
        .map(|inline| inline.read_identifier)
        .collect::<BTreeSet<_>>();
    for statement in expression_statements {
        let Some(IrNodeData::ExpressionStatement {
            expression,
            directive: false,
        }) = program.node(statement).map(|node| node.data())
        else {
            continue;
        };
        if !is_removable_statement(program, statement)
            || plan
                .remove_declarations
                .iter()
                .chain(&plan.preserve_class_evaluations)
                .any(|declaration| is_descendant(program, statement, *declaration))
            || inline_reads
                .iter()
                .any(|read| is_descendant(program, *read, statement))
            || !effect_is_removable(analysis.effect(*expression), program, analysis, *expression)
        {
            continue;
        }
        plan.remove_expressions.insert(statement);
    }

    plan.preserve_initializers
        .sort_unstable_by_key(|item| item.declaration);
    plan.preserve_class_evaluations.sort_unstable();
    Ok(plan)
}

fn commit_local_plan(
    program: &mut TypedProgram,
    plan: LocalPlan,
) -> Result<TypedInlineStats, TypedInlineError> {
    let mut stats = TypedInlineStats::default();
    for inline in plan.inlines {
        if !is_live(program, inline.read_identifier) || !is_live(program, inline.initializer) {
            continue;
        }
        let replacement = program.clone_detached_subtree(inline.initializer)?;
        program.replace_node(inline.read_identifier, replacement)?;
        stats.record_inline_change();
        #[cfg(test)]
        {
            stats.variables_inlined += 1;
        }
    }

    for declarator in plan.inline_declarators {
        if remove_variable_declarator(program, declarator)? {
            stats.record_inline_change();
            #[cfg(test)]
            {
                stats.declarations_removed += 1;
            }
        }
    }

    for preserved in plan.preserve_initializers {
        if !is_live(program, preserved.declaration) || !is_live(program, preserved.initializer) {
            continue;
        }
        let origin = program
            .node(preserved.declaration)
            .expect("live declaration")
            .origin();
        let expression = program.clone_detached_subtree(preserved.initializer)?;
        let replacement = program.append_detached_node_with(origin, |_| {
            Ok(IrNodeData::ExpressionStatement {
                expression,
                directive: false,
            })
        })?;
        program.replace_node(preserved.declaration, replacement)?;
        stats.record_dce_change();
        #[cfg(test)]
        {
            stats.declarations_removed += 1;
            stats.initializers_preserved += 1;
        }
    }

    for declarator in plan.dce_declarators {
        if remove_variable_declarator(program, declarator)? {
            stats.record_dce_change();
            #[cfg(test)]
            {
                stats.declarations_removed += 1;
            }
        }
    }
    for declaration in plan.preserve_class_evaluations {
        if preserve_class_evaluation(program, declaration)? {
            stats.record_dce_change();
            #[cfg(test)]
            {
                stats.declarations_removed += 1;
            }
        }
    }
    for declaration in plan.remove_declarations {
        if remove_attached_list_node(program, declaration)? {
            stats.record_dce_change();
            #[cfg(test)]
            {
                stats.declarations_removed += 1;
            }
        }
    }
    for statement in plan.remove_expressions {
        if remove_attached_list_node(program, statement)? {
            stats.record_dce_change();
            #[cfg(test)]
            {
                stats.pure_statements_removed += 1;
            }
        }
    }
    debug_assert_eq!(
        stats.changes,
        stats.function_changes + stats.inline_changes + stats.dce_changes
    );
    Ok(stats)
}

fn declaration_is_unread_and_removable(
    program: &TypedProgram,
    analysis: &TypedAnalysis,
    declaration: NodeId,
) -> bool {
    if !is_program_level_statement(program, declaration) || is_direct_export(program, declaration) {
        return false;
    }
    let name = match program.node(declaration).map(|node| node.data()) {
        Some(
            IrNodeData::Function {
                context: FunctionContext::Declaration,
                name: Some(name),
                ..
            }
            | IrNodeData::Class {
                context: ClassContext::Declaration,
                name: Some(name),
                ..
            },
        ) => *name,
        Some(
            IrNodeData::Program { .. }
            | IrNodeData::VariableDeclaration { .. }
            | IrNodeData::VariableDeclarator { .. }
            | IrNodeData::Function { .. }
            | IrNodeData::FunctionBody { .. }
            | IrNodeData::Class { .. }
            | IrNodeData::Block { .. }
            | IrNodeData::EmptyStatement
            | IrNodeData::DebuggerStatement
            | IrNodeData::ExpressionStatement { .. }
            | IrNodeData::IfStatement { .. }
            | IrNodeData::ForStatement { .. }
            | IrNodeData::ForInStatement { .. }
            | IrNodeData::ForOfStatement { .. }
            | IrNodeData::WhileStatement { .. }
            | IrNodeData::DoWhileStatement { .. }
            | IrNodeData::SwitchStatement { .. }
            | IrNodeData::SwitchCase { .. }
            | IrNodeData::ReturnStatement { .. }
            | IrNodeData::BreakStatement { .. }
            | IrNodeData::ContinueStatement { .. }
            | IrNodeData::ThrowStatement { .. }
            | IrNodeData::TryStatement { .. }
            | IrNodeData::CatchClause { .. }
            | IrNodeData::LabeledStatement { .. }
            | IrNodeData::WithStatement { .. }
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
            | IrNodeData::ThisExpression
            | IrNodeData::SuperExpression
            | IrNodeData::MetaProperty { .. }
            | IrNodeData::ArrayExpression { .. }
            | IrNodeData::Elision
            | IrNodeData::ObjectExpression { .. }
            | IrNodeData::ObjectProperty { .. }
            | IrNodeData::UnaryExpression { .. }
            | IrNodeData::UpdateExpression { .. }
            | IrNodeData::BinaryExpression { .. }
            | IrNodeData::LogicalExpression { .. }
            | IrNodeData::AssignmentExpression { .. }
            | IrNodeData::ConditionalExpression { .. }
            | IrNodeData::CallExpression { .. }
            | IrNodeData::NewExpression { .. }
            | IrNodeData::MemberExpression { .. }
            | IrNodeData::SequenceExpression { .. }
            | IrNodeData::TaggedTemplateExpression { .. }
            | IrNodeData::SpreadElement { .. }
            | IrNodeData::AwaitExpression { .. }
            | IrNodeData::YieldExpression { .. }
            | IrNodeData::ImportExpression { .. }
            | IrNodeData::ArrowFunction { .. }
            | IrNodeData::MethodDefinition { .. }
            | IrNodeData::PropertyDefinition { .. }
            | IrNodeData::StaticBlock { .. }
            | IrNodeData::ArrayPattern { .. }
            | IrNodeData::ObjectPattern { .. }
            | IrNodeData::ObjectPatternProperty { .. }
            | IrNodeData::AssignmentPattern { .. }
            | IrNodeData::RestPattern { .. }
            | IrNodeData::ImportDeclaration { .. }
            | IrNodeData::ImportSpecifier { .. }
            | IrNodeData::ImportAttributes { .. }
            | IrNodeData::ImportAttribute { .. }
            | IrNodeData::ExportNamedDeclaration { .. }
            | IrNodeData::ExportSpecifier { .. }
            | IrNodeData::ExportDefaultDeclaration { .. }
            | IrNodeData::ExportAllDeclaration { .. },
        )
        | None => return false,
    };
    let Some(symbol) = symbol_of_name_node(program, name) else {
        return false;
    };
    let Some(facts) = analysis.symbol(symbol) else {
        return false;
    };
    facts.declarations().len() == 1
        && facts.reads().is_empty()
        && facts.writes().is_empty()
        && !facts.is_frozen()
        && !facts.escape().captured()
        && !facts.escape().escaped()
        && !facts.escape().aliased()
}

fn plan_dead_function_declarations(
    program: &TypedProgram,
    analysis: &TypedAnalysis,
    declarations: &[NodeId],
) -> BTreeSet<NodeId> {
    let candidates = declarations
        .iter()
        .filter_map(|declaration| {
            let IrNodeData::Function {
                context: FunctionContext::Declaration,
                name: Some(name),
                ..
            } = program.node(*declaration)?.data()
            else {
                return None;
            };
            let symbol = symbol_of_name_node(program, *name)?;
            let facts = analysis.symbol(symbol)?;
            (is_program_level_statement(program, *declaration)
                && !is_direct_export(program, *declaration)
                && facts.declarations().len() == 1
                && facts.writes().is_empty()
                && !facts.is_frozen())
            .then_some((symbol, *declaration))
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    let candidate_owners = candidates
        .iter()
        .map(|(&symbol, &declaration)| (declaration, symbol))
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut edges = std::collections::BTreeMap::<SymbolId, BTreeSet<SymbolId>>::new();
    let mut live = BTreeSet::<SymbolId>::new();
    for (&target, &target_declaration) in &candidates {
        let Some(facts) = analysis.symbol(target) else {
            live.insert(target);
            continue;
        };
        for read in facts.reads() {
            let Some(read_use) = analysis.name_use(*read) else {
                live.insert(target);
                continue;
            };
            let owner = containing_candidate(program, read_use.node(), &candidate_owners);
            if let Some(owner) = owner {
                edges.entry(owner).or_default().insert(target);
            } else {
                live.insert(target);
            }
        }
        if facts.declarations().is_empty()
            || program
                .node(target_declaration)
                .is_none_or(|node| node.is_tombstone())
        {
            live.insert(target);
        }
    }
    let mut queue = live.iter().copied().collect::<Vec<_>>();
    while let Some(owner) = queue.pop() {
        for dependency in edges.get(&owner).into_iter().flatten() {
            if live.insert(*dependency) {
                queue.push(*dependency);
            }
        }
    }
    candidates
        .into_iter()
        .filter_map(|(symbol, declaration)| (!live.contains(&symbol)).then_some(declaration))
        .collect()
}

fn containing_candidate(
    program: &TypedProgram,
    node: NodeId,
    candidates: &std::collections::BTreeMap<NodeId, SymbolId>,
) -> Option<SymbolId> {
    let mut current = Some(node);
    while let Some(node) = current {
        if let Some(&symbol) = candidates.get(&node) {
            return Some(symbol);
        }
        current = parent_node(program, node);
    }
    None
}

fn class_evaluation_is_removable(
    program: &TypedProgram,
    analysis: &TypedAnalysis,
    declaration: NodeId,
) -> bool {
    let Some(IrNodeData::Class {
        super_class,
        context: ClassContext::Declaration,
        ..
    }) = program.node(declaration).map(|node| node.data())
    else {
        return false;
    };
    super_class.is_none()
        && effect_is_removable(analysis.effect(declaration), program, analysis, declaration)
}

fn preserve_class_evaluation(
    program: &mut TypedProgram,
    declaration: NodeId,
) -> Result<bool, TypedIrError> {
    if !is_live(program, declaration) {
        return Ok(false);
    }
    let origin = program
        .node(declaration)
        .ok_or_else(|| TypedIrError {
            node: Some(declaration),
            message: "planned class declaration disappeared".into(),
        })?
        .origin();
    let Some(IrNodeData::Class {
        name,
        super_class,
        members,
        decorators,
        ..
    }) = program.node(declaration).map(|node| node.data().clone())
    else {
        return Ok(false);
    };
    let name = name
        .map(|name| program.clone_detached_subtree(name))
        .transpose()?;
    let super_class = super_class
        .map(|super_class| program.clone_detached_subtree(super_class))
        .transpose()?;
    let members = list_items(program, members)
        .into_iter()
        .map(|member| program.clone_detached_subtree(member))
        .collect::<Result<Vec<_>, _>>()?;
    let decorators = list_items(program, decorators)
        .into_iter()
        .map(|decorator| program.clone_detached_subtree(decorator))
        .collect::<Result<Vec<_>, _>>()?;
    let expression = program.append_detached_node_with(origin, |builder| {
        let members = builder.list(ChildRole::ClassMembers, members)?;
        let decorators = builder.list(ChildRole::Decorators, decorators)?;
        Ok(IrNodeData::Class {
            context: ClassContext::Expression,
            name,
            super_class,
            members,
            decorators,
        })
    })?;
    let replacement = program.append_detached_node_with(origin, |_| {
        Ok(IrNodeData::ExpressionStatement {
            expression,
            directive: false,
        })
    })?;
    program.replace_node(declaration, replacement)?;
    Ok(true)
}

fn simple_unused_declarator(
    program: &TypedProgram,
    analysis: &TypedAnalysis,
    declarator: NodeId,
) -> Option<(SymbolId, Option<NodeId>)> {
    let IrNodeData::VariableDeclarator {
        binding,
        initializer,
    } = program.node(declarator)?.data()
    else {
        return None;
    };
    let symbol = symbol_of_identifier(program, *binding)?;
    let facts = analysis.symbol(symbol)?;
    (facts.declarations().len() == 1 && facts.reads().is_empty()).then_some((symbol, *initializer))
}

fn effect_is_removable(
    effect: Option<TypedEffectSummary>,
    program: &TypedProgram,
    analysis: &TypedAnalysis,
    root: NodeId,
) -> bool {
    if matches!(
        program.node(root).map(|node| node.data()),
        Some(IrNodeData::Class {
            super_class: Some(_),
            ..
        })
    ) {
        // Evaluating even a primitive-looking heritage expression performs the class-constructor
        // check (`class extends 1 {}` throws), which is not represented by the child expression's
        // own effect summary.
        return false;
    }
    effect.is_some_and(|effect| !effect.may_have_side_effects() && !effect.may_throw())
        && reads_are_definitely_initialized(program, analysis, root)
}

fn reads_are_definitely_initialized(
    program: &TypedProgram,
    analysis: &TypedAnalysis,
    root: NodeId,
) -> bool {
    let Ok(nodes) = program.subtree_preorder(root) else {
        return false;
    };
    nodes.into_iter().all(|node| {
        let Some(node) = program.node(node) else {
            return true;
        };
        let IrNodeData::Name { name } = node.data() else {
            return true;
        };
        let Some(name_use) = analysis.name_use(*name) else {
            return true;
        };
        !matches!(name_use.access(), NameAccess::Read | NameAccess::ReadWrite)
            || name_use.symbol().is_none()
            || analysis.read_is_definitely_initialized(name_use.name()) == Some(true)
    })
}

fn identifier_is_replaceable_expression(program: &TypedProgram, identifier: NodeId) -> bool {
    let Some(link) = program.node(identifier).and_then(|node| node.parent()) else {
        return false;
    };
    if matches!(
        program.node(link.parent()).map(|node| node.data()),
        Some(IrNodeData::ObjectProperty {
            value,
            shorthand: true,
            ..
        }) if *value == identifier
    ) {
        return false;
    }
    matches!(
        link.role(),
        ChildRole::Expression
            | ChildRole::Test
            | ChildRole::ForInitializer
            | ChildRole::ForTest
            | ChildRole::ForUpdate
            | ChildRole::ForRight
            | ChildRole::SwitchDiscriminant
            | ChildRole::SwitchCaseTest
            | ChildRole::ReturnArgument
            | ChildRole::ThrowArgument
            | ChildRole::WithObject
            | ChildRole::ClassSuper
            | ChildRole::PropertyValue
            | ChildRole::UnaryArgument
            | ChildRole::Left
            | ChildRole::Right
            | ChildRole::Callee
            | ChildRole::Arguments
            | ChildRole::Object
            | ChildRole::MemberProperty
            | ChildRole::SequenceItems
            | ChildRole::Tag
            | ChildRole::TemplateExpressions
            | ChildRole::SpreadArgument
            | ChildRole::AwaitArgument
            | ChildRole::YieldArgument
            | ChildRole::ImportSource
            | ChildRole::ImportOptions
            | ChildRole::ArrayElements
            | ChildRole::ObjectMembers
            | ChildRole::PatternDefault
            | ChildRole::AttributeValue
            | ChildRole::ExportDefaultValue
    )
}

fn symbol_of_identifier(program: &TypedProgram, identifier: NodeId) -> Option<SymbolId> {
    let IrNodeData::Identifier { name } = program.node(identifier)?.data() else {
        return None;
    };
    symbol_of_name_node(program, *name)
}

fn symbol_of_name_node(program: &TypedProgram, node: NodeId) -> Option<SymbolId> {
    let IrNodeData::Name { name } = program.node(node)?.data() else {
        return None;
    };
    program.name(*name)?.symbol()
}

fn parent_node(program: &TypedProgram, node: NodeId) -> Option<NodeId> {
    program.node(node)?.parent().map(|parent| parent.parent())
}

fn list_items(program: &TypedProgram, list: ListId) -> Vec<NodeId> {
    program
        .list(list)
        .expect("validated typed list")
        .items()
        .to_vec()
}

fn is_direct_export(program: &TypedProgram, declaration: NodeId) -> bool {
    let Some(parent) = parent_node(program, declaration) else {
        return false;
    };
    matches!(
        program.node(parent).map(|node| node.data()),
        Some(IrNodeData::ExportNamedDeclaration {
            declaration: Some(exported),
            ..
        }) if *exported == declaration
    ) || matches!(
        program.node(parent).map(|node| node.data()),
        Some(IrNodeData::ExportDefaultDeclaration { value, .. }) if *value == declaration
    )
}

fn is_removable_statement(program: &TypedProgram, node: NodeId) -> bool {
    let Some(link) = program.node(node).and_then(|node| node.parent()) else {
        return false;
    };
    link.list().is_some()
        && matches!(
            link.role(),
            ChildRole::ProgramBody
                | ChildRole::BlockBody
                | ChildRole::FunctionStatements
                | ChildRole::SwitchCaseBody
                | ChildRole::StaticBlockBody
        )
}

fn is_program_level_statement(program: &TypedProgram, node: NodeId) -> bool {
    program
        .node(node)
        .and_then(|node| node.parent())
        .is_some_and(|parent| parent.list().is_some() && parent.role() == ChildRole::ProgramBody)
}

fn remove_attached_list_node(
    program: &mut TypedProgram,
    node: NodeId,
) -> Result<bool, TypedIrError> {
    let Some(record) = program.node(node) else {
        return Ok(false);
    };
    if record.is_tombstone() {
        return Ok(false);
    }
    let Some(link) = record.parent() else {
        return Ok(false);
    };
    let Some(list) = link.list() else {
        return Ok(false);
    };
    let Some(index) = program
        .list(list)
        .and_then(|list| list.items().iter().position(|item| *item == node))
    else {
        return Ok(false);
    };
    program.splice_list(list, index..index + 1, &[])?;
    Ok(true)
}

fn remove_variable_declarator(
    program: &mut TypedProgram,
    declarator: NodeId,
) -> Result<bool, TypedIrError> {
    let Some(record) = program.node(declarator) else {
        return Ok(false);
    };
    if record.is_tombstone() {
        return Ok(false);
    }
    let Some(link) = record.parent() else {
        return Ok(false);
    };
    let Some(list) = link.list() else {
        return Ok(false);
    };
    let declaration = link.parent();
    if !matches!(
        program.node(declaration).map(|node| node.data()),
        Some(IrNodeData::VariableDeclaration { declarations, .. }) if *declarations == list
    ) {
        return Ok(false);
    }
    let item_count = program.list(list).map_or(0, |list| list.items().len());
    if item_count == 1 {
        if !is_removable_statement(program, declaration) {
            return Ok(false);
        }
        remove_attached_list_node(program, declaration)
    } else {
        remove_attached_list_node(program, declarator)
    }
}

fn is_descendant(program: &TypedProgram, node: NodeId, root: NodeId) -> bool {
    let mut current = Some(node);
    while let Some(node) = current {
        if node == root {
            return true;
        }
        current = parent_node(program, node);
    }
    false
}

fn is_live(program: &TypedProgram, node: NodeId) -> bool {
    program.node(node).is_some_and(|node| !node.is_tombstone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use wake_common::Interner;
    use wake_ecma_ast::SourceType;

    fn lower(source: &str) -> TypedProgram {
        lower_as(source, SourceType::Script)
    }

    fn lower_as(source: &str, source_type: SourceType) -> TypedProgram {
        let interner = Interner::new();
        let parsed = wake_ecma_parser::parse(source, &interner, source_type);
        assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);
        parsed.module.with_ast(|program| {
            let semantic = wake_ecma_semantic::analyze(program);
            TypedProgram::lower(program, &interner, Some(&semantic)).unwrap()
        })
    }

    fn count(program: &TypedProgram, predicate: impl Fn(&IrNodeData) -> bool) -> usize {
        program
            .preorder()
            .unwrap()
            .into_iter()
            .filter(|node| predicate(program.node(*node).unwrap().data()))
            .count()
    }

    fn original_names(program: &TypedProgram) -> Vec<&str> {
        program
            .preorder()
            .unwrap()
            .into_iter()
            .filter_map(|node| match program.node(node)?.data() {
                IrNodeData::Name { name } => Some(program.name(*name)?.original()),
                IrNodeData::Program { .. }
                | IrNodeData::VariableDeclaration { .. }
                | IrNodeData::VariableDeclarator { .. }
                | IrNodeData::Function { .. }
                | IrNodeData::FunctionBody { .. }
                | IrNodeData::Class { .. }
                | IrNodeData::Block { .. }
                | IrNodeData::EmptyStatement
                | IrNodeData::DebuggerStatement
                | IrNodeData::ExpressionStatement { .. }
                | IrNodeData::IfStatement { .. }
                | IrNodeData::ForStatement { .. }
                | IrNodeData::ForInStatement { .. }
                | IrNodeData::ForOfStatement { .. }
                | IrNodeData::WhileStatement { .. }
                | IrNodeData::DoWhileStatement { .. }
                | IrNodeData::SwitchStatement { .. }
                | IrNodeData::SwitchCase { .. }
                | IrNodeData::ReturnStatement { .. }
                | IrNodeData::BreakStatement { .. }
                | IrNodeData::ContinueStatement { .. }
                | IrNodeData::ThrowStatement { .. }
                | IrNodeData::TryStatement { .. }
                | IrNodeData::CatchClause { .. }
                | IrNodeData::LabeledStatement { .. }
                | IrNodeData::WithStatement { .. }
                | IrNodeData::NumberLiteral { .. }
                | IrNodeData::StringLiteral { .. }
                | IrNodeData::BooleanLiteral { .. }
                | IrNodeData::NullLiteral
                | IrNodeData::BigIntLiteral { .. }
                | IrNodeData::RegExpLiteral { .. }
                | IrNodeData::TemplateLiteral { .. }
                | IrNodeData::TemplateElement { .. }
                | IrNodeData::Identifier { .. }
                | IrNodeData::ThisExpression
                | IrNodeData::SuperExpression
                | IrNodeData::MetaProperty { .. }
                | IrNodeData::ArrayExpression { .. }
                | IrNodeData::Elision
                | IrNodeData::ObjectExpression { .. }
                | IrNodeData::ObjectProperty { .. }
                | IrNodeData::UnaryExpression { .. }
                | IrNodeData::UpdateExpression { .. }
                | IrNodeData::BinaryExpression { .. }
                | IrNodeData::LogicalExpression { .. }
                | IrNodeData::AssignmentExpression { .. }
                | IrNodeData::ConditionalExpression { .. }
                | IrNodeData::CallExpression { .. }
                | IrNodeData::NewExpression { .. }
                | IrNodeData::MemberExpression { .. }
                | IrNodeData::SequenceExpression { .. }
                | IrNodeData::TaggedTemplateExpression { .. }
                | IrNodeData::SpreadElement { .. }
                | IrNodeData::AwaitExpression { .. }
                | IrNodeData::YieldExpression { .. }
                | IrNodeData::ImportExpression { .. }
                | IrNodeData::ArrowFunction { .. }
                | IrNodeData::MethodDefinition { .. }
                | IrNodeData::PropertyDefinition { .. }
                | IrNodeData::StaticBlock { .. }
                | IrNodeData::ArrayPattern { .. }
                | IrNodeData::ObjectPattern { .. }
                | IrNodeData::ObjectPatternProperty { .. }
                | IrNodeData::AssignmentPattern { .. }
                | IrNodeData::RestPattern { .. }
                | IrNodeData::ImportDeclaration { .. }
                | IrNodeData::ImportSpecifier { .. }
                | IrNodeData::ImportAttributes { .. }
                | IrNodeData::ImportAttribute { .. }
                | IrNodeData::ExportNamedDeclaration { .. }
                | IrNodeData::ExportSpecifier { .. }
                | IrNodeData::ExportDefaultDeclaration { .. }
                | IrNodeData::ExportAllDeclaration { .. } => None,
            })
            .collect()
    }

    fn analyze(source: &str) -> (TypedProgram, TypedAnalysis) {
        let program = lower(source);
        let analysis = TypedAnalysis::rebuild(&program).unwrap();
        (program, analysis)
    }

    fn analyze_as(source: &str, source_type: SourceType) -> (TypedProgram, TypedAnalysis) {
        let program = lower_as(source, source_type);
        let analysis = TypedAnalysis::rebuild(&program).unwrap();
        (program, analysis)
    }

    fn assert_change_partition(stats: &TypedInlineStats) {
        assert_eq!(
            stats.changes(),
            stats.function_changes() + stats.inline_changes() + stats.dce_changes()
        );
    }

    #[test]
    fn inlines_zero_argument_primitive_function_and_its_definition_origin() {
        let (mut program, analysis) = analyze("function one(){return 1}one();");
        let source_origin = program
            .preorder()
            .unwrap()
            .into_iter()
            .find_map(|node| match program.node(node).unwrap().data() {
                IrNodeData::NumberLiteral { value } if *value == 1.0 => {
                    Some(program.node(node).unwrap().origin())
                }
                IrNodeData::Program { .. }
                | IrNodeData::VariableDeclaration { .. }
                | IrNodeData::VariableDeclarator { .. }
                | IrNodeData::Function { .. }
                | IrNodeData::FunctionBody { .. }
                | IrNodeData::Class { .. }
                | IrNodeData::Block { .. }
                | IrNodeData::EmptyStatement
                | IrNodeData::DebuggerStatement
                | IrNodeData::ExpressionStatement { .. }
                | IrNodeData::IfStatement { .. }
                | IrNodeData::ForStatement { .. }
                | IrNodeData::ForInStatement { .. }
                | IrNodeData::ForOfStatement { .. }
                | IrNodeData::WhileStatement { .. }
                | IrNodeData::DoWhileStatement { .. }
                | IrNodeData::SwitchStatement { .. }
                | IrNodeData::SwitchCase { .. }
                | IrNodeData::ReturnStatement { .. }
                | IrNodeData::BreakStatement { .. }
                | IrNodeData::ContinueStatement { .. }
                | IrNodeData::ThrowStatement { .. }
                | IrNodeData::TryStatement { .. }
                | IrNodeData::CatchClause { .. }
                | IrNodeData::LabeledStatement { .. }
                | IrNodeData::WithStatement { .. }
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
                | IrNodeData::ThisExpression
                | IrNodeData::SuperExpression
                | IrNodeData::MetaProperty { .. }
                | IrNodeData::ArrayExpression { .. }
                | IrNodeData::Elision
                | IrNodeData::ObjectExpression { .. }
                | IrNodeData::ObjectProperty { .. }
                | IrNodeData::UnaryExpression { .. }
                | IrNodeData::UpdateExpression { .. }
                | IrNodeData::BinaryExpression { .. }
                | IrNodeData::LogicalExpression { .. }
                | IrNodeData::AssignmentExpression { .. }
                | IrNodeData::ConditionalExpression { .. }
                | IrNodeData::CallExpression { .. }
                | IrNodeData::NewExpression { .. }
                | IrNodeData::MemberExpression { .. }
                | IrNodeData::SequenceExpression { .. }
                | IrNodeData::TaggedTemplateExpression { .. }
                | IrNodeData::SpreadElement { .. }
                | IrNodeData::AwaitExpression { .. }
                | IrNodeData::YieldExpression { .. }
                | IrNodeData::ImportExpression { .. }
                | IrNodeData::ArrowFunction { .. }
                | IrNodeData::MethodDefinition { .. }
                | IrNodeData::PropertyDefinition { .. }
                | IrNodeData::StaticBlock { .. }
                | IrNodeData::ArrayPattern { .. }
                | IrNodeData::ObjectPattern { .. }
                | IrNodeData::ObjectPatternProperty { .. }
                | IrNodeData::AssignmentPattern { .. }
                | IrNodeData::RestPattern { .. }
                | IrNodeData::ImportDeclaration { .. }
                | IrNodeData::ImportSpecifier { .. }
                | IrNodeData::ImportAttributes { .. }
                | IrNodeData::ImportAttribute { .. }
                | IrNodeData::ExportNamedDeclaration { .. }
                | IrNodeData::ExportSpecifier { .. }
                | IrNodeData::ExportDefaultDeclaration { .. }
                | IrNodeData::ExportAllDeclaration { .. } => None,
            })
            .unwrap();
        let stats = inline_closed_functions(&mut program, &analysis).unwrap();
        program.validate().unwrap();
        assert_eq!(stats.functions_inlined(), 1);
        assert_eq!(stats.calls_inlined(), 1);
        assert_eq!(stats.function_changes(), 2);
        assert_eq!(stats.inline_changes(), 0);
        assert_eq!(stats.dce_changes(), 0);
        assert_change_partition(&stats);
        assert_eq!(
            count(&program, |node| matches!(
                node,
                IrNodeData::Function { .. } | IrNodeData::CallExpression { .. }
            )),
            0
        );
        let replacement = program
            .preorder()
            .unwrap()
            .into_iter()
            .find(|node| {
                matches!(
                    program.node(*node).unwrap().data(),
                    IrNodeData::NumberLiteral { value } if *value == 1.0
                )
            })
            .unwrap();
        assert_eq!(program.node(replacement).unwrap().origin(), source_origin);
    }

    #[test]
    fn identity_inline_keeps_the_exact_argument_and_call_site_origin() {
        let (mut program, analysis) = analyze("function id(value){return value}id(7);");
        let argument_origin = program
            .preorder()
            .unwrap()
            .into_iter()
            .find_map(|node| match program.node(node).unwrap().data() {
                IrNodeData::NumberLiteral { value } if *value == 7.0 => {
                    Some(program.node(node).unwrap().origin())
                }
                _ => None,
            })
            .unwrap();
        let stats = inline_closed_functions(&mut program, &analysis).unwrap();
        assert_eq!(stats.functions_inlined(), 1);
        assert_eq!(stats.calls_inlined(), 1);
        assert_eq!(stats.function_changes(), 2);
        assert_change_partition(&stats);
        let result = program
            .preorder()
            .unwrap()
            .into_iter()
            .find(|node| {
                matches!(
                    program.node(*node).unwrap().data(),
                    IrNodeData::NumberLiteral { value } if *value == 7.0
                )
            })
            .unwrap();
        assert_eq!(program.node(result).unwrap().origin(), argument_origin);
    }

    #[test]
    fn identity_inline_selects_one_of_multiple_effect_free_arguments() {
        let (mut program, analysis) = analyze("function pick(left,right){return right}pick(1,2);");
        let argument_origin = program
            .preorder()
            .unwrap()
            .into_iter()
            .find_map(|node| match program.node(node).unwrap().data() {
                IrNodeData::NumberLiteral { value } if *value == 2.0 => {
                    Some(program.node(node).unwrap().origin())
                }
                _ => None,
            })
            .unwrap();
        let stats = inline_closed_functions(&mut program, &analysis).unwrap();
        assert_eq!(stats.functions_inlined(), 1);
        assert_eq!(stats.calls_inlined(), 1);
        assert_eq!(
            count(&program, |node| matches!(
                node,
                IrNodeData::Function { .. } | IrNodeData::CallExpression { .. }
            )),
            0
        );
        let result = program
            .preorder()
            .unwrap()
            .into_iter()
            .find(|node| {
                matches!(
                    program.node(*node).unwrap().data(),
                    IrNodeData::NumberLiteral { value } if *value == 2.0
                )
            })
            .unwrap();
        assert_eq!(program.node(result).unwrap().origin(), argument_origin);
    }

    #[test]
    fn specializes_one_primitive_argument_for_a_later_global_round() {
        let (mut program, analysis) =
            analyze("function choose(flag){if(flag)return 10;return 20}choose(true);");
        let argument_origin = program
            .preorder()
            .unwrap()
            .into_iter()
            .find_map(|node| match program.node(node).unwrap().data() {
                IrNodeData::BooleanLiteral { value: true } => {
                    Some(program.node(node).unwrap().origin())
                }
                _ => None,
            })
            .unwrap();

        let stats = inline_closed_functions(&mut program, &analysis).unwrap();

        assert_eq!(stats.function_changes(), 3);
        assert_eq!(stats.functions_inlined(), 0);
        assert_eq!(stats.calls_inlined(), 0);
        assert_change_partition(&stats);
        assert!(!original_names(&program).contains(&"flag"));
        let replacement = program
            .preorder()
            .unwrap()
            .into_iter()
            .find(|node| {
                matches!(
                    program.node(*node).unwrap().data(),
                    IrNodeData::BooleanLiteral { value: true }
                )
            })
            .unwrap();
        assert_eq!(program.node(replacement).unwrap().origin(), argument_origin);
    }

    #[test]
    fn primitive_specialization_rejects_observable_or_non_exact_calls() {
        for source in [
            "function choose(flag){if(flag)return arguments.length;return 0}choose(true);",
            "function choose(flag){if(flag)return 1;return 0}choose(side());",
            "function choose(flag){if(flag)return 1;return 0}choose(true);choose(false);",
            "function choose(flag){if(flag)return()=>flag;return 0}choose(true);",
            "function choose(flag){eval('flag');if(flag)return 1;return 0}choose(true);",
            "function choose(flag){if(flag)return 1;return 0}choose?.(true);",
        ] {
            let (mut program, analysis) = analyze(source);
            assert_eq!(
                inline_closed_functions(&mut program, &analysis)
                    .unwrap()
                    .changes(),
                0,
                "unexpected specialization for {source}"
            );
        }
    }

    #[test]
    fn rejects_unsupported_or_observable_function_calls() {
        let source = r#"
async function asyncFn(){return 1} asyncFn();
function* generator(){return 1} generator();
function optional(){return 1} optional?.();
function constructed(){return 1} new constructed();
function defaulted(value=1){return value} defaulted();
function destructured({value}){return 1} destructured({value:1});
function usesThis(){return this} usesThis();
function usesArguments(){return arguments} usesArguments();
"#;
        let (mut program, analysis) = analyze(source);
        let before = count(&program, |node| matches!(node, IrNodeData::Function { .. }));
        let stats = inline_closed_functions(&mut program, &analysis).unwrap();
        assert_eq!(stats.changes(), 0);
        assert_eq!(
            count(&program, |node| matches!(node, IrNodeData::Function { .. })),
            before
        );
    }

    #[test]
    fn rejects_escape_capture_dynamic_scope_multiwrite_and_growth() {
        for source in [
            "function f(){return 1}const alias=f;",
            "function f(){return 1}function outer(){return()=>f()}",
            "function f(){return 1}eval('');f();",
            "function f(){return 1}f=other;f();",
            "function f(){return 1}sink(f);",
        ] {
            let (mut program, analysis) = analyze(source);
            assert_eq!(
                inline_closed_functions(&mut program, &analysis)
                    .unwrap()
                    .changes(),
                0,
                "unexpected inline for {source}"
            );
        }

        let long = "x".repeat(128);
        let source = format!("function f(){{return '{long}'}}f();f();");
        let (mut program, analysis) = analyze(&source);
        assert_eq!(
            inline_closed_functions(&mut program, &analysis)
                .unwrap()
                .changes(),
            0
        );
    }

    #[test]
    fn sibling_eval_does_not_freeze_an_unrelated_function_environment() {
        let source = r#"
function dynamic(){eval("")}
function outer(){function closed(){return 1}return closed()}
outer();
"#;
        let (mut program, analysis) = analyze(source);
        let stats = inline_closed_functions(&mut program, &analysis).unwrap();
        assert_eq!(stats.functions_inlined(), 1);
        assert!(!original_names(&program).contains(&"closed"));
    }

    #[test]
    fn single_use_const_inline_is_tdz_and_dynamic_scope_safe() {
        let (mut program, analysis) =
            analyze("function outer(){const value=1;return value}outer();");
        let stats = inline_single_use_and_dce(&mut program, &analysis).unwrap();
        assert_eq!(stats.variables_inlined(), 1);
        assert_eq!(stats.inline_changes(), 2);
        assert_eq!(stats.function_changes(), 0);
        assert_change_partition(&stats);
        assert_eq!(
            count(&program, |node| matches!(
                node,
                IrNodeData::VariableDeclaration { .. }
            )),
            0
        );

        for source in [
            "function outer(){value;const value=1}outer();",
            "function outer(){const value=1;return()=>value}outer();",
            "function outer(){const value=1;eval('');return value}outer();",
            "function outer(object){const value=1;with(object){value;}}outer({});",
        ] {
            let (mut program, analysis) = analyze(source);
            let stats = inline_single_use_and_dce(&mut program, &analysis).unwrap();
            assert_eq!(
                stats.variables_inlined(),
                0,
                "unexpected inline for {source}"
            );
            assert!(original_names(&program).contains(&"value"));
        }
    }

    #[test]
    fn dce_deletes_pure_unused_bindings_and_keeps_using_and_directives() {
        let source = r#"
function outer(){"use strict";const unused=1;let empty;using resource=open();keep()}outer();
"#;
        let (mut program, analysis) = analyze(source);
        let stats = run_typed_inline_fixed_point(&mut program, &analysis).unwrap();
        assert_change_partition(&stats);
        assert_eq!(stats.initializers_preserved(), 0);
        assert!(stats.declarations_removed() >= 2);
        assert!(original_names(&program).contains(&"resource"));
        assert_eq!(
            count(&program, |node| matches!(
                node,
                IrNodeData::ExpressionStatement {
                    directive: true,
                    ..
                }
            )),
            1
        );
    }

    #[test]
    fn dce_preserves_effectful_and_may_throw_initializers_in_place() {
        let source = r#"
function calls(){const unused=unknown()}
function getter(){const unused=object.value}
function proxy(){const unused=new Proxy({}, {})}
async function suspension(){const unused=await source}
function* generator(){const unused=yield source}
unknownA?calls:getter;unknownB?proxy:suspension;unknownC&&generator;
"#;
        let (mut program, analysis) = analyze(source);
        let stats = inline_single_use_and_dce(&mut program, &analysis).unwrap();
        assert_eq!(stats.initializers_preserved(), 5);
        assert_eq!(stats.dce_changes(), 5);
        assert_change_partition(&stats);
        assert_eq!(
            count(&program, |node| matches!(
                node,
                IrNodeData::VariableDeclaration { .. }
            )),
            0
        );
        assert_eq!(
            count(&program, |node| matches!(
                node,
                IrNodeData::CallExpression { .. }
                    | IrNodeData::MemberExpression { .. }
                    | IrNodeData::NewExpression { .. }
                    | IrNodeData::AwaitExpression { .. }
                    | IrNodeData::YieldExpression { .. }
            )),
            5
        );
    }

    #[test]
    fn dce_removes_unread_pure_function_and_class_declarations() {
        let source = r#"
function helper(){sideOnlyWhenCalled()}
class Plain {}
class PrimitiveKey {[1](){}}
class InstanceInitializer {value=sideOnlyWhenConstructed()}
keep();
"#;
        let (mut program, analysis) = analyze(source);
        let stats = inline_single_use_and_dce(&mut program, &analysis).unwrap();
        assert_eq!(stats.dce_changes(), 4);
        assert_eq!(stats.declarations_removed(), 4);
        assert_eq!(
            count(&program, |node| matches!(
                node,
                IrNodeData::Function { .. } | IrNodeData::Class { .. }
            )),
            0
        );
        assert!(original_names(&program).contains(&"keep"));
    }

    #[test]
    fn dce_keeps_exported_and_dynamic_scope_visible_declarations() {
        let (mut exported, analysis) = analyze_as(
            "export function helper(){} export default class Widget{}",
            SourceType::Module,
        );
        assert_eq!(
            inline_single_use_and_dce(&mut exported, &analysis)
                .unwrap()
                .dce_changes(),
            0
        );

        for source in [
            "function helper(){}eval('helper');",
            "function helper(){}with({}){}",
        ] {
            let (mut program, analysis) = analyze(source);
            assert_eq!(
                inline_single_use_and_dce(&mut program, &analysis)
                    .unwrap()
                    .dce_changes(),
                0,
                "dynamic scope declaration was removed for {source}"
            );
            assert!(original_names(&program).contains(&"helper"));
        }
    }

    #[test]
    fn dce_preserves_unused_class_evaluation_as_an_expression() {
        let source = r#"
class Heritage extends unknownBase {}
class InvalidHeritage extends 1 {}
class Computed {[key()](){}}
class StaticField {static value=side()}
class StaticBlock {static {cleanup()}}
"#;
        let (mut program, analysis) = analyze(source);
        let stats = run_typed_inline_fixed_point(&mut program, &analysis).unwrap();
        assert_eq!(stats.dce_changes(), 5);
        assert_eq!(stats.declarations_removed(), 5);
        assert_eq!(
            count(&program, |node| matches!(
                node,
                IrNodeData::Class {
                    context: crate::typed_ir::ClassContext::Declaration,
                    ..
                }
            )),
            0
        );
        assert_eq!(
            count(&program, |node| matches!(
                node,
                IrNodeData::Class {
                    context: crate::typed_ir::ClassContext::Expression,
                    ..
                }
            )),
            5
        );
        for observable in ["unknownBase", "key", "side", "cleanup"] {
            assert!(original_names(&program).contains(&observable));
        }
    }

    #[test]
    fn fixed_point_recursively_removes_dead_binding_chains() {
        let source =
            "function helper(){}const alias=helper;const first=1;const second=first;keep();";
        let (mut program, analysis) = analyze(source);
        let stats = run_typed_inline_fixed_point(&mut program, &analysis).unwrap();
        assert!(stats.rounds() >= 2);
        for removed in ["helper", "alias", "first", "second"] {
            assert!(!original_names(&program).contains(&removed));
        }
        assert!(original_names(&program).contains(&"keep"));
    }

    #[test]
    fn dce_removes_unrooted_recursive_function_components_but_keeps_live_ones() {
        let source = r#"
function selfDead(value){return value?selfDead(value-1):0}
function firstDead(value){return value?secondDead(value-1):0}
function secondDead(value){return value?firstDead(value-1):0}
function live(value){return value?live(value-1):0}
consume(live);
"#;
        let (mut program, analysis) = analyze(source);
        let stats = inline_single_use_and_dce(&mut program, &analysis).unwrap();
        assert_eq!(stats.dce_changes(), 3);
        for removed in ["selfDead", "firstDead", "secondDead"] {
            assert!(!original_names(&program).contains(&removed));
        }
        assert!(original_names(&program).contains(&"live"));
    }

    #[test]
    fn multi_declarator_effect_order_and_try_finally_structure_are_preserved() {
        let source = r#"
function mixed(){const unused=side(),kept=1;return kept}
function guarded(){try{const unused=side()}finally{cleanup()}}
consume(mixed,guarded);
"#;
        let (mut program, analysis) = analyze(source);
        let stats = inline_single_use_and_dce(&mut program, &analysis).unwrap();
        assert_eq!(stats.initializers_preserved(), 1);
        assert!(original_names(&program).contains(&"unused"));
        let try_block = program
            .preorder()
            .unwrap()
            .into_iter()
            .find_map(|node| match program.node(node).unwrap().data() {
                IrNodeData::TryStatement { block, .. } => Some(*block),
                _ => None,
            })
            .unwrap();
        let IrNodeData::Block { body } = program.node(try_block).unwrap().data() else {
            panic!("try block must remain a block")
        };
        assert!(matches!(
            program
                .node(program.list(*body).unwrap().items()[0])
                .unwrap()
                .data(),
            IrNodeData::ExpressionStatement { .. }
        ));
    }

    #[test]
    fn fixed_point_takes_a_second_opportunity_after_local_inline() {
        let (mut program, analysis) = analyze("function outer(){const value=1;value;}outer();");
        let stats = run_typed_inline_fixed_point(&mut program, &analysis).unwrap();
        assert_change_partition(&stats);
        assert!(stats.rounds() >= 2);
        assert_eq!(stats.variables_inlined(), 1);
        assert_eq!(stats.pure_statements_removed(), 1);
        let body = program
            .preorder()
            .unwrap()
            .into_iter()
            .find_map(|node| match program.node(node).unwrap().data() {
                IrNodeData::FunctionBody { statements, .. } => Some(*statements),
                _ => None,
            })
            .unwrap();
        assert!(program.list(body).unwrap().items().is_empty());
    }

    #[test]
    fn stale_analysis_returns_a_diagnostic_before_any_rewrite() {
        let (mut program, analysis) = analyze("function outer(){const value=1;return value}");
        let name = program
            .preorder()
            .unwrap()
            .into_iter()
            .find_map(|node| match program.node(node).unwrap().data() {
                IrNodeData::Name { name } => Some(*name),
                _ => None,
            })
            .unwrap();
        let emitted = program.name(name).unwrap().emitted().to_owned();
        program.set_emitted_name(name, emitted).unwrap();
        assert!(matches!(
            inline_single_use_and_dce(&mut program, &analysis),
            Err(TypedInlineError::StaleAnalysis { .. })
        ));
    }

    #[test]
    fn output_is_deterministic_and_large_sources_do_not_disable_inline() {
        let padding = "x".repeat(5000);
        let source = format!(
            "/*{padding}*/function outer(){{function closed(){{return 1}}closed();}}outer();"
        );
        let mut left = lower(&source);
        let left_analysis = TypedAnalysis::rebuild(&left).unwrap();
        let left_stats = run_typed_inline_fixed_point(&mut left, &left_analysis).unwrap();
        let mut right = lower(&source);
        let right_analysis = TypedAnalysis::rebuild(&right).unwrap();
        run_typed_inline_fixed_point(&mut right, &right_analysis).unwrap();
        assert!(left_stats.functions_inlined() >= 1);
        assert_eq!(left.fingerprint(), right.fingerprint());
    }
}
