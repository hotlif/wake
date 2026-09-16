//! Source-bound semantic inputs. Backend handles and flags never enter this contract.
mod assertions;
mod assignments;
mod awaits;
mod floating;
mod members;
mod misused;
mod returns;
mod switches;
mod templates;
use crate::{
    EffectiveRule, LintDiagnostic, LintError, LintOptions, LintResult, ModuleGraph, ModuleId,
    RuleLevel, SourceCallKind, SourceCallbackKind, TypeSource,
};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub(crate) const RULE: &str = "ts/no-unsafe-call";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TypeId(pub usize);

/// Categories needed by current rules, independent of a compiler's numeric type flags.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TypeKind {
    Any,
    Error,
    Unknown,
    Never,
    Void,
    String,
    Number,
    BigInt,
    Boolean,
    Null,
    Undefined,
    Symbol,
    Object,
    Parameter,
    Union,
    Intersection,
    #[default]
    Other,
}

/// Finite literal identity retained for exhaustiveness proofs. The backend never exposes its
/// numeric flags or handles; values are copied into this source-bound representation.
#[derive(Clone, Debug, PartialEq)]
pub enum TypeLiteral {
    String(wake_common::JsString),
    Number(f64),
    Boolean(bool),
    /// Canonical signed decimal text for an exact BigInt literal.
    BigInt(String),
    Null,
    Undefined,
}

impl TypeLiteral {
    /// Decode a backend's escaped string representation without accepting type names, truncated
    /// syntax or executable expressions. A complete single ECMAScript string literal is required.
    pub fn from_string_source(source: &str) -> Result<Self, LintError> {
        use wake_ecma_ast::{Expression, Statement};
        if source.len() > TypeSource::MAX_SOURCE_BYTES {
            return Err(failure("string literal source exceeds the source budget"));
        }
        let interner = wake_common::Interner::new();
        let parsed = wake_ecma_parser::parse(source, &interner, wake_ecma_ast::SourceType::Module);
        if parsed.has_errors() {
            return Err(failure("invalid string literal source"));
        }
        parsed.module.with_ast(|program| {
            if let [Statement::Expression(statement)] = program.body.as_slice()
                && let Expression::StringLiteral(literal) = statement.expression
                && literal.span.lo == 0
                && literal.span.hi as usize == source.len()
            {
                return Ok(Self::String(interner.resolve_js(literal.value)));
            }
            Err(failure("expected one complete string literal"))
        })
    }
}

#[derive(Clone, Debug, Default)]
pub struct TypeNode {
    pub kind: TypeKind,
    pub literal: Option<TypeLiteral>,
    /// Session-local identity for a non-standard enum declaration. The adapter must never put a
    /// compiler symbol handle or member spelling here; `None` is used for non-enum types.
    pub enum_identity: Option<u32>,
    /// Session-local identity for a `unique symbol` declaration. The adapter must never put a
    /// compiler symbol handle or spelling here.
    pub unique_symbol_identity: Option<u32>,
    /// Session-local identity for a class declaration. Interfaces and anonymous objects remain
    /// structural; the adapter never exposes a compiler symbol handle here.
    pub class_identity: Option<u32>,
    /// Identity of the program's standard-library Function symbol, never its spelling alone.
    pub standard_function: bool,
    /// Identity of the program's standard-library Promise symbol.
    pub standard_promise: bool,
    /// Identity of the program's standard-library PromiseLike symbol.
    pub standard_thenable: bool,
    pub standard_regexp: bool,
    pub constraint: Option<TypeId>,
    pub reference_target: Option<TypeId>,
    pub bases: Vec<TypeId>,
    pub parts: Vec<TypeId>,
    /// Type arguments of a generic reference, retained for recursive any/error proofs.
    pub type_arguments: Vec<TypeId>,
    /// Property value types of structural objects, retained for recursive any/error proofs.
    /// Property identity itself is intentionally not exposed to rule consumers.
    pub properties: Vec<TypeId>,
    /// Named structural properties used by conservative object-equivalence proofs. Each tuple is
    /// `(name, optional, value_type)`; names and optionalness are source data, while compiler
    /// handles and declaration addresses never enter this contract.
    pub structural_properties: Vec<(String, bool, TypeId)>,
    /// Readonly names from `structural_properties`. An empty set proves writability only when
    /// `structural_complete` is true; unknown permissions must leave the shape incomplete.
    pub readonly_properties: BTreeSet<String>,
    /// True only after the entire object shape was collected, including an empty shape. Call or
    /// construct signatures without parameter facts, nominal classes, opaque keys and unknown
    /// property permissions leave this false. Partial member lists must not prove equivalence.
    pub structural_complete: bool,
    /// Index signatures as `(key_type, value_type, readonly)`. These source-bound relations
    /// participate in object equivalence; only their values propagate assignment unsafety.
    pub index_signatures: Vec<(TypeId, TypeId, bool)>,
    /// Return types of callable object signatures, retained for recursive any/error proofs.
    /// Parameter identities and labels are intentionally not exposed to rule consumers.
    pub signature_returns: Vec<TypeId>,
}

