//! Trusted structural edits for the optimizer-owned [`TypedProgram`].
//!
//! This boundary accepts only parser-validated expression owners and typed edit descriptions.
//! It never creates source-text replacements or consults span-indexed rewrite plans.

use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fmt;

use wake_common::Span;
use wake_ecma_ast::{UnaryOperator, VarKind};

use crate::ConstVal;
use crate::typed_analysis::{TypedAnalysis, TypedEffectSummary};
use crate::typed_ir::{
    ChildRole, ClassContext, FunctionContext, IrNodeData, IrOrigin, IrPropertyKey, ListId,
    NameRole, NodeId, PropertyKeyKind, SyntheticOriginKind, TypedExpressionOwner, TypedIrError,
    TypedProgram,
};

/// One validated define value ready to enter the typed-IR boundary.
#[derive(Clone, Debug, PartialEq)]
pub enum TypedDefineValue {
    Primitive(ConstVal),
    Expression(TypedExpressionOwner),
}

/// A binding-safe dotted define key and its validated replacement expression.
#[derive(Clone, Debug, PartialEq)]
pub struct TypedValidatedDefine {
    pub key: String,
    pub value: TypedDefineValue,
}

impl TypedValidatedDefine {
    #[cfg(test)]
    pub fn primitive(key: impl Into<String>, value: ConstVal) -> Self {
        Self {
            key: key.into(),
            value: TypedDefineValue::Primitive(value),
        }
    }

    #[cfg(test)]
    pub fn expression(key: impl Into<String>, owner: TypedExpressionOwner) -> Self {
        Self {
            key: key.into(),
            value: TypedDefineValue::Expression(owner),
        }
    }
}

/// One parser-classified expression replacement.
#[derive(Clone, Debug, PartialEq)]
pub struct TypedTrustedExpressionEdit {
    pub target: Span,
    pub owner: TypedExpressionOwner,
}

impl TypedTrustedExpressionEdit {
    pub const fn new(target: Span, owner: TypedExpressionOwner) -> Self {
        Self { target, owner }
    }
}

/// Structural syntax category removed by a trusted edit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TypedRemovalKind {
    Statement,
    Binding,
}

/// One exact source-backed statement or declaration binding removal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TypedTrustedRemoval {
    pub target: Span,
    pub kind: TypedRemovalKind,
}

impl TypedTrustedRemoval {
    #[cfg(test)]
    pub const fn statement(target: Span) -> Self {
        Self {
            target,
            kind: TypedRemovalKind::Statement,
        }
    }

    #[cfg(test)]
    pub const fn binding(target: Span) -> Self {
        Self {
            target,
            kind: TypedRemovalKind::Binding,
        }
    }
}

/// Dependency edge whose liveness is owned by a typed IR origin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypedEditDependency {
    pub specifier: String,
    pub origin: IrOrigin,
}

/// Complete trusted-edit input. The caller converts public optimizer input into this owned form.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TypedEditInput {
    pub defines: Vec<TypedValidatedDefine>,
    pub expression_edits: Vec<TypedTrustedExpressionEdit>,
    pub removals: Vec<TypedTrustedRemoval>,
    pub dependencies: Vec<TypedEditDependency>,
}

/// Successful atomic edit result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypedEditReport {
    pub change_count: usize,
    pub retained_dependencies: Vec<TypedEditDependency>,
}

/// Expected source-backed target category used in diagnostics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TypedEditTargetKind {
    Expression,
    Statement,
    Binding,
}

impl TypedEditTargetKind {
    const fn name(self) -> &'static str {
        match self {
            Self::Expression => "expression",
            Self::Statement => "statement",
            Self::Binding => "binding",
        }
    }
}

/// Trusted edits fail explicitly and leave the caller's program unchanged.
#[derive(Debug)]
pub enum TypedEditError {
    InvalidInput {
        message: String,
    },
    MissingTarget {
        target: Span,
        expected: TypedEditTargetKind,
    },
    CategoryMismatch {
        target: Span,
        expected: TypedEditTargetKind,
    },
    AmbiguousTarget {
        target: Span,
        expected: TypedEditTargetKind,
        matches: usize,
    },
    Conflict {
        first: Span,
        second: Span,
    },
    UnsupportedContext {
        target: Span,
        message: String,
    },
    Ir(TypedIrError),
}

impl fmt::Display for TypedEditError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput { message } => {
                write!(formatter, "invalid typed edit input: {message}")
            }
            Self::MissingTarget { target, expected } => write!(
                formatter,
                "trusted {} target {}..{} does not exist",
                expected.name(),
                target.lo,
                target.hi
            ),
            Self::CategoryMismatch { target, expected } => write!(
                formatter,
                "trusted target {}..{} is not {} syntax",
                target.lo,
                target.hi,
                expected.name()
            ),
            Self::AmbiguousTarget {
                target,
                expected,
                matches,
            } => write!(
                formatter,
                "trusted {} target {}..{} matches {matches} typed occurrences",
                expected.name(),
                target.lo,
                target.hi
            ),
            Self::Conflict { first, second } => write!(
                formatter,
                "trusted edits overlap at {}..{} and {}..{}",
                first.lo, first.hi, second.lo, second.hi
            ),
            Self::UnsupportedContext { target, message } => write!(
                formatter,
                "trusted edit at {}..{} is not structurally supported: {message}",
                target.lo, target.hi
            ),
            Self::Ir(error) => write!(formatter, "typed edit failed: {error}"),
        }
    }
}

impl Error for TypedEditError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Ir(error) => Some(error),
            _ => None,
        }
    }
}

impl From<TypedIrError> for TypedEditError {
    fn from(value: TypedIrError) -> Self {
        Self::Ir(value)
    }
}

#[derive(Clone, Copy)]
struct ResolvedExpressionEdit<'input> {
    target: Span,
    node: NodeId,
    owner: &'input TypedExpressionOwner,
}

#[derive(Clone, Copy)]
struct ResolvedRemoval {
    target: Span,
    node: NodeId,
    kind: TypedRemovalKind,
}

#[derive(Clone, Copy)]
struct DefineCandidate {
    target: Span,
    node: NodeId,
    define: usize,
}

struct EditPlan<'input> {
    expressions: Vec<ResolvedExpressionEdit<'input>>,
    defines: Vec<DefineCandidate>,
    removals: Vec<ResolvedRemoval>,
}

/// Apply all trusted edits transactionally.
///
/// Validation and target resolution happen before mutation. Work is committed from a cloned
/// arena only after every replacement/removal and the final grammar validation succeeds.
pub fn apply_typed_edits(
    program: &mut TypedProgram,
    input: &TypedEditInput,
) -> Result<TypedEditReport, TypedEditError> {
    if input.expression_edits.is_empty() && input.defines.is_empty() && input.removals.is_empty() {
        return Ok(TypedEditReport {
            change_count: 0,
            retained_dependencies: retain_live_dependencies(program, &input.dependencies)?,
        });
    }
    program.validate()?;
    let plan = build_plan(program, input)?;
    if plan.expressions.is_empty() && plan.defines.is_empty() && plan.removals.is_empty() {
        return Ok(TypedEditReport {
            change_count: 0,
            retained_dependencies: retain_live_dependencies(program, &input.dependencies)?,
        });
    }
    let mut working = program.clone();
    let change_count = apply_plan(&mut working, input, &plan)?;
    working.validate()?;
    let retained_dependencies = retain_live_dependencies(&working, &input.dependencies)?;
    *program = working;
    Ok(TypedEditReport {
        change_count,
        retained_dependencies,
    })
}

/// Recompute dependency retention from the current live root after later optimizer passes.
///
/// Trusted edits report an initial set, but constant-branch promotion, inlining and DCE can make
/// additional source-owned edges unreachable. The production scheduler calls this once after the
/// fixed point instead of retaining a stale pre-pass snapshot.
pub fn retain_typed_dependencies(
    program: &TypedProgram,
    dependencies: &[TypedEditDependency],
) -> Result<Vec<TypedEditDependency>, TypedEditError> {
    retain_live_dependencies(program, dependencies)
}

