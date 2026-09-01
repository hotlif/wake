//! Closure-style structural optimization passes for [`TypedProgram`].
//!
//! Every accepted rewrite changes the owned node/list structure itself, reports through one
//! change-count path, and is visible to the next pass in the same fixed-point round.

#[cfg(test)]
use std::error::Error;
#[cfg(test)]
use std::fmt;

use wake_ecma_ast::{BinaryOperator, LogicalOperator, UnaryOperator, VarKind};

use crate::typed_ir::{
    ChildRole, DerivedOriginKind, IrNodeData, IrOrigin, ListId, NameRole, NameSyntax, NodeId,
    PropertyKeyKind, SyntheticOriginKind, TypedIrError, TypedProgram,
};
use crate::{ConstVal, write_number_minified};

/// Hard convergence cap shared by every typed-IR optimization run.
#[cfg(test)]
pub const MAX_TYPED_FIXED_POINT_ITERATIONS: usize = 100;

/// Stable pass order. Reordering this array changes optimizer semantics and fingerprints.
#[cfg(test)]
pub const TYPED_PASS_ORDER: [TypedPassKind; 6] = [
    TypedPassKind::PrimitiveFolding,
    TypedPassKind::BranchSimplification,
    TypedPassKind::ConfiguredDrops,
    TypedPassKind::DeadStatementCleanup,
    TypedPassKind::StatementMerging,
    TypedPassKind::LatePeephole,
];

/// One named structural phase in the fixed-point loop.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TypedPassKind {
    PrimitiveFolding,
    BranchSimplification,
    ConfiguredDrops,
    DeadStatementCleanup,
    StatementMerging,
    LatePeephole,
}

impl TypedPassKind {
    pub const fn name(self) -> &'static str {
        match self {
            Self::PrimitiveFolding => "primitive-folding",
            Self::BranchSimplification => "branch-simplification",
            Self::ConfiguredDrops => "configured-drops",
            Self::DeadStatementCleanup => "dead-statement-cleanup",
            Self::StatementMerging => "statement-merging",
            Self::LatePeephole => "late-peephole",
        }
    }
}

/// Flags whose semantics explicitly authorize removal of otherwise observable syntax.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TypedPassOptions {
    pub drop_debugger: bool,
    pub drop_console: bool,
}

/// Accumulated activity for one pass across all fixed-point rounds.
#[cfg(test)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypedPassStats {
    pub pass: TypedPassKind,
    pub runs: usize,
    pub changes: usize,
}

/// Successful fixed-point result.
#[cfg(test)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypedPassReport {
    /// Includes the final no-change round which proved convergence.
    pub iterations: usize,
    pub total_changes: usize,
    pub passes: Vec<TypedPassStats>,
}

/// A typed pass never silently falls back to unoptimized syntax.
#[cfg(test)]
#[derive(Debug)]
pub enum TypedPassError {
    InvalidInput(TypedIrError),
    PassFailed {
        pass: TypedPassKind,
        source: TypedIrError,
    },
    DidNotConverge {
        iterations: usize,
        last_round_changes: usize,
    },
}

#[cfg(test)]
impl fmt::Display for TypedPassError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput(error) => {
                write!(formatter, "typed optimizer input is invalid: {error}")
            }
            Self::PassFailed { pass, source } => {
                write!(
                    formatter,
                    "typed optimizer pass {} failed: {source}",
                    pass.name()
                )
            }
            Self::DidNotConverge {
                iterations,
                last_round_changes,
            } => write!(
                formatter,
                "typed optimizer did not converge after {iterations} rounds ({last_round_changes} changes in the final round)"
            ),
        }
    }
}

#[cfg(test)]
impl Error for TypedPassError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidInput(error) | Self::PassFailed { source: error, .. } => Some(error),
            Self::DidNotConverge { .. } => None,
        }
    }
}

#[derive(Default)]
struct ChangeTracker {
    changes: usize,
}

impl ChangeTracker {
    fn changed(&mut self) {
        self.changes += 1;
    }

    fn record(&mut self, changes: usize) {
        self.changes += changes;
    }

    const fn count(&self) -> usize {
        self.changes
    }
}

/// Run the structural pipeline to a real fixed point.
#[cfg(test)]
pub fn optimize_typed_program(
    program: &mut TypedProgram,
    options: TypedPassOptions,
) -> Result<TypedPassReport, TypedPassError> {
    program.validate().map_err(TypedPassError::InvalidInput)?;
    let mut stats = TYPED_PASS_ORDER
        .iter()
        .copied()
        .map(|pass| TypedPassStats {
            pass,
            runs: 0,
            changes: 0,
        })
        .collect::<Vec<_>>();

    let iterations = run_to_fixed_point(MAX_TYPED_FIXED_POINT_ITERATIONS, || {
        let mut round_changes = 0usize;
        for (index, pass) in TYPED_PASS_ORDER.iter().copied().enumerate() {
            let changes = run_typed_pass(program, options, pass)
                .map_err(|source| TypedPassError::PassFailed { pass, source })?;
            program
                .validate()
                .map_err(|source| TypedPassError::PassFailed { pass, source })?;
            stats[index].runs += 1;
            stats[index].changes += changes;
            round_changes += changes;
        }
        Ok(round_changes)
    })?;

    let total_changes = stats.iter().map(|stat| stat.changes).sum();
    Ok(TypedPassReport {
        iterations,
        total_changes,
        passes: stats,
    })
}

#[cfg(test)]
fn run_to_fixed_point(
    max_iterations: usize,
    mut round: impl FnMut() -> Result<usize, TypedPassError>,
) -> Result<usize, TypedPassError> {
    let mut last_round_changes = 0;
    for iteration in 1..=max_iterations {
        last_round_changes = round()?;
        if last_round_changes == 0 {
            return Ok(iteration);
        }
    }
    Err(TypedPassError::DidNotConverge {
        iterations: max_iterations,
        last_round_changes,
    })
}

/// Run exactly one named structural pass.
///
/// The production scheduler uses this entry point so binding-sensitive stages can rebuild
/// [`crate::typed_analysis::TypedAnalysis`] between mutations while still preserving this
/// module's single, explicit pass implementation and change count.
pub fn run_typed_pass(
    program: &mut TypedProgram,
    options: TypedPassOptions,
    pass: TypedPassKind,
) -> Result<usize, TypedIrError> {
    let mut changes = ChangeTracker::default();
    match pass {
        TypedPassKind::PrimitiveFolding => fold_primitive_constants(program, &mut changes)?,
        TypedPassKind::BranchSimplification => simplify_known_branches(program, &mut changes)?,
        TypedPassKind::ConfiguredDrops => configured_drops(program, options, &mut changes)?,
        TypedPassKind::DeadStatementCleanup => cleanup_dead_statements(program, &mut changes)?,
        TypedPassKind::StatementMerging => merge_statements(program, &mut changes)?,
        TypedPassKind::LatePeephole => late_peephole(program, &mut changes)?,
    }
    Ok(changes.count())
}