#[derive(Clone, Debug)]
pub struct CallType {
    pub type_id: TypeId,
    /// Return categories of the callee's call signatures, including all overloads.
    pub returns: Vec<TypeKind>,
    /// Return type identities for Promise-aware project rules.
    pub return_types: Vec<TypeId>,
    pub construct_signatures: u32,
}

#[derive(Clone, Debug)]
pub struct CallArgumentType {
    /// Actual type/signatures of the argument expression when it is callable.
    pub actual: Option<CallType>,
    /// Contextual type/signatures expected at the argument position.
    pub contextual: Option<CallType>,
}

#[derive(Clone, Copy, Debug)]
pub struct MemberType {
    pub object: TypeId,
    pub property: Option<TypeId>,
}
impl CallType {
    pub fn new(type_id: TypeId) -> Self {
        Self {
            type_id,
            returns: Vec::new(),
            return_types: Vec::new(),
            construct_signatures: 0,
        }
    }
}

pub struct TypedSource {
    input: TypeSource,
    calls: Vec<Option<CallType>>,
    call_arguments: Option<Vec<Vec<CallArgumentType>>>,
    assignment_call_types: Option<Vec<CallArgumentType>>,
    return_call_types: Option<Vec<CallArgumentType>>,
    callback_types: Option<Vec<CallArgumentType>>,
    resolved: Vec<(TypeKind, bool)>,
    resolved_unsafe: Vec<Option<TypeKind>>,
    resolved_uncertain: Vec<bool>,
    resolved_promise: Vec<bool>,
    resolved_thenable: Vec<bool>,
    nodes: Vec<TypeNode>,
    order: Vec<usize>,
    relations: usize,
    templates: Option<Vec<Option<Vec<TypeId>>>>,
    members: Option<Vec<MemberType>>,
    expression_statements: Option<Vec<TypeId>>,
    awaits: Option<Vec<TypeId>>,
    assertions: Option<Vec<(TypeId, TypeId)>>,
    assignments: Option<Vec<TypeId>>,
    returns: Option<Vec<TypeId>>,
    conditions: Option<Vec<TypeId>>,
    switch_discriminants: Option<Vec<TypeId>>,
    switch_case_types: Option<Vec<Vec<TypeId>>>,
}