fn build_plan<'input>(
    program: &TypedProgram,
    input: &'input TypedEditInput,
) -> Result<EditPlan<'input>, TypedEditError> {
    validate_defines(input)?;
    let mut expressions = Vec::with_capacity(input.expression_edits.len());
    let mut removals = Vec::with_capacity(input.removals.len());
    let mut explicit_spans =
        Vec::with_capacity(input.expression_edits.len() + input.removals.len());

    for edit in &input.expression_edits {
        validate_source_span(edit.target)?;
        edit.owner.validate()?;
        let node = resolve_unique_target(program, edit.target, TypedEditTargetKind::Expression)?;
        if in_write_context(program, node) {
            return Err(TypedEditError::UnsupportedContext {
                target: edit.target,
                message: "an expression write/update/delete target cannot be replaced".into(),
            });
        }
        expressions.push(ResolvedExpressionEdit {
            target: edit.target,
            node,
            owner: &edit.owner,
        });
        explicit_spans.push(edit.target);
    }

    for removal in &input.removals {
        validate_source_span(removal.target)?;
        let expected = match removal.kind {
            TypedRemovalKind::Statement => TypedEditTargetKind::Statement,
            TypedRemovalKind::Binding => TypedEditTargetKind::Binding,
        };
        let node = resolve_unique_target(program, removal.target, expected)?;
        removals.push(ResolvedRemoval {
            target: removal.target,
            node,
            kind: removal.kind,
        });
        explicit_spans.push(removal.target);
    }

    reject_overlaps(&explicit_spans)?;
    let defines = collect_define_candidates(program, &input.defines)?;
    for candidate in &defines {
        for &span in &explicit_spans {
            if spans_overlap(candidate.target, span) {
                return Err(TypedEditError::Conflict {
                    first: candidate.target,
                    second: span,
                });
            }
        }
    }

    Ok(EditPlan {
        expressions,
        defines,
        removals,
    })
}

fn validate_defines(input: &TypedEditInput) -> Result<(), TypedEditError> {
    let mut keys = HashSet::with_capacity(input.defines.len());
    for define in &input.defines {
        if !valid_define_key(&define.key) {
            return Err(TypedEditError::InvalidInput {
                message: format!(
                    "define key {:?} is not a dotted identifier chain",
                    define.key
                ),
            });
        }
        if !keys.insert(define.key.as_str()) {
            return Err(TypedEditError::InvalidInput {
                message: format!("define key {:?} is duplicated", define.key),
            });
        }
        if let TypedDefineValue::Expression(owner) = &define.value {
            owner.validate()?;
        }
    }
    for dependency in &input.dependencies {
        if dependency.specifier.is_empty() {
            return Err(TypedEditError::InvalidInput {
                message: "dependency specifier must not be empty".into(),
            });
        }
    }
    Ok(())
}

fn validate_source_span(span: Span) -> Result<(), TypedEditError> {
    if span.is_dummy() || span.lo >= span.hi {
        return Err(TypedEditError::InvalidInput {
            message: format!("target span {}..{} is empty or synthetic", span.lo, span.hi),
        });
    }
    Ok(())
}

fn reject_overlaps(spans: &[Span]) -> Result<(), TypedEditError> {
    let mut sorted = spans.to_vec();
    sorted.sort_unstable_by_key(|span| (span.lo, span.hi));
    for pair in sorted.windows(2) {
        if spans_overlap(pair[0], pair[1]) {
            return Err(TypedEditError::Conflict {
                first: pair[0],
                second: pair[1],
            });
        }
    }
    Ok(())
}

const fn spans_overlap(left: Span, right: Span) -> bool {
    left.lo < right.hi && right.lo < left.hi
}

fn resolve_unique_target(
    program: &TypedProgram,
    target: Span,
    expected: TypedEditTargetKind,
) -> Result<NodeId, TypedEditError> {
    let mut source_occurrences = 0usize;
    let mut matches = Vec::new();
    for node in program.preorder_validated()? {
        if program.node(node).expect("validated node").origin() != IrOrigin::Source(target) {
            continue;
        }
        source_occurrences += 1;
        let matches_category = match expected {
            TypedEditTargetKind::Expression => is_expression_occurrence(program, node),
            TypedEditTargetKind::Statement => is_statement_occurrence(program, node),
            TypedEditTargetKind::Binding => is_binding_occurrence(program, node),
        };
        if matches_category {
            matches.push(node);
        }
    }

    match matches.as_slice() {
        [node] => Ok(*node),
        [] if source_occurrences == 0 => Err(TypedEditError::MissingTarget { target, expected }),
        [] => Err(TypedEditError::CategoryMismatch { target, expected }),
        _ => Err(TypedEditError::AmbiguousTarget {
            target,
            expected,
            matches: matches.len(),
        }),
    }
}