#[derive(Clone, Debug, PartialEq)]
enum Primitive {
    Bool(bool),
    Number(f64),
    String(String),
    Null,
    Undefined,
}

impl Primitive {
    fn truthy(&self) -> bool {
        match self {
            Self::Bool(value) => *value,
            Self::Number(value) => *value != 0.0 && !value.is_nan(),
            Self::String(value) => !value.is_empty(),
            Self::Null | Self::Undefined => false,
        }
    }

    const fn nullish(&self) -> bool {
        matches!(self, Self::Null | Self::Undefined)
    }

    fn token_cost(&self) -> usize {
        match self {
            Self::Bool(_) => 2,
            Self::Number(value) => write_number_minified(*value).len(),
            Self::String(value) => ConstVal::Str(value.clone()).to_source().len(),
            Self::Null => 4,
            Self::Undefined => 6,
        }
    }
}

fn primitive(program: &TypedProgram, node: NodeId) -> Option<Primitive> {
    match program.node(node)?.data() {
        IrNodeData::NumberLiteral { value } => Some(Primitive::Number(*value)),
        IrNodeData::StringLiteral { value } => Some(Primitive::String(value.clone())),
        IrNodeData::BooleanLiteral { value } => Some(Primitive::Bool(*value)),
        IrNodeData::NullLiteral => Some(Primitive::Null),
        IrNodeData::UnaryExpression { operator, argument } => {
            let argument = primitive(program, *argument)?;
            match (operator, argument) {
                (UnaryOperator::LogicalNot, value) => Some(Primitive::Bool(!value.truthy())),
                (UnaryOperator::Void, _) => Some(Primitive::Undefined),
                (UnaryOperator::Plus, Primitive::Number(value)) => Some(Primitive::Number(value)),
                (UnaryOperator::Minus, Primitive::Number(value)) => Some(Primitive::Number(-value)),
                (UnaryOperator::BitwiseNot, Primitive::Number(value)) => {
                    let value = exact_i32(value)?;
                    Some(Primitive::Number(f64::from(!value)))
                }
                _ => None,
            }
        }
        _ => None,
    }
}

fn fold_primitive_constants(
    program: &mut TypedProgram,
    changes: &mut ChangeTracker,
) -> Result<(), TypedIrError> {
    let nodes = program.preorder_validated()?;
    for node in nodes.into_iter().rev() {
        let Some(record) = program.node(node) else {
            continue;
        };
        if record.is_tombstone() {
            continue;
        }
        let data = record.data().clone();
        let origin = record.origin();
        let folded = match data {
            IrNodeData::UnaryExpression { operator, argument } => {
                fold_unary(program, operator, argument)
            }
            IrNodeData::BinaryExpression {
                operator,
                left,
                right,
            } => fold_binary(program, operator, left, right),
            _ => None,
        };
        let Some((value, old_cost)) = folded else {
            continue;
        };
        if value.token_cost() > old_cost {
            continue;
        }
        let replacement = append_primitive(program, value, rewrite_origin(origin))?;
        program.replace_node(node, replacement)?;
        changes.changed();
    }
    Ok(())
}

fn fold_unary(
    program: &TypedProgram,
    operator: UnaryOperator,
    argument: NodeId,
) -> Option<(Primitive, usize)> {
    // `void 0` is already the canonical undefined spelling. Rebuilding an equal-cost identical
    // tree would report a false change forever and trip the fixed-point cap.
    if operator == UnaryOperator::Void
        && matches!(
            program.node(argument).map(|node| node.data()),
            Some(IrNodeData::NumberLiteral { value }) if *value == 0.0 && !value.is_sign_negative()
        )
    {
        return None;
    }
    let value = primitive(program, argument)?;
    let argument_cost = value.token_cost();
    let word_space = usize::from(matches!(
        operator,
        UnaryOperator::Typeof | UnaryOperator::Void | UnaryOperator::Delete
    ));
    let old_cost = operator.as_str().len() + word_space + argument_cost;
    let result = match (operator, value) {
        (UnaryOperator::LogicalNot, value) => Primitive::Bool(!value.truthy()),
        (UnaryOperator::Void, _) => Primitive::Undefined,
        (UnaryOperator::Typeof, Primitive::Bool(_)) => Primitive::String("boolean".into()),
        (UnaryOperator::Typeof, Primitive::Number(_)) => Primitive::String("number".into()),
        (UnaryOperator::Typeof, Primitive::String(_)) => Primitive::String("string".into()),
        (UnaryOperator::Typeof, Primitive::Null) => Primitive::String("object".into()),
        (UnaryOperator::Typeof, Primitive::Undefined) => Primitive::String("undefined".into()),
        (UnaryOperator::Plus, Primitive::Number(value)) if safe_number(value) => {
            Primitive::Number(value)
        }
        // Keep unary minus structural. A negative NumberLiteral is not a primary-expression token
        // and would require emitter-side precedence metadata which the literal node does not own.
        (UnaryOperator::BitwiseNot, Primitive::Number(value)) => {
            Primitive::Number(f64::from(!exact_i32(value)?))
        }
        _ => return None,
    };
    Some((result, old_cost))
}