fn failure(message: &str) -> LintError {
    LintError::Analysis(format!("Type facts: {message}"))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct VoidReturnChecks {
    pub arguments: bool,
    pub attributes: bool,
    pub properties: bool,
    pub returns: bool,
    pub variables: bool,
}

pub(super) fn void_return_checks(value: &Value) -> VoidReturnChecks {
    let enabled = value.as_bool();
    let member = |name: &str| {
        enabled.unwrap_or_else(|| {
            value
                .as_object()
                .and_then(|object| object.get(name))
                .and_then(Value::as_bool)
                .unwrap_or(true)
        })
    };
    VoidReturnChecks {
        arguments: member("arguments"),
        attributes: member("attributes"),
        properties: member("properties"),
        returns: member("returns"),
        variables: member("variables"),
    }
}

impl TypedSource {
    pub const MAX_TYPES: usize = 50_000;
    pub const MAX_RELATIONS: usize = 200_000;

    /// Consumes the parsed source; each original call has exactly one source-bound fact,
    /// including dynamic imports whose expression type is their Promise return type.
    pub fn new(
        input: TypeSource,
        types: Vec<TypeNode>,
        calls: Vec<Option<CallType>>,
    ) -> Result<Self, LintError> {
        if types.len() > Self::MAX_TYPES || calls.len() != input.calls().len() {
            return Err(failure("type budget exceeded or incomplete call facts"));
        }
        let mut relations = 0usize;
        let mut count = |value: usize| -> Result<(), LintError> {
            relations = relations
                .checked_add(value)
                .filter(|value| *value <= Self::MAX_RELATIONS)
                .ok_or_else(|| failure("relation/signature budget exceeded"))?;
            Ok(())
        };
        for (site, call) in input.calls().iter().zip(&calls) {
            if call.is_none() {
                return Err(failure(match site.kind {
                    SourceCallKind::DynamicImport => "missing dynamic-import return fact",
                    _ => "missing call fact",
                }));
            }
            if let Some(call) = call {
                if call.type_id.0 >= types.len() {
                    return Err(failure("unknown callee type"));
                }
                count(1 + call.returns.len() + call.return_types.len())?;
                if call.return_types.iter().any(|id| id.0 >= types.len()) {
                    return Err(failure("unknown call return type"));
                }
                count(call.construct_signatures as usize)?;
            }
        }
        for node in &types {
            if let Some(literal) = &node.literal
                && !matches!(
                    (&node.kind, literal),
                    (TypeKind::String, TypeLiteral::String(_))
                        | (TypeKind::Number, TypeLiteral::Number(_))
                        | (TypeKind::Boolean, TypeLiteral::Boolean(_))
                        | (TypeKind::BigInt, TypeLiteral::BigInt(_))
                        | (TypeKind::Null, TypeLiteral::Null)
                        | (TypeKind::Undefined, TypeLiteral::Undefined)
                )
            {
                return Err(failure("literal fact does not match its type category"));
            }
            if node.enum_identity.is_some()
                && !matches!(node.kind, TypeKind::String | TypeKind::Number)
            {
                return Err(failure("enum identity requires a string or number type"));
            }
            if node.unique_symbol_identity.is_some() && node.kind != TypeKind::Symbol {
                return Err(failure("unique symbol identity requires a symbol type"));
            }
            if node.class_identity.is_some() && node.kind != TypeKind::Object {
                return Err(failure("class identity requires an object type"));
            }
            if (node.standard_function
                || node.standard_promise
                || node.standard_thenable
                || node.standard_regexp
                || node.reference_target.is_some()
                || !node.bases.is_empty())
                && node.kind != TypeKind::Object
                || node.constraint.is_some() && node.kind != TypeKind::Parameter
                || (!node.type_arguments.is_empty()
                    || !node.properties.is_empty()
                    || !node.structural_properties.is_empty()
                    || !node.readonly_properties.is_empty()
                    || node.structural_complete
                    || !node.index_signatures.is_empty()
                    || !node.signature_returns.is_empty())
                    && node.kind != TypeKind::Object
                || matches!(node.kind, TypeKind::Union | TypeKind::Intersection)
                    != !node.parts.is_empty()
            {
                return Err(failure("inconsistent type category and relations"));
            }
            if node.structural_complete
                && (node.class_identity.is_some() || !node.signature_returns.is_empty())
            {
                return Err(failure("incomplete callable or nominal object shape"));
            }
            let mut property_names = std::collections::HashSet::new();
            if node
                .structural_properties
                .iter()
                .any(|(name, _, _)| !property_names.insert(name))
            {
                return Err(failure("duplicate structural property name"));
            }
            if node
                .readonly_properties
                .iter()
                .any(|name| !property_names.contains(name))
            {
                return Err(failure("readonly property omitted from structural shape"));
            }
            for child in node
                .constraint
                .iter()
                .chain(node.reference_target.iter())
                .chain(&node.parts)
                .chain(&node.bases)
                .chain(&node.type_arguments)
                .chain(&node.properties)
                .chain(&node.signature_returns)
                .chain(node.structural_properties.iter().map(|(_, _, id)| id))
                .chain(
                    node.index_signatures
                        .iter()
                        .flat_map(|(key, value, _)| [key, value]),
                )
            {
                count(1)?;
                if child.0 >= types.len() {
                    return Err(failure("unknown related type"));
                }
            }
        }
        // Iterative postorder visits each non-property node/edge once and rejects cyclic
        // constraints/inheritance proofs explicitly. Property graphs are allowed to recurse;
        // their any/error summaries are completed by the monotone pass below.
        let mut state = vec![0u8; types.len()];
        let mut resolved = vec![(TypeKind::Other, false); types.len()];
        let mut resolved_unsafe = vec![None; types.len()];
        let mut resolved_uncertain = vec![false; types.len()];
        let mut resolved_promise = vec![false; types.len()];
        let mut resolved_thenable = vec![false; types.len()];
        let mut order = Vec::with_capacity(types.len());
        for root in 0..types.len() {
            if state[root] != 0 {
                continue;
            }
            let mut stack = vec![(root, false)];
            while let Some((index, finish)) = stack.pop() {
                let node = &types[index];
                if finish {
                    let value = if let Some(constraint) = node.constraint {
                        resolved[constraint.0]
                    } else {
                        let standard = node.standard_function
                            || node.reference_target.is_some_and(|id| resolved[id.0].1)
                            || match node.kind {
                                TypeKind::Union => node.parts.iter().all(|id| resolved[id.0].1),
                                TypeKind::Intersection => {
                                    node.parts.iter().any(|id| resolved[id.0].1)
                                }
                                _ => node.bases.iter().any(|id| resolved[id.0].1),
                            };
                        (node.kind, standard)
                    };
                    let promise = if let Some(constraint) = node.constraint {
                        resolved_promise[constraint.0]
                    } else {
                        (node.standard_promise || node.standard_thenable)
                            || node
                                .reference_target
                                .is_some_and(|id| resolved_promise[id.0])
                            || match node.kind {
                                TypeKind::Union => {
                                    node.parts.iter().all(|id| resolved_promise[id.0])
                                }
                                TypeKind::Intersection => {
                                    node.parts.iter().any(|id| resolved_promise[id.0])
                                }
                                _ => node.bases.iter().any(|id| resolved_promise[id.0]),
                            }
                    };
                    let thenable = if let Some(constraint) = node.constraint {
                        resolved_thenable[constraint.0]
                    } else {
                        (node.standard_promise || node.standard_thenable)
                            || node
                                .reference_target
                                .is_some_and(|id| resolved_thenable[id.0])
                            || match node.kind {
                                TypeKind::Union => {
                                    node.parts.iter().all(|id| resolved_thenable[id.0])
                                }
                                TypeKind::Intersection => {
                                    node.parts.iter().any(|id| resolved_thenable[id.0])
                                }
                                _ => node.bases.iter().any(|id| resolved_thenable[id.0]),
                            }
                    };
                    let unsafe_kind = match node.kind {
                        TypeKind::Error => Some(TypeKind::Error),
                        TypeKind::Any => Some(TypeKind::Any),
                        _ => node
                            .reference_target
                            .iter()
                            .chain(&node.bases)
                            .chain(&node.parts)
                            .chain(&node.type_arguments)
                            .chain(node.structural_properties.iter().map(|(_, _, id)| id))
                            .chain(&node.signature_returns)
                            .filter_map(|id| resolved_unsafe[id.0])
                            .min_by_key(|kind| match kind {
                                TypeKind::Error => 0,
                                TypeKind::Any => 1,
                                _ => 2,
                            }),
                    };
                    let uncertain = matches!(
                        node.kind,
                        TypeKind::Any | TypeKind::Unknown | TypeKind::Error
                    ) || node
                        .constraint
                        .iter()
                        .chain(node.reference_target.iter())
                        .chain(&node.bases)
                        .chain(&node.parts)
                        .chain(&node.type_arguments)
                        .any(|id| resolved_uncertain[id.0]);
                    resolved[index] = value;
                    resolved_promise[index] = promise;
                    resolved_thenable[index] = thenable;
                    resolved_unsafe[index] = unsafe_kind;
                    resolved_uncertain[index] = uncertain;
                    state[index] = 2;
                    order.push(index);
                } else {
                    if state[index] == 2 {
                        continue;
                    }
                    if state[index] == 1 {
                        return Err(failure("cyclic constraint or inheritance proof"));
                    }
                    state[index] = 1;
                    stack.push((index, true));
                    stack.extend(
                        node.constraint
                            .iter()
                            .chain(node.reference_target.iter())
                            .chain(&node.parts)
                            .chain(&node.bases)
                            .chain(&node.type_arguments)
                            .map(|id| (id.0, false)),
                    );
                }
            }
        }
        // Property values and callable signature returns may form recursive structural types. A
        // least fixed point preserves every reachable any/error proof without asking the
        // postorder walk to invent an order for a recursive object graph. The summaries only move
        // toward a more unsafe result, so the bounded type graph cannot oscillate.
        loop {
            let mut changed = false;
            for index in 0..types.len() {
                let node = &types[index];
                if !matches!(node.kind, TypeKind::Any | TypeKind::Error) {
                    let candidate = node
                        .constraint
                        .iter()
                        .chain(node.reference_target.iter())
                        .chain(&node.parts)
                        .chain(&node.bases)
                        .chain(&node.type_arguments)
                        .chain(&node.properties)
                        .chain(node.structural_properties.iter().map(|(_, _, id)| id))
                        .chain(node.index_signatures.iter().map(|(_, value, _)| value))
                        .chain(&node.signature_returns)
                        .filter_map(|id| resolved_unsafe[id.0])
                        .min_by_key(|kind| match kind {
                            TypeKind::Error => 0,
                            TypeKind::Any => 1,
                            _ => 2,
                        });
                    if let Some(candidate) = candidate
                        && resolved_unsafe[index].is_none_or(|current| {
                            let rank = |kind| match kind {
                                TypeKind::Error => 0,
                                TypeKind::Any => 1,
                                _ => 2,
                            };
                            rank(candidate) < rank(current)
                        })
                    {
                        resolved_unsafe[index] = Some(candidate);
                        changed = true;
                    }
                }
                if !resolved_uncertain[index]
                    && node
                        .constraint
                        .iter()
                        .chain(node.reference_target.iter())
                        .chain(&node.parts)
                        .chain(&node.bases)
                        .chain(&node.type_arguments)
                        .chain(&node.properties)
                        .chain(node.structural_properties.iter().map(|(_, _, id)| id))
                        .chain(
                            node.index_signatures
                                .iter()
                                .flat_map(|(key, value, _)| [key, value]),
                        )
                        .any(|id| resolved_uncertain[id.0])
                {
                    resolved_uncertain[index] = true;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        Ok(Self {
            input,
            calls,
            call_arguments: None,
            assignment_call_types: None,
            return_call_types: None,
            callback_types: None,
            resolved,
            resolved_unsafe,
            resolved_uncertain,
            resolved_promise,
            resolved_thenable,
            nodes: types,
            order,
            relations,
            templates: None,
            members: None,
            expression_statements: None,
            awaits: None,
            assertions: None,
            assignments: None,
            returns: None,
            conditions: None,
            switch_discriminants: None,
            switch_case_types: None,
        })
    }

    /// Bind untagged interpolation types by the core-owned template and expression order.
    /// Tagged templates carry None because their tag defines argument interpretation.
    pub fn with_template_types(
        mut self,
        templates: Vec<Option<Vec<TypeId>>>,
    ) -> Result<Self, LintError> {
        if templates.len() != self.input.templates().len() {
            return Err(failure("incomplete template facts"));
        }
        let mut relations = self.relations;
        for (site, facts) in self.input.templates().iter().zip(&templates) {
            match (site.tagged, facts) {
                (true, None) => {}
                (false, Some(types)) if types.len() == site.expressions.len() => {
                    relations = relations
                        .checked_add(types.len())
                        .filter(|value| *value <= Self::MAX_RELATIONS)
                        .ok_or_else(|| failure("template relation budget exceeded"))?;
                    if types.iter().any(|id| id.0 >= self.nodes.len()) {
                        return Err(failure("unknown template expression type"));
                    }
                }
                _ => return Err(failure("template facts differ from the original grammar")),
            }
        }
        self.templates = Some(templates);
        self.fact_budget()?;
        Ok(self)
    }

    pub fn with_member_types(mut self, members: Vec<MemberType>) -> Result<Self, LintError> {
        if members.len() != self.input.members().len() {
            return Err(failure("incomplete member facts"));
        }
        for (site, facts) in self.input.members().iter().zip(&members) {
            if (site.kind == crate::SourceMemberKind::Computed) != facts.property.is_some()
                || facts.object.0 >= self.nodes.len()
                || facts.property.is_some_and(|id| id.0 >= self.nodes.len())
            {
                return Err(failure("invalid receiver or computed member type"));
            }
        }
        self.members = Some(members);
        self.fact_budget()?;
        Ok(self)
    }

    pub fn with_await_types(mut self, awaits: Vec<TypeId>) -> Result<Self, LintError> {
        if awaits.len() != self.input.awaits().len()
            || awaits.iter().any(|id| id.0 >= self.nodes.len())
        {
            return Err(failure("incomplete await operand type facts"));
        }
        self.awaits = Some(awaits);
        self.fact_budget()?;
        Ok(self)
    }

    pub fn with_call_argument_types(
        mut self,
        arguments: Vec<Vec<CallArgumentType>>,
    ) -> Result<Self, LintError> {
        if arguments.len() != self.input.calls().len()
            || arguments
                .iter()
                .zip(self.input.calls())
                .any(|(arguments, site)| arguments.len() != site.arguments.len())
        {
            return Err(failure("incomplete call argument type facts"));
        }
        for arguments in &arguments {
            for argument in arguments {
                for call in [&argument.actual, &argument.contextual]
                    .into_iter()
                    .flatten()
                {
                    if call.type_id.0 >= self.nodes.len()
                        || call.return_types.iter().any(|id| id.0 >= self.nodes.len())
                    {
                        return Err(failure("unknown call argument type"));
                    }
                }
            }
        }
        self.call_arguments = Some(arguments);
        self.fact_budget()?;
        Ok(self)
    }

    pub fn with_assignment_call_types(
        mut self,
        assignments: Vec<CallArgumentType>,
    ) -> Result<Self, LintError> {
        if assignments.len() != self.input.assignments().len() {
            return Err(failure("incomplete assignment callback type facts"));
        }
        for assignment in &assignments {
            for call in [&assignment.actual, &assignment.contextual]
                .into_iter()
                .flatten()
            {
                if call.type_id.0 >= self.nodes.len()
                    || call.return_types.iter().any(|id| id.0 >= self.nodes.len())
                {
                    return Err(failure("unknown assignment callback type"));
                }
            }
        }
        self.assignment_call_types = Some(assignments);
        self.fact_budget()?;
        Ok(self)
    }

    pub fn with_return_call_types(
        mut self,
        returns: Vec<CallArgumentType>,
    ) -> Result<Self, LintError> {
        if returns.len() != self.input.returns().len() {
            return Err(failure("incomplete return callback type facts"));
        }
        for returned in &returns {
            for call in [&returned.actual, &returned.contextual]
                .into_iter()
                .flatten()
            {
                if call.type_id.0 >= self.nodes.len()
                    || call.return_types.iter().any(|id| id.0 >= self.nodes.len())
                {
                    return Err(failure("unknown return callback type"));
                }
            }
        }
        self.return_call_types = Some(returns);
        self.fact_budget()?;
        Ok(self)
    }

    pub fn with_callback_types(
        mut self,
        callbacks: Vec<CallArgumentType>,
    ) -> Result<Self, LintError> {
        if callbacks.len() != self.input.callbacks().len() {
            return Err(failure("incomplete property callback type facts"));
        }
        for callback in &callbacks {
            for call in [&callback.actual, &callback.contextual]
                .into_iter()
                .flatten()
            {
                if call.type_id.0 >= self.nodes.len()
                    || call.return_types.iter().any(|id| id.0 >= self.nodes.len())
                {
                    return Err(failure("unknown property callback type"));
                }
            }
        }
        self.callback_types = Some(callbacks);
        self.fact_budget()?;
        Ok(self)
    }

    pub fn with_expression_statement_types(
        mut self,
        expressions: Vec<TypeId>,
    ) -> Result<Self, LintError> {
        if expressions.len() != self.input.expression_statements().len()
            || expressions.iter().any(|id| id.0 >= self.nodes.len())
        {
            return Err(failure("incomplete expression statement type facts"));
        }
        self.expression_statements = Some(expressions);
        self.fact_budget()?;
        Ok(self)
    }

    pub fn with_assertion_types(
        mut self,
        assertions: Vec<(TypeId, TypeId)>,
    ) -> Result<Self, LintError> {
        if assertions.len() != self.input.assertions().len()
            || assertions.iter().any(|(operand, asserted)| {
                operand.0 >= self.nodes.len() || asserted.0 >= self.nodes.len()
            })
        {
            return Err(failure("incomplete type assertion facts"));
        }
        self.assertions = Some(assertions);
        self.fact_budget()?;
        Ok(self)
    }

    pub fn with_assignment_types(mut self, assignments: Vec<TypeId>) -> Result<Self, LintError> {
        if assignments.len() != self.input.assignments().len()
            || assignments.iter().any(|id| id.0 >= self.nodes.len())
        {
            return Err(failure("incomplete assignment value type facts"));
        }
        self.assignments = Some(assignments);
        self.fact_budget()?;
        Ok(self)
    }

    pub fn with_return_types(mut self, returns: Vec<TypeId>) -> Result<Self, LintError> {
        if returns.len() != self.input.returns().len()
            || returns.iter().any(|id| id.0 >= self.nodes.len())
        {
            return Err(failure("incomplete return value type facts"));
        }
        self.returns = Some(returns);
        self.fact_budget()?;
        Ok(self)
    }

    pub fn with_condition_types(mut self, conditions: Vec<TypeId>) -> Result<Self, LintError> {
        if conditions.len() != self.input.conditions().len()
            || conditions.iter().any(|id| id.0 >= self.nodes.len())
        {
            return Err(failure("incomplete condition type facts"));
        }
        self.conditions = Some(conditions);
        self.fact_budget()?;
        Ok(self)
    }

    pub fn with_switch_types(mut self, switches: Vec<TypeId>) -> Result<Self, LintError> {
        if switches.len() != self.input.switches().len()
            || switches.iter().any(|id| id.0 >= self.nodes.len())
        {
            return Err(failure("incomplete switch discriminant type facts"));
        }
        self.switch_discriminants = Some(switches);
        self.fact_budget()?;
        Ok(self)
    }

    pub fn with_switch_case_types(mut self, cases: Vec<Vec<TypeId>>) -> Result<Self, LintError> {
        if cases.len() != self.input.switches().len()
            || cases
                .iter()
                .zip(self.input.switches())
                .any(|(cases, site)| {
                    cases.len() != site.case_spans.len()
                        || site.case_clause_spans.len() != site.case_spans.len()
                        || cases.iter().any(|id| id.0 >= self.nodes.len())
                })
        {
            return Err(failure("incomplete switch case types"));
        }
        self.switch_case_types = Some(cases);
        self.fact_budget()?;
        Ok(self)
    }

    fn fact_budget(&self) -> Result<(), LintError> {
        let mut count = self.relations;
        for amount in self
            .templates
            .iter()
            .flatten()
            .flatten()
            .map(Vec::len)
            .chain(
                self.members
                    .iter()
                    .flatten()
                    .map(|member| 1 + usize::from(member.property.is_some())),
            )
            .chain(self.awaits.iter().map(Vec::len))
            .chain(self.call_arguments.iter().flatten().map(|arguments| {
                arguments
                    .iter()
                    .map(|argument| {
                        [&argument.actual, &argument.contextual]
                            .into_iter()
                            .flatten()
                            .map(|call| {
                                1 + call.returns.len()
                                    + call.return_types.len()
                                    + call.construct_signatures as usize
                            })
                            .sum::<usize>()
                    })
                    .sum::<usize>()
            }))
            .chain(self.assignment_call_types.iter().map(|assignments| {
                assignments
                    .iter()
                    .flat_map(|assignment| [&assignment.actual, &assignment.contextual])
                    .flatten()
                    .map(|call| {
                        1 + call.returns.len()
                            + call.return_types.len()
                            + call.construct_signatures as usize
                    })
                    .sum::<usize>()
            }))
            .chain(self.return_call_types.iter().map(|returns| {
                returns
                    .iter()
                    .flat_map(|returned| [&returned.actual, &returned.contextual])
                    .flatten()
                    .map(|call| {
                        1 + call.returns.len()
                            + call.return_types.len()
                            + call.construct_signatures as usize
                    })
                    .sum::<usize>()
            }))
            .chain(self.callback_types.iter().map(|callbacks| {
                callbacks
                    .iter()
                    .flat_map(|callback| [&callback.actual, &callback.contextual])
                    .flatten()
                    .map(|call| {
                        1 + call.returns.len()
                            + call.return_types.len()
                            + call.construct_signatures as usize
                    })
                    .sum::<usize>()
            }))
            .chain(self.expression_statements.iter().map(Vec::len))
            .chain(self.assignments.iter().map(Vec::len))
            .chain(self.returns.iter().map(Vec::len))
            .chain(self.conditions.iter().map(Vec::len))
            .chain(self.switch_discriminants.iter().map(Vec::len))
            .chain(self.switch_case_types.iter().flatten().map(Vec::len))
        {
            count = count
                .checked_add(amount)
                .filter(|value| *value <= Self::MAX_RELATIONS)
                .ok_or_else(|| failure("combined source fact budget exceeded"))?;
        }
        let assertion_count = self
            .assertions
            .as_ref()
            .map_or(0, |values| values.len().saturating_mul(2));
        count
            .checked_add(assertion_count)
            .filter(|value| *value <= Self::MAX_RELATIONS)
            .ok_or_else(|| failure("combined source fact budget exceeded"))?;
        Ok(())
    }

    pub(crate) fn validate(
        &self,
        configuration: &BTreeMap<String, EffectiveRule>,
    ) -> Result<(), LintError> {
        if configuration[members::RULE].configuration.level != RuleLevel::Off
            && self.members.is_none()
        {
            return Err(failure(
                "enabled member rule requires complete receiver and key types",
            ));
        }
        if configuration[templates::RULE].configuration.level != RuleLevel::Off
            && self.templates.is_none()
        {
            return Err(failure(
                "enabled template rule requires complete interpolation types",
            ));
        }
        if configuration[awaits::RULE].configuration.level != RuleLevel::Off
            && self.awaits.is_none()
        {
            return Err(failure(
                "enabled await rule requires complete operand types",
            ));
        }
        if configuration[floating::RULE].configuration.level != RuleLevel::Off
            && self.expression_statements.is_none()
        {
            return Err(failure(
                "enabled floating-promise rule requires complete expression statement types",
            ));
        }
        if configuration[assertions::RULE].configuration.level != RuleLevel::Off
            && self.assertions.is_none()
        {
            return Err(failure(
                "enabled assertion rule requires complete operand and asserted types",
            ));
        }
        if configuration[assignments::RULE].configuration.level != RuleLevel::Off
            && self.assignments.is_none()
        {
            return Err(failure(
                "enabled assignment rule requires complete assigned value types",
            ));
        }
        if configuration[returns::RULE].configuration.level != RuleLevel::Off
            && self.returns.is_none()
        {
            return Err(failure(
                "enabled return rule requires complete returned value types",
            ));
        }
        let misused_configuration = &configuration[misused::RULE].configuration;
        let checks_conditionals = misused_configuration.options["checks_conditionals"]
            .as_bool()
            .expect("validated checks_conditionals option");
        let checks_void_return =
            void_return_checks(&misused_configuration.options["checks_void_return"]);
        if misused_configuration.level != RuleLevel::Off
            && checks_conditionals
            && self.conditions.is_none()
        {
            return Err(failure(
                "enabled misused-promise rule requires complete condition types",
            ));
        }
        if misused_configuration.level != RuleLevel::Off
            && checks_void_return.arguments
            && self
                .input
                .calls()
                .iter()
                .any(|site| !site.arguments.is_empty())
            && self.call_arguments.is_none()
        {
            return Err(failure(
                "enabled misused-promise rule requires complete call argument types",
            ));
        }
        if misused_configuration.level != RuleLevel::Off
            && checks_void_return.variables
            && !self.input.assignments().is_empty()
            && self.assignment_call_types.is_none()
        {
            return Err(failure(
                "enabled misused-promise rule requires complete assignment callback types",
            ));
        }
        if misused_configuration.level != RuleLevel::Off
            && checks_void_return.returns
            && !self.input.returns().is_empty()
            && self.return_call_types.is_none()
        {
            return Err(failure(
                "enabled misused-promise rule requires complete return callback types",
            ));
        }
        if misused_configuration.level != RuleLevel::Off
            && self.input.callbacks().iter().any(|site| match site.kind {
                SourceCallbackKind::ObjectProperty => checks_void_return.properties,
                SourceCallbackKind::JsxAttribute => checks_void_return.attributes,
            })
            && self.callback_types.is_none()
        {
            return Err(failure(
                "enabled misused-promise rule requires complete property callback types",
            ));
        }
        if configuration[switches::RULE].configuration.level != RuleLevel::Off
            && self.switch_discriminants.is_none()
        {
            return Err(failure(
                "enabled switch rule requires complete discriminant types",
            ));
        }
        if configuration[floating::RULE].configuration.level != RuleLevel::Off {
            for statement in self.input.expression_statements() {
                if let Some((_, Some(call))) = self
                    .input
                    .calls()
                    .iter()
                    .zip(&self.calls)
                    .find(|(site, _)| site.span == statement.expression)
                    && call.return_types.is_empty()
                {
                    return Err(failure(
                        "enabled floating-promise rule requires complete return types",
                    ));
                }
            }
        }
        Ok(())
    }

    pub fn input(&self) -> &TypeSource {
        &self.input
    }

    pub fn lint(&self, options: &LintOptions) -> Result<LintResult, LintError> {
        crate::lint_text_inner(
            self.input.source(),
            self.input.source_type(),
            options,
            None,
            Some(self),
        )
    }

    pub fn lint_with_module(
        &self,
        graph: &ModuleGraph,
        id: ModuleId,
        options: &LintOptions,
    ) -> Result<LintResult, LintError> {
        let file = graph.file(id)?;
        if file.path() != self.input.path()
            || file.source().as_ref() != self.input.source()
            || file.source_type() != self.input.source_type()
        {
            return Err(failure(
                "module graph and type facts describe different source identities",
            ));
        }
        crate::lint_text_inner(
            self.input.source(),
            self.input.source_type(),
            options,
            Some((graph, id)),
            Some(self),
        )
    }

    pub(crate) fn check(
        &self,
        configuration: &BTreeMap<String, EffectiveRule>,
        diagnostics: &mut Vec<LintDiagnostic>,
    ) {
        templates::check(self, &configuration[templates::RULE], diagnostics);
        members::check(self, &configuration[members::RULE], diagnostics);
        awaits::check(self, &configuration[awaits::RULE], diagnostics);
        assertions::check(self, &configuration[assertions::RULE], diagnostics);
        assignments::check(self, &configuration[assignments::RULE], diagnostics);
        returns::check(self, &configuration[returns::RULE], diagnostics);
        misused::check(self, &configuration[misused::RULE], diagnostics);
        switches::check(self, &configuration[switches::RULE], diagnostics);
        floating::check(self, &configuration[floating::RULE], diagnostics);
        let level = configuration[RULE].configuration.level;
        if level == RuleLevel::Off {
            return;
        }
        for (site, call) in self.input.calls().iter().zip(&self.calls) {
            let Some(call) = call else {
                continue;
            };
            let (kind, standard) = self.resolved[call.type_id.0];
            let any = matches!(kind, TypeKind::Any | TypeKind::Error);
            let invalid_function = standard
                && call.construct_signatures == 0
                && if site.kind == SourceCallKind::Construct {
                    !call.returns.iter().any(|kind| *kind != TypeKind::Void)
                } else {
                    call.returns.is_empty()
                };
            if !any && !invalid_function {
                continue;
            }
            let (action, unsafe_id, error_id) = match site.kind {
                SourceCallKind::Call => ("call", "unsafeCall", "errorCall"),
                SourceCallKind::Construct => ("construction", "unsafeNew", "errorNew"),
                SourceCallKind::TaggedTemplate => ("template tag", "unsafeTag", "errorTag"),
                SourceCallKind::DynamicImport => continue,
            };
            let span = if site.kind == SourceCallKind::Construct {
                site.span
            } else {
                site.head
            };
            let description = if kind == TypeKind::Error {
                "an unresolved type"
            } else if any {
                "any"
            } else {
                "Function without a usable signature"
            };
            diagnostics.push(LintDiagnostic {
                rule_id: RULE.into(),
                level,
                message_id: if kind == TypeKind::Error {
                    error_id
                } else {
                    unsafe_id
                }
                .into(),
                message: format!("Unsafe {action} of {description}."),
                start: span.lo,
                end: span.hi,
                fix: None,
            });
        }
    }
}