/// This dispatch intentionally names every typed node variant. Adding syntax must decide whether
/// it is a replaceable expression occurrence instead of silently inheriting a wildcard policy.
fn is_expression_occurrence(program: &TypedProgram, node: NodeId) -> bool {
    match program.node(node).expect("validated node").data() {
        IrNodeData::Function { context, .. } => *context == FunctionContext::Expression,
        IrNodeData::Class { context, .. } => *context == ClassContext::Expression,
        IrNodeData::NumberLiteral { .. }
        | IrNodeData::StringLiteral { .. }
        | IrNodeData::BooleanLiteral { .. }
        | IrNodeData::NullLiteral
        | IrNodeData::BigIntLiteral { .. }
        | IrNodeData::RegExpLiteral { .. }
        | IrNodeData::TemplateLiteral { .. }
        | IrNodeData::ThisExpression
        | IrNodeData::SuperExpression
        | IrNodeData::MetaProperty { .. }
        | IrNodeData::ArrayExpression { .. }
        | IrNodeData::ObjectExpression { .. }
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
        | IrNodeData::AwaitExpression { .. }
        | IrNodeData::YieldExpression { .. }
        | IrNodeData::ImportExpression { .. }
        | IrNodeData::ArrowFunction { .. } => true,
        IrNodeData::Identifier { name } => program
            .node(*name)
            .and_then(|name| match name.data() {
                IrNodeData::Name { name } => program.name(*name),
                _ => None,
            })
            .is_some_and(|name| {
                matches!(
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
        | IrNodeData::WithStatement { .. }
        | IrNodeData::TemplateElement { .. }
        | IrNodeData::Name { .. }
        | IrNodeData::Elision
        | IrNodeData::ObjectProperty { .. }
        | IrNodeData::SpreadElement { .. }
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
    }
}

fn is_statement_occurrence(program: &TypedProgram, node: NodeId) -> bool {
    match program.node(node).expect("validated node").data() {
        IrNodeData::Function { context, .. } => *context == FunctionContext::Declaration,
        IrNodeData::Class { context, .. } => *context == ClassContext::Declaration,
        IrNodeData::VariableDeclaration { .. }
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
        | IrNodeData::ReturnStatement { .. }
        | IrNodeData::BreakStatement { .. }
        | IrNodeData::ContinueStatement { .. }
        | IrNodeData::ThrowStatement { .. }
        | IrNodeData::TryStatement { .. }
        | IrNodeData::LabeledStatement { .. }
        | IrNodeData::WithStatement { .. }
        | IrNodeData::ImportDeclaration { .. }
        | IrNodeData::ExportNamedDeclaration { .. }
        | IrNodeData::ExportDefaultDeclaration { .. }
        | IrNodeData::ExportAllDeclaration { .. } => true,
        IrNodeData::Program { .. }
        | IrNodeData::VariableDeclarator { .. }
        | IrNodeData::FunctionBody { .. }
        | IrNodeData::SwitchCase { .. }
        | IrNodeData::CatchClause { .. }
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
        | IrNodeData::ImportSpecifier { .. }
        | IrNodeData::ImportAttributes { .. }
        | IrNodeData::ImportAttribute { .. }
        | IrNodeData::ExportSpecifier { .. } => false,
    }
}

fn is_binding_occurrence(program: &TypedProgram, node: NodeId) -> bool {
    let Some(link) = program.node(node).and_then(|node| node.parent()) else {
        return false;
    };
    match link.role() {
        ChildRole::Binding => matches!(
            program.node(node).expect("validated binding").data(),
            IrNodeData::Identifier { .. }
                | IrNodeData::ArrayPattern { .. }
                | IrNodeData::ObjectPattern { .. }
                | IrNodeData::AssignmentPattern { .. }
                | IrNodeData::RestPattern { .. }
        ),
        ChildRole::ImportLocal | ChildRole::FunctionName | ChildRole::ClassName => {
            matches!(
                program.node(node).expect("validated binding").data(),
                IrNodeData::Name { .. }
            ) && name_role(program, node).is_some_and(|role| {
                matches!(
                    role,
                    NameRole::ImportBinding | NameRole::FunctionName | NameRole::ClassName
                )
            })
        }
        ChildRole::ProgramBody
        | ChildRole::DeclarationItems
        | ChildRole::Initializer
        | ChildRole::Expression
        | ChildRole::IdentifierName
        | ChildRole::BlockBody
        | ChildRole::Test
        | ChildRole::Consequent
        | ChildRole::Alternate
        | ChildRole::ForInitializer
        | ChildRole::ForTest
        | ChildRole::ForUpdate
        | ChildRole::ForLeft
        | ChildRole::ForRight
        | ChildRole::LoopBody
        | ChildRole::SwitchDiscriminant
        | ChildRole::SwitchCases
        | ChildRole::SwitchCaseTest
        | ChildRole::SwitchCaseBody
        | ChildRole::ReturnArgument
        | ChildRole::ThrowArgument
        | ChildRole::TryBlock
        | ChildRole::CatchClause
        | ChildRole::FinallyBlock
        | ChildRole::CatchParameter
        | ChildRole::CatchBody
        | ChildRole::Label
        | ChildRole::LabeledBody
        | ChildRole::WithObject
        | ChildRole::WithBody
        | ChildRole::FunctionParameters
        | ChildRole::FunctionBody
        | ChildRole::FunctionStatements
        | ChildRole::ArrowBody
        | ChildRole::ClassSuper
        | ChildRole::ClassMembers
        | ChildRole::Decorators
        | ChildRole::MethodKey
        | ChildRole::MethodValue
        | ChildRole::PropertyKey
        | ChildRole::PropertyValue
        | ChildRole::StaticBlockBody
        | ChildRole::UnaryArgument
        | ChildRole::UpdateArgument
        | ChildRole::Left
        | ChildRole::Right
        | ChildRole::Callee
        | ChildRole::Arguments
        | ChildRole::Object
        | ChildRole::MemberProperty
        | ChildRole::SequenceItems
        | ChildRole::Tag
        | ChildRole::Template
        | ChildRole::TemplateQuasis
        | ChildRole::TemplateExpressions
        | ChildRole::SpreadArgument
        | ChildRole::AwaitArgument
        | ChildRole::YieldArgument
        | ChildRole::ImportSource
        | ChildRole::ModuleSource
        | ChildRole::ImportOptions
        | ChildRole::ArrayElements
        | ChildRole::ObjectMembers
        | ChildRole::PatternElements
        | ChildRole::PatternProperties
        | ChildRole::PatternRest
        | ChildRole::PatternDefault
        | ChildRole::ImportSpecifiers
        | ChildRole::ImportImported
        | ChildRole::ImportAttributes
        | ChildRole::AttributeItems
        | ChildRole::AttributeKey
        | ChildRole::AttributeValue
        | ChildRole::ExportDeclaration
        | ChildRole::ExportSpecifiers
        | ChildRole::ExportLocal
        | ChildRole::Exported
        | ChildRole::ExportDefaultValue
        | ChildRole::ExportAllName
        | ChildRole::MetaKeyword
        | ChildRole::MetaProperty => false,
    }
}

fn name_role(program: &TypedProgram, node: NodeId) -> Option<NameRole> {
    let IrNodeData::Name { name } = program.node(node)?.data() else {
        return None;
    };
    Some(program.name(*name)?.role())
}

fn valid_define_key(key: &str) -> bool {
    !key.is_empty() && key.split('.').all(valid_identifier_segment)
}

fn valid_identifier_segment(segment: &str) -> bool {
    let mut bytes = segment.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || matches!(first, b'_' | b'$'))
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$'))
}

fn collect_define_candidates(
    program: &TypedProgram,
    defines: &[TypedValidatedDefine],
) -> Result<Vec<DefineCandidate>, TypedEditError> {
    let by_key = defines
        .iter()
        .enumerate()
        .map(|(index, define)| (define.key.as_str(), index))
        .collect::<HashMap<_, _>>();
    let mut candidates: Vec<DefineCandidate> = Vec::new();
    for node in program.preorder_validated()? {
        if candidates
            .iter()
            .any(|candidate| is_descendant(program, node, candidate.node))
        {
            continue;
        }
        if in_write_context(program, node) {
            continue;
        }
        let Some(chain) = static_unresolved_chain(program, node) else {
            continue;
        };
        let Some(&define) = by_key.get(chain.as_str()) else {
            continue;
        };
        let IrOrigin::Source(target) = program.node(node).expect("validated node").origin() else {
            continue;
        };
        candidates.push(DefineCandidate {
            target,
            node,
            define,
        });
    }
    let mut spans = HashMap::new();
    for candidate in &candidates {
        let matches = spans
            .entry((candidate.target.lo, candidate.target.hi))
            .or_insert(0usize);
        *matches += 1;
        if *matches > 1 {
            return Err(TypedEditError::AmbiguousTarget {
                target: candidate.target,
                expected: TypedEditTargetKind::Expression,
                matches: *matches,
            });
        }
    }
    Ok(candidates)
}

fn static_unresolved_chain(program: &TypedProgram, node: NodeId) -> Option<String> {
    match program.node(node)?.data() {
        IrNodeData::Identifier { name } => {
            let IrNodeData::Name { name } = program.node(*name)?.data() else {
                return None;
            };
            let name = program.name(*name)?;
            (name.role() == NameRole::Reference && name.symbol().is_none())
                .then(|| name.original().to_owned())
        }
        IrNodeData::MetaProperty { meta, property } => {
            let meta = static_name(program, *meta)?;
            let property = static_name(program, *property)?;
            Some(format!("{meta}.{property}"))
        }
        IrNodeData::MemberExpression {
            object,
            property,
            property_kind,
            optional,
        } if !optional => {
            let mut chain = static_unresolved_chain(program, *object)?;
            let property = match property_kind {
                PropertyKeyKind::Identifier => {
                    let IrNodeData::Name { name } = program.node(*property)?.data() else {
                        return None;
                    };
                    program.name(*name)?.original().to_owned()
                }
                PropertyKeyKind::String => match program.node(*property)?.data() {
                    IrNodeData::StringLiteral { value } => value.clone(),
                    _ => return None,
                },
                PropertyKeyKind::Number | PropertyKeyKind::Computed | PropertyKeyKind::Private => {
                    return None;
                }
            };
            if !valid_identifier_segment(&property) {
                return None;
            }
            chain.push('.');
            chain.push_str(&property);
            Some(chain)
        }
        _ => None,
    }
}

fn static_name(program: &TypedProgram, node: NodeId) -> Option<&str> {
    let IrNodeData::Name { name } = program.node(node)?.data() else {
        return None;
    };
    Some(program.name(*name)?.original())
}

fn in_write_context(program: &TypedProgram, mut node: NodeId) -> bool {
    while let Some(link) = program.node(node).and_then(|node| node.parent()) {
        let parent = link.parent();
        let protected = matches!(
            (
                link.role(),
                program.node(parent).expect("validated parent").data(),
            ),
            (ChildRole::Left, IrNodeData::AssignmentExpression { .. })
                | (
                    ChildRole::UpdateArgument,
                    IrNodeData::UpdateExpression { .. }
                )
                | (
                    ChildRole::UnaryArgument,
                    IrNodeData::UnaryExpression {
                        operator: UnaryOperator::Delete,
                        ..
                    },
                )
                | (
                    ChildRole::ForLeft,
                    IrNodeData::ForInStatement { .. } | IrNodeData::ForOfStatement { .. },
                )
        );
        if protected {
            return true;
        }
        node = parent;
    }
    false
}

fn is_descendant(program: &TypedProgram, mut node: NodeId, ancestor: NodeId) -> bool {
    while let Some(link) = program.node(node).and_then(|node| node.parent()) {
        if link.parent() == ancestor {
            return true;
        }
        node = link.parent();
    }
    false
}

fn apply_plan(
    program: &mut TypedProgram,
    input: &TypedEditInput,
    plan: &EditPlan<'_>,
) -> Result<usize, TypedEditError> {
    let mut changes = 0usize;

    for edit in &plan.expressions {
        let replacement = program.import_expression_owner_at(
            edit.owner,
            edit.target,
            SyntheticOriginKind::TrustedEdit,
        )?;
        replace_expression(program, edit.node, replacement, edit.target)?;
        changes += 1;
    }

    for candidate in &plan.defines {
        let replacement = match &input.defines[candidate.define].value {
            TypedDefineValue::Primitive(value) => {
                append_primitive(program, value, trusted_origin(candidate.target))?
            }
            TypedDefineValue::Expression(owner) => program.import_expression_owner_at(
                owner,
                candidate.target,
                SyntheticOriginKind::TrustedEdit,
            )?,
        };
        replace_expression(program, candidate.node, replacement, candidate.target)?;
        changes += 1;
    }

    let mut statements = plan
        .removals
        .iter()
        .copied()
        .filter(|removal| removal.kind == TypedRemovalKind::Statement)
        .collect::<Vec<_>>();
    statements.sort_unstable_by_key(|removal| (removal.target.lo, removal.target.hi));
    for removal in statements {
        if matches!(
            program.node(removal.node).map(|node| node.data()),
            Some(IrNodeData::ExpressionStatement {
                directive: true,
                ..
            })
        ) {
            continue;
        }
        remove_statement(program, removal.node, removal.target)?;
        changes += 1;
    }

    let bindings = plan
        .removals
        .iter()
        .copied()
        .filter(|removal| removal.kind == TypedRemovalKind::Binding)
        .collect::<Vec<_>>();
    if !bindings.is_empty() {
        let analysis = TypedAnalysis::rebuild(program)?;
        apply_binding_removals(program, &analysis, &bindings)?;
        changes += bindings.len();
    }

    Ok(changes)
}

const fn trusted_origin(anchor: Span) -> IrOrigin {
    IrOrigin::Synthetic {
        anchor: Some(anchor),
        kind: SyntheticOriginKind::TrustedEdit,
    }
}

fn append_primitive(
    program: &mut TypedProgram,
    value: &ConstVal,
    origin: IrOrigin,
) -> Result<NodeId, TypedIrError> {
    match value {
        ConstVal::Bool(value) => {
            program.append_detached_leaf(IrNodeData::BooleanLiteral { value: *value }, origin)
        }
        ConstVal::Str(value) => program.append_detached_leaf(
            IrNodeData::StringLiteral {
                value: value.clone(),
            },
            origin,
        ),
        ConstVal::Num(value) if value.is_sign_negative() && !value.is_nan() => {
            let argument = program
                .append_detached_leaf(IrNodeData::NumberLiteral { value: value.abs() }, origin)?;
            program.append_detached_node_with(origin, |_| {
                Ok(IrNodeData::UnaryExpression {
                    operator: UnaryOperator::Minus,
                    argument,
                })
            })
        }
        ConstVal::Num(value) => {
            program.append_detached_leaf(IrNodeData::NumberLiteral { value: *value }, origin)
        }
        ConstVal::Null => program.append_detached_leaf(IrNodeData::NullLiteral, origin),
        ConstVal::Undefined => {
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

fn replace_expression(
    program: &mut TypedProgram,
    target: NodeId,
    replacement: NodeId,
    anchor: Span,
) -> Result<(), TypedEditError> {
    if let Some(property) = shorthand_property(program, target) {
        return replace_shorthand_value(program, property, replacement, anchor);
    }

    if let Some(parent) = program.node(target).and_then(|node| node.parent()) {
        let parent_id = parent.parent();
        if matches!(
            program.node(parent_id).expect("validated parent").data(),
            IrNodeData::ExpressionStatement {
                expression,
                directive: true,
            } if *expression == target
        ) {
            // Standalone expression owners do not retain whether their source literal was
            // parenthesized. Trusted replacements therefore never inherit directive semantics by
            // accident. Clear the remaining prologue from the end so every intermediate typed
            // tree remains grammar-valid.
            disable_directive_suffix(program, parent_id)?;
        }
    }

    program.replace_node(target, replacement)?;
    Ok(())
}

fn disable_directive_suffix(
    program: &mut TypedProgram,
    first_statement: NodeId,
) -> Result<(), TypedIrError> {
    let Some(link) = program
        .node(first_statement)
        .and_then(|statement| statement.parent())
    else {
        return Ok(());
    };
    let Some(list) = link.list() else {
        return Ok(());
    };
    if !matches!(
        link.role(),
        ChildRole::ProgramBody | ChildRole::FunctionStatements
    ) {
        return Ok(());
    }
    let items = program
        .list(list)
        .expect("validated directive list")
        .items();
    let Some(first) = items.iter().position(|item| *item == first_statement) else {
        return Ok(());
    };
    let suffix = items[first..]
        .iter()
        .copied()
        .take_while(|statement| {
            matches!(
                program
                    .node(*statement)
                    .expect("validated statement")
                    .data(),
                IrNodeData::ExpressionStatement {
                    directive: true,
                    ..
                }
            )
        })
        .collect::<Vec<_>>();
    for statement in suffix.into_iter().rev() {
        let IrNodeData::ExpressionStatement { expression, .. } =
            program.node(statement).expect("validated directive").data()
        else {
            unreachable!()
        };
        program.replace_node_data(
            statement,
            IrNodeData::ExpressionStatement {
                expression: *expression,
                directive: false,
            },
        )?;
    }
    Ok(())
}

fn shorthand_property(program: &TypedProgram, value: NodeId) -> Option<NodeId> {
    let link = program.node(value)?.parent()?;
    let property = link.parent();
    match program.node(property)?.data() {
        IrNodeData::ObjectProperty {
            value: candidate,
            shorthand: true,
            ..
        } if *candidate == value => Some(property),
        _ => None,
    }
}

fn replace_shorthand_value(
    program: &mut TypedProgram,
    property: NodeId,
    replacement: NodeId,
    anchor: Span,
) -> Result<(), TypedEditError> {
    let IrNodeData::ObjectProperty {
        key,
        value: _,
        kind,
        method,
        shorthand: true,
        computed: _,
        prototype_setter,
    } = program
        .node(property)
        .expect("validated shorthand property")
        .data()
        .clone()
    else {
        return Err(TypedEditError::UnsupportedContext {
            target: anchor,
            message: "expected an object-literal shorthand property".into(),
        });
    };

    let (new_key, computed) = if property_key_text(program, key).as_deref() == Some("__proto__") {
        let key_origin = program
            .node(key.value)
            .expect("validated property key")
            .origin();
        let value = program.append_detached_leaf(
            IrNodeData::StringLiteral {
                value: "__proto__".into(),
            },
            key_origin,
        )?;
        (
            IrPropertyKey {
                kind: PropertyKeyKind::Computed,
                value,
            },
            true,
        )
    } else {
        let value = program.clone_detached_subtree(key.value)?;
        (
            IrPropertyKey {
                kind: key.kind,
                value,
            },
            key.kind == PropertyKeyKind::Computed,
        )
    };

    let rewritten = program.append_detached_node_with(trusted_origin(anchor), |_| {
        Ok(IrNodeData::ObjectProperty {
            key: new_key,
            value: replacement,
            kind,
            method,
            shorthand: false,
            computed,
            prototype_setter,
        })
    })?;
    program.replace_node(property, rewritten)?;
    Ok(())
}

fn property_key_text(program: &TypedProgram, key: IrPropertyKey) -> Option<String> {
    match key.kind {
        PropertyKeyKind::Identifier | PropertyKeyKind::Private => {
            let IrNodeData::Name { name } = program.node(key.value)?.data() else {
                return None;
            };
            Some(program.name(*name)?.original().to_owned())
        }
        PropertyKeyKind::String => match program.node(key.value)?.data() {
            IrNodeData::StringLiteral { value } => Some(value.clone()),
            IrNodeData::Name { name } => Some(program.name(*name)?.original().to_owned()),
            _ => None,
        },
        PropertyKeyKind::Number | PropertyKeyKind::Computed => None,
    }
}

fn remove_statement(
    program: &mut TypedProgram,
    statement: NodeId,
    target: Span,
) -> Result<(), TypedEditError> {
    let Some(link) = program.node(statement).and_then(|node| node.parent()) else {
        return Err(TypedEditError::UnsupportedContext {
            target,
            message: "the program root cannot be removed as a statement".into(),
        });
    };
    if let Some(list) = link.list() {
        if !matches!(
            link.role(),
            ChildRole::ProgramBody
                | ChildRole::BlockBody
                | ChildRole::FunctionStatements
                | ChildRole::SwitchCaseBody
                | ChildRole::StaticBlockBody
        ) {
            return Err(TypedEditError::UnsupportedContext {
                target,
                message: format!("statement-list role {:?} cannot be removed", link.role()),
            });
        }
        let index = list_position(program, list, statement)?;
        program.splice_list(list, index..index + 1, &[])?;
        return Ok(());
    }

    match link.role() {
        ChildRole::Consequent
        | ChildRole::Alternate
        | ChildRole::LoopBody
        | ChildRole::LabeledBody
        | ChildRole::WithBody => {
            let empty =
                program.append_detached_leaf(IrNodeData::EmptyStatement, trusted_origin(target))?;
            program.replace_node(statement, empty)?;
            Ok(())
        }
        ChildRole::TryBlock | ChildRole::FinallyBlock | ChildRole::CatchBody => {
            if !matches!(
                program.node(statement).expect("validated block").data(),
                IrNodeData::Block { .. }
            ) {
                return Err(TypedEditError::UnsupportedContext {
                    target,
                    message: "try/catch/finally removal requires block syntax".into(),
                });
            }
            let empty = program.append_detached_node_with(trusted_origin(target), |builder| {
                let body = builder.list(ChildRole::BlockBody, [])?;
                Ok(IrNodeData::Block { body })
            })?;
            program.replace_node(statement, empty)?;
            Ok(())
        }
        _ => Err(TypedEditError::UnsupportedContext {
            target,
            message: format!("singular role {:?} cannot be removed", link.role()),
        }),
    }
}

fn list_position(
    program: &TypedProgram,
    list: ListId,
    node: NodeId,
) -> Result<usize, TypedEditError> {
    program
        .list(list)
        .and_then(|list| list.items().iter().position(|item| *item == node))
        .ok_or_else(|| TypedEditError::UnsupportedContext {
            target: program
                .node(node)
                .and_then(|node| match node.origin() {
                    IrOrigin::Source(span) => Some(span),
                    IrOrigin::Derived { anchor, .. } | IrOrigin::Synthetic { anchor, .. } => anchor,
                })
                .unwrap_or(Span::DUMMY),
            message: "parent list no longer contains the target occurrence".into(),
        })
}

#[derive(Clone, Copy)]
enum BindingSite {
    Variable {
        declaration: NodeId,
        declarator: NodeId,
    },
    Import {
        specifier: NodeId,
    },
    Function {
        declaration: NodeId,
    },
    Class {
        declaration: NodeId,
    },
}

fn apply_binding_removals(
    program: &mut TypedProgram,
    analysis: &TypedAnalysis,
    removals: &[ResolvedRemoval],
) -> Result<(), TypedEditError> {
    let mut variable_groups: HashMap<NodeId, Vec<(NodeId, Span)>> = HashMap::new();
    let mut imports = Vec::new();
    let mut functions = Vec::new();
    let mut classes = Vec::new();

    for removal in removals {
        match binding_site(program, removal.node, removal.target)? {
            BindingSite::Variable {
                declaration,
                declarator,
            } => variable_groups
                .entry(declaration)
                .or_default()
                .push((declarator, removal.target)),
            BindingSite::Import { specifier } => imports.push((specifier, removal.target)),
            BindingSite::Function { declaration } => {
                functions.push((declaration, removal.target));
            }
            BindingSite::Class { declaration } => classes.push((declaration, removal.target)),
        }
    }

    let mut variable_groups = variable_groups.into_iter().collect::<Vec<_>>();
    variable_groups.sort_unstable_by_key(|(declaration, _)| declaration.index());
    for (declaration, mut removed) in variable_groups {
        removed.sort_unstable_by_key(|(_, span)| (span.lo, span.hi));
        rewrite_variable_declaration(program, analysis, declaration, &removed)?;
    }

    imports.sort_unstable_by_key(|(_, span)| (span.lo, span.hi));
    for (specifier, target) in imports {
        let Some(link) = program.node(specifier).and_then(|node| node.parent()) else {
            return Err(TypedEditError::UnsupportedContext {
                target,
                message: "import specifier is detached".into(),
            });
        };
        if link.role() != ChildRole::ImportSpecifiers {
            return Err(TypedEditError::UnsupportedContext {
                target,
                message: "import binding is not owned by an import specifier list".into(),
            });
        }
        let list = link
            .list()
            .ok_or_else(|| TypedEditError::UnsupportedContext {
                target,
                message: "import binding has no structural specifier list".into(),
            })?;
        let index = list_position(program, list, specifier)?;
        program.splice_list(list, index..index + 1, &[])?;
    }

    functions.sort_unstable_by_key(|(_, span)| (span.lo, span.hi));
    for (declaration, target) in functions {
        remove_statement(program, declaration, target)?;
    }

    if let Some((_, target)) = classes.first() {
        return Err(TypedEditError::UnsupportedContext {
            target: *target,
            message: "class binding removal requires a class-evaluation preservation proof".into(),
        });
    }

    Ok(())
}

fn binding_site(
    program: &TypedProgram,
    binding: NodeId,
    target: Span,
) -> Result<BindingSite, TypedEditError> {
    let link = program
        .node(binding)
        .and_then(|node| node.parent())
        .ok_or_else(|| TypedEditError::UnsupportedContext {
            target,
            message: "binding occurrence is detached".into(),
        })?;
    match link.role() {
        ChildRole::Binding => {
            if !matches!(
                program.node(binding).expect("validated binding").data(),
                IrNodeData::Identifier { .. }
            ) {
                return Err(TypedEditError::UnsupportedContext {
                    target,
                    message: "destructuring binding removal must preserve pattern evaluation"
                        .into(),
                });
            }
            let declarator = link.parent();
            if !matches!(
                program.node(declarator).expect("validated declarator").data(),
                IrNodeData::VariableDeclarator { binding: candidate, .. } if *candidate == binding
            ) {
                return Err(TypedEditError::UnsupportedContext {
                    target,
                    message: "binding is not the root of a variable declarator".into(),
                });
            }
            let declaration = program
                .node(declarator)
                .and_then(|node| node.parent())
                .map(|link| link.parent())
                .ok_or_else(|| TypedEditError::UnsupportedContext {
                    target,
                    message: "variable declarator is detached".into(),
                })?;
            Ok(BindingSite::Variable {
                declaration,
                declarator,
            })
        }
        ChildRole::ImportLocal => Ok(BindingSite::Import {
            specifier: link.parent(),
        }),
        ChildRole::FunctionName => {
            let declaration = link.parent();
            if matches!(
                program
                    .node(declaration)
                    .expect("validated function")
                    .data(),
                IrNodeData::Function {
                    context: FunctionContext::Declaration,
                    ..
                }
            ) {
                Ok(BindingSite::Function { declaration })
            } else {
                Err(TypedEditError::UnsupportedContext {
                    target,
                    message: "only a function declaration binding can be removed".into(),
                })
            }
        }
        ChildRole::ClassName => {
            let declaration = link.parent();
            if matches!(
                program.node(declaration).expect("validated class").data(),
                IrNodeData::Class {
                    context: ClassContext::Declaration,
                    ..
                }
            ) {
                Ok(BindingSite::Class { declaration })
            } else {
                Err(TypedEditError::UnsupportedContext {
                    target,
                    message: "only a class declaration binding can be removed".into(),
                })
            }
        }
        _ => Err(TypedEditError::UnsupportedContext {
            target,
            message: format!("binding role {:?} cannot be removed", link.role()),
        }),
    }
}

fn rewrite_variable_declaration(
    program: &mut TypedProgram,
    analysis: &TypedAnalysis,
    declaration: NodeId,
    removed: &[(NodeId, Span)],
) -> Result<(), TypedEditError> {
    let (kind, declarations) = match program
        .node(declaration)
        .expect("validated variable declaration")
        .data()
    {
        IrNodeData::VariableDeclaration { kind, declarations } => (*kind, *declarations),
        _ => {
            return Err(TypedEditError::UnsupportedContext {
                target: removed[0].1,
                message: "binding parent is not a variable declaration".into(),
            });
        }
    };
    if kind.is_using() {
        return Err(TypedEditError::UnsupportedContext {
            target: removed[0].1,
            message: "using declarations own mandatory disposal semantics".into(),
        });
    }
    let parent = program
        .node(declaration)
        .and_then(|node| node.parent())
        .ok_or_else(|| TypedEditError::UnsupportedContext {
            target: removed[0].1,
            message: "variable declaration is detached".into(),
        })?;
    let Some(statement_list) = parent.list() else {
        return Err(TypedEditError::UnsupportedContext {
            target: removed[0].1,
            message: "for/export declaration bindings require a dedicated structural rewrite"
                .into(),
        });
    };
    if !matches!(
        parent.role(),
        ChildRole::ProgramBody
            | ChildRole::BlockBody
            | ChildRole::FunctionStatements
            | ChildRole::SwitchCaseBody
            | ChildRole::StaticBlockBody
    ) {
        return Err(TypedEditError::UnsupportedContext {
            target: removed[0].1,
            message: format!(
                "declaration list role {:?} is not a statement list",
                parent.role()
            ),
        });
    }

    let items = program
        .list(declarations)
        .expect("validated declarator list")
        .items()
        .to_vec();
    let removed_by_declarator = removed.iter().copied().collect::<HashMap<_, _>>();
    if removed_by_declarator.len() != removed.len()
        || removed_by_declarator
            .keys()
            .any(|declarator| !items.contains(declarator))
    {
        return Err(TypedEditError::UnsupportedContext {
            target: removed[0].1,
            message: "binding removals do not identify distinct declarators".into(),
        });
    }

    let mut replacements = Vec::new();
    let mut kept_run = Vec::new();
    for declarator in items {
        let Some(&target) = removed_by_declarator.get(&declarator) else {
            kept_run.push(declarator);
            continue;
        };
        flush_declaration_run(program, kind, &mut kept_run, target, &mut replacements)?;
        let initializer = match program
            .node(declarator)
            .expect("validated declarator")
            .data()
        {
            IrNodeData::VariableDeclarator { initializer, .. } => *initializer,
            _ => unreachable!("declaration list contains only declarators"),
        };
        if let Some(initializer) = initializer
            && initializer_requires_evaluation(program, analysis, initializer)?
        {
            let expression = program.clone_detached_subtree(initializer)?;
            let statement = program.append_detached_node_with(trusted_origin(target), |_| {
                Ok(IrNodeData::ExpressionStatement {
                    expression,
                    directive: false,
                })
            })?;
            replacements.push(statement);
        }
    }
    flush_declaration_run(
        program,
        kind,
        &mut kept_run,
        removed[0].1,
        &mut replacements,
    )?;

    let index = list_position(program, statement_list, declaration)?;
    program.splice_list(statement_list, index..index + 1, &replacements)?;
    Ok(())
}

fn flush_declaration_run(
    program: &mut TypedProgram,
    kind: VarKind,
    run: &mut Vec<NodeId>,
    anchor: Span,
    output: &mut Vec<NodeId>,
) -> Result<(), TypedIrError> {
    if run.is_empty() {
        return Ok(());
    }
    let mut declarations = Vec::with_capacity(run.len());
    for declarator in run.drain(..) {
        declarations.push(program.clone_detached_subtree(declarator)?);
    }
    let declaration = program.append_detached_node_with(trusted_origin(anchor), |builder| {
        let declarations = builder.list(ChildRole::DeclarationItems, declarations)?;
        Ok(IrNodeData::VariableDeclaration { kind, declarations })
    })?;
    output.push(declaration);
    Ok(())
}

fn initializer_requires_evaluation(
    program: &TypedProgram,
    analysis: &TypedAnalysis,
    initializer: NodeId,
) -> Result<bool, TypedEditError> {
    let Some(effect) = analysis.effect(initializer) else {
        return Ok(true);
    };
    if effect_is_observable(effect) {
        return Ok(true);
    }

    for node in program.preorder_validated()? {
        if node != initializer && !is_descendant(program, node, initializer) {
            continue;
        }
        if node != initializer && descendant_is_deferred(program, node, initializer) {
            continue;
        }
        match program
            .node(node)
            .expect("validated initializer node")
            .data()
        {
            IrNodeData::Identifier { name } => {
                let IrNodeData::Name { name: name_id } =
                    program.node(*name).expect("identifier name").data()
                else {
                    continue;
                };
                let name = program.name(*name_id).expect("validated name");
                if name.role() == NameRole::Reference
                    && analysis.read_is_definitely_initialized(*name_id) != Some(true)
                {
                    return Ok(true);
                }
            }
            IrNodeData::ThisExpression | IrNodeData::SuperExpression => return Ok(true),
            _ => {}
        }
    }
    Ok(false)
}

const fn effect_is_observable(effect: TypedEffectSummary) -> bool {
    effect.may_have_side_effects()
        || effect.may_throw()
        || effect.reads_member()
        || effect.writes_state()
        || effect.calls_unknown()
        || effect.accesses_unresolved()
        || effect.suspends()
}

fn descendant_is_deferred(program: &TypedProgram, mut node: NodeId, ancestor: NodeId) -> bool {
    while node != ancestor {
        let Some(link) = program.node(node).and_then(|node| node.parent()) else {
            return false;
        };
        node = link.parent();
        if matches!(
            program.node(node).expect("validated ancestor").data(),
            IrNodeData::Function { .. } | IrNodeData::ArrowFunction { .. }
        ) {
            return true;
        }
    }
    false
}

fn retain_live_dependencies(
    program: &TypedProgram,
    dependencies: &[TypedEditDependency],
) -> Result<Vec<TypedEditDependency>, TypedEditError> {
    if dependencies.is_empty() {
        return Ok(Vec::new());
    }
    let live_origins = program
        .preorder_validated()?
        .into_iter()
        .map(|node| program.node(node).expect("validated live node").origin())
        .collect::<Vec<_>>();
    Ok(dependencies
        .iter()
        .filter(|dependency| {
            origin_anchor(dependency.origin).is_none() || live_origins.contains(&dependency.origin)
        })
        .cloned()
        .collect())
}

const fn origin_anchor(origin: IrOrigin) -> Option<Span> {
    match origin {
        IrOrigin::Source(span) => Some(span),
        IrOrigin::Derived { anchor, .. } | IrOrigin::Synthetic { anchor, .. } => anchor,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wake_common::Interner;
    use wake_ecma_ast::{SourceType, Statement};

    fn lower(source: &str) -> TypedProgram {
        let interner = Interner::new();
        let parsed = wake_ecma_parser::parse(source, &interner, SourceType::Script);
        assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);
        parsed.module.with_ast(|program| {
            let semantic = wake_ecma_semantic::analyze(program);
            TypedProgram::lower(program, &interner, Some(&semantic)).unwrap()
        })
    }

    fn owner(source: &str) -> TypedExpressionOwner {
        let interner = Interner::new();
        let parsed = wake_ecma_parser::parse(source, &interner, SourceType::Script);
        assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);
        parsed.module.with_ast(|program| {
            let semantic = wake_ecma_semantic::analyze(program);
            let Statement::Expression(statement) = &program.body[0] else {
                panic!("owner fixture must contain one expression statement")
            };
            crate::typed_ir::lower_expression_owner(
                &statement.expression,
                &interner,
                Some(&semantic),
            )
            .unwrap()
        })
    }

    fn body(program: &TypedProgram) -> Vec<NodeId> {
        let IrNodeData::Program { body, .. } =
            program.node(program.root()).expect("program root").data()
        else {
            panic!("expected program root")
        };
        program.list(*body).unwrap().items().to_vec()
    }

    fn source_span(
        program: &TypedProgram,
        predicate: impl Fn(NodeId, &IrNodeData) -> bool,
    ) -> Span {
        program
            .preorder()
            .unwrap()
            .into_iter()
            .find_map(|node| {
                let record = program.node(node).unwrap();
                let IrOrigin::Source(span) = record.origin() else {
                    return None;
                };
                predicate(node, record.data()).then_some(span)
            })
            .expect("source-backed fixture node")
    }

    fn binding_span(program: &TypedProgram, spelling: &str) -> Span {
        source_span(program, |_, data| {
            let IrNodeData::Identifier { name } = data else {
                return false;
            };
            let IrNodeData::Name { name } = program.node(*name).unwrap().data() else {
                return false;
            };
            let name = program.name(*name).unwrap();
            name.role() == NameRole::Binding && name.original() == spelling
        })
    }

    fn call_name(program: &TypedProgram, expression: NodeId) -> Option<&str> {
        let IrNodeData::CallExpression { callee, .. } =
            program.node(expression).expect("expression node").data()
        else {
            return None;
        };
        let IrNodeData::Identifier { name } = program.node(*callee)?.data() else {
            return None;
        };
        let IrNodeData::Name { name } = program.node(*name)?.data() else {
            return None;
        };
        Some(program.name(*name)?.original())
    }

    fn declaration_initializer(program: &TypedProgram, statement: NodeId) -> NodeId {
        let IrNodeData::VariableDeclaration { declarations, .. } =
            program.node(statement).unwrap().data()
        else {
            panic!("expected declaration")
        };
        let declarator = program.list(*declarations).unwrap().items()[0];
        let IrNodeData::VariableDeclarator {
            initializer: Some(initializer),
            ..
        } = program.node(declarator).unwrap().data()
        else {
            panic!("expected initialized declarator")
        };
        *initializer
    }

    #[test]
    fn css_expression_edit_is_structural_and_reanchored() {
        let mut program = lower("const style=css`color:red`; ");
        let target = source_span(&program, |_, data| {
            matches!(data, IrNodeData::TaggedTemplateExpression { .. })
        });
        let input = TypedEditInput {
            expression_edits: vec![TypedTrustedExpressionEdit::new(
                target,
                owner("({className:'wake'})"),
            )],
            ..TypedEditInput::default()
        };
        let report = apply_typed_edits(&mut program, &input).unwrap();
        assert_eq!(report.change_count, 1);
        let initializer = declaration_initializer(&program, body(&program)[0]);
        assert!(matches!(
            program.node(initializer).unwrap().data(),
            IrNodeData::ObjectExpression { .. }
        ));
        assert_eq!(
            program.node(initializer).unwrap().origin(),
            trusted_origin(target)
        );
        assert!(program.preorder().unwrap().into_iter().all(|node| {
            if node == initializer || is_descendant(&program, node, initializer) {
                program.node(node).unwrap().origin() == trusted_origin(target)
            } else {
                true
            }
        }));
        assert!(!program.preorder().unwrap().into_iter().any(|node| {
            matches!(
                program.node(node).unwrap().data(),
                IrNodeData::TaggedTemplateExpression { .. }
            )
        }));
    }

    #[test]
    fn primitive_object_and_arrow_defines_replace_only_unresolved_reads() {
        let mut program = lower("const a=FLAG,b=CONFIG,c=FACTORY;");
        let input = TypedEditInput {
            defines: vec![
                TypedValidatedDefine::primitive("FLAG", ConstVal::Bool(false)),
                TypedValidatedDefine::expression("CONFIG", owner("({mode:'test'})")),
                TypedValidatedDefine::expression("FACTORY", owner("value=>value")),
            ],
            ..TypedEditInput::default()
        };
        let report = apply_typed_edits(&mut program, &input).unwrap();
        assert_eq!(report.change_count, 3);
        let variants = program
            .preorder()
            .unwrap()
            .into_iter()
            .filter_map(|node| match program.node(node).unwrap().data() {
                IrNodeData::BooleanLiteral { value: false } => Some("bool"),
                IrNodeData::ObjectExpression { .. } => Some("object"),
                IrNodeData::ArrowFunction { .. } => Some("arrow"),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(variants.contains(&"bool"));
        assert!(variants.contains(&"object"));
        assert!(variants.contains(&"arrow"));
    }

    #[test]
    fn dotted_define_matches_an_import_meta_root() {
        let mut program = lower("consume(import.meta.hot,import.meta.url);");
        let input = TypedEditInput {
            defines: vec![TypedValidatedDefine::primitive(
                "import.meta.hot",
                ConstVal::Bool(false),
            )],
            ..TypedEditInput::default()
        };
        let report = apply_typed_edits(&mut program, &input).unwrap();
        assert_eq!(report.change_count, 1);
        assert!(program.preorder().unwrap().into_iter().any(|node| matches!(
            program.node(node).unwrap().data(),
            IrNodeData::BooleanLiteral { value: false }
        )));
        assert_eq!(
            program
                .preorder()
                .unwrap()
                .into_iter()
                .filter(|node| matches!(
                    program.node(*node).unwrap().data(),
                    IrNodeData::MetaProperty { .. }
                ))
                .count(),
            1,
            "the unrelated import.meta.url chain must remain"
        );
    }

    #[test]
    fn shorthand_define_expands_value_without_renaming_the_key() {
        let mut program = lower("const value={DEBUG};");
        let input = TypedEditInput {
            defines: vec![TypedValidatedDefine::primitive(
                "DEBUG",
                ConstVal::Bool(false),
            )],
            ..TypedEditInput::default()
        };
        apply_typed_edits(&mut program, &input).unwrap();
        let property = program
            .preorder()
            .unwrap()
            .into_iter()
            .find(|node| {
                matches!(
                    program.node(*node).unwrap().data(),
                    IrNodeData::ObjectProperty { .. }
                )
            })
            .unwrap();
        let IrNodeData::ObjectProperty {
            key,
            value,
            shorthand,
            ..
        } = program.node(property).unwrap().data()
        else {
            unreachable!()
        };
        assert!(!shorthand);
        assert_eq!(property_key_text(&program, *key).as_deref(), Some("DEBUG"));
        assert!(matches!(
            program.node(*value).unwrap().data(),
            IrNodeData::BooleanLiteral { value: false }
        ));
    }

    #[test]
    fn define_never_rewrites_assignment_update_delete_or_for_of_targets() {
        let mut program = lower("FLAG=1;FLAG++;delete FLAG;for(FLAG of values){};consume(FLAG);");
        let input = TypedEditInput {
            defines: vec![TypedValidatedDefine::primitive(
                "FLAG",
                ConstVal::Bool(false),
            )],
            ..TypedEditInput::default()
        };
        let report = apply_typed_edits(&mut program, &input).unwrap();
        assert_eq!(report.change_count, 1);
        let remaining = program
            .preorder()
            .unwrap()
            .into_iter()
            .filter(|node| match program.node(*node).unwrap().data() {
                IrNodeData::Name { name } => program.name(*name).unwrap().original() == "FLAG",
                _ => false,
            })
            .count();
        assert_eq!(remaining, 4);
    }

    #[test]
    fn overlapping_edit_and_removal_fail_atomically() {
        let mut program = lower("const binding=compute();");
        let statement = source_span(&program, |_, data| {
            matches!(data, IrNodeData::VariableDeclaration { .. })
        });
        let expression = source_span(&program, |_, data| {
            matches!(data, IrNodeData::CallExpression { .. })
        });
        let before = program.fingerprint();
        let input = TypedEditInput {
            expression_edits: vec![TypedTrustedExpressionEdit::new(
                expression,
                owner("replacement()"),
            )],
            removals: vec![TypedTrustedRemoval::statement(statement)],
            ..TypedEditInput::default()
        };
        assert!(matches!(
            apply_typed_edits(&mut program, &input),
            Err(TypedEditError::Conflict { .. })
        ));
        assert_eq!(program.fingerprint(), before);
    }

    #[test]
    fn trusted_literal_edit_does_not_accidentally_create_a_directive() {
        let mut program = lower("\"use strict\";\"next\";body();");
        let target = source_span(
            &program,
            |_, data| matches!(data, IrNodeData::StringLiteral { value } if value == "use strict"),
        );
        let input = TypedEditInput {
            expression_edits: vec![TypedTrustedExpressionEdit::new(
                target,
                owner("'replacement'"),
            )],
            ..TypedEditInput::default()
        };
        apply_typed_edits(&mut program, &input).unwrap();
        let directives = body(&program)
            .into_iter()
            .filter(|statement| {
                matches!(
                    program.node(*statement).unwrap().data(),
                    IrNodeData::ExpressionStatement {
                        directive: true,
                        ..
                    }
                )
            })
            .count();
        assert_eq!(directives, 0);
    }

    #[test]
    fn trusted_statement_removal_does_not_delete_a_directive() {
        let mut program = lower("'use strict';boot();");
        let directive = source_span(&program, |_, data| {
            matches!(
                data,
                IrNodeData::ExpressionStatement {
                    directive: true,
                    ..
                }
            )
        });
        let input = TypedEditInput {
            removals: vec![TypedTrustedRemoval::statement(directive)],
            ..TypedEditInput::default()
        };
        let report = apply_typed_edits(&mut program, &input).unwrap();
        assert_eq!(report.change_count, 0);
        assert!(matches!(
            program.node(body(&program)[0]).unwrap().data(),
            IrNodeData::ExpressionStatement {
                directive: true,
                ..
            }
        ));
    }

    #[test]
    fn binding_removal_preserves_initializer_evaluation_order() {
        let mut program = lower("const a=first(),b=second(),c=third();after();");
        let target = binding_span(&program, "b");
        let input = TypedEditInput {
            removals: vec![TypedTrustedRemoval::binding(target)],
            ..TypedEditInput::default()
        };
        apply_typed_edits(&mut program, &input).unwrap();
        let statements = body(&program);
        assert_eq!(statements.len(), 4);
        assert_eq!(
            call_name(&program, declaration_initializer(&program, statements[0])),
            Some("first")
        );
        let IrNodeData::ExpressionStatement { expression, .. } =
            program.node(statements[1]).unwrap().data()
        else {
            panic!("removed initializer must become an expression statement")
        };
        assert_eq!(call_name(&program, *expression), Some("second"));
        assert_eq!(
            call_name(&program, declaration_initializer(&program, statements[2])),
            Some("third")
        );
        let IrNodeData::ExpressionStatement { expression, .. } =
            program.node(statements[3]).unwrap().data()
        else {
            panic!("expected trailing statement")
        };
        assert_eq!(call_name(&program, *expression), Some("after"));
    }

    #[test]
    fn dependency_report_uses_only_live_root_reachable_origins() {
        let mut program = lower("keep();drop();");
        let calls = program
            .preorder()
            .unwrap()
            .into_iter()
            .filter_map(|node| match program.node(node).unwrap().data() {
                IrNodeData::CallExpression { .. } => match program.node(node).unwrap().origin() {
                    IrOrigin::Source(span) => Some((node, span)),
                    _ => None,
                },
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(calls.len(), 2);
        let drop_statement = source_span(&program, |node, data| {
            let IrNodeData::ExpressionStatement { expression, .. } = data else {
                return false;
            };
            call_name(&program, *expression) == Some("drop") && node != calls[0].0
        });
        let input = TypedEditInput {
            removals: vec![TypedTrustedRemoval::statement(drop_statement)],
            dependencies: vec![
                TypedEditDependency {
                    specifier: "keep-dependency".into(),
                    origin: IrOrigin::Source(calls[0].1),
                },
                TypedEditDependency {
                    specifier: "drop-dependency".into(),
                    origin: IrOrigin::Source(calls[1].1),
                },
                TypedEditDependency {
                    specifier: "trusted-runtime".into(),
                    origin: IrOrigin::Synthetic {
                        anchor: None,
                        kind: SyntheticOriginKind::TrustedEdit,
                    },
                },
            ],
            ..TypedEditInput::default()
        };
        let report = apply_typed_edits(&mut program, &input).unwrap();
        assert_eq!(
            report
                .retained_dependencies
                .iter()
                .map(|dependency| dependency.specifier.as_str())
                .collect::<Vec<_>>(),
            vec!["keep-dependency", "trusted-runtime"]
        );
    }

    #[test]
    fn dependency_only_edit_input_keeps_the_owned_arena_in_place() {
        let mut program = lower("consume(value);");
        let call_origin = program
            .preorder()
            .unwrap()
            .into_iter()
            .find_map(|node| match program.node(node).unwrap().data() {
                IrNodeData::CallExpression { .. } => Some(program.node(node).unwrap().origin()),
                _ => None,
            })
            .expect("call origin");
        let input = TypedEditInput {
            dependencies: vec![TypedEditDependency {
                specifier: "dependency".into(),
                origin: call_origin,
            }],
            ..TypedEditInput::default()
        };
        let nodes = program.nodes().as_ptr();
        let lists = program.lists().as_ptr();
        let names = program.names().as_ptr();

        let report = apply_typed_edits(&mut program, &input).unwrap();

        assert_eq!(report.change_count, 0);
        assert_eq!(report.retained_dependencies, input.dependencies);
        assert_eq!(program.nodes().as_ptr(), nodes);
        assert_eq!(program.lists().as_ptr(), lists);
        assert_eq!(program.names().as_ptr(), names);
    }

    #[test]
    fn nonmatching_validated_defines_keep_the_owned_arena_in_place() {
        let mut program = lower("globalThis.registry=1;");
        let input = TypedEditInput {
            defines: vec![TypedValidatedDefine::primitive(
                "process.env.NODE_ENV",
                ConstVal::Str("production".into()),
            )],
            ..TypedEditInput::default()
        };
        let nodes = program.nodes().as_ptr();
        let lists = program.lists().as_ptr();
        let names = program.names().as_ptr();

        let report = apply_typed_edits(&mut program, &input).unwrap();

        assert_eq!(report.change_count, 0);
        assert!(report.retained_dependencies.is_empty());
        assert_eq!(program.nodes().as_ptr(), nodes);
        assert_eq!(program.lists().as_ptr(), lists);
        assert_eq!(program.names().as_ptr(), names);
    }
}