fn fold_binary(
    program: &TypedProgram,
    operator: BinaryOperator,
    left: NodeId,
    right: NodeId,
) -> Option<(Primitive, usize)> {
    let left = primitive(program, left)?;
    let right = primitive(program, right)?;
    let word_spaces = usize::from(matches!(
        operator,
        BinaryOperator::In | BinaryOperator::Instanceof
    )) * 2;
    let old_cost = left.token_cost() + operator.as_str().len() + word_spaces + right.token_cost();

    let result = match (operator, &left, &right) {
        (BinaryOperator::Add, Primitive::String(left), Primitive::String(right)) => {
            Primitive::String(format!("{left}{right}"))
        }
        (
            BinaryOperator::Add
            | BinaryOperator::Sub
            | BinaryOperator::Mul
            | BinaryOperator::Div
            | BinaryOperator::Rem,
            Primitive::Number(left),
            Primitive::Number(right),
        ) => {
            if !safe_arithmetic_operands(*left, *right)
                || matches!(operator, BinaryOperator::Div | BinaryOperator::Rem) && *right == 0.0
            {
                return None;
            }
            let value = match operator {
                BinaryOperator::Add => left + right,
                BinaryOperator::Sub => left - right,
                BinaryOperator::Mul => left * right,
                BinaryOperator::Div => left / right,
                BinaryOperator::Rem => left % right,
                _ => unreachable!(),
            };
            if !safe_number(value) || is_negative_zero(value) {
                return None;
            }
            Primitive::Number(value)
        }
        (BinaryOperator::Exp, Primitive::Number(left), Primitive::Number(right)) => {
            if !safe_arithmetic_operands(*left, *right)
                || *left < 0.0
                || right.fract() != 0.0
                || !(0.0..=32.0).contains(right)
            {
                return None;
            }
            let value = left.powf(*right);
            if !safe_number(value) || is_negative_zero(value) {
                return None;
            }
            Primitive::Number(value)
        }
        (
            BinaryOperator::Lt | BinaryOperator::Gt | BinaryOperator::LtEq | BinaryOperator::GtEq,
            Primitive::Number(left),
            Primitive::Number(right),
        ) if left.is_finite() && right.is_finite() => Primitive::Bool(match operator {
            BinaryOperator::Lt => left < right,
            BinaryOperator::Gt => left > right,
            BinaryOperator::LtEq => left <= right,
            BinaryOperator::GtEq => left >= right,
            _ => unreachable!(),
        }),
        (
            BinaryOperator::StrictEq
            | BinaryOperator::StrictNotEq
            | BinaryOperator::Eq
            | BinaryOperator::NotEq,
            _,
            _,
        ) if same_primitive_kind(&left, &right) => {
            let equal = primitive_strict_equal(&left, &right);
            Primitive::Bool(
                matches!(operator, BinaryOperator::StrictEq | BinaryOperator::Eq) == equal,
            )
        }
        (
            BinaryOperator::BitAnd | BinaryOperator::BitOr | BinaryOperator::BitXor,
            Primitive::Number(left),
            Primitive::Number(right),
        ) => {
            let (left, right) = (exact_i32(*left)?, exact_i32(*right)?);
            let value = match operator {
                BinaryOperator::BitAnd => left & right,
                BinaryOperator::BitOr => left | right,
                BinaryOperator::BitXor => left ^ right,
                _ => unreachable!(),
            };
            Primitive::Number(f64::from(value))
        }
        (
            BinaryOperator::Shl | BinaryOperator::Shr | BinaryOperator::Ushr,
            Primitive::Number(left),
            Primitive::Number(right),
        ) => {
            let left = exact_i32(*left)?;
            let right = u32::try_from(exact_i32(*right)?).ok()?;
            if right > 31 {
                return None;
            }
            let value = match operator {
                BinaryOperator::Shl => f64::from(left.wrapping_shl(right)),
                BinaryOperator::Shr => f64::from(left.wrapping_shr(right)),
                BinaryOperator::Ushr => f64::from((left as u32).wrapping_shr(right)),
                _ => unreachable!(),
            };
            Primitive::Number(value)
        }
        // BigInt and cross-kind loose equality/coercion are intentionally outside this slice.
        _ => return None,
    };
    Some((result, old_cost))
}

fn same_primitive_kind(left: &Primitive, right: &Primitive) -> bool {
    matches!(
        (left, right),
        (Primitive::Bool(_), Primitive::Bool(_))
            | (Primitive::Number(_), Primitive::Number(_))
            | (Primitive::String(_), Primitive::String(_))
            | (Primitive::Null, Primitive::Null)
            | (Primitive::Undefined, Primitive::Undefined)
    )
}

fn primitive_strict_equal(left: &Primitive, right: &Primitive) -> bool {
    match (left, right) {
        (Primitive::Bool(left), Primitive::Bool(right)) => left == right,
        (Primitive::Number(left), Primitive::Number(right)) => left == right,
        (Primitive::String(left), Primitive::String(right)) => left == right,
        (Primitive::Null, Primitive::Null) | (Primitive::Undefined, Primitive::Undefined) => true,
        _ => false,
    }
}

fn safe_arithmetic_operands(left: f64, right: f64) -> bool {
    safe_number(left) && safe_number(right) && !is_negative_zero(left) && !is_negative_zero(right)
}

fn safe_number(value: f64) -> bool {
    value.is_finite() && !value.is_nan()
}

fn is_negative_zero(value: f64) -> bool {
    value == 0.0 && value.is_sign_negative()
}

fn exact_i32(value: f64) -> Option<i32> {
    if !value.is_finite()
        || value.fract() != 0.0
        || value < f64::from(i32::MIN)
        || value > f64::from(i32::MAX)
    {
        return None;
    }
    Some(value as i32)
}

fn append_primitive(
    program: &mut TypedProgram,
    value: Primitive,
    origin: IrOrigin,
) -> Result<NodeId, TypedIrError> {
    match value {
        Primitive::Bool(value) => {
            program.append_detached_leaf(IrNodeData::BooleanLiteral { value }, origin)
        }
        Primitive::Number(value) if value.is_sign_negative() && value != 0.0 => {
            let argument = program
                .append_detached_leaf(IrNodeData::NumberLiteral { value: -value }, origin)?;
            program.append_detached_node_with(origin, |_| {
                Ok(IrNodeData::UnaryExpression {
                    operator: UnaryOperator::Minus,
                    argument,
                })
            })
        }
        Primitive::Number(value) => {
            program.append_detached_leaf(IrNodeData::NumberLiteral { value }, origin)
        }
        Primitive::String(value) => {
            program.append_detached_leaf(IrNodeData::StringLiteral { value }, origin)
        }
        Primitive::Null => program.append_detached_leaf(IrNodeData::NullLiteral, origin),
        Primitive::Undefined => {
            let argument =
                program.append_detached_leaf(IrNodeData::NumberLiteral { value: 0.0 }, origin)?;
            program.append_detached_node_with(origin, |_| {
                Ok(IrNodeData::UnaryExpression {
                    operator: UnaryOperator::Void,
                    argument,
                })
            })
        }
    }
}

fn simplify_known_branches(
    program: &mut TypedProgram,
    changes: &mut ChangeTracker,
) -> Result<(), TypedIrError> {
    let nodes = program.preorder_validated()?;
    for node in nodes.into_iter().rev() {
        let Some(record) = program.node(node) else {
            continue;
        };
        if record.is_tombstone() {
            continue;
        }
        let data = record.data().clone();
        let old_origin = record.origin();
        let selected = match data {
            IrNodeData::IfStatement {
                test,
                consequent,
                alternate,
            } => {
                let truthy = primitive(program, test).map(|value| value.truthy());
                match truthy {
                    Some(true) => Some(Some(consequent)),
                    Some(false) => Some(alternate),
                    None => None,
                }
            }
            IrNodeData::ConditionalExpression {
                test,
                consequent,
                alternate,
            } => primitive(program, test).map(|value| {
                Some(if value.truthy() {
                    consequent
                } else {
                    alternate
                })
            }),
            IrNodeData::LogicalExpression {
                operator,
                left,
                right,
            } => primitive(program, left).map(|value| {
                let use_right = match operator {
                    LogicalOperator::And => value.truthy(),
                    LogicalOperator::Or => !value.truthy(),
                    LogicalOperator::Coalesce => value.nullish(),
                };
                Some(if use_right { right } else { left })
            }),
            _ => None,
        };

        let Some(selected) = selected else {
            continue;
        };
        let replacement = match selected {
            Some(selected) => program.clone_detached_subtree(selected)?,
            None => program
                .append_detached_leaf(IrNodeData::EmptyStatement, rewrite_origin(old_origin))?,
        };
        program.replace_node(node, replacement)?;
        changes.changed();
    }
    Ok(())
}

fn configured_drops(
    program: &mut TypedProgram,
    options: TypedPassOptions,
    changes: &mut ChangeTracker,
) -> Result<(), TypedIrError> {
    if !options.drop_debugger && !options.drop_console {
        return Ok(());
    }

    let local_console_exists = program
        .symbols()
        .iter()
        .any(|symbol| symbol.original_name() == "console");
    let nodes = program.preorder_validated()?;
    for node in nodes.into_iter().rev() {
        let Some(record) = program.node(node) else {
            continue;
        };
        if record.is_tombstone() {
            continue;
        }
        match record.data() {
            IrNodeData::DebuggerStatement if options.drop_debugger => {
                remove_or_empty_statement(program, node)?;
                changes.changed();
            }
            IrNodeData::CallExpression { .. }
                if options.drop_console
                    && !local_console_exists
                    && is_direct_console_call(program, node) =>
            {
                let origin = rewrite_origin(record.origin());
                let replacement = append_primitive(program, Primitive::Undefined, origin)?;
                program.replace_node(node, replacement)?;
                changes.changed();
            }
            _ => {}
        }
    }
    Ok(())
}

fn is_direct_console_call(program: &TypedProgram, call: NodeId) -> bool {
    let Some(IrNodeData::CallExpression {
        callee,
        optional: false,
        ..
    }) = program.node(call).map(|node| node.data())
    else {
        return false;
    };
    let Some(IrNodeData::MemberExpression {
        object,
        property,
        property_kind,
        optional: false,
    }) = program.node(*callee).map(|node| node.data())
    else {
        return false;
    };
    if !identifier_is_unresolved_name(program, *object, "console") {
        return false;
    }
    match property_kind {
        PropertyKeyKind::Identifier => name_text(program, *property).is_some(),
        PropertyKeyKind::Computed => matches!(
            program.node(*property).map(|node| node.data()),
            Some(IrNodeData::StringLiteral { .. })
        ),
        _ => false,
    }
}

fn identifier_is_unresolved_name(program: &TypedProgram, node: NodeId, expected: &str) -> bool {
    let Some(IrNodeData::Identifier { name }) = program.node(node).map(|node| node.data()) else {
        return false;
    };
    let Some(IrNodeData::Name { name }) = program.node(*name).map(|node| node.data()) else {
        return false;
    };
    program
        .name(*name)
        .is_some_and(|name| name.symbol().is_none() && name.original() == expected)
}

fn name_text(program: &TypedProgram, node: NodeId) -> Option<&str> {
    let IrNodeData::Name { name } = program.node(node)?.data() else {
        return None;
    };
    Some(program.name(*name)?.emitted())
}

fn cleanup_dead_statements(
    program: &mut TypedProgram,
    changes: &mut ChangeTracker,
) -> Result<(), TypedIrError> {
    for list in statement_lists(program)? {
        let items = program
            .list(list)
            .map(|list| list.items().to_vec())
            .unwrap_or_default();
        let mut abrupt = false;
        let mut removals = Vec::new();
        for (index, statement) in items.iter().copied().enumerate() {
            let Some(node) = program.node(statement) else {
                continue;
            };
            let removable = match node.data() {
                IrNodeData::EmptyStatement => true,
                IrNodeData::ExpressionStatement {
                    directive: false, ..
                } if abrupt => true,
                IrNodeData::DebuggerStatement if abrupt => true,
                IrNodeData::ReturnStatement { .. }
                | IrNodeData::ThrowStatement { .. }
                | IrNodeData::BreakStatement { .. }
                | IrNodeData::ContinueStatement { .. }
                    if abrupt =>
                {
                    true
                }
                IrNodeData::ExpressionStatement {
                    expression,
                    directive: false,
                } if expression_is_discardable(program, *expression) => true,
                _ => false,
            };
            if removable {
                removals.push(index);
            } else if matches!(
                node.data(),
                IrNodeData::ReturnStatement { .. }
                    | IrNodeData::ThrowStatement { .. }
                    | IrNodeData::BreakStatement { .. }
                    | IrNodeData::ContinueStatement { .. }
            ) {
                abrupt = true;
            }
        }
        for index in removals.into_iter().rev() {
            program.splice_list(list, index..index + 1, &[])?;
            changes.changed();
        }
    }
    Ok(())
}

fn expression_is_discardable(program: &TypedProgram, node: NodeId) -> bool {
    match program.node(node).map(|node| node.data()) {
        Some(
            IrNodeData::NumberLiteral { .. }
            | IrNodeData::BooleanLiteral { .. }
            | IrNodeData::NullLiteral
            | IrNodeData::Function { .. }
            | IrNodeData::ArrowFunction { .. },
        ) => true,
        Some(IrNodeData::UnaryExpression { operator, argument })
            if !matches!(operator, UnaryOperator::Delete) =>
        {
            primitive(program, *argument).is_some()
        }
        Some(IrNodeData::BinaryExpression {
            operator,
            left,
            right,
        }) => {
            !matches!(operator, BinaryOperator::In | BinaryOperator::Instanceof)
                && primitive(program, *left).is_some()
                && primitive(program, *right).is_some()
        }
        Some(IrNodeData::LogicalExpression { left, right, .. }) => {
            primitive(program, *left).is_some() && primitive(program, *right).is_some()
        }
        Some(IrNodeData::ConditionalExpression {
            test,
            consequent,
            alternate,
        }) => {
            primitive(program, *test).is_some()
                && expression_is_discardable(program, *consequent)
                && expression_is_discardable(program, *alternate)
        }
        Some(IrNodeData::SequenceExpression { expressions }) => {
            program.list(*expressions).is_some_and(|list| {
                list.items()
                    .iter()
                    .all(|item| expression_is_discardable(program, *item))
            })
        }
        _ => false,
    }
}

fn remove_or_empty_statement(
    program: &mut TypedProgram,
    statement: NodeId,
) -> Result<(), TypedIrError> {
    let record = program
        .node(statement)
        .ok_or_else(|| missing_node(statement))?;
    let origin = rewrite_origin(record.origin());
    let parent = record.parent();
    if let Some(parent) = parent
        && let Some(list) = parent.list()
    {
        let index = program
            .list(list)
            .and_then(|list| list.items().iter().position(|item| *item == statement))
            .ok_or_else(|| missing_node(statement))?;
        program.splice_list(list, index..index + 1, &[])?;
    } else {
        let empty = program.append_detached_leaf(IrNodeData::EmptyStatement, origin)?;
        program.replace_node(statement, empty)?;
    }
    Ok(())
}

fn merge_statements(
    program: &mut TypedProgram,
    changes: &mut ChangeTracker,
) -> Result<(), TypedIrError> {
    for list in statement_lists(program)? {
        merge_declaration_runs(program, list, changes)?;
        merge_expression_runs(program, list, changes)?;
    }
    Ok(())
}

fn merge_declaration_runs(
    program: &mut TypedProgram,
    list: ListId,
    changes: &mut ChangeTracker,
) -> Result<(), TypedIrError> {
    let items = program
        .list(list)
        .map(|list| list.items().to_vec())
        .unwrap_or_default();
    let runs = mergeable_runs(&items, |statement| {
        variable_kind(program, statement).filter(|kind| !kind.is_using())
    });
    let mut removed_before = 0;
    for run in runs {
        let index = run.start - removed_before;
        let end = run.end - removed_before;
        let kind = variable_kind(program, items[run.start])
            .expect("run was classified as a variable declaration");

        let origin = rewrite_origin(
            program
                .node(items[run.start])
                .ok_or_else(|| missing_node(items[run.start]))?
                .origin(),
        );
        let mut declarators = Vec::new();
        for declaration in &items[run.clone()] {
            let IrNodeData::VariableDeclaration { declarations, .. } = program
                .node(*declaration)
                .ok_or_else(|| missing_node(*declaration))?
                .data()
            else {
                unreachable!("run was classified as variable declarations")
            };
            let children = program
                .list(*declarations)
                .ok_or_else(|| missing_node(*declaration))?
                .items()
                .to_vec();
            for declarator in children {
                declarators.push(program.clone_detached_subtree(declarator)?);
            }
        }
        let merged = program.append_detached_node_with(origin, |builder| {
            let declarations = builder.list(ChildRole::DeclarationItems, declarators)?;
            Ok(IrNodeData::VariableDeclaration { kind, declarations })
        })?;
        program.splice_list(list, index..end, &[merged])?;
        let removed = run.len() - 1;
        changes.record(removed);
        removed_before += removed;
    }
    Ok(())
}

fn merge_expression_runs(
    program: &mut TypedProgram,
    list: ListId,
    changes: &mut ChangeTracker,
) -> Result<(), TypedIrError> {
    let items = program
        .list(list)
        .map(|list| list.items().to_vec())
        .unwrap_or_default();
    let runs = mergeable_runs(&items, |statement| {
        (expression_statement(program, statement).is_some()
            && !statement_is_directive(program, statement))
        .then_some(())
    });
    let mut removed_before = 0;
    for run in runs {
        let index = run.start - removed_before;
        let end = run.end - removed_before;

        let origin = rewrite_origin(
            program
                .node(items[run.start])
                .ok_or_else(|| missing_node(items[run.start]))?
                .origin(),
        );
        let mut expressions = Vec::new();
        for statement in &items[run.clone()] {
            let expression = expression_statement(program, *statement)
                .expect("run was classified as expression statements");
            append_flat_sequence_clones(program, expression, &mut expressions)?;
        }
        let sequence = program.append_detached_node_with(origin, |builder| {
            let expressions = builder.list(ChildRole::SequenceItems, expressions)?;
            Ok(IrNodeData::SequenceExpression { expressions })
        })?;
        let merged = program.append_detached_node_with(origin, |_| {
            Ok(IrNodeData::ExpressionStatement {
                expression: sequence,
                directive: false,
            })
        })?;
        program.splice_list(list, index..end, &[merged])?;
        let removed = run.len() - 1;
        changes.record(removed);
        removed_before += removed;
    }
    Ok(())
}

fn mergeable_runs<T, K>(
    items: &[T],
    mut classify: impl FnMut(T) -> Option<K>,
) -> Vec<std::ops::Range<usize>>
where
    T: Copy,
    K: Copy + Eq,
{
    // Plan against one immutable snapshot. Applying the non-overlapping runs from left to right
    // preserves the old rewrite order while avoiding a full statement-list clone after every
    // successful splice.
    let mut runs = Vec::new();
    let mut index = 0;
    while index < items.len() {
        let Some(kind) = classify(items[index]) else {
            index += 1;
            continue;
        };
        let mut end = index + 1;
        while end < items.len() && classify(items[end]) == Some(kind) {
            end += 1;
        }
        if end - index >= 2 {
            runs.push(index..end);
        }
        index = end;
    }
    runs
}

fn append_flat_sequence_clones(
    program: &mut TypedProgram,
    expression: NodeId,
    output: &mut Vec<NodeId>,
) -> Result<(), TypedIrError> {
    let nested = match program.node(expression).map(|node| node.data()) {
        Some(IrNodeData::SequenceExpression { expressions }) => Some(
            program
                .list(*expressions)
                .ok_or_else(|| missing_node(expression))?
                .items()
                .to_vec(),
        ),
        _ => None,
    };
    if let Some(nested) = nested {
        for expression in nested {
            append_flat_sequence_clones(program, expression, output)?;
        }
    } else {
        output.push(program.clone_detached_subtree(expression)?);
    }
    Ok(())
}

fn variable_kind(program: &TypedProgram, statement: NodeId) -> Option<VarKind> {
    match program.node(statement)?.data() {
        IrNodeData::VariableDeclaration { kind, .. } => Some(*kind),
        _ => None,
    }
}

fn expression_statement(program: &TypedProgram, statement: NodeId) -> Option<NodeId> {
    match program.node(statement)?.data() {
        IrNodeData::ExpressionStatement { expression, .. } => Some(*expression),
        _ => None,
    }
}

fn statement_is_directive(program: &TypedProgram, statement: NodeId) -> bool {
    matches!(
        program.node(statement).map(|node| node.data()),
        Some(IrNodeData::ExpressionStatement {
            directive: true,
            ..
        })
    )
}

fn late_peephole(
    program: &mut TypedProgram,
    changes: &mut ChangeTracker,
) -> Result<(), TypedIrError> {
    let nodes = program.preorder_validated()?;
    for node in nodes.into_iter().rev() {
        let Some(record) = program.node(node) else {
            continue;
        };
        if record.is_tombstone() {
            continue;
        }
        let data = record.data().clone();
        match data {
            IrNodeData::ReturnStatement {
                argument: Some(result),
            } => {
                let Some(IrNodeData::UnaryExpression {
                    operator: UnaryOperator::Void,
                    argument,
                }) = program.node(result).map(|node| node.data())
                else {
                    continue;
                };
                if !expression_is_discardable(program, *argument) {
                    continue;
                }
                let origin = rewrite_origin(record.origin());
                let replacement = program
                    .append_detached_leaf(IrNodeData::ReturnStatement { argument: None }, origin)?;
                program.replace_node(node, replacement)?;
                changes.changed();
            }
            IrNodeData::MemberExpression {
                object,
                property,
                property_kind: PropertyKeyKind::Computed,
                optional,
            } => {
                let Some(IrNodeData::StringLiteral { value }) =
                    program.node(property).map(|node| node.data())
                else {
                    continue;
                };
                if !is_ascii_identifier_name(value) {
                    continue;
                }
                let value = value.clone();
                let member_origin = rewrite_origin(record.origin());
                let property_origin = program
                    .node(property)
                    .ok_or_else(|| missing_node(property))?
                    .origin();
                let object = program.clone_detached_subtree(object)?;
                let property = program.append_detached_name(
                    value,
                    NameRole::Property,
                    NameSyntax::Identifier,
                    None,
                    property_origin,
                )?;
                let replacement = program.append_detached_node_with(member_origin, |_| {
                    Ok(IrNodeData::MemberExpression {
                        object,
                        property,
                        property_kind: PropertyKeyKind::Identifier,
                        optional,
                    })
                })?;
                program.replace_node(node, replacement)?;
                changes.changed();
            }
            IrNodeData::SequenceExpression { expressions } => {
                let items = program
                    .list(expressions)
                    .ok_or_else(|| missing_node(node))?
                    .items()
                    .to_vec();
                if !items.iter().any(|item| {
                    matches!(
                        program.node(*item).map(|node| node.data()),
                        Some(IrNodeData::SequenceExpression { .. })
                    )
                }) {
                    continue;
                }
                let origin = rewrite_origin(record.origin());
                let mut flattened = Vec::new();
                for item in items {
                    append_flat_sequence_clones(program, item, &mut flattened)?;
                }
                let replacement = program.append_detached_node_with(origin, |builder| {
                    let expressions = builder.list(ChildRole::SequenceItems, flattened)?;
                    Ok(IrNodeData::SequenceExpression { expressions })
                })?;
                program.replace_node(node, replacement)?;
                changes.changed();
            }
            _ => {}
        }
    }
    Ok(())
}

fn is_ascii_identifier_name(value: &str) -> bool {
    let mut bytes = value.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || matches!(first, b'_' | b'$'))
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$'))
}

fn statement_lists(program: &TypedProgram) -> Result<Vec<ListId>, TypedIrError> {
    let mut lists = Vec::new();
    // Process descendants before their owning statements. Merging an outer expression or
    // declaration run deep-clones the accepted statements and tombstones the originals; a
    // root-first snapshot would therefore leave later entries pointing at lists owned by those
    // tombstoned subtrees. Postorder keeps every list live until its own rewrite is complete.
    for node in program.preorder_validated()?.into_iter().rev() {
        let Some(node) = program.node(node) else {
            continue;
        };
        let list = match node.data() {
            IrNodeData::Program { body, .. }
            | IrNodeData::Block { body }
            | IrNodeData::StaticBlock { body } => Some(*body),
            IrNodeData::FunctionBody { statements, .. } => Some(*statements),
            IrNodeData::SwitchCase { consequent, .. } => Some(*consequent),
            _ => None,
        };
        if let Some(list) = list {
            lists.push(list);
        }
    }
    Ok(lists)
}

fn rewrite_origin(origin: IrOrigin) -> IrOrigin {
    match origin {
        IrOrigin::Source(span) => IrOrigin::Derived {
            anchor: Some(span),
            kind: DerivedOriginKind::Optimization,
        },
        IrOrigin::Derived { anchor, .. } => IrOrigin::Derived {
            anchor,
            kind: DerivedOriginKind::Optimization,
        },
        IrOrigin::Synthetic {
            anchor: Some(anchor),
            ..
        } => IrOrigin::Derived {
            anchor: Some(anchor),
            kind: DerivedOriginKind::Optimization,
        },
        IrOrigin::Synthetic { anchor: None, .. } => IrOrigin::Synthetic {
            anchor: None,
            kind: SyntheticOriginKind::Optimization,
        },
    }
}

fn missing_node(node: NodeId) -> TypedIrError {
    TypedIrError {
        node: Some(node),
        message: "typed pass referenced a missing node".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wake_common::{Interner, Span};
    use wake_ecma_ast::{SourceType, Statement};

    use crate::typed_edits::{TypedEditInput, TypedTrustedExpressionEdit, apply_typed_edits};
    use crate::typed_ir::{IrOrigin, TypedExpressionOwner, lower_expression_owner};

    fn lower(source: &str) -> TypedProgram {
        lower_with_source_type(source, SourceType::Script)
    }

    fn lower_with_source_type(source: &str, source_type: SourceType) -> TypedProgram {
        let interner = Interner::new();
        let parsed = wake_ecma_parser::parse(source, &interner, source_type);
        assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);
        parsed.module.with_ast(|program| {
            let semantic = wake_ecma_semantic::analyze(program);
            TypedProgram::lower(program, &interner, Some(&semantic)).unwrap()
        })
    }

    fn expression_owner(source: &str) -> TypedExpressionOwner {
        let interner = Interner::new();
        let parsed = wake_ecma_parser::parse(source, &interner, SourceType::Script);
        assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);
        parsed.module.with_ast(|program| {
            let semantic = wake_ecma_semantic::analyze(program);
            let Statement::Expression(statement) = &program.body[0] else {
                panic!("owner fixture must contain one expression statement")
            };
            lower_expression_owner(&statement.expression, &interner, Some(&semantic)).unwrap()
        })
    }

    fn source_span(program: &TypedProgram, predicate: impl Fn(&IrNodeData) -> bool) -> Span {
        program
            .preorder()
            .unwrap()
            .into_iter()
            .find_map(|node| {
                let record = program.node(node).unwrap();
                let IrOrigin::Source(span) = record.origin() else {
                    return None;
                };
                predicate(record.data()).then_some(span)
            })
            .expect("source-backed fixture node")
    }

    fn optimize(source: &str, options: TypedPassOptions) -> (TypedProgram, TypedPassReport) {
        let mut program = lower(source);
        let report = optimize_typed_program(&mut program, options).unwrap();
        program.validate().unwrap();
        (program, report)
    }

    fn live_nodes(program: &TypedProgram) -> Vec<NodeId> {
        program.preorder().unwrap()
    }

    fn count(program: &TypedProgram, predicate: impl Fn(&IrNodeData) -> bool) -> usize {
        live_nodes(program)
            .into_iter()
            .filter(|node| predicate(program.node(*node).unwrap().data()))
            .count()
    }

    fn live_names(program: &TypedProgram) -> Vec<&str> {
        live_nodes(program)
            .into_iter()
            .filter_map(|node| match program.node(node)?.data() {
                IrNodeData::Name { name } => Some(program.name(*name)?.original()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn pass_order_is_stable_and_explicit() {
        assert_eq!(
            TYPED_PASS_ORDER.map(TypedPassKind::name),
            [
                "primitive-folding",
                "branch-simplification",
                "configured-drops",
                "dead-statement-cleanup",
                "statement-merging",
                "late-peephole",
            ]
        );
    }

    #[test]
    fn folds_only_cost_effective_primitive_operations() {
        let (program, report) = optimize(
            r#"
const sum = 1 + 2;
const text = "a" + "b";
const expensive = 1 / 3;
const negativeZero = 0 * -1;
const bigint = 1n + 2n;
const coercion = value + 1;
"#,
            TypedPassOptions::default(),
        );
        assert!(report.total_changes >= 2);
        assert!(live_nodes(&program).into_iter().any(|node| {
            matches!(
                program.node(node).unwrap().data(),
                IrNodeData::NumberLiteral { value } if *value == 3.0
            )
        }));
        assert!(live_nodes(&program).into_iter().any(|node| {
            matches!(
                program.node(node).unwrap().data(),
                IrNodeData::StringLiteral { value } if value == "ab"
            )
        }));
        // 1/3 grows substantially, -0 is grammar/identity-sensitive, BigInt mixing and unknown
        // coercion stay represented by binary expressions.
        assert!(
            count(&program, |node| matches!(
                node,
                IrNodeData::BinaryExpression { .. }
            )) >= 4
        );
    }

    #[test]
    fn fixed_point_exposes_a_second_constant_folding_opportunity() {
        let (program, report) = optimize("const answer=(true?1:2)+2;", TypedPassOptions::default());
        assert!(
            report.iterations >= 3,
            "one changing round per dependency plus proof round"
        );
        assert_eq!(
            count(&program, |node| matches!(
                node,
                IrNodeData::ConditionalExpression { .. }
            )),
            0
        );
        assert!(live_nodes(&program).into_iter().any(|node| {
            matches!(
                program.node(node).unwrap().data(),
                IrNodeData::NumberLiteral { value } if *value == 3.0
            )
        }));
    }

    #[test]
    fn promotes_known_if_conditional_and_logical_branches_structurally() {
        let (program, _) = optimize(
            r#"
if (true) chosen(); else rejected();
const a = false ? rejectedA() : 1;
const b = false && rejectedB();
const c = null ?? 3;
"#,
            TypedPassOptions::default(),
        );
        assert_eq!(
            count(&program, |node| matches!(
                node,
                IrNodeData::IfStatement { .. }
            )),
            0
        );
        assert_eq!(
            count(&program, |node| matches!(
                node,
                IrNodeData::ConditionalExpression { .. }
            )),
            0
        );
        assert_eq!(
            count(&program, |node| matches!(
                node,
                IrNodeData::LogicalExpression { .. }
            )),
            0
        );
        let names = live_names(&program);
        assert!(names.contains(&"chosen"));
        assert!(!names.contains(&"rejected"));
        assert!(!names.contains(&"rejectedA"));
        assert!(!names.contains(&"rejectedB"));
    }

    #[test]
    fn selected_branch_keeps_its_own_source_origin() {
        let mut program = lower("true?chosen():rejected();");
        let selected_origin = source_span(&program, |data| {
            matches!(data, IrNodeData::CallExpression { .. })
        });

        assert_eq!(
            run_typed_pass(
                &mut program,
                TypedPassOptions::default(),
                TypedPassKind::BranchSimplification,
            )
            .unwrap(),
            1
        );
        let surviving_call = program
            .preorder()
            .unwrap()
            .into_iter()
            .find(|&node| {
                matches!(
                    program.node(node).unwrap().data(),
                    IrNodeData::CallExpression { .. }
                )
            })
            .expect("selected call remains live");
        assert_eq!(
            program.node(surviving_call).unwrap().origin(),
            IrOrigin::Source(selected_origin)
        );
    }

    #[test]
    fn configured_drops_remove_global_console_and_debugger_but_freeze_local_console() {
        let (program, _) = optimize(
            "debugger;console.log(effect());keep();",
            TypedPassOptions {
                drop_debugger: true,
                drop_console: true,
            },
        );
        let names = live_names(&program);
        assert!(!names.contains(&"effect"));
        assert!(names.contains(&"keep"));
        assert_eq!(
            count(&program, |node| matches!(
                node,
                IrNodeData::DebuggerStatement
            )),
            0
        );

        let (local, _) = optimize(
            "const console={log(){}};console.log(kept());",
            TypedPassOptions {
                drop_debugger: false,
                drop_console: true,
            },
        );
        assert!(live_names(&local).contains(&"kept"));
        assert!(
            count(&local, |node| matches!(
                node,
                IrNodeData::CallExpression { .. }
            )) >= 2
        );
    }

    #[test]
    fn unreachable_expression_is_removed_but_hoisted_var_declaration_survives() {
        let (program, _) = optimize(
            "function f(){return 1;effect();var hoisted;}",
            TypedPassOptions::default(),
        );
        let names = live_names(&program);
        assert!(!names.contains(&"effect"));
        assert!(names.contains(&"hoisted"));
        assert_eq!(
            count(&program, |node| matches!(
                node,
                IrNodeData::VariableDeclaration { .. }
            )),
            1
        );
    }

    #[test]
    fn dead_statement_cleanup_keeps_throwing_primitive_operators() {
        let (program, _) = optimize(
            "1n+1;+1n;null in null;null instanceof null;1+2;",
            TypedPassOptions::default(),
        );
        assert_eq!(
            count(&program, |node| matches!(
                node,
                IrNodeData::BinaryExpression {
                    operator: BinaryOperator::Add,
                    ..
                }
            )),
            1,
            "mixed BigInt addition must remain observable"
        );
        assert_eq!(
            count(&program, |node| matches!(
                node,
                IrNodeData::UnaryExpression {
                    operator: UnaryOperator::Plus,
                    ..
                }
            )),
            1,
            "unary plus on BigInt must retain its TypeError"
        );
        for operator in [BinaryOperator::In, BinaryOperator::Instanceof] {
            assert_eq!(
                count(&program, |node| matches!(
                    node,
                    IrNodeData::BinaryExpression {
                        operator: current,
                        ..
                    } if *current == operator
                )),
                1,
                "{operator:?} must retain its primitive-RHS TypeError"
            );
        }
    }

    #[test]
    fn merges_safe_statement_runs_without_merging_using_or_directives() {
        let (program, _) = optimize(
            r#""use strict";let a=sideA();let b=sideB();first();second();using resource=open();using other=open2();"#,
            TypedPassOptions::default(),
        );
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
        let declarations = live_nodes(&program)
            .into_iter()
            .filter_map(|node| match program.node(node)?.data() {
                IrNodeData::VariableDeclaration { kind, declarations } => {
                    Some((*kind, program.list(*declarations)?.items().len()))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(declarations.contains(&(VarKind::Let, 2)));
        assert_eq!(
            declarations
                .iter()
                .filter(|(kind, _)| *kind == VarKind::Using)
                .count(),
            2
        );
        assert!(live_nodes(&program).into_iter().any(|node| {
            matches!(
                program.node(node).unwrap().data(),
                IrNodeData::SequenceExpression { expressions }
                    if program.list(*expressions).unwrap().items().len() == 2
            )
        }));
    }

    #[test]
    fn long_statement_run_planning_has_linear_classification_work() {
        const GROUPS: usize = 8_192;
        let items = (0..GROUPS * 3).collect::<Vec<_>>();
        let classifications = std::cell::Cell::new(0usize);

        let runs = mergeable_runs(&items, |item| {
            classifications.set(classifications.get() + 1);
            match item % 3 {
                0 | 1 => Some(item / 3),
                _ => None,
            }
        });

        assert_eq!(runs.len(), GROUPS);
        assert_eq!(runs.first(), Some(&(0..2)));
        assert_eq!(runs.last(), Some(&((GROUPS - 1) * 3..(GROUPS - 1) * 3 + 2)));
        assert!(
            classifications.get() <= items.len() * 2,
            "each item may terminate one run and start the next, but work must remain linear"
        );
    }

    #[test]
    fn statement_merging_handles_many_short_runs_in_one_long_list() {
        use std::fmt::Write as _;

        const GROUPS: usize = 512;
        let mut source = String::with_capacity(GROUPS * 96);
        for index in 0..GROUPS {
            write!(
                source,
                "let a{index}=left({index});let b{index}=right({index});first({index});second({index});const barrier{index}={index};"
            )
            .unwrap();
        }

        let mut program = lower(&source);
        let changes = run_typed_pass(
            &mut program,
            TypedPassOptions::default(),
            TypedPassKind::StatementMerging,
        )
        .unwrap();

        assert_eq!(changes, GROUPS * 2);
        assert_eq!(
            count(&program, |node| matches!(
                node,
                IrNodeData::VariableDeclaration { .. }
            )),
            GROUPS * 2
        );
        assert_eq!(
            count(&program, |node| matches!(
                node,
                IrNodeData::SequenceExpression { .. }
            )),
            GROUPS
        );
        program.validate().unwrap();
    }

    #[test]
    fn statement_merging_rewrites_nested_lists_before_their_owning_statements() {
        let mut program = lower("(()=>{first();second()})();after();");
        let changes = run_typed_pass(
            &mut program,
            TypedPassOptions::default(),
            TypedPassKind::StatementMerging,
        )
        .unwrap();
        assert_eq!(
            changes, 2,
            "inner and outer expression runs must both merge"
        );
        program.validate().unwrap();
    }

    #[test]
    fn statement_merging_accepts_typescript_namespace_and_enum_runtime_lowering() {
        let mut program = lower_with_source_type(
            "enum Kind{First=1,Second} namespace Bag{export const value=Kind.Second}",
            SourceType::TypeScript,
        );
        run_typed_pass(
            &mut program,
            TypedPassOptions::default(),
            TypedPassKind::StatementMerging,
        )
        .unwrap();
        program.validate().unwrap();
    }

    #[test]
    fn statement_merging_accepts_materialized_decorated_default_export() {
        let mut program = lower_with_source_type(
            "function dec(value){return value}\n@dec export default class Defaulted{@dec field=1}",
            SourceType::TypeScript,
        );
        crate::typed_decorators::materialize_decorators(&mut program).unwrap();
        run_typed_pass(
            &mut program,
            TypedPassOptions::default(),
            TypedPassKind::StatementMerging,
        )
        .unwrap();
        program.validate().unwrap();
    }

    #[test]
    fn statement_merging_accepts_trusted_directive_replacement_with_nested_body() {
        let mut program = lower("\"wake\";next();after();");
        let target = source_span(
            &program,
            |data| matches!(data, IrNodeData::StringLiteral { value } if value == "wake"),
        );
        apply_typed_edits(
            &mut program,
            &TypedEditInput {
                expression_edits: vec![TypedTrustedExpressionEdit::new(
                    target,
                    expression_owner("(()=>{inside();again()})()"),
                )],
                ..TypedEditInput::default()
            },
        )
        .unwrap();
        run_typed_pass(
            &mut program,
            TypedPassOptions::default(),
            TypedPassKind::StatementMerging,
        )
        .unwrap();
        program.validate().unwrap();
    }

    #[test]
    fn late_peephole_only_converts_static_identifier_brackets() {
        let (program, _) = optimize(
            "sink=obj[\"valid\"];sink2=obj[\"not-valid\"];",
            TypedPassOptions::default(),
        );
        let kinds = live_nodes(&program)
            .into_iter()
            .filter_map(|node| match program.node(node)?.data() {
                IrNodeData::MemberExpression { property_kind, .. } => Some(*property_kind),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(kinds.contains(&PropertyKeyKind::Identifier));
        assert!(kinds.contains(&PropertyKeyKind::Computed));
    }

    #[test]
    fn late_peephole_elides_only_effect_free_void_return_arguments() {
        let mut program =
            lower("function compact(){return void 0}function preserve(){return void sideEffect()}");
        let changes = run_typed_pass(
            &mut program,
            TypedPassOptions::default(),
            TypedPassKind::LatePeephole,
        )
        .unwrap();
        assert_eq!(changes, 1);
        program.validate().unwrap();

        let returns = live_nodes(&program)
            .into_iter()
            .filter_map(|node| match program.node(node)?.data() {
                IrNodeData::ReturnStatement { argument } => Some(*argument),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(returns.len(), 2);
        assert_eq!(
            returns.iter().filter(|argument| argument.is_none()).count(),
            1
        );
        let preserved = returns.into_iter().flatten().next().unwrap();
        assert!(matches!(
            program.node(preserved).unwrap().data(),
            IrNodeData::UnaryExpression {
                operator: UnaryOperator::Void,
                argument
            } if matches!(
                program.node(*argument).unwrap().data(),
                IrNodeData::CallExpression { .. }
            )
        ));
    }

    #[test]
    fn rewritten_constant_keeps_an_optimization_origin_anchor() {
        let mut program = lower("const value=1+2;");
        let binary = live_nodes(&program)
            .into_iter()
            .find(|node| {
                matches!(
                    program.node(*node).unwrap().data(),
                    IrNodeData::BinaryExpression { .. }
                )
            })
            .unwrap();
        let expected = match program.node(binary).unwrap().origin() {
            IrOrigin::Source(span) => span,
            other => panic!("unexpected parser origin: {other:?}"),
        };
        optimize_typed_program(&mut program, TypedPassOptions::default()).unwrap();
        let folded = live_nodes(&program)
            .into_iter()
            .find(|node| {
                matches!(
                    program.node(*node).unwrap().data(),
                    IrNodeData::NumberLiteral { value } if *value == 3.0
                )
            })
            .unwrap();
        assert_eq!(
            program.node(folded).unwrap().origin(),
            IrOrigin::Derived {
                anchor: Some(expected),
                kind: DerivedOriginKind::Optimization,
            }
        );
    }

    #[test]
    fn convergence_cap_returns_a_diagnostic_instead_of_falling_back() {
        let mut rounds = 0;
        let error = run_to_fixed_point(100, || {
            rounds += 1;
            Ok(1)
        })
        .unwrap_err();
        assert_eq!(rounds, 100);
        assert!(matches!(
            error,
            TypedPassError::DidNotConverge {
                iterations: 100,
                last_round_changes: 1
            }
        ));
    }

    #[test]
    fn canonical_void_zero_and_nested_console_drop_reach_a_real_fixed_point() {
        let (_, plain_report) = optimize("const missing=void 0;", TypedPassOptions::default());
        assert_eq!(plain_report.total_changes, 0);
        assert_eq!(plain_report.iterations, 1);

        let (program, dropped_report) = optimize(
            "const result=console.log(effect());",
            TypedPassOptions {
                drop_debugger: false,
                drop_console: true,
            },
        );
        assert!(dropped_report.iterations < MAX_TYPED_FIXED_POINT_ITERATIONS);
        assert!(!live_names(&program).contains(&"effect"));
        assert!(live_nodes(&program).into_iter().any(|node| {
            matches!(
                program.node(node).unwrap().data(),
                IrNodeData::UnaryExpression {
                    operator: UnaryOperator::Void,
                    ..
                }
            )
        }));
    }
}
