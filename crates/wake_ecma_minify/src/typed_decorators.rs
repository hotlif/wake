//! Stage-3 decorator lowering over the owned typed IR.
//!
//! Decorated classes and their runtime helpers are materialized as ordinary typed-IR syntax.
//! Code generation therefore has no decorator-specific semantic templates and final analysis /
//! mangling sees every generated binding and reference.

use std::collections::HashSet;
use std::error::Error;
use std::fmt;

use wake_ecma_ast::{
    BinaryOperator, LogicalOperator, MethodKind, PropertyKind, UnaryOperator, UpdateOperator,
    VarKind,
};
use wake_ecma_semantic::{DeclKind, SymbolId};

use crate::typed_ir::{
    ArrowBodyKind, ChildRole, ClassContext, DerivedOriginKind, ExportDefaultValueKind,
    FunctionContext, IrNodeData, IrOrigin, IrPropertyKey, ListId, NameRole, NameSyntax, NodeId,
    PropertyKeyKind, TypedIrError, TypedProgram,
};
use crate::typed_lowering::{Binding, HELPER_ORIGIN, SyntheticFactory};

/// Names and counts produced by decorator lowering.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DecoratorLoweringReport {
    /// Classes whose decorator syntax was fully replaced by ordinary runtime IR.
    pub decorated_classes: usize,
    pub es_decorate_name: Option<String>,
    pub run_initializers_name: Option<String>,
}

/// Build-facing failure raised while converting Stage-3 decorator syntax into runtime IR.
///
/// The pass is transactional: when this error is returned, the caller's [`TypedProgram`] is
/// unchanged. Keeping a decorator-specific type lets the build pipeline name the failing phase
/// without treating a rejected decorator invariant as an arbitrary IR mutation failure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DecoratorLoweringError {
    /// The input or generated tree violated a typed-IR invariant.
    InvalidIr(TypedIrError),
    /// A source shape cannot be lowered without changing observable decorator semantics.
    ///
    /// This is deliberately a build failure instead of a request to re-emit decorator syntax:
    /// production output must never depend on the host runtime implementing decorators.
    Unsupported {
        /// Stable source node that prevented lowering.
        node: NodeId,
        /// Specific semantic invariant the lowering cannot currently preserve.
        reason: &'static str,
    },
}

impl fmt::Display for DecoratorLoweringError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidIr(error) => write!(formatter, "decorator typed-IR failure: {error}"),
            Self::Unsupported { node, reason } => {
                write!(formatter, "unsupported decorated class {node:?}: {reason}")
            }
        }
    }
}

impl Error for DecoratorLoweringError {}

impl From<TypedIrError> for DecoratorLoweringError {
    fn from(error: TypedIrError) -> Self {
        Self::InvalidIr(error)
    }
}

/// Atomically lower every decorated class and materialize the two Stage-3 runtime helpers.
pub fn materialize_decorators(
    program: &mut TypedProgram,
) -> Result<DecoratorLoweringReport, DecoratorLoweringError> {
    if !program
        .nodes()
        .iter()
        .any(|node| !node.is_tombstone() && class_has_decorators(program, node.id()))
    {
        return Ok(DecoratorLoweringReport::default());
    }
    let mut working = program.clone();
    let report = materialize_decorators_inner(&mut working)?;
    *program = working;
    Ok(report)
}

fn materialize_decorators_inner(
    program: &mut TypedProgram,
) -> Result<DecoratorLoweringReport, DecoratorLoweringError> {
    let mut decorated = program
        .preorder_validated()?
        .into_iter()
        .filter(|&node| class_has_decorators(program, node))
        .collect::<Vec<_>>();
    if decorated.is_empty() {
        return Ok(DecoratorLoweringReport::default());
    }
    decorated.reverse();

    for &class in &decorated {
        if let Some(reason) = unsupported_lowering_reason(program, class) {
            return Err(DecoratorLoweringError::Unsupported {
                node: class,
                reason,
            });
        }
    }

    let mut names = GeneratedNames::new(program);
    let run_initializers = names.binding(program, "__runInitializers", DeclKind::Var)?;
    let es_decorate = names.binding(program, "__esDecorate", DeclKind::Var)?;

    let mut lowered = 0;
    for class in decorated {
        if program.node(class).is_some_and(|node| !node.is_tombstone()) {
            lower_one_class(program, class, &es_decorate, &run_initializers, &mut names)?;
            lowered += 1;
        }
    }

    let helpers = {
        let factory = SyntheticFactory::new(program);
        vec![
            build_run_initializers(&factory, &run_initializers, &mut names)?,
            build_es_decorate(&factory, &es_decorate, &mut names)?,
        ]
    };
    insert_after_program_directives(program, &helpers)?;
    program.validate()?;
    Ok(DecoratorLoweringReport {
        decorated_classes: lowered,
        es_decorate_name: Some(es_decorate.name),
        run_initializers_name: Some(run_initializers.name),
    })
}

fn typed_error(node: Option<NodeId>, message: impl Into<String>) -> TypedIrError {
    TypedIrError {
        node,
        message: message.into(),
    }
}

fn class_has_decorators(program: &TypedProgram, node: NodeId) -> bool {
    let Some(IrNodeData::Class {
        members,
        decorators,
        ..
    }) = program.node(node).map(|node| node.data())
    else {
        return false;
    };
    if !list_items(program, *decorators).is_empty() {
        return true;
    }
    list_items(program, *members).iter().any(|member| {
        let decorators = match program.node(*member).map(|node| node.data()) {
            Some(IrNodeData::MethodDefinition { decorators, .. })
            | Some(IrNodeData::PropertyDefinition { decorators, .. }) => *decorators,
            _ => return false,
        };
        !list_items(program, decorators).is_empty()
    })
}

fn unsupported_lowering_reason(program: &TypedProgram, class: NodeId) -> Option<&'static str> {
    let IrNodeData::Class {
        name,
        super_class,
        members,
        decorators,
        ..
    } = program.node(class)?.data()
    else {
        return Some("decorator target is not a class");
    };
    let class_symbol = name.and_then(|name| {
        let IrNodeData::Name { name } = program.node(name)?.data() else {
            return None;
        };
        program.name(*name)?.symbol()
    });
    let mut evaluation_roots = list_items(program, *decorators).to_vec();
    evaluation_roots.extend(*super_class);
    let mut has_instance_field_like = false;
    let mut constructor = None;
    for &member in list_items(program, *members) {
        if member_runtime_contains_direct_eval(program, member) {
            return Some(
                "decorated class member contains direct eval whose visible environment would change",
            );
        }
        match program.node(member)?.data() {
            IrNodeData::MethodDefinition {
                key,
                kind,
                value,
                decorators,
                ..
            } => {
                evaluation_roots.extend(list_items(program, *decorators));
                if key.kind == PropertyKeyKind::Computed {
                    evaluation_roots.push(key.value);
                }
                if *kind == MethodKind::Constructor {
                    constructor = Some(member);
                    if !list_items(program, *decorators).is_empty() {
                        return Some("constructors cannot be decorated");
                    }
                }
                if key.kind == PropertyKeyKind::Private
                    && !list_items(program, *decorators).is_empty()
                    && subtree_contains_super(program, *value)
                {
                    return Some("decorated private method/accessor contains lexical super");
                }
            }
            IrNodeData::PropertyDefinition {
                key,
                is_static,
                decorators,
                ..
            } if !list_items(program, *decorators).is_empty() => {
                evaluation_roots.extend(list_items(program, *decorators));
                if key.kind == PropertyKeyKind::Computed {
                    evaluation_roots.push(key.value);
                }
                has_instance_field_like |= !*is_static;
            }
            IrNodeData::PropertyDefinition { key, .. } if key.kind == PropertyKeyKind::Computed => {
                evaluation_roots.push(key.value);
            }
            _ => {}
        }
    }
    for root in evaluation_roots {
        if subtree_contains_lexical_suspension(program, root) {
            return Some("decorator/class-key evaluation contains lexical await or yield");
        }
        if subtree_contains_private_name(program, root) {
            return Some("decorator/class-key evaluation depends on class-private scope");
        }
        if class_symbol.is_some_and(|symbol| subtree_references_symbol(program, root, symbol)) {
            return Some("decorator/class-key evaluation depends on the class-name TDZ scope");
        }
        if subtree_contains_direct_eval(program, root) {
            return Some("decorator/class-key evaluation contains direct eval");
        }
    }
    if super_class.is_some()
        && has_instance_field_like
        && constructor.is_some_and(|constructor| !has_direct_super_call(program, constructor))
    {
        return Some("derived constructor requires expression-position super initialization");
    }
    None
}

fn subtree_contains_super(program: &TypedProgram, root: NodeId) -> bool {
    program.nodes().iter().any(|node| {
        matches!(node.data(), IrNodeData::SuperExpression)
            && is_descendant_of(program, node.id(), root)
    })
}

fn subtree_contains_lexical_suspension(program: &TypedProgram, root: NodeId) -> bool {
    program.nodes().iter().any(|node| {
        matches!(
            node.data(),
            IrNodeData::AwaitExpression { .. } | IrNodeData::YieldExpression { .. }
        ) && is_lexical_descendant_of(program, node.id(), root)
    })
}

fn is_lexical_descendant_of(program: &TypedProgram, mut node: NodeId, root: NodeId) -> bool {
    if node == root {
        return true;
    }
    loop {
        let Some(parent) = program.node(node).and_then(|node| node.parent()) else {
            return false;
        };
        node = parent.parent();
        if matches!(
            program.node(node).map(|node| node.data()),
            Some(IrNodeData::Function { .. } | IrNodeData::ArrowFunction { .. })
        ) {
            return false;
        }
        if node == root {
            return true;
        }
    }
}

fn subtree_contains_private_name(program: &TypedProgram, root: NodeId) -> bool {
    program.nodes().iter().any(|node| {
        let IrNodeData::Name { name } = node.data() else {
            return false;
        };
        program
            .name(*name)
            .is_some_and(|name| name.syntax() == NameSyntax::PrivateIdentifier)
            && is_descendant_of(program, node.id(), root)
    })
}

fn subtree_references_symbol(program: &TypedProgram, root: NodeId, symbol: SymbolId) -> bool {
    program.nodes().iter().any(|node| {
        let IrNodeData::Name { name } = node.data() else {
            return false;
        };
        program.name(*name).is_some_and(|name| {
            name.symbol() == Some(symbol)
                && matches!(
                    name.role(),
                    NameRole::Reference | NameRole::AssignmentTarget
                )
        }) && is_descendant_of(program, node.id(), root)
    })
}

fn subtree_contains_direct_eval(program: &TypedProgram, root: NodeId) -> bool {
    program.nodes().iter().any(|node| {
        let IrNodeData::CallExpression { callee, .. } = node.data() else {
            return false;
        };
        let Some(IrNodeData::Identifier { name }) = program.node(*callee).map(|node| node.data())
        else {
            return false;
        };
        let Some(IrNodeData::Name { name }) = program.node(*name).map(|node| node.data()) else {
            return false;
        };
        program
            .name(*name)
            .is_some_and(|name| name.original() == "eval" && name.symbol().is_none())
            && is_descendant_of(program, node.id(), root)
    })
}

fn member_runtime_contains_direct_eval(program: &TypedProgram, member: NodeId) -> bool {
    match program.node(member).map(|node| node.data()) {
        Some(IrNodeData::MethodDefinition { value, .. }) => {
            subtree_contains_direct_eval(program, *value)
        }
        Some(IrNodeData::PropertyDefinition { value, .. }) => {
            value.is_some_and(|value| subtree_contains_direct_eval(program, value))
        }
        Some(IrNodeData::StaticBlock { body }) => list_items(program, *body)
            .iter()
            .any(|&statement| subtree_contains_direct_eval(program, statement)),
        _ => false,
    }
}

fn is_descendant_of(program: &TypedProgram, mut node: NodeId, ancestor: NodeId) -> bool {
    loop {
        if node == ancestor {
            return true;
        }
        let Some(parent) = program.node(node).and_then(|node| node.parent()) else {
            return false;
        };
        node = parent.parent();
    }
}

fn has_direct_super_call(program: &TypedProgram, constructor: NodeId) -> bool {
    let Some(IrNodeData::MethodDefinition { value, .. }) =
        program.node(constructor).map(|node| node.data())
    else {
        return false;
    };
    let Some(IrNodeData::Function {
        body: Some(body), ..
    }) = program.node(*value).map(|node| node.data())
    else {
        return false;
    };
    let Some(IrNodeData::FunctionBody { statements, .. }) =
        program.node(*body).map(|node| node.data())
    else {
        return false;
    };
    list_items(program, *statements)
        .iter()
        .any(|&statement| is_direct_super_call(program, statement))
}

fn list_items(program: &TypedProgram, list: ListId) -> &[NodeId] {
    program.list(list).map_or(&[], |list| list.items())
}

struct GeneratedNames {
    reserved: HashSet<String>,
}

impl GeneratedNames {
    fn new(program: &TypedProgram) -> Self {
        Self {
            reserved: program
                .names()
                .iter()
                .flat_map(|name| [name.original().to_owned(), name.emitted().to_owned()])
                .collect(),
        }
    }

    fn unique(&mut self, requested: &str) -> Result<String, TypedIrError> {
        let mut candidate = requested.to_owned();
        let mut suffix = 1_u32;
        while self.reserved.contains(&candidate) {
            candidate = format!("{requested}${suffix}");
            suffix = suffix
                .checked_add(1)
                .ok_or_else(|| typed_error(None, "decorator generated-name suffix overflow"))?;
        }
        self.reserved.insert(candidate.clone());
        Ok(candidate)
    }

    fn binding(
        &mut self,
        program: &mut TypedProgram,
        requested: &str,
        kind: DeclKind,
    ) -> Result<Binding, TypedIrError> {
        let name = self.unique(requested)?;
        let symbol = program.allocate_symbol(name.clone(), kind)?;
        Ok(Binding { name, symbol })
    }

    fn binding_with_factory(
        &mut self,
        factory: &SyntheticFactory<'_>,
        requested: &str,
        kind: DeclKind,
    ) -> Result<Binding, TypedIrError> {
        let name = self.unique(requested)?;
        factory.symbol(&name, kind)
    }
}

trait DecoratorFactoryExt {
    fn clone_subtree(&self, node: NodeId) -> Result<NodeId, TypedIrError>;
    #[allow(clippy::too_many_arguments)]
    fn identifier_occurrence(
        &self,
        original: &str,
        emitted: &str,
        role: NameRole,
        syntax: NameSyntax,
        symbol: Option<SymbolId>,
        origin: IrOrigin,
    ) -> Result<NodeId, TypedIrError>;
    fn variable_kind(
        &self,
        kind: VarKind,
        declarations: Vec<(Binding, Option<NodeId>)>,
    ) -> Result<NodeId, TypedIrError>;
    fn object_method(&self, key: &str, value: NodeId) -> Result<NodeId, TypedIrError>;
    fn update_with(
        &self,
        operator: UpdateOperator,
        argument: NodeId,
    ) -> Result<NodeId, TypedIrError>;
    fn sequence(&self, expressions: Vec<NodeId>) -> Result<NodeId, TypedIrError>;
    fn this_expression(&self) -> Result<NodeId, TypedIrError>;
    fn super_expression(&self) -> Result<NodeId, TypedIrError>;
    fn spread(&self, argument: NodeId) -> Result<NodeId, TypedIrError>;
    fn rest(&self, argument: NodeId) -> Result<NodeId, TypedIrError>;
    fn arrow_expression(
        &self,
        parameters: &[Binding],
        body: NodeId,
    ) -> Result<NodeId, TypedIrError>;
    fn arrow_block(
        &self,
        parameters: &[Binding],
        statements: Vec<NodeId>,
    ) -> Result<NodeId, TypedIrError>;
    fn for_in(&self, left: NodeId, right: NodeId, body: NodeId) -> Result<NodeId, TypedIrError>;
    fn member_private(
        &self,
        object: NodeId,
        name: &str,
        symbol: Option<SymbolId>,
    ) -> Result<NodeId, TypedIrError>;
    fn static_block(&self, statements: Vec<NodeId>) -> Result<NodeId, TypedIrError>;
    fn class_expression(
        &self,
        name: Option<NodeId>,
        super_class: Option<NodeId>,
        members: Vec<NodeId>,
        origin: IrOrigin,
    ) -> Result<NodeId, TypedIrError>;
    #[allow(clippy::too_many_arguments)]
    fn method_definition(
        &self,
        key: IrPropertyKey,
        value: NodeId,
        kind: MethodKind,
        is_static: bool,
        computed: bool,
        origin: IrOrigin,
    ) -> Result<NodeId, TypedIrError>;
    #[allow(clippy::too_many_arguments)]
    fn property_definition(
        &self,
        key: IrPropertyKey,
        value: Option<NodeId>,
        is_static: bool,
        computed: bool,
        accessor: bool,
        origin: IrOrigin,
    ) -> Result<NodeId, TypedIrError>;
    fn method_function(
        &self,
        parameters: Vec<NodeId>,
        statements: Vec<NodeId>,
        is_async: bool,
        is_generator: bool,
        origin: IrOrigin,
    ) -> Result<NodeId, TypedIrError>;
}

impl DecoratorFactoryExt for SyntheticFactory<'_> {
    fn clone_subtree(&self, node: NodeId) -> Result<NodeId, TypedIrError> {
        self.program.borrow_mut().clone_detached_subtree(node)
    }

    fn identifier_occurrence(
        &self,
        original: &str,
        emitted: &str,
        role: NameRole,
        syntax: NameSyntax,
        symbol: Option<SymbolId>,
        origin: IrOrigin,
    ) -> Result<NodeId, TypedIrError> {
        let name_node = self
            .program
            .borrow_mut()
            .append_detached_name(original, role, syntax, symbol, origin)?;
        let name_id = match self
            .program
            .borrow()
            .node(name_node)
            .map(|node| node.data())
        {
            Some(IrNodeData::Name { name }) => *name,
            _ => unreachable!(),
        };
        self.program
            .borrow_mut()
            .set_emitted_name(name_id, emitted)?;
        self.program
            .borrow_mut()
            .append_detached_node_with(origin, |_| Ok(IrNodeData::Identifier { name: name_node }))
    }

    fn variable_kind(
        &self,
        kind: VarKind,
        declarations: Vec<(Binding, Option<NodeId>)>,
    ) -> Result<NodeId, TypedIrError> {
        let mut declarators = Vec::with_capacity(declarations.len());
        for (binding, initializer) in declarations {
            let binding = self.binding_pattern(&binding)?;
            let declarator =
                self.program
                    .borrow_mut()
                    .append_detached_node_with(HELPER_ORIGIN, |_| {
                        Ok(IrNodeData::VariableDeclarator {
                            binding,
                            initializer,
                        })
                    })?;
            declarators.push(declarator);
        }
        self.program
            .borrow_mut()
            .append_detached_node_with(HELPER_ORIGIN, |builder| {
                let declarations = builder.list(ChildRole::DeclarationItems, declarators)?;
                Ok(IrNodeData::VariableDeclaration { kind, declarations })
            })
    }

    fn object_method(&self, key: &str, value: NodeId) -> Result<NodeId, TypedIrError> {
        let key = self.name_node(key, NameRole::Property, None)?;
        self.program
            .borrow_mut()
            .append_detached_node_with(HELPER_ORIGIN, |_| {
                Ok(IrNodeData::ObjectProperty {
                    key: IrPropertyKey {
                        kind: PropertyKeyKind::Identifier,
                        value: key,
                    },
                    value,
                    kind: PropertyKind::Init,
                    method: true,
                    shorthand: false,
                    computed: false,
                    prototype_setter: false,
                })
            })
    }

    fn update_with(
        &self,
        operator: UpdateOperator,
        argument: NodeId,
    ) -> Result<NodeId, TypedIrError> {
        self.program
            .borrow_mut()
            .append_detached_node_with(HELPER_ORIGIN, |_| {
                Ok(IrNodeData::UpdateExpression {
                    operator,
                    prefix: false,
                    argument,
                })
            })
    }

    fn sequence(&self, expressions: Vec<NodeId>) -> Result<NodeId, TypedIrError> {
        self.program
            .borrow_mut()
            .append_detached_node_with(HELPER_ORIGIN, |builder| {
                let expressions = builder.list(ChildRole::SequenceItems, expressions)?;
                Ok(IrNodeData::SequenceExpression { expressions })
            })
    }

    fn this_expression(&self) -> Result<NodeId, TypedIrError> {
        self.leaf(IrNodeData::ThisExpression)
    }

    fn super_expression(&self) -> Result<NodeId, TypedIrError> {
        self.leaf(IrNodeData::SuperExpression)
    }

    fn spread(&self, argument: NodeId) -> Result<NodeId, TypedIrError> {
        self.program
            .borrow_mut()
            .append_detached_node_with(HELPER_ORIGIN, |_| {
                Ok(IrNodeData::SpreadElement { argument })
            })
    }

    fn rest(&self, argument: NodeId) -> Result<NodeId, TypedIrError> {
        self.program
            .borrow_mut()
            .append_detached_node_with(HELPER_ORIGIN, |_| Ok(IrNodeData::RestPattern { argument }))
    }

    fn arrow_expression(
        &self,
        parameters: &[Binding],
        body: NodeId,
    ) -> Result<NodeId, TypedIrError> {
        let parameters = parameters
            .iter()
            .map(|parameter| self.binding_pattern(parameter))
            .collect::<Result<Vec<_>, _>>()?;
        self.program
            .borrow_mut()
            .append_detached_node_with(HELPER_ORIGIN, |builder| {
                let parameters = builder.list(ChildRole::FunctionParameters, parameters)?;
                Ok(IrNodeData::ArrowFunction {
                    parameters,
                    body,
                    body_kind: ArrowBodyKind::Expression,
                    is_async: false,
                })
            })
    }

    fn arrow_block(
        &self,
        parameters: &[Binding],
        statements: Vec<NodeId>,
    ) -> Result<NodeId, TypedIrError> {
        let body = self.function_body(statements)?;
        let parameters = parameters
            .iter()
            .map(|parameter| self.binding_pattern(parameter))
            .collect::<Result<Vec<_>, _>>()?;
        self.program
            .borrow_mut()
            .append_detached_node_with(HELPER_ORIGIN, |builder| {
                let parameters = builder.list(ChildRole::FunctionParameters, parameters)?;
                Ok(IrNodeData::ArrowFunction {
                    parameters,
                    body,
                    body_kind: ArrowBodyKind::Block,
                    is_async: false,
                })
            })
    }

    fn for_in(&self, left: NodeId, right: NodeId, body: NodeId) -> Result<NodeId, TypedIrError> {
        self.program
            .borrow_mut()
            .append_detached_node_with(HELPER_ORIGIN, |_| {
                Ok(IrNodeData::ForInStatement {
                    left,
                    left_kind: crate::typed_ir::ForLeftKind::Variable,
                    right,
                    body,
                })
            })
    }

    fn member_private(
        &self,
        object: NodeId,
        name: &str,
        symbol: Option<SymbolId>,
    ) -> Result<NodeId, TypedIrError> {
        let property = self.program.borrow_mut().append_detached_name(
            name,
            NameRole::PrivateProperty,
            NameSyntax::PrivateIdentifier,
            symbol,
            HELPER_ORIGIN,
        )?;
        self.program
            .borrow_mut()
            .append_detached_node_with(HELPER_ORIGIN, |_| {
                Ok(IrNodeData::MemberExpression {
                    object,
                    property,
                    property_kind: PropertyKeyKind::Private,
                    optional: false,
                })
            })
    }

    fn static_block(&self, statements: Vec<NodeId>) -> Result<NodeId, TypedIrError> {
        self.program
            .borrow_mut()
            .append_detached_node_with(HELPER_ORIGIN, |builder| {
                let body = builder.list(ChildRole::StaticBlockBody, statements)?;
                Ok(IrNodeData::StaticBlock { body })
            })
    }

    fn class_expression(
        &self,
        name: Option<NodeId>,
        super_class: Option<NodeId>,
        members: Vec<NodeId>,
        origin: IrOrigin,
    ) -> Result<NodeId, TypedIrError> {
        self.program
            .borrow_mut()
            .append_detached_node_with(origin, |builder| {
                let members = builder.list(ChildRole::ClassMembers, members)?;
                let decorators = builder.list(ChildRole::Decorators, [])?;
                Ok(IrNodeData::Class {
                    context: ClassContext::Expression,
                    name,
                    super_class,
                    members,
                    decorators,
                })
            })
    }

    fn method_definition(
        &self,
        key: IrPropertyKey,
        value: NodeId,
        kind: MethodKind,
        is_static: bool,
        computed: bool,
        origin: IrOrigin,
    ) -> Result<NodeId, TypedIrError> {
        self.program
            .borrow_mut()
            .append_detached_node_with(origin, |builder| {
                let decorators = builder.list(ChildRole::Decorators, [])?;
                Ok(IrNodeData::MethodDefinition {
                    key,
                    value,
                    kind,
                    is_static,
                    computed,
                    decorators,
                })
            })
    }

    fn property_definition(
        &self,
        key: IrPropertyKey,
        value: Option<NodeId>,
        is_static: bool,
        computed: bool,
        accessor: bool,
        origin: IrOrigin,
    ) -> Result<NodeId, TypedIrError> {
        self.program
            .borrow_mut()
            .append_detached_node_with(origin, |builder| {
                let decorators = builder.list(ChildRole::Decorators, [])?;
                Ok(IrNodeData::PropertyDefinition {
                    key,
                    value,
                    is_static,
                    computed,
                    decorators,
                    accessor,
                })
            })
    }

    fn method_function(
        &self,
        parameters: Vec<NodeId>,
        statements: Vec<NodeId>,
        is_async: bool,
        is_generator: bool,
        origin: IrOrigin,
    ) -> Result<NodeId, TypedIrError> {
        let body = self.function_body(statements)?;
        self.program.borrow_mut().set_origin(body, origin)?;
        self.program
            .borrow_mut()
            .append_detached_node_with(origin, |builder| {
                let parameters = builder.list(ChildRole::FunctionParameters, parameters)?;
                Ok(IrNodeData::Function {
                    context: FunctionContext::Method,
                    name: None,
                    parameters,
                    body: Some(body),
                    is_async,
                    is_generator,
                })
            })
    }
}

fn insert_after_program_directives(
    program: &mut TypedProgram,
    statements: &[NodeId],
) -> Result<(), TypedIrError> {
    let root = program.root();
    let IrNodeData::Program { body, .. } = program
        .node(root)
        .ok_or_else(|| typed_error(Some(root), "typed program root is missing"))?
        .data()
    else {
        return Err(typed_error(
            Some(root),
            "typed program root is not Program syntax",
        ));
    };
    let body = *body;
    let insertion = list_items(program, body)
        .iter()
        .take_while(|&&statement| {
            matches!(
                program.node(statement).map(|node| node.data()),
                Some(IrNodeData::ExpressionStatement {
                    directive: true,
                    ..
                })
            )
        })
        .count();
    program.splice_list(body, insertion..insertion, statements)?;
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DecoratedKind {
    Method,
    Getter,
    Setter,
    Field,
    Accessor,
}

impl DecoratedKind {
    const fn text(self) -> &'static str {
        match self {
            Self::Method => "method",
            Self::Getter => "getter",
            Self::Setter => "setter",
            Self::Field => "field",
            Self::Accessor => "accessor",
        }
    }

    const fn is_field_like(self) -> bool {
        matches!(self, Self::Field | Self::Accessor)
    }
}

#[derive(Clone)]
enum RuntimeKey {
    Known { name: String, private: bool },
    Computed { temporary: Binding },
}

impl RuntimeKey {
    const fn is_private(&self) -> bool {
        matches!(self, Self::Known { private: true, .. })
    }
}

#[derive(Clone)]
struct ItemPlan {
    member: NodeId,
    origin: IrOrigin,
    key: IrPropertyKey,
    runtime_key: RuntimeKey,
    kind: DecoratedKind,
    is_static: bool,
    decorator_nodes: Vec<NodeId>,
    decorators: Binding,
    /// Descriptor storage for private methods/accessors. Unlike public elements, private
    /// descriptors cannot be discovered with `Object.getOwnPropertyDescriptor`.
    descriptor: Option<Binding>,
    /// Side-specific synthetic private field used for the exact, side-effect-free brand probe
    /// required by `context.access.has`. Reading the decorated private accessor itself would run
    /// user code and is therefore not a valid brand check.
    has_brand: Option<Binding>,
    initializers: Option<Binding>,
    extra_initializers: Option<Binding>,
    storage: Option<Binding>,
}

#[derive(Clone)]
struct ComputedKeyPlan {
    member: NodeId,
    source: NodeId,
    temporary: Binding,
}

#[derive(Clone)]
struct PlainAutoAccessorPlan {
    member: NodeId,
    storage: Binding,
}

#[derive(Clone)]
struct SourceName {
    original: String,
    emitted: String,
    symbol: Option<SymbolId>,
    origin: IrOrigin,
}

#[allow(clippy::too_many_arguments)]
fn lower_one_class(
    program: &mut TypedProgram,
    class: NodeId,
    es_decorate: &Binding,
    run_initializers: &Binding,
    names: &mut GeneratedNames,
) -> Result<(), TypedIrError> {
    let class_origin = program
        .node(class)
        .ok_or_else(|| typed_error(Some(class), "decorated class is missing"))?
        .origin();
    let IrNodeData::Class {
        context,
        name,
        super_class,
        members,
        decorators,
    } = program
        .node(class)
        .ok_or_else(|| typed_error(Some(class), "decorated class is missing"))?
        .data()
        .clone()
    else {
        return Err(typed_error(Some(class), "decorator target is not a class"));
    };
    let source_name = name.map(|node| source_name(program, node)).transpose()?;
    let class_label = source_name
        .as_ref()
        .map_or("default", |name| name.original.as_str());
    let internal = names.binding(
        program,
        &format!("_{}_decorated", sanitize(class_label)),
        DeclKind::Var,
    )?;

    let member_ids = list_items(program, members).to_vec();
    let mut items = Vec::new();
    for &member in &member_ids {
        let Some((key, kind, is_static, decorator_list, accessor)) =
            program.node(member).and_then(|node| match node.data() {
                IrNodeData::MethodDefinition {
                    key,
                    kind,
                    is_static,
                    decorators,
                    ..
                } => Some((
                    *key,
                    match kind {
                        MethodKind::Get => DecoratedKind::Getter,
                        MethodKind::Set => DecoratedKind::Setter,
                        _ => DecoratedKind::Method,
                    },
                    *is_static,
                    *decorators,
                    false,
                )),
                IrNodeData::PropertyDefinition {
                    key,
                    is_static,
                    decorators,
                    accessor,
                    ..
                } => Some((
                    *key,
                    if *accessor {
                        DecoratedKind::Accessor
                    } else {
                        DecoratedKind::Field
                    },
                    *is_static,
                    *decorators,
                    *accessor,
                )),
                _ => None,
            })
        else {
            continue;
        };
        let decorator_nodes = list_items(program, decorator_list).to_vec();
        if decorator_nodes.is_empty() {
            continue;
        }
        let runtime_key = runtime_key(program, key, names)?;
        let key_label = match &runtime_key {
            RuntimeKey::Known { name, .. } => sanitize(name),
            RuntimeKey::Computed { temporary } => sanitize(&temporary.name),
        };
        let side = if is_static { "static" } else { "instance" };
        let decorators_binding = names.binding(
            program,
            &format!("_{side}_{key_label}_decorators"),
            DeclKind::Let,
        )?;
        let descriptor = (runtime_key.is_private() && kind != DecoratedKind::Field)
            .then(|| {
                names.binding(
                    program,
                    &format!("_{side}_{key_label}_descriptor"),
                    DeclKind::Let,
                )
            })
            .transpose()?;
        let (initializers, extra_initializers) = if kind.is_field_like() {
            (
                Some(names.binding(
                    program,
                    &format!("_{side}_{key_label}_initializers"),
                    DeclKind::Let,
                )?),
                Some(names.binding(
                    program,
                    &format!("_{side}_{key_label}_extraInitializers"),
                    DeclKind::Let,
                )?),
            )
        } else {
            (None, None)
        };
        let storage = accessor.then(|| {
            names.binding(
                program,
                &format!("_{side}_{key_label}_accessor_storage"),
                DeclKind::Var,
            )
        });
        let storage = storage.transpose()?;
        items.push(ItemPlan {
            member,
            origin: program.node(member).expect("member is live").origin(),
            key,
            runtime_key,
            kind,
            is_static,
            decorator_nodes,
            decorators: decorators_binding,
            descriptor,
            has_brand: None,
            initializers,
            extra_initializers,
            storage,
        });
    }

    // All private instance elements share one instance brand, and all private static elements
    // share one static brand. The synthetic fields are ordinary private declarations, so probing
    // them has the same receiver/side distinction as `#name in receiver` without invoking a
    // decorated getter. The parser does not currently expose a private-in expression node.
    let instance_private_brand = items
        .iter()
        .any(|item| {
            item.runtime_key.is_private() && item.kind != DecoratedKind::Field && !item.is_static
        })
        .then(|| names.binding(program, "_instance_decorator_brand", DeclKind::Var))
        .transpose()?;
    let static_private_brand = items
        .iter()
        .any(|item| {
            item.runtime_key.is_private() && item.kind != DecoratedKind::Field && item.is_static
        })
        .then(|| names.binding(program, "_static_decorator_brand", DeclKind::Var))
        .transpose()?;
    for item in &mut items {
        if item.runtime_key.is_private() && item.kind != DecoratedKind::Field {
            item.has_brand = if item.is_static {
                static_private_brand.clone()
            } else {
                instance_private_brand.clone()
            };
        }
    }

    // Decorator expressions and every computed key are evaluated left-to-right before the class
    // elements are installed. Decorated keys already own a stable temporary; undecorated keys
    // need one as well so moving decorator evaluation cannot reorder them.
    let mut computed_keys = Vec::new();
    for &member in &member_ids {
        let key = program.node(member).and_then(|node| match node.data() {
            IrNodeData::MethodDefinition { key, .. }
            | IrNodeData::PropertyDefinition { key, .. } => Some(*key),
            _ => None,
        });
        let Some(key) = key.filter(|key| key.kind == PropertyKeyKind::Computed) else {
            continue;
        };
        let temporary = items
            .iter()
            .find(|item| item.member == member)
            .and_then(|item| match &item.runtime_key {
                RuntimeKey::Computed { temporary } => Some(temporary.clone()),
                RuntimeKey::Known { .. } => None,
            })
            .map_or_else(|| names.binding(program, "_computedKey", DeclKind::Let), Ok)?;
        computed_keys.push(ComputedKeyPlan {
            member,
            source: key.value,
            temporary,
        });
    }
    let mut plain_auto_accessors = Vec::new();
    for &member in &member_ids {
        if items.iter().any(|item| item.member == member) {
            continue;
        }
        let accessor = matches!(
            program.node(member).map(|node| node.data()),
            Some(IrNodeData::PropertyDefinition { accessor: true, .. })
        );
        if accessor {
            plain_auto_accessors.push(PlainAutoAccessorPlan {
                member,
                storage: names.binding(program, "_auto_accessor_storage", DeclKind::Var)?,
            });
        }
    }

    let class_decorator_nodes = list_items(program, decorators).to_vec();
    let has_class_decorators = !class_decorator_nodes.is_empty();
    let has_instance_nonfield = items
        .iter()
        .any(|item| !item.is_static && !item.kind.is_field_like());
    let has_static_nonfield = items
        .iter()
        .any(|item| item.is_static && !item.kind.is_field_like());
    let instance_extra = has_instance_nonfield
        .then(|| names.binding(program, "_instanceExtraInitializers", DeclKind::Let))
        .transpose()?;
    let static_extra = has_static_nonfield
        .then(|| names.binding(program, "_staticExtraInitializers", DeclKind::Let))
        .transpose()?;
    let class_decorators = has_class_decorators
        .then(|| names.binding(program, "_classDecorators", DeclKind::Let))
        .transpose()?;
    let class_descriptor = has_class_decorators
        .then(|| names.binding(program, "_classDescriptor", DeclKind::Let))
        .transpose()?;
    let class_extra = has_class_decorators
        .then(|| names.binding(program, "_classExtraInitializers", DeclKind::Let))
        .transpose()?;
    let class_this = has_class_decorators
        .then(|| names.binding(program, "_classThis", DeclKind::Let))
        .transpose()?;
    let heritage = super_class
        .is_some()
        .then(|| names.binding(program, "_classSuper", DeclKind::Let))
        .transpose()?;

    // Instance/static extra-initializer chains advance only at field-like runtime elements.
    // Constructors are methods syntactically, but engine field initialization occurs before their
    // body, regardless of where the constructor appears in the class body. Precomputing these
    // edges prevents a source-order constructor from consuming an initializer too late.
    let mut initializer_before = vec![None; member_ids.len()];
    let mut instance_tail = instance_extra.clone();
    let mut static_tail = static_extra.clone();
    for (index, &member) in member_ids.iter().enumerate() {
        let member_data = program.node(member).map(|node| node.data());
        match member_data {
            Some(IrNodeData::PropertyDefinition { is_static, .. }) => {
                let pending = if *is_static {
                    &mut static_tail
                } else {
                    &mut instance_tail
                };
                initializer_before[index] = pending.take();
                if let Some(item) = items
                    .iter()
                    .find(|item| item.member == member && item.kind.is_field_like())
                {
                    *pending = item.extra_initializers.clone();
                }
            }
            Some(IrNodeData::StaticBlock { .. }) => {
                initializer_before[index] = static_tail.take();
            }
            _ => {}
        }
    }

    let factory = SyntheticFactory::new(program);
    let mut iife_statements = build_class_declarations(
        &factory,
        &items,
        &computed_keys,
        instance_extra.as_ref(),
        static_extra.as_ref(),
        class_decorators.as_ref(),
        class_descriptor.as_ref(),
        class_extra.as_ref(),
        class_this.as_ref(),
        heritage.as_ref(),
    )?;
    iife_statements.extend(build_evaluation_prelude(
        &factory,
        &member_ids,
        &items,
        &computed_keys,
        &class_decorator_nodes,
        class_decorators.as_ref(),
        super_class,
        heritage.as_ref(),
    )?);
    let super_class = heritage
        .as_ref()
        .map(|binding| factory.reference(binding))
        .transpose()?;
    let class_name_node = source_name
        .as_ref()
        .map(|name| clone_class_name(&factory, name))
        .transpose()?;
    let mut class_members = Vec::new();
    for (brand, is_static) in [
        (instance_private_brand.as_ref(), false),
        (static_private_brand.as_ref(), true),
    ] {
        if let Some(brand) = brand {
            class_members.push(build_private_brand_field(
                &factory,
                brand,
                is_static,
                class_origin,
            )?);
        }
    }
    if let Some(class_this) = class_this.as_ref() {
        let set_class_this = factory.expression_statement(
            factory.assignment(factory.target(class_this)?, factory.this_expression()?)?,
        )?;
        class_members.push(factory.static_block(vec![set_class_this])?);
    }
    let decoration_block = build_decoration_block(
        &factory,
        &items,
        &class_decorator_nodes,
        es_decorate,
        run_initializers,
        instance_extra.as_ref(),
        static_extra.as_ref(),
        class_decorators.as_ref(),
        class_descriptor.as_ref(),
        class_extra.as_ref(),
        class_this.as_ref(),
        &internal,
        source_name.as_ref(),
        names,
    )?;
    class_members.push(decoration_block);

    let mut constructor_emitted = false;
    for (index, member) in member_ids.into_iter().enumerate() {
        let before = initializer_before[index].as_ref();
        let item = items.iter().find(|item| item.member == member);
        if let Some(item) = item {
            let mut lowered = lower_decorated_member(
                &factory,
                item,
                item.kind.is_field_like().then_some(before).flatten(),
                run_initializers,
                class_this.as_ref(),
                names,
            )?;
            class_members.append(&mut lowered);
        } else {
            let (constructor, static_block) = {
                let program = factory.program.borrow();
                (
                    is_constructor(&program, member),
                    matches!(
                        program.node(member).map(|node| node.data()),
                        Some(IrNodeData::StaticBlock { .. })
                    ),
                )
            };
            if constructor {
                constructor_emitted = true;
                if let Some(tail) = instance_tail.as_ref() {
                    class_members.push(lower_constructor(
                        &factory,
                        member,
                        tail,
                        run_initializers,
                        super_class.is_some(),
                    )?);
                } else {
                    class_members.push(factory.clone_subtree(member)?);
                }
            } else if let Some(accessor) = plain_auto_accessors
                .iter()
                .find(|accessor| accessor.member == member)
            {
                class_members.extend(lower_plain_auto_accessor(
                    &factory,
                    member,
                    accessor,
                    computed_keys.iter().find(|plan| plan.member == member),
                    before,
                    run_initializers,
                    class_this.as_ref(),
                    names,
                )?);
            } else {
                if static_block && let Some(previous) = before {
                    class_members.push(build_static_tail(
                        &factory,
                        Some(previous),
                        None,
                        class_this.as_ref(),
                        run_initializers,
                    )?);
                }
                class_members.push(clone_undecorated_member(
                    &factory,
                    member,
                    computed_keys.iter().find(|plan| plan.member == member),
                    (!static_block).then_some(before).flatten(),
                    run_initializers,
                    class_this.as_ref(),
                )?);
            }
        }
    }
    if let Some(tail) = instance_tail.as_ref().filter(|_| !constructor_emitted) {
        class_members.push(synthetic_constructor(
            &factory,
            tail,
            run_initializers,
            super_class.is_some(),
            names,
        )?);
    }
    if static_tail.is_some() || has_class_decorators {
        class_members.push(build_static_tail(
            &factory,
            static_tail.as_ref(),
            class_extra.as_ref(),
            class_this.as_ref(),
            run_initializers,
        )?);
    }

    let class_expression = factory.class_expression(
        class_name_node,
        super_class,
        class_members,
        derived_origin(class_origin),
    )?;
    let class_declaration =
        factory.variable_declaration(vec![(internal.clone(), Some(class_expression))])?;
    iife_statements.push(class_declaration);
    let returned = class_this.as_ref().unwrap_or(&internal);
    iife_statements.push(factory.return_statement(Some(factory.reference(returned)?))?);
    let iife = factory.arrow_block(&[], iife_statements)?;
    let iife = factory.call(iife, Vec::new())?;

    let replacement =
        build_context_replacement(&factory, context, source_name.as_ref(), iife, class)?;
    commit_context_replacement(program, context, class, replacement)
}

fn build_private_brand_field(
    factory: &SyntheticFactory<'_>,
    brand: &Binding,
    is_static: bool,
    class_origin: IrOrigin,
) -> Result<NodeId, TypedIrError> {
    let key = factory.program.borrow_mut().append_detached_name(
        &brand.name,
        NameRole::PrivateProperty,
        NameSyntax::PrivateIdentifier,
        Some(brand.symbol),
        derived_origin(class_origin),
    )?;
    factory.property_definition(
        IrPropertyKey {
            kind: PropertyKeyKind::Private,
            value: key,
        },
        None,
        is_static,
        false,
        false,
        derived_origin(class_origin),
    )
}

fn source_name(program: &TypedProgram, node: NodeId) -> Result<SourceName, TypedIrError> {
    let IrNodeData::Name { name } = program
        .node(node)
        .ok_or_else(|| typed_error(Some(node), "class name node is missing"))?
        .data()
    else {
        return Err(typed_error(Some(node), "class name is not name syntax"));
    };
    let record = program
        .name(*name)
        .ok_or_else(|| typed_error(Some(node), "class name record is missing"))?;
    Ok(SourceName {
        original: record.original().to_owned(),
        emitted: record.emitted().to_owned(),
        symbol: record.symbol(),
        origin: program.node(node).expect("name exists").origin(),
    })
}

fn runtime_key(
    program: &mut TypedProgram,
    key: IrPropertyKey,
    names: &mut GeneratedNames,
) -> Result<RuntimeKey, TypedIrError> {
    match key.kind {
        PropertyKeyKind::Computed => Ok(RuntimeKey::Computed {
            temporary: names.binding(program, "_computedKey", DeclKind::Let)?,
        }),
        PropertyKeyKind::Identifier | PropertyKeyKind::String | PropertyKeyKind::Private => {
            let IrNodeData::Name { name } = program
                .node(key.value)
                .ok_or_else(|| typed_error(Some(key.value), "property key is missing"))?
                .data()
            else {
                return Err(typed_error(
                    Some(key.value),
                    "property key is not name syntax",
                ));
            };
            let record = program
                .name(*name)
                .ok_or_else(|| typed_error(Some(key.value), "property name is missing"))?;
            Ok(RuntimeKey::Known {
                name: record.original().to_owned(),
                private: key.kind == PropertyKeyKind::Private,
            })
        }
        PropertyKeyKind::Number => {
            let IrNodeData::NumberLiteral { value } = program
                .node(key.value)
                .ok_or_else(|| typed_error(Some(key.value), "numeric property key is missing"))?
                .data()
            else {
                return Err(typed_error(
                    Some(key.value),
                    "numeric property key is not a number literal",
                ));
            };
            Ok(RuntimeKey::Known {
                name: value.to_string(),
                private: false,
            })
        }
    }
}

fn sanitize(name: &str) -> String {
    let mut value = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '_' | '$') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    if value.is_empty() {
        value.push('_');
    }
    value
}

fn derived_origin(origin: IrOrigin) -> IrOrigin {
    let anchor = match origin {
        IrOrigin::Source(span) => Some(span),
        IrOrigin::Derived { anchor, .. } | IrOrigin::Synthetic { anchor, .. } => anchor,
    };
    IrOrigin::Derived {
        anchor,
        kind: DerivedOriginKind::Optimization,
    }
}

#[allow(clippy::too_many_arguments)]
fn build_class_declarations(
    factory: &SyntheticFactory<'_>,
    items: &[ItemPlan],
    computed_keys: &[ComputedKeyPlan],
    instance_extra: Option<&Binding>,
    static_extra: Option<&Binding>,
    class_decorators: Option<&Binding>,
    class_descriptor: Option<&Binding>,
    class_extra: Option<&Binding>,
    class_this: Option<&Binding>,
    heritage: Option<&Binding>,
) -> Result<Vec<NodeId>, TypedIrError> {
    let mut declarations = Vec::new();
    for binding in [instance_extra, static_extra, class_extra]
        .into_iter()
        .flatten()
    {
        declarations.push((binding.clone(), Some(factory.array(Vec::new())?)));
    }
    for binding in [class_decorators, class_descriptor, class_this, heritage]
        .into_iter()
        .flatten()
    {
        declarations.push((binding.clone(), None));
    }
    for item in items {
        declarations.push((item.decorators.clone(), None));
        if let Some(descriptor) = &item.descriptor {
            declarations.push((descriptor.clone(), None));
        }
        if let Some(initializers) = &item.initializers {
            declarations.push((initializers.clone(), Some(factory.array(Vec::new())?)));
        }
        if let Some(extra) = &item.extra_initializers {
            declarations.push((extra.clone(), Some(factory.array(Vec::new())?)));
        }
        if let RuntimeKey::Computed { temporary } = &item.runtime_key {
            declarations.push((temporary.clone(), None));
        }
    }
    for plan in computed_keys {
        let already_declared = items.iter().any(|item| {
            matches!(
                &item.runtime_key,
                RuntimeKey::Computed { temporary } if temporary.symbol == plan.temporary.symbol
            )
        });
        if !already_declared {
            declarations.push((plan.temporary.clone(), None));
        }
    }
    if declarations.is_empty() {
        Ok(Vec::new())
    } else {
        Ok(vec![factory.variable_kind(VarKind::Let, declarations)?])
    }
}

fn clone_class_name(
    factory: &SyntheticFactory<'_>,
    name: &SourceName,
) -> Result<NodeId, TypedIrError> {
    let node = factory.program.borrow_mut().append_detached_name(
        &name.original,
        NameRole::ClassName,
        NameSyntax::Identifier,
        name.symbol,
        name.origin,
    )?;
    let name_id = match factory.program.borrow().node(node).map(|node| node.data()) {
        Some(IrNodeData::Name { name }) => *name,
        _ => unreachable!(),
    };
    factory
        .program
        .borrow_mut()
        .set_emitted_name(name_id, &name.emitted)?;
    Ok(node)
}

#[allow(clippy::too_many_arguments)]
fn build_evaluation_prelude(
    factory: &SyntheticFactory<'_>,
    member_ids: &[NodeId],
    items: &[ItemPlan],
    computed_keys: &[ComputedKeyPlan],
    class_decorator_nodes: &[NodeId],
    class_decorators: Option<&Binding>,
    source_heritage: Option<NodeId>,
    heritage: Option<&Binding>,
) -> Result<Vec<NodeId>, TypedIrError> {
    let mut statements = Vec::new();

    // Class decorator expressions precede heritage evaluation.
    if !class_decorator_nodes.is_empty() {
        let target = class_decorators
            .ok_or_else(|| typed_error(None, "decorated class lacks class-decorator storage"))?;
        let decorators = class_decorator_nodes
            .iter()
            .map(|&decorator| factory.clone_subtree(decorator))
            .collect::<Result<Vec<_>, _>>()?;
        let value = factory.array(decorators)?;
        statements.push(
            factory.expression_statement(factory.assignment(factory.target(target)?, value)?)?,
        );
    }

    // Heritage is evaluated before member decorators and computed names. Capturing it also makes
    // every following expression run exactly once even though the class body consumes temporaries.
    if let Some(source) = source_heritage {
        let target = heritage.ok_or_else(|| {
            typed_error(
                Some(source),
                "decorated derived class lacks heritage storage",
            )
        })?;
        let source = factory.clone_subtree(source)?;
        statements.push(
            factory.expression_statement(factory.assignment(factory.target(target)?, source)?)?,
        );
    }

    for &member in member_ids {
        if let Some(item) = items.iter().find(|item| item.member == member) {
            let decorators = item
                .decorator_nodes
                .iter()
                .map(|&decorator| factory.clone_subtree(decorator))
                .collect::<Result<Vec<_>, _>>()?;
            let value = factory.array(decorators)?;
            statements.push(factory.expression_statement(
                factory.assignment(factory.target(&item.decorators)?, value)?,
            )?);
        }
        if let Some(plan) = computed_keys.iter().find(|plan| plan.member == member) {
            let source = factory.clone_subtree(plan.source)?;
            statements.push(factory.expression_statement(
                factory.assignment(factory.target(&plan.temporary)?, source)?,
            )?);
            // ClassElementName performs ToPropertyKey during key evaluation, before the next
            // decorator/key expression. Store that primitive key (rather than the raw object) so
            // class syntax and `context.name` reuse it without a second user coercion.
            let temporary = factory.reference(&plan.temporary)?;
            let is_symbol = factory.binary(
                BinaryOperator::StrictEq,
                factory.typeof_expression(temporary)?,
                factory.string("symbol")?,
            )?;
            let key = factory.conditional(
                is_symbol,
                factory.reference(&plan.temporary)?,
                template_string(factory, factory.reference(&plan.temporary)?)?,
            )?;
            statements.push(factory.expression_statement(
                factory.assignment(factory.target(&plan.temporary)?, key)?,
            )?);
        }
    }
    Ok(statements)
}

fn template_string(
    factory: &SyntheticFactory<'_>,
    expression: NodeId,
) -> Result<NodeId, TypedIrError> {
    // Template interpolation performs the same string-hinted ToPrimitive + ToString sequence as
    // ToPropertyKey's non-Symbol branch without depending on a shadowable `String` global.
    let head = factory.leaf(IrNodeData::TemplateElement {
        cooked: Some(String::new()),
        raw: String::new(),
        tail: false,
    })?;
    let tail = factory.leaf(IrNodeData::TemplateElement {
        cooked: Some(String::new()),
        raw: String::new(),
        tail: true,
    })?;
    factory
        .program
        .borrow_mut()
        .append_detached_node_with(HELPER_ORIGIN, |builder| {
            let quasis = builder.list(ChildRole::TemplateQuasis, [head, tail])?;
            let expressions = builder.list(ChildRole::TemplateExpressions, [expression])?;
            Ok(IrNodeData::TemplateLiteral {
                quasis,
                expressions,
            })
        })
}

fn clone_undecorated_member(
    factory: &SyntheticFactory<'_>,
    member: NodeId,
    computed: Option<&ComputedKeyPlan>,
    previous: Option<&Binding>,
    run_initializers: &Binding,
    class_this: Option<&Binding>,
) -> Result<NodeId, TypedIrError> {
    if computed.is_none() && previous.is_none() {
        return factory.clone_subtree(member);
    }
    let (origin, data) = {
        let program = factory.program.borrow();
        let node = program
            .node(member)
            .ok_or_else(|| typed_error(Some(member), "class member is missing"))?;
        (node.origin(), node.data().clone())
    };
    let key = if let Some(computed) = computed {
        IrPropertyKey {
            kind: PropertyKeyKind::Computed,
            value: factory.reference(&computed.temporary)?,
        }
    } else {
        match &data {
            IrNodeData::MethodDefinition { key, .. }
            | IrNodeData::PropertyDefinition { key, .. } => IrPropertyKey {
                kind: key.kind,
                value: factory.clone_subtree(key.value)?,
            },
            _ => {
                return Err(typed_error(
                    Some(member),
                    "initializer plan targets a non-class-field element",
                ));
            }
        }
    };
    match data {
        IrNodeData::MethodDefinition {
            value,
            kind,
            is_static,
            computed: true,
            ..
        } => {
            let value = factory.clone_subtree(value)?;
            factory.method_definition(key, value, kind, is_static, true, origin)
        }
        IrNodeData::PropertyDefinition {
            value,
            is_static,
            computed,
            accessor,
            ..
        } => {
            let mut value = value
                .map(|value| factory.clone_subtree(value))
                .transpose()?;
            if let Some(previous) = previous {
                let source = value.unwrap_or(factory.void_zero()?);
                let run_previous = run_initializers_expression(
                    factory,
                    previous,
                    run_initializers,
                    is_static,
                    class_this,
                )?;
                value = Some(factory.sequence(vec![run_previous, source])?);
            }
            factory.property_definition(key, value, is_static, computed, accessor, origin)
        }
        _ => Err(typed_error(
            Some(member),
            "computed-key plan targets a non-computed class element",
        )),
    }
}

#[allow(clippy::too_many_arguments)]
fn build_decoration_block(
    factory: &SyntheticFactory<'_>,
    items: &[ItemPlan],
    class_decorator_nodes: &[NodeId],
    es_decorate: &Binding,
    _run_initializers: &Binding,
    instance_extra: Option<&Binding>,
    static_extra: Option<&Binding>,
    class_decorators: Option<&Binding>,
    class_descriptor: Option<&Binding>,
    class_extra: Option<&Binding>,
    class_this: Option<&Binding>,
    internal: &Binding,
    source_name: Option<&SourceName>,
    names: &mut GeneratedNames,
) -> Result<NodeId, TypedIrError> {
    let mut statements = Vec::new();
    for item in items {
        if let Some(descriptor) = &item.descriptor {
            let descriptor_value = build_private_descriptor(factory, item, names)?;
            statements.push(factory.expression_statement(
                factory.assignment(factory.target(descriptor)?, descriptor_value)?,
            )?);
            statements.extend(build_private_function_names(factory, item, descriptor)?);
        }
    }
    let _ = class_decorator_nodes;

    for field_like in [false, true] {
        for is_static in [true, false] {
            for item in items.iter().filter(|item| {
                item.kind.is_field_like() == field_like && item.is_static == is_static
            }) {
                statements.push(build_item_decoration_call(
                    factory,
                    item,
                    es_decorate,
                    instance_extra,
                    static_extra,
                    names,
                )?);
            }
        }
    }

    if let (Some(class_decorators), Some(class_descriptor), Some(class_extra), Some(class_this)) =
        (class_decorators, class_descriptor, class_extra, class_this)
    {
        let descriptor_value = factory.data_property("value", factory.reference(class_this)?)?;
        let descriptor_object = factory.object(vec![descriptor_value])?;
        let descriptor_assignment =
            factory.assignment(factory.target(class_descriptor)?, descriptor_object)?;
        let context_kind = factory.data_property("kind", factory.string("class")?)?;
        let context_name_value = source_name
            .map(|name| factory.string(&name.original))
            .transpose()?
            .unwrap_or(factory.void_zero()?);
        let context_name = factory.data_property("name", context_name_value)?;
        let context = factory.object(vec![context_kind, context_name])?;
        let decorate = factory.call(
            factory.reference(es_decorate)?,
            vec![
                factory.null()?,
                descriptor_assignment,
                factory.reference(class_decorators)?,
                context,
                factory.null()?,
                factory.reference(class_extra)?,
            ],
        )?;
        statements.push(factory.expression_statement(decorate)?);
        let replacement = factory.member(factory.reference(class_descriptor)?, "value")?;
        let set_class_this = factory.assignment(factory.target(class_this)?, replacement)?;
        let set_internal = factory.assignment(factory.target(internal)?, set_class_this)?;
        statements.push(factory.expression_statement(set_internal)?);
    }
    factory.static_block(statements)
}

/// Build the explicit descriptor required by private methods and accessors.
///
/// Public elements can be reflected from the class/prototype. Private elements intentionally
/// cannot, so the source method function is moved into a closure-owned descriptor and the class
/// receives a private forwarding accessor during `lower_decorated_member`.
fn build_private_descriptor(
    factory: &SyntheticFactory<'_>,
    item: &ItemPlan,
    names: &mut GeneratedNames,
) -> Result<NodeId, TypedIrError> {
    if !item.runtime_key.is_private() {
        return Err(typed_error(
            Some(item.member),
            "private decorator descriptor requested for a public element",
        ));
    }
    let member = {
        let program = factory.program.borrow();
        program
            .node(item.member)
            .ok_or_else(|| typed_error(Some(item.member), "decorated private member is missing"))?
            .data()
            .clone()
    };
    let properties = match member {
        IrNodeData::MethodDefinition { value, kind, .. } => {
            let function = clone_method_function(factory, value)?;
            let property = match kind {
                MethodKind::Method => "value",
                MethodKind::Get => "get",
                MethodKind::Set => "set",
                MethodKind::Constructor => {
                    return Err(typed_error(
                        Some(item.member),
                        "constructors cannot be decorated",
                    ));
                }
            };
            vec![factory.object_method(property, function)?]
        }
        IrNodeData::PropertyDefinition { accessor: true, .. } => {
            let storage = item.storage.as_ref().ok_or_else(|| {
                typed_error(
                    Some(item.member),
                    "private auto-accessor descriptor lacks storage",
                )
            })?;
            let getter_read = factory.member_private(
                factory.this_expression()?,
                &storage.name,
                Some(storage.symbol),
            )?;
            let getter_return = factory.return_statement(Some(getter_read))?;
            let getter = factory.function_expression(&[], vec![getter_return])?;

            let value = names.binding_with_factory(factory, "value", DeclKind::Param)?;
            let setter_target = factory.member_private(
                factory.this_expression()?,
                &storage.name,
                Some(storage.symbol),
            )?;
            let setter_assignment = factory.expression_statement(
                factory.assignment(setter_target, factory.reference(&value)?)?,
            )?;
            let setter = factory.function_expression(&[value], vec![setter_assignment])?;
            vec![
                factory.object_method("get", getter)?,
                factory.object_method("set", setter)?,
            ]
        }
        _ => {
            return Err(typed_error(
                Some(item.member),
                "private decorator descriptor target changed category",
            ));
        }
    };
    factory.object(properties)
}

fn clone_method_function(
    factory: &SyntheticFactory<'_>,
    value: NodeId,
) -> Result<NodeId, TypedIrError> {
    let (origin, name, parameters, body, is_async, is_generator) = {
        let program = factory.program.borrow();
        let IrNodeData::Function {
            context: FunctionContext::Method,
            name,
            parameters,
            body,
            is_async,
            is_generator,
        } = program
            .node(value)
            .ok_or_else(|| typed_error(Some(value), "private method is missing"))?
            .data()
            .clone()
        else {
            return Err(typed_error(
                Some(value),
                "private method descriptor value is not method syntax",
            ));
        };
        let parameters = list_items(&program, parameters).to_vec();
        (
            program.node(value).expect("method value exists").origin(),
            name,
            parameters,
            body,
            is_async,
            is_generator,
        )
    };
    let name = name.map(|name| factory.clone_subtree(name)).transpose()?;
    let parameters = parameters
        .into_iter()
        .map(|parameter| factory.clone_subtree(parameter))
        .collect::<Result<Vec<_>, _>>()?;
    let body = body.map(|body| factory.clone_subtree(body)).transpose()?;
    factory
        .program
        .borrow_mut()
        .append_detached_node_with(origin, |builder| {
            let parameters = builder.list(ChildRole::FunctionParameters, parameters)?;
            Ok(IrNodeData::Function {
                // The object property emits this expression-context function with method syntax,
                // preserving the non-constructable method internal slot in the output.
                context: FunctionContext::Expression,
                name,
                parameters,
                body,
                is_async,
                is_generator,
            })
        })
}

fn build_private_function_names(
    factory: &SyntheticFactory<'_>,
    item: &ItemPlan,
    descriptor: &Binding,
) -> Result<Vec<NodeId>, TypedIrError> {
    let RuntimeKey::Known {
        name,
        private: true,
    } = &item.runtime_key
    else {
        return Err(typed_error(
            Some(item.member),
            "private descriptor lacks a statically known private name",
        ));
    };
    let properties = match item.kind {
        DecoratedKind::Method => vec![("value", format!("#{name}"))],
        DecoratedKind::Getter => vec![("get", format!("get #{name}"))],
        DecoratedKind::Setter => vec![("set", format!("set #{name}"))],
        DecoratedKind::Accessor => vec![
            ("get", format!("get #{name}")),
            ("set", format!("set #{name}")),
        ],
        DecoratedKind::Field => Vec::new(),
    };
    properties
        .into_iter()
        .map(|(property, name)| {
            let function = factory.member(factory.reference(descriptor)?, property)?;
            let configurable = factory.data_property("configurable", factory.boolean(true)?)?;
            let value = factory.data_property("value", factory.string(&name)?)?;
            let attributes = factory.object(vec![configurable, value])?;
            let set_name = factory.call(
                factory.member(factory.global("Object")?, "defineProperty")?,
                vec![function, factory.string("name")?, attributes],
            )?;
            factory.expression_statement(set_name)
        })
        .collect()
}

fn build_item_decoration_call(
    factory: &SyntheticFactory<'_>,
    item: &ItemPlan,
    es_decorate: &Binding,
    instance_extra: Option<&Binding>,
    static_extra: Option<&Binding>,
    names: &mut GeneratedNames,
) -> Result<NodeId, TypedIrError> {
    let constructor = if item.kind == DecoratedKind::Field {
        factory.null()?
    } else {
        factory.this_expression()?
    };
    let context = build_item_context(factory, item, names)?;
    let initializers = item
        .initializers
        .as_ref()
        .map(|binding| factory.reference(binding))
        .transpose()?
        .unwrap_or(factory.null()?);
    let extra =
        if let Some(extra) = &item.extra_initializers {
            factory.reference(extra)?
        } else if item.is_static {
            factory.reference(static_extra.ok_or_else(|| {
                typed_error(Some(item.member), "missing static extra initializers")
            })?)?
        } else {
            factory.reference(instance_extra.ok_or_else(|| {
                typed_error(Some(item.member), "missing instance extra initializers")
            })?)?
        };
    let call = factory.call(
        factory.reference(es_decorate)?,
        vec![
            constructor,
            item.descriptor
                .as_ref()
                .map(|descriptor| factory.reference(descriptor))
                .transpose()?
                .unwrap_or(factory.null()?),
            factory.reference(&item.decorators)?,
            context,
            initializers,
            extra,
        ],
    )?;
    factory.expression_statement(call)
}

fn build_item_context(
    factory: &SyntheticFactory<'_>,
    item: &ItemPlan,
    names: &mut GeneratedNames,
) -> Result<NodeId, TypedIrError> {
    let kind = factory.data_property("kind", factory.string(item.kind.text())?)?;
    let name =
        factory.data_property("name", runtime_key_expression(factory, &item.runtime_key)?)?;
    let is_static = factory.data_property("static", factory.boolean(item.is_static)?)?;
    let is_private = matches!(item.runtime_key, RuntimeKey::Known { private: true, .. });
    let private = factory.data_property("private", factory.boolean(is_private)?)?;
    let access = factory.data_property("access", build_access(factory, item, names)?)?;
    factory.object(vec![kind, name, is_static, private, access])
}

fn runtime_key_expression(
    factory: &SyntheticFactory<'_>,
    key: &RuntimeKey,
) -> Result<NodeId, TypedIrError> {
    match key {
        // Context names for private elements include the leading `#`.
        RuntimeKey::Known {
            name,
            private: true,
        } => factory.string(&format!("#{name}")),
        RuntimeKey::Known {
            name,
            private: false,
        } => factory.string(name),
        RuntimeKey::Computed { temporary } => factory.reference(temporary),
    }
}

fn build_access(
    factory: &SyntheticFactory<'_>,
    item: &ItemPlan,
    names: &mut GeneratedNames,
) -> Result<NodeId, TypedIrError> {
    let private = matches!(item.runtime_key, RuntimeKey::Known { private: true, .. });

    let has_object = names.binding_with_factory(factory, "o", DeclKind::Param)?;
    let has = if private {
        let read = if item.kind == DecoratedKind::Field {
            // A private field read has no user-observable getter and preserves the exact point at
            // which the field brand is installed during instance/static initialization.
            private_member(factory, factory.reference(&has_object)?, &item.runtime_key)?
        } else {
            let brand = item.has_brand.as_ref().ok_or_else(|| {
                typed_error(
                    Some(item.member),
                    "decorated private element lacks a side-effect-free brand probe",
                )
            })?;
            factory.member_private(
                factory.reference(&has_object)?,
                &brand.name,
                Some(brand.symbol),
            )?
        };
        let read = factory.expression_statement(read)?;
        let return_true = factory.return_statement(Some(factory.boolean(true)?))?;
        let try_block = factory.block(vec![read, return_true])?;
        let return_false = factory.return_statement(Some(factory.boolean(false)?))?;
        let catch_body = factory.block(vec![return_false])?;
        let catch =
            factory
                .program
                .borrow_mut()
                .append_detached_node_with(HELPER_ORIGIN, |_| {
                    Ok(IrNodeData::CatchClause {
                        parameter: None,
                        body: catch_body,
                    })
                })?;
        let attempt =
            factory
                .program
                .borrow_mut()
                .append_detached_node_with(HELPER_ORIGIN, |_| {
                    Ok(IrNodeData::TryStatement {
                        block: try_block,
                        handler: Some(catch),
                        finalizer: None,
                    })
                })?;
        factory.arrow_block(std::slice::from_ref(&has_object), vec![attempt])?
    } else {
        let key = runtime_key_expression(factory, &item.runtime_key)?;
        let has = factory.binary(BinaryOperator::In, key, factory.reference(&has_object)?)?;
        factory.arrow_expression(std::slice::from_ref(&has_object), has)?
    };
    let has = factory.data_property("has", has)?;

    let mut properties = vec![has];
    if !matches!(item.kind, DecoratedKind::Setter) {
        let get_object = names.binding_with_factory(factory, "o", DeclKind::Param)?;
        let read = if private {
            private_member(factory, factory.reference(&get_object)?, &item.runtime_key)?
        } else {
            factory.computed_member(
                factory.reference(&get_object)?,
                runtime_key_expression(factory, &item.runtime_key)?,
            )?
        };
        let get = factory.arrow_expression(std::slice::from_ref(&get_object), read)?;
        properties.push(factory.data_property("get", get)?);
    }
    if item.kind.is_field_like() || matches!(item.kind, DecoratedKind::Setter) {
        let set_object = names.binding_with_factory(factory, "o", DeclKind::Param)?;
        let set_value = names.binding_with_factory(factory, "v", DeclKind::Param)?;
        let target = if private {
            private_member(factory, factory.reference(&set_object)?, &item.runtime_key)?
        } else {
            factory.computed_member(
                factory.reference(&set_object)?,
                runtime_key_expression(factory, &item.runtime_key)?,
            )?
        };
        let assignment = factory
            .expression_statement(factory.assignment(target, factory.reference(&set_value)?)?)?;
        let set = factory.arrow_block(&[set_object, set_value], vec![assignment])?;
        properties.push(factory.data_property("set", set)?);
    }
    factory.object(properties)
}

fn private_member(
    factory: &SyntheticFactory<'_>,
    object: NodeId,
    key: &RuntimeKey,
) -> Result<NodeId, TypedIrError> {
    let RuntimeKey::Known {
        name,
        private: true,
    } = key
    else {
        return Err(typed_error(None, "private access requested for public key"));
    };
    factory.member_private(object, name, None)
}

fn member_key(
    factory: &SyntheticFactory<'_>,
    item: &ItemPlan,
    evaluate_computed: bool,
) -> Result<IrPropertyKey, TypedIrError> {
    if let RuntimeKey::Computed { temporary } = &item.runtime_key {
        let value = if evaluate_computed {
            let original = factory.clone_subtree(item.key.value)?;
            factory.assignment(factory.target(temporary)?, original)?
        } else {
            factory.reference(temporary)?
        };
        Ok(IrPropertyKey {
            kind: PropertyKeyKind::Computed,
            value,
        })
    } else {
        Ok(IrPropertyKey {
            kind: item.key.kind,
            value: factory.clone_subtree(item.key.value)?,
        })
    }
}

fn field_initializer(
    factory: &SyntheticFactory<'_>,
    item: &ItemPlan,
    previous: Option<&Binding>,
    run_initializers: &Binding,
    class_this: Option<&Binding>,
    source_value: Option<NodeId>,
) -> Result<NodeId, TypedIrError> {
    let target = if item.is_static {
        class_this
            .map(|binding| factory.reference(binding))
            .transpose()?
            .unwrap_or(factory.this_expression()?)
    } else {
        factory.this_expression()?
    };
    let value = source_value.unwrap_or(factory.void_zero()?);
    let initialize = factory.call(
        factory.reference(run_initializers)?,
        vec![
            target,
            factory.reference(item.initializers.as_ref().ok_or_else(|| {
                typed_error(Some(item.member), "field-like decorator lacks initializers")
            })?)?,
            value,
        ],
    )?;
    if let Some(previous) = previous {
        let run_previous = run_initializers_expression(
            factory,
            previous,
            run_initializers,
            item.is_static,
            class_this,
        )?;
        factory.sequence(vec![run_previous, initialize])
    } else {
        Ok(initialize)
    }
}

fn run_initializers_expression(
    factory: &SyntheticFactory<'_>,
    initializers: &Binding,
    run_initializers: &Binding,
    is_static: bool,
    class_this: Option<&Binding>,
) -> Result<NodeId, TypedIrError> {
    let target = if is_static {
        class_this
            .map(|binding| factory.reference(binding))
            .transpose()?
            .unwrap_or(factory.this_expression()?)
    } else {
        factory.this_expression()?
    };
    factory.call(
        factory.reference(run_initializers)?,
        vec![target, factory.reference(initializers)?],
    )
}

#[allow(clippy::too_many_arguments)]
fn lower_decorated_member(
    factory: &SyntheticFactory<'_>,
    item: &ItemPlan,
    previous: Option<&Binding>,
    run_initializers: &Binding,
    class_this: Option<&Binding>,
    names: &mut GeneratedNames,
) -> Result<Vec<NodeId>, TypedIrError> {
    let member_data = {
        let program = factory.program.borrow();
        program
            .node(item.member)
            .ok_or_else(|| typed_error(Some(item.member), "decorated member is missing"))?
            .data()
            .clone()
    };
    match member_data {
        IrNodeData::MethodDefinition {
            value,
            kind,
            is_static,
            computed,
            ..
        } => {
            if kind == MethodKind::Constructor {
                return Err(typed_error(
                    Some(item.member),
                    "constructors cannot be decorated",
                ));
            }
            if item.runtime_key.is_private() {
                return lower_private_method_or_accessor(
                    factory, item, kind, is_static, computed, names,
                );
            }
            let key = member_key(factory, item, false)?;
            let value = factory.clone_subtree(value)?;
            Ok(vec![factory.method_definition(
                key,
                value,
                kind,
                is_static,
                computed,
                item.origin,
            )?])
        }
        IrNodeData::PropertyDefinition {
            value,
            is_static,
            computed,
            accessor,
            ..
        } if !accessor => {
            let key = member_key(factory, item, false)?;
            let value = value
                .map(|value| factory.clone_subtree(value))
                .transpose()?;
            let value =
                field_initializer(factory, item, previous, run_initializers, class_this, value)?;
            Ok(vec![factory.property_definition(
                key,
                Some(value),
                is_static,
                computed,
                false,
                item.origin,
            )?])
        }
        IrNodeData::PropertyDefinition {
            value,
            is_static,
            computed,
            accessor: true,
            ..
        } => lower_auto_accessor(
            factory,
            item,
            previous,
            run_initializers,
            class_this,
            value,
            is_static,
            computed,
            names,
        ),
        _ => Err(typed_error(
            Some(item.member),
            "decorated member changed category during lowering",
        )),
    }
}

fn lower_private_method_or_accessor(
    factory: &SyntheticFactory<'_>,
    item: &ItemPlan,
    kind: MethodKind,
    is_static: bool,
    computed: bool,
    names: &mut GeneratedNames,
) -> Result<Vec<NodeId>, TypedIrError> {
    let descriptor = item.descriptor.as_ref().ok_or_else(|| {
        typed_error(
            Some(item.member),
            "decorated private method/accessor lacks a descriptor",
        )
    })?;
    let key = member_key(factory, item, false)?;
    match kind {
        MethodKind::Method => {
            // A getter preserves the private brand and assignment restrictions while making
            // `this.#method` evaluate to the exact replacement function from the descriptor.
            let replacement = factory.member(factory.reference(descriptor)?, "value")?;
            let returned = factory.return_statement(Some(replacement))?;
            let function = factory.method_function(
                Vec::new(),
                vec![returned],
                false,
                false,
                derived_origin(item.origin),
            )?;
            Ok(vec![factory.method_definition(
                key,
                function,
                MethodKind::Get,
                is_static,
                computed,
                derived_origin(item.origin),
            )?])
        }
        MethodKind::Get => {
            let replacement = factory.member(factory.reference(descriptor)?, "get")?;
            let call = factory.call(
                factory.member(replacement, "call")?,
                vec![factory.this_expression()?],
            )?;
            let returned = factory.return_statement(Some(call))?;
            let function = factory.method_function(
                Vec::new(),
                vec![returned],
                false,
                false,
                derived_origin(item.origin),
            )?;
            Ok(vec![factory.method_definition(
                key,
                function,
                MethodKind::Get,
                is_static,
                computed,
                derived_origin(item.origin),
            )?])
        }
        MethodKind::Set => {
            let value = names.binding_with_factory(factory, "value", DeclKind::Param)?;
            let replacement = factory.member(factory.reference(descriptor)?, "set")?;
            let call = factory.call(
                factory.member(replacement, "call")?,
                vec![factory.this_expression()?, factory.reference(&value)?],
            )?;
            let statement = factory.expression_statement(call)?;
            let parameter = factory.binding_pattern(&value)?;
            let function = factory.method_function(
                vec![parameter],
                vec![statement],
                false,
                false,
                derived_origin(item.origin),
            )?;
            Ok(vec![factory.method_definition(
                key,
                function,
                MethodKind::Set,
                is_static,
                computed,
                derived_origin(item.origin),
            )?])
        }
        MethodKind::Constructor => Err(typed_error(
            Some(item.member),
            "constructors cannot be decorated",
        )),
    }
}

#[allow(clippy::too_many_arguments)]
fn lower_plain_auto_accessor(
    factory: &SyntheticFactory<'_>,
    member: NodeId,
    plan: &PlainAutoAccessorPlan,
    computed: Option<&ComputedKeyPlan>,
    previous: Option<&Binding>,
    run_initializers: &Binding,
    class_this: Option<&Binding>,
    names: &mut GeneratedNames,
) -> Result<Vec<NodeId>, TypedIrError> {
    let (key, value, is_static, is_computed, origin) = {
        let program = factory.program.borrow();
        let node = program
            .node(member)
            .ok_or_else(|| typed_error(Some(member), "auto-accessor member is missing"))?;
        let IrNodeData::PropertyDefinition {
            key,
            value,
            is_static,
            computed,
            accessor: true,
            ..
        } = node.data().clone()
        else {
            return Err(typed_error(
                Some(member),
                "plain auto-accessor plan targets non-accessor syntax",
            ));
        };
        (key, value, is_static, computed, node.origin())
    };
    let clone_key = || -> Result<IrPropertyKey, TypedIrError> {
        if let Some(computed) = computed {
            Ok(IrPropertyKey {
                kind: PropertyKeyKind::Computed,
                value: factory.reference(&computed.temporary)?,
            })
        } else {
            Ok(IrPropertyKey {
                kind: key.kind,
                value: factory.clone_subtree(key.value)?,
            })
        }
    };

    let mut initialized = value
        .map(|value| factory.clone_subtree(value))
        .transpose()?
        .unwrap_or(factory.void_zero()?);
    if let Some(previous) = previous {
        let run_previous = run_initializers_expression(
            factory,
            previous,
            run_initializers,
            is_static,
            class_this,
        )?;
        initialized = factory.sequence(vec![run_previous, initialized])?;
    }

    let storage_name = factory.program.borrow_mut().append_detached_name(
        &plan.storage.name,
        NameRole::PrivateProperty,
        NameSyntax::PrivateIdentifier,
        Some(plan.storage.symbol),
        derived_origin(origin),
    )?;
    let storage = factory.property_definition(
        IrPropertyKey {
            kind: PropertyKeyKind::Private,
            value: storage_name,
        },
        Some(initialized),
        is_static,
        false,
        false,
        derived_origin(origin),
    )?;

    let getter_value = factory.member_private(
        factory.this_expression()?,
        &plan.storage.name,
        Some(plan.storage.symbol),
    )?;
    let getter_return = factory.return_statement(Some(getter_value))?;
    let getter_function = factory.method_function(
        Vec::new(),
        vec![getter_return],
        false,
        false,
        derived_origin(origin),
    )?;
    let getter = factory.method_definition(
        clone_key()?,
        getter_function,
        MethodKind::Get,
        is_static,
        is_computed,
        derived_origin(origin),
    )?;

    let value = names.binding_with_factory(factory, "value", DeclKind::Param)?;
    let parameter = factory.binding_pattern(&value)?;
    let setter_target = factory.member_private(
        factory.this_expression()?,
        &plan.storage.name,
        Some(plan.storage.symbol),
    )?;
    let assignment = factory
        .expression_statement(factory.assignment(setter_target, factory.reference(&value)?)?)?;
    let setter_function = factory.method_function(
        vec![parameter],
        vec![assignment],
        false,
        false,
        derived_origin(origin),
    )?;
    let setter = factory.method_definition(
        clone_key()?,
        setter_function,
        MethodKind::Set,
        is_static,
        is_computed,
        derived_origin(origin),
    )?;
    Ok(vec![storage, getter, setter])
}

#[allow(clippy::too_many_arguments)]
fn lower_auto_accessor(
    factory: &SyntheticFactory<'_>,
    item: &ItemPlan,
    previous: Option<&Binding>,
    run_initializers: &Binding,
    class_this: Option<&Binding>,
    source_value: Option<NodeId>,
    is_static: bool,
    computed: bool,
    names: &mut GeneratedNames,
) -> Result<Vec<NodeId>, TypedIrError> {
    let storage = item
        .storage
        .as_ref()
        .ok_or_else(|| typed_error(Some(item.member), "auto-accessor storage is missing"))?;
    let storage_name = factory.program.borrow_mut().append_detached_name(
        &storage.name,
        NameRole::PrivateProperty,
        NameSyntax::PrivateIdentifier,
        Some(storage.symbol),
        HELPER_ORIGIN,
    )?;
    let source_value = source_value
        .map(|value| factory.clone_subtree(value))
        .transpose()?;
    let initialized = field_initializer(
        factory,
        item,
        previous,
        run_initializers,
        class_this,
        source_value,
    )?;
    let storage_field = factory.property_definition(
        IrPropertyKey {
            kind: PropertyKeyKind::Private,
            value: storage_name,
        },
        Some(initialized),
        is_static,
        false,
        false,
        derived_origin(item.origin),
    )?;

    let getter_key = member_key(factory, item, false)?;
    let getter_read = if let Some(descriptor) = &item.descriptor {
        let replacement = factory.member(factory.reference(descriptor)?, "get")?;
        factory.call(
            factory.member(replacement, "call")?,
            vec![factory.this_expression()?],
        )?
    } else {
        factory.member_private(
            factory.this_expression()?,
            &storage.name,
            Some(storage.symbol),
        )?
    };
    let getter_return = factory.return_statement(Some(getter_read))?;
    let getter_function = factory.method_function(
        Vec::new(),
        vec![getter_return],
        false,
        false,
        derived_origin(item.origin),
    )?;
    let getter = factory.method_definition(
        getter_key,
        getter_function,
        MethodKind::Get,
        is_static,
        computed,
        derived_origin(item.origin),
    )?;

    let setter_key = member_key(factory, item, false)?;
    let setter_value = names.binding_with_factory(factory, "value", DeclKind::Param)?;
    let setter_parameter = factory.binding_pattern(&setter_value)?;
    let setter_assignment = if let Some(descriptor) = &item.descriptor {
        let replacement = factory.member(factory.reference(descriptor)?, "set")?;
        let call = factory.call(
            factory.member(replacement, "call")?,
            vec![
                factory.this_expression()?,
                factory.reference(&setter_value)?,
            ],
        )?;
        factory.expression_statement(call)?
    } else {
        let setter_target = factory.member_private(
            factory.this_expression()?,
            &storage.name,
            Some(storage.symbol),
        )?;
        factory.expression_statement(
            factory.assignment(setter_target, factory.reference(&setter_value)?)?,
        )?
    };
    let setter_function = factory.method_function(
        vec![setter_parameter],
        vec![setter_assignment],
        false,
        false,
        derived_origin(item.origin),
    )?;
    let setter = factory.method_definition(
        setter_key,
        setter_function,
        MethodKind::Set,
        is_static,
        computed,
        derived_origin(item.origin),
    )?;
    Ok(vec![storage_field, getter, setter])
}

fn is_constructor(program: &TypedProgram, member: NodeId) -> bool {
    matches!(
        program.node(member).map(|node| node.data()),
        Some(IrNodeData::MethodDefinition {
            kind: MethodKind::Constructor,
            ..
        })
    )
}

fn lower_constructor(
    factory: &SyntheticFactory<'_>,
    member: NodeId,
    tail: &Binding,
    run_initializers: &Binding,
    derived: bool,
) -> Result<NodeId, TypedIrError> {
    let (key, value, is_static, computed, origin) = {
        let program = factory.program.borrow();
        let origin = program
            .node(member)
            .ok_or_else(|| typed_error(Some(member), "constructor is missing"))?
            .origin();
        let IrNodeData::MethodDefinition {
            key,
            value,
            is_static,
            computed,
            ..
        } = program
            .node(member)
            .expect("constructor exists")
            .data()
            .clone()
        else {
            return Err(typed_error(Some(member), "constructor is not a method"));
        };
        (key, value, is_static, computed, origin)
    };
    let (parameters, body, is_async, is_generator) = {
        let program = factory.program.borrow();
        let IrNodeData::Function {
            parameters,
            body,
            is_async,
            is_generator,
            ..
        } = program
            .node(value)
            .ok_or_else(|| typed_error(Some(value), "constructor function is missing"))?
            .data()
            .clone()
        else {
            return Err(typed_error(
                Some(value),
                "constructor value is not a function",
            ));
        };
        (parameters, body, is_async, is_generator)
    };
    let parameter_nodes = {
        let program = factory.program.borrow();
        list_items(&program, parameters).to_vec()
    };
    let parameters = parameter_nodes
        .iter()
        .map(|&parameter| factory.clone_subtree(parameter))
        .collect::<Result<Vec<_>, _>>()?;
    let body = body.ok_or_else(|| typed_error(Some(value), "constructor body is missing"))?;
    let statements_list = match factory
        .program
        .borrow()
        .node(body)
        .ok_or_else(|| typed_error(Some(body), "constructor body is missing"))?
        .data()
    {
        IrNodeData::FunctionBody { statements, .. } => *statements,
        _ => {
            return Err(typed_error(
                Some(body),
                "constructor body is not function syntax",
            ));
        }
    };
    let source_statements = list_items(&factory.program.borrow(), statements_list).to_vec();
    let insertion = if derived {
        source_statements
            .iter()
            .position(|&statement| is_direct_super_call(&factory.program.borrow(), statement))
            .map(|index| index + 1)
            .ok_or_else(|| {
                typed_error(
                    Some(member),
                    "derived decorated class constructor has no direct super() initialization point",
                )
            })?
    } else {
        0
    };
    let mut statements = source_statements
        .iter()
        .map(|&statement| factory.clone_subtree(statement))
        .collect::<Result<Vec<_>, _>>()?;
    let run = run_initializer_statement(factory, tail, run_initializers, false, None)?;
    statements.insert(insertion, run);
    let function =
        factory.method_function(parameters, statements, is_async, is_generator, origin)?;
    let key = IrPropertyKey {
        kind: key.kind,
        value: factory.clone_subtree(key.value)?,
    };
    factory.method_definition(
        key,
        function,
        MethodKind::Constructor,
        is_static,
        computed,
        origin,
    )
}

fn is_direct_super_call(program: &TypedProgram, statement: NodeId) -> bool {
    let Some(IrNodeData::ExpressionStatement { expression, .. }) =
        program.node(statement).map(|node| node.data())
    else {
        return false;
    };
    let Some(IrNodeData::CallExpression { callee, .. }) =
        program.node(*expression).map(|node| node.data())
    else {
        return false;
    };
    matches!(
        program.node(*callee).map(|node| node.data()),
        Some(IrNodeData::SuperExpression)
    )
}

fn synthetic_constructor(
    factory: &SyntheticFactory<'_>,
    tail: &Binding,
    run_initializers: &Binding,
    derived: bool,
    names: &mut GeneratedNames,
) -> Result<NodeId, TypedIrError> {
    let key_name = factory.program.borrow_mut().append_detached_name(
        "constructor",
        NameRole::Property,
        NameSyntax::Identifier,
        None,
        HELPER_ORIGIN,
    )?;
    let mut parameters = Vec::new();
    let mut statements = Vec::new();
    if derived {
        let args = names.binding_with_factory(factory, "args", DeclKind::Param)?;
        parameters.push(factory.rest(factory.binding_pattern(&args)?)?);
        let spread = factory.spread(factory.reference(&args)?)?;
        let super_call = factory.call(factory.super_expression()?, vec![spread])?;
        statements.push(factory.expression_statement(super_call)?);
    }
    statements.push(run_initializer_statement(
        factory,
        tail,
        run_initializers,
        false,
        None,
    )?);
    let function = factory.method_function(parameters, statements, false, false, HELPER_ORIGIN)?;
    factory.method_definition(
        IrPropertyKey {
            kind: PropertyKeyKind::Identifier,
            value: key_name,
        },
        function,
        MethodKind::Constructor,
        false,
        false,
        HELPER_ORIGIN,
    )
}

fn run_initializer_statement(
    factory: &SyntheticFactory<'_>,
    initializers: &Binding,
    run_initializers: &Binding,
    is_static: bool,
    class_this: Option<&Binding>,
) -> Result<NodeId, TypedIrError> {
    let target = if is_static {
        class_this
            .map(|binding| factory.reference(binding))
            .transpose()?
            .unwrap_or(factory.this_expression()?)
    } else {
        factory.this_expression()?
    };
    let call = factory.call(
        factory.reference(run_initializers)?,
        vec![target, factory.reference(initializers)?],
    )?;
    factory.expression_statement(call)
}

fn build_static_tail(
    factory: &SyntheticFactory<'_>,
    static_tail: Option<&Binding>,
    class_extra: Option<&Binding>,
    class_this: Option<&Binding>,
    run_initializers: &Binding,
) -> Result<NodeId, TypedIrError> {
    let mut statements = Vec::new();
    if let Some(static_tail) = static_tail {
        statements.push(run_initializer_statement(
            factory,
            static_tail,
            run_initializers,
            true,
            class_this,
        )?);
    }
    if let (Some(class_extra), Some(class_this)) = (class_extra, class_this) {
        let call = factory.call(
            factory.reference(run_initializers)?,
            vec![
                factory.reference(class_this)?,
                factory.reference(class_extra)?,
            ],
        )?;
        statements.push(factory.expression_statement(call)?);
    }
    factory.static_block(statements)
}

enum ContextReplacement {
    Direct(NodeId),
    ExportDefault {
        wrapper: NodeId,
        statements: Vec<NodeId>,
    },
}

fn build_context_replacement(
    factory: &SyntheticFactory<'_>,
    context: ClassContext,
    source_name: Option<&SourceName>,
    iife: NodeId,
    class: NodeId,
) -> Result<ContextReplacement, TypedIrError> {
    match context {
        ClassContext::Expression => Ok(ContextReplacement::Direct(iife)),
        ClassContext::Declaration => {
            let name = source_name.ok_or_else(|| {
                typed_error(Some(class), "class declaration is missing its binding name")
            })?;
            let declaration = source_variable(factory, name, iife)?;
            Ok(ContextReplacement::Direct(declaration))
        }
        ClassContext::ExportDefault => {
            let wrapper = factory
                .program
                .borrow()
                .node(class)
                .and_then(|node| node.parent())
                .map(|parent| parent.parent())
                .ok_or_else(|| {
                    typed_error(Some(class), "default-export class wrapper is missing")
                })?;
            if let Some(name) = source_name {
                let declaration = source_variable(factory, name, iife)?;
                let value = factory.identifier_occurrence(
                    &name.original,
                    &name.emitted,
                    NameRole::Reference,
                    NameSyntax::Identifier,
                    name.symbol,
                    name.origin,
                )?;
                let export = factory.program.borrow_mut().append_detached_node_with(
                    derived_origin(name.origin),
                    |_| {
                        Ok(IrNodeData::ExportDefaultDeclaration {
                            value,
                            kind: ExportDefaultValueKind::Expression,
                        })
                    },
                )?;
                Ok(ContextReplacement::ExportDefault {
                    wrapper,
                    statements: vec![declaration, export],
                })
            } else {
                let class_origin = factory
                    .program
                    .borrow()
                    .node(class)
                    .expect("class exists")
                    .origin();
                let export = factory.program.borrow_mut().append_detached_node_with(
                    derived_origin(class_origin),
                    |_| {
                        Ok(IrNodeData::ExportDefaultDeclaration {
                            value: iife,
                            kind: ExportDefaultValueKind::Expression,
                        })
                    },
                )?;
                Ok(ContextReplacement::ExportDefault {
                    wrapper,
                    statements: vec![export],
                })
            }
        }
    }
}

fn source_variable(
    factory: &SyntheticFactory<'_>,
    name: &SourceName,
    initializer: NodeId,
) -> Result<NodeId, TypedIrError> {
    let binding = factory.identifier_occurrence(
        &name.original,
        &name.emitted,
        NameRole::Binding,
        NameSyntax::Identifier,
        name.symbol,
        name.origin,
    )?;
    let declarator = factory.program.borrow_mut().append_detached_node_with(
        derived_origin(name.origin),
        |_| {
            Ok(IrNodeData::VariableDeclarator {
                binding,
                initializer: Some(initializer),
            })
        },
    )?;
    factory
        .program
        .borrow_mut()
        .append_detached_node_with(derived_origin(name.origin), |builder| {
            let declarations = builder.list(ChildRole::DeclarationItems, [declarator])?;
            Ok(IrNodeData::VariableDeclaration {
                kind: VarKind::Let,
                declarations,
            })
        })
}

fn commit_context_replacement(
    program: &mut TypedProgram,
    _context: ClassContext,
    class: NodeId,
    replacement: ContextReplacement,
) -> Result<(), TypedIrError> {
    match replacement {
        ContextReplacement::Direct(replacement) => program.replace_node(class, replacement),
        ContextReplacement::ExportDefault {
            wrapper,
            statements,
        } => {
            let parent = program
                .node(wrapper)
                .and_then(|node| node.parent())
                .ok_or_else(|| typed_error(Some(wrapper), "default export is detached"))?;
            let list = parent.list().ok_or_else(|| {
                typed_error(Some(wrapper), "default export is not in a statement list")
            })?;
            let index = list_items(program, list)
                .iter()
                .position(|&node| node == wrapper)
                .ok_or_else(|| typed_error(Some(wrapper), "default export list slot is missing"))?;
            program.splice_list(list, index..index + 1, &statements)?;
            Ok(())
        }
    }
}

// The two structural runtime-helper builders follow below.

fn build_run_initializers(
    factory: &SyntheticFactory<'_>,
    helper: &Binding,
    names: &mut GeneratedNames,
) -> Result<NodeId, TypedIrError> {
    let this_arg = names.binding_with_factory(factory, "thisArg", DeclKind::Param)?;
    let initializers = names.binding_with_factory(factory, "initializers", DeclKind::Param)?;
    let value = names.binding_with_factory(factory, "value", DeclKind::Param)?;
    let use_value = names.binding_with_factory(factory, "useValue", DeclKind::Var)?;
    let index = names.binding_with_factory(factory, "i", DeclKind::Var)?;

    let arguments_length = factory.member(factory.global("arguments")?, "length")?;
    let has_value = factory.binary(BinaryOperator::Gt, arguments_length, factory.number(2.0)?)?;
    let use_value_declaration =
        factory.variable_declaration(vec![(use_value.clone(), Some(has_value))])?;

    let initializer =
        factory.variable_declaration(vec![(index.clone(), Some(factory.number(0.0)?))])?;
    let test = factory.binary(
        BinaryOperator::Lt,
        factory.reference(&index)?,
        factory.member(factory.reference(&initializers)?, "length")?,
    )?;
    let update = factory.update(factory.target(&index)?)?;
    let current = factory.computed_member(
        factory.reference(&initializers)?,
        factory.reference(&index)?,
    )?;
    let call_with_value = factory.call(
        factory.member(current, "call")?,
        vec![factory.reference(&this_arg)?, factory.reference(&value)?],
    )?;
    let current = factory.computed_member(
        factory.reference(&initializers)?,
        factory.reference(&index)?,
    )?;
    let call_without_value = factory.call(
        factory.member(current, "call")?,
        vec![factory.reference(&this_arg)?],
    )?;
    let next_value = factory.conditional(
        factory.reference(&use_value)?,
        call_with_value,
        call_without_value,
    )?;
    let assign_value =
        factory.expression_statement(factory.assignment(factory.target(&value)?, next_value)?)?;
    let body = factory.block(vec![assign_value])?;
    let loop_statement =
        factory.for_statement(Some(initializer), Some(test), Some(update), body)?;
    let return_value = factory.conditional(
        factory.reference(&use_value)?,
        factory.reference(&value)?,
        factory.void_zero()?,
    )?;
    let return_statement = factory.return_statement(Some(return_value))?;
    let function = factory.function_expression(
        &[this_arg, initializers, value],
        vec![use_value_declaration, loop_statement, return_statement],
    )?;
    factory.variable_declaration(vec![(helper.clone(), Some(function))])
}

fn build_es_decorate(
    factory: &SyntheticFactory<'_>,
    helper: &Binding,
    names: &mut GeneratedNames,
) -> Result<NodeId, TypedIrError> {
    let ctor = names.binding_with_factory(factory, "ctor", DeclKind::Param)?;
    let descriptor_in = names.binding_with_factory(factory, "descriptorIn", DeclKind::Param)?;
    let decorators = names.binding_with_factory(factory, "decorators", DeclKind::Param)?;
    let context_in = names.binding_with_factory(factory, "contextIn", DeclKind::Param)?;
    let initializers = names.binding_with_factory(factory, "initializers", DeclKind::Param)?;
    let extra_initializers =
        names.binding_with_factory(factory, "extraInitializers", DeclKind::Param)?;
    let accept = names.binding_with_factory(factory, "accept", DeclKind::Function)?;
    let accepted = names.binding_with_factory(factory, "f", DeclKind::Param)?;
    let kind = names.binding_with_factory(factory, "kind", DeclKind::Var)?;
    let key = names.binding_with_factory(factory, "key", DeclKind::Var)?;
    let target = names.binding_with_factory(factory, "target", DeclKind::Var)?;
    let descriptor = names.binding_with_factory(factory, "descriptor", DeclKind::Var)?;
    let temporary = names.binding_with_factory(factory, "_", DeclKind::Var)?;
    let done = names.binding_with_factory(factory, "done", DeclKind::Var)?;
    let index = names.binding_with_factory(factory, "i", DeclKind::Var)?;
    let context = names.binding_with_factory(factory, "context", DeclKind::Var)?;
    let property = names.binding_with_factory(factory, "p", DeclKind::Var)?;
    let initializer = names.binding_with_factory(factory, "f", DeclKind::Param)?;
    let result = names.binding_with_factory(factory, "result", DeclKind::Var)?;

    let accepted_defined = factory.binary(
        BinaryOperator::StrictNotEq,
        factory.reference(&accepted)?,
        factory.void_zero()?,
    )?;
    let accepted_not_function = factory.binary(
        BinaryOperator::StrictNotEq,
        factory.typeof_expression(factory.reference(&accepted)?)?,
        factory.string("function")?,
    )?;
    let invalid_accepted = factory.logical(
        LogicalOperator::And,
        accepted_defined,
        accepted_not_function,
    )?;
    let reject_accepted = factory.if_statement(
        invalid_accepted,
        factory.throw_type_error("Function expected")?,
        None,
    )?;
    let accept_return = factory.return_statement(Some(factory.reference(&accepted)?))?;
    let accept_declaration =
        factory.function_declaration(&accept, &[accepted], vec![reject_accepted, accept_return])?;

    let kind_value = factory.member(factory.reference(&context_in)?, "kind")?;
    let getter = factory.binary(
        BinaryOperator::StrictEq,
        factory.reference(&kind)?,
        factory.string("getter")?,
    )?;
    let setter = factory.binary(
        BinaryOperator::StrictEq,
        factory.reference(&kind)?,
        factory.string("setter")?,
    )?;
    let key_value = factory.conditional(
        getter,
        factory.string("get")?,
        factory.conditional(setter, factory.string("set")?, factory.string("value")?)?,
    )?;
    let kind_key = factory.variable_declaration(vec![
        (kind.clone(), Some(kind_value)),
        (key.clone(), Some(key_value)),
    ])?;

    let no_descriptor = factory.unary(
        UnaryOperator::LogicalNot,
        factory.reference(&descriptor_in)?,
    )?;
    let no_descriptor_and_ctor = factory.logical(
        LogicalOperator::And,
        no_descriptor,
        factory.reference(&ctor)?,
    )?;
    let static_key = factory.string("static")?;
    let is_static = factory.computed_member(factory.reference(&context_in)?, static_key)?;
    let ctor_target = factory.conditional(
        is_static,
        factory.reference(&ctor)?,
        factory.member(factory.reference(&ctor)?, "prototype")?,
    )?;
    let target_value = factory.conditional(no_descriptor_and_ctor, ctor_target, factory.null()?)?;

    let target_descriptor = factory.call(
        factory.member(factory.global("Object")?, "getOwnPropertyDescriptor")?,
        vec![
            factory.reference(&target)?,
            factory.member(factory.reference(&context_in)?, "name")?,
        ],
    )?;
    let target_or_empty = factory.conditional(
        factory.reference(&target)?,
        target_descriptor,
        factory.object(Vec::new())?,
    )?;
    let descriptor_value = factory.logical(
        LogicalOperator::Or,
        factory.reference(&descriptor_in)?,
        target_or_empty,
    )?;
    let target_descriptor_vars = factory.variable_declaration(vec![
        (target.clone(), Some(target_value)),
        (descriptor.clone(), Some(descriptor_value)),
        (temporary.clone(), None),
        (done.clone(), Some(factory.boolean(false)?)),
    ])?;

    let loop_initializer = factory.variable_declaration(vec![(
        index.clone(),
        Some(factory.binary(
            BinaryOperator::Sub,
            factory.member(factory.reference(&decorators)?, "length")?,
            factory.number(1.0)?,
        )?),
    )])?;
    let loop_test = factory.binary(
        BinaryOperator::GtEq,
        factory.reference(&index)?,
        factory.number(0.0)?,
    )?;
    let loop_update = factory.update_with(UpdateOperator::Decrement, factory.target(&index)?)?;

    let context_declaration =
        factory.variable_declaration(vec![(context.clone(), Some(factory.object(Vec::new())?))])?;
    let copy_key =
        factory.computed_member(factory.reference(&context)?, factory.reference(&property)?)?;
    let is_access = factory.binary(
        BinaryOperator::Eq,
        factory.reference(&property)?,
        factory.string("access")?,
    )?;
    let source_value = factory.computed_member(
        factory.reference(&context_in)?,
        factory.reference(&property)?,
    )?;
    let copied_value = factory.conditional(is_access, factory.object(Vec::new())?, source_value)?;
    let copy_assignment =
        factory.expression_statement(factory.assignment(copy_key, copied_value)?)?;
    let copy_property_binding = factory.variable_declaration(vec![(property.clone(), None)])?;
    let copy_context = factory.for_in(
        copy_property_binding,
        factory.reference(&context_in)?,
        copy_assignment,
    )?;

    let access_target = factory.computed_member(
        factory.member(factory.reference(&context)?, "access")?,
        factory.reference(&property)?,
    )?;
    let access_source = factory.computed_member(
        factory.member(factory.reference(&context_in)?, "access")?,
        factory.reference(&property)?,
    )?;
    let copy_access_assignment =
        factory.expression_statement(factory.assignment(access_target, access_source)?)?;
    let copy_access_binding = factory.variable_declaration(vec![(property.clone(), None)])?;
    let copy_access = factory.for_in(
        copy_access_binding,
        factory.member(factory.reference(&context_in)?, "access")?,
        copy_access_assignment,
    )?;

    let reject_late_initializer = factory.if_statement(
        factory.reference(&done)?,
        factory.throw_type_error("Cannot add initializers after decoration has completed")?,
        None,
    )?;
    let initializer_or_null = factory.logical(
        LogicalOperator::Or,
        factory.reference(&initializer)?,
        factory.null()?,
    )?;
    let accepted_initializer =
        factory.call(factory.reference(&accept)?, vec![initializer_or_null])?;
    let push_initializer = factory.expression_statement(factory.call(
        factory.member(factory.reference(&extra_initializers)?, "push")?,
        vec![accepted_initializer],
    )?)?;
    let add_initializer = factory.function_expression(
        &[initializer],
        vec![reject_late_initializer, push_initializer],
    )?;
    let install_add_initializer = factory.expression_statement(factory.assignment(
        factory.member(factory.reference(&context)?, "addInitializer")?,
        add_initializer,
    )?)?;

    let decorator =
        factory.computed_member(factory.reference(&decorators)?, factory.reference(&index)?)?;
    let stripped_decorator = factory.sequence(vec![factory.number(0.0)?, decorator])?;
    let accessor_kind = factory.binary(
        BinaryOperator::StrictEq,
        factory.reference(&kind)?,
        factory.string("accessor")?,
    )?;
    let accessor_get = factory.data_property(
        "get",
        factory.member(factory.reference(&descriptor)?, "get")?,
    )?;
    let accessor_set = factory.data_property(
        "set",
        factory.member(factory.reference(&descriptor)?, "set")?,
    )?;
    let accessor_value = factory.object(vec![accessor_get, accessor_set])?;
    let descriptor_value =
        factory.computed_member(factory.reference(&descriptor)?, factory.reference(&key)?)?;
    let decorated_value = factory.conditional(accessor_kind, accessor_value, descriptor_value)?;
    let call_decorator = factory.call(
        stripped_decorator,
        vec![decorated_value, factory.reference(&context)?],
    )?;
    let result_declaration =
        factory.variable_declaration(vec![(result.clone(), Some(call_decorator))])?;

    let accessor_result_undefined = factory.binary(
        BinaryOperator::StrictEq,
        factory.reference(&result)?,
        factory.void_zero()?,
    )?;
    let continue_undefined = factory.if_statement(
        accessor_result_undefined,
        factory.continue_statement()?,
        None,
    )?;
    let result_null = factory.binary(
        BinaryOperator::StrictEq,
        factory.reference(&result)?,
        factory.null()?,
    )?;
    let result_not_object = factory.binary(
        BinaryOperator::StrictNotEq,
        factory.typeof_expression(factory.reference(&result)?)?,
        factory.string("object")?,
    )?;
    let invalid_result = factory.logical(LogicalOperator::Or, result_null, result_not_object)?;
    let reject_result = factory.if_statement(
        invalid_result,
        factory.throw_type_error("Object expected")?,
        None,
    )?;

    let accessor_replacements = ["get", "set"]
        .into_iter()
        .map(|property_name| {
            let accept_call = factory.call(
                factory.reference(&accept)?,
                vec![factory.member(factory.reference(&result)?, property_name)?],
            )?;
            let assign_temporary = factory.assignment(factory.target(&temporary)?, accept_call)?;
            let assign_descriptor = factory.expression_statement(factory.assignment(
                factory.member(factory.reference(&descriptor)?, property_name)?,
                factory.reference(&temporary)?,
            )?)?;
            factory.if_statement(assign_temporary, assign_descriptor, None)
        })
        .collect::<Result<Vec<_>, TypedIrError>>()?;
    let accept_init = factory.call(
        factory.reference(&accept)?,
        vec![factory.member(factory.reference(&result)?, "init")?],
    )?;
    let assign_init = factory.assignment(factory.target(&temporary)?, accept_init)?;
    let unshift_init = factory.expression_statement(factory.call(
        factory.member(factory.reference(&initializers)?, "unshift")?,
        vec![factory.reference(&temporary)?],
    )?)?;
    let maybe_init = factory.if_statement(assign_init, unshift_init, None)?;
    let mut accessor_statements = vec![continue_undefined, reject_result];
    accessor_statements.extend(accessor_replacements);
    accessor_statements.push(maybe_init);
    let accessor_branch = factory.block(accessor_statements)?;

    let accepted_result = factory.call(
        factory.reference(&accept)?,
        vec![factory.reference(&result)?],
    )?;
    let assign_result = factory.assignment(factory.target(&temporary)?, accepted_result)?;
    let field_kind = factory.binary(
        BinaryOperator::StrictEq,
        factory.reference(&kind)?,
        factory.string("field")?,
    )?;
    let unshift_result = factory.expression_statement(factory.call(
        factory.member(factory.reference(&initializers)?, "unshift")?,
        vec![factory.reference(&temporary)?],
    )?)?;
    let set_descriptor = factory.expression_statement(factory.assignment(
        factory.computed_member(factory.reference(&descriptor)?, factory.reference(&key)?)?,
        factory.reference(&temporary)?,
    )?)?;
    let apply_normal = factory.if_statement(field_kind, unshift_result, Some(set_descriptor))?;
    let normal_branch = factory.if_statement(assign_result, apply_normal, None)?;
    let handle_result = factory.if_statement(
        factory.binary(
            BinaryOperator::StrictEq,
            factory.reference(&kind)?,
            factory.string("accessor")?,
        )?,
        accessor_branch,
        Some(normal_branch),
    )?;

    let loop_body = factory.block(vec![
        context_declaration,
        copy_context,
        copy_access,
        install_add_initializer,
        result_declaration,
        handle_result,
    ])?;
    let decorator_loop = factory.for_statement(
        Some(loop_initializer),
        Some(loop_test),
        Some(loop_update),
        loop_body,
    )?;

    let define_property = factory.expression_statement(factory.call(
        factory.member(factory.global("Object")?, "defineProperty")?,
        vec![
            factory.reference(&target)?,
            factory.member(factory.reference(&context_in)?, "name")?,
            factory.reference(&descriptor)?,
        ],
    )?)?;
    let maybe_define = factory.if_statement(factory.reference(&target)?, define_property, None)?;
    let set_done = factory.expression_statement(
        factory.assignment(factory.target(&done)?, factory.boolean(true)?)?,
    )?;
    let function = factory.function_expression(
        &[
            ctor,
            descriptor_in,
            decorators,
            context_in,
            initializers,
            extra_initializers,
        ],
        vec![
            accept_declaration,
            kind_key,
            target_descriptor_vars,
            decorator_loop,
            maybe_define,
            set_done,
        ],
    )?;
    factory.variable_declaration(vec![(helper.clone(), Some(function))])
}

#[cfg(test)]
mod tests {
    use wake_common::Interner;
    use wake_ecma_ast::SourceType;
    use wake_ecma_parser::parse;

    use super::*;

    fn lower(source: &str, source_type: SourceType) -> TypedProgram {
        let interner = Interner::new();
        let parsed = parse(source, &interner, source_type);
        assert!(
            !parsed.has_errors(),
            "decorator fixture did not parse:\n{source}\n{:?}",
            parsed.diagnostics
        );
        parsed.module.with_ast(|program| {
            let semantic = wake_ecma_semantic::analyze(program);
            TypedProgram::lower(program, &interner, Some(&semantic))
                .expect("decorator fixture should lower to typed IR")
        })
    }

    #[test]
    fn materializes_private_computed_and_auto_accessor_elements_without_residual_syntax() {
        let mut program = lower(
            concat!(
                "function dec(value,context){return value}\n",
                "@dec class Supported {\n",
                " @dec field=1; @dec #privateField=2;\n",
                " @dec method(value){return value+this.#privateField}\n",
                " @dec get value(){return this.field}\n",
                " @dec set value(next){this.field=next}\n",
                " @dec static staticMethod(){return 5}\n",
                "}\n",
                "@dec class Complete extends Base {\n",
                " @dec #privateMethod(value){return value + 1}\n",
                " @dec get #privateValue(){return this.#privateField}\n",
                " @dec set #privateValue(next){this.#privateField=next}\n",
                " @dec accessor automatic=3;\n",
                " @dec accessor #privateAutomatic=4;\n",
                "}\n"
            ),
            SourceType::TypeScript,
        );

        let report = materialize_decorators(&mut program).expect("complete decorator lowering");

        assert_eq!(report.decorated_classes, 2);
        assert!(report.es_decorate_name.is_some());
        assert!(report.run_initializers_name.is_some());
        let residual_decorator_lists = program
            .preorder()
            .expect("preorder")
            .into_iter()
            .filter(|&node| {
                matches!(
                    program.node(node).expect("node").data(),
                    IrNodeData::Class { decorators, .. }
                        | IrNodeData::MethodDefinition { decorators, .. }
                        | IrNodeData::PropertyDefinition { decorators, .. }
                        if !list_items(&program, *decorators).is_empty()
                )
            })
            .collect::<Vec<_>>();
        assert!(
            residual_decorator_lists.is_empty(),
            "successful lowering must never leave decorator syntax in the emitted IR"
        );
        assert!(program.preorder().expect("preorder").iter().any(|&node| {
            matches!(
                program.node(node).expect("node").origin(),
                IrOrigin::Synthetic { anchor: None, .. }
            )
        }));
        program
            .validate()
            .expect("materialized decorators validate");
    }

    #[test]
    fn derived_constructor_with_complex_super_is_an_explicit_transactional_error() {
        let mut expression = lower(
            "class C extends B{@dec field=1;constructor(flag){flag ? super(1) : super(2)}}",
            SourceType::TypeScript,
        );
        let before = expression.fingerprint();
        let expression_error = materialize_decorators(&mut expression)
            .expect_err("expression-position super must be diagnosed");
        assert!(matches!(
            expression_error,
            DecoratorLoweringError::Unsupported { .. }
        ));
        assert!(
            expression_error
                .to_string()
                .contains("derived constructor requires expression-position super initialization")
        );
        assert_eq!(
            expression.fingerprint(),
            before,
            "lowering is transactional"
        );

        let mut branch = lower(
            "class C extends B{@dec field=1;constructor(flag){if(flag){super(1)}else{super(2)}}}",
            SourceType::TypeScript,
        );
        let branch_error =
            materialize_decorators(&mut branch).expect_err("branch-position super must fail");
        assert!(matches!(
            branch_error,
            DecoratorLoweringError::Unsupported { .. }
        ));

        let mut private_super = lower(
            "class C extends B{@dec #method(){return super.method()}}",
            SourceType::TypeScript,
        );
        let private_super_error = materialize_decorators(&mut private_super)
            .expect_err("moving a private method with lexical super must fail");
        assert!(
            private_super_error
                .to_string()
                .contains("decorated private method/accessor contains lexical super")
        );
    }

    #[test]
    fn evaluation_scope_hazards_are_explicit_errors_instead_of_raw_decorators() {
        let mut suspension = lower(
            "async function outer(){class C{@dec [await key()]=1}}",
            SourceType::TypeScript,
        );
        let suspension_error = materialize_decorators(&mut suspension)
            .expect_err("lexical await cannot enter the synchronous class wrapper");
        assert!(
            suspension_error
                .to_string()
                .contains("decorator/class-key evaluation contains lexical await or yield")
        );

        let mut direct_eval = lower(
            "class C{@eval('globalThis.seen=1') field=1}",
            SourceType::TypeScript,
        );
        let eval_error = materialize_decorators(&mut direct_eval)
            .expect_err("direct eval must not observe the generated wrapper scope");
        assert!(direct_eval
            .preorder()
            .expect("transactional source")
            .into_iter()
            .any(|node| matches!(direct_eval.node(node).map(|node| node.data()), Some(IrNodeData::Class { decorators, .. }) if !list_items(&direct_eval, *decorators).is_empty()) || matches!(direct_eval.node(node).map(|node| node.data()), Some(IrNodeData::PropertyDefinition { decorators, .. }) if !list_items(&direct_eval, *decorators).is_empty())));
        assert!(
            eval_error
                .to_string()
                .contains("decorator/class-key evaluation contains direct eval")
        );

        let mut member_eval = lower(
            "class C{@dec method(){return eval('typeof _classThis')}}",
            SourceType::TypeScript,
        );
        let before = member_eval.fingerprint();
        let member_eval_error = materialize_decorators(&mut member_eval)
            .expect_err("the generated wrapper must not change a direct eval environment");
        assert!(member_eval_error.to_string().contains(
            "decorated class member contains direct eval whose visible environment would change"
        ));
        assert_eq!(
            member_eval.fingerprint(),
            before,
            "lowering is transactional"
        );
    }

    #[test]
    fn computed_decorator_and_all_computed_keys_are_owned_by_the_evaluation_prelude() {
        let mut program = lower(
            concat!(
                "function make(){return value=>value}function key(value){return value}",
                "class C{@make() [key('decorated')]=1;[key('plain')]=2}"
            ),
            SourceType::TypeScript,
        );

        let report = materialize_decorators(&mut program).expect("computed decorator lowering");
        assert_eq!(report.decorated_classes, 1);
        let computed_temporaries = program
            .symbols()
            .iter()
            .filter(|symbol| symbol.original_name().starts_with("_computedKey"))
            .count();
        assert_eq!(
            computed_temporaries, 2,
            "decorated and undecorated keys both require ordered temporaries"
        );
        assert!(
            program
                .preorder()
                .expect("preorder")
                .into_iter()
                .all(|node| {
                    match program.node(node).expect("node").data() {
                        IrNodeData::Class { decorators, .. }
                        | IrNodeData::MethodDefinition { decorators, .. }
                        | IrNodeData::PropertyDefinition { decorators, .. } => {
                            list_items(&program, *decorators).is_empty()
                        }
                        _ => true,
                    }
                })
        );
        program.validate().expect("computed lowering validates");
    }

    #[test]
    fn lowers_declaration_expression_and_default_export_contexts() {
        let mut program = lower(
            concat!(
                "function dec(value){return value}\n",
                "@dec class Declared{@dec field=1}\n",
                "const Expression=class Inner{@dec method(){return 2}};\n",
                "@dec export default class Defaulted{@dec field=3};"
            ),
            SourceType::TypeScript,
        );

        let report = materialize_decorators(&mut program).expect("context lowering");
        assert_eq!(report.decorated_classes, 3);
        for node in program.preorder().expect("preorder") {
            let decorators = match program.node(node).expect("node").data() {
                IrNodeData::Class { decorators, .. }
                | IrNodeData::MethodDefinition { decorators, .. }
                | IrNodeData::PropertyDefinition { decorators, .. } => Some(*decorators),
                _ => None,
            };
            if let Some(decorators) = decorators {
                assert!(list_items(&program, decorators).is_empty());
            }
        }
        assert!(program.preorder().expect("preorder").iter().any(|&node| {
            matches!(
                program.node(node).expect("node").data(),
                IrNodeData::ExportDefaultDeclaration {
                    kind: ExportDefaultValueKind::Expression,
                    ..
                }
            )
        }));
        program.validate().expect("context result validates");
    }

    #[test]
    fn helper_names_avoid_original_and_emitted_collisions() {
        let mut program = lower(
            "let __esDecorate=1,__runInitializers=2,_instance_value_accessor_storage=3;class Supported{@dec field=0}class C{@dec accessor value=1}",
            SourceType::TypeScript,
        );
        let colliding = program
            .nodes()
            .iter()
            .filter_map(|node| {
                let IrNodeData::Name { name } = node.data() else {
                    return None;
                };
                (program.name(*name)?.original() == "_instance_value_accessor_storage")
                    .then_some(*name)
            })
            .collect::<Vec<_>>();
        for name in colliding {
            program
                .set_emitted_name(name, "__esDecorate$1")
                .expect("fixture rename");
        }

        let report = materialize_decorators(&mut program).expect("collision-safe lowering");
        let helper_names = [
            report.es_decorate_name.expect("decorate helper"),
            report.run_initializers_name.expect("initializer helper"),
        ];
        assert!(!helper_names.iter().any(|name| matches!(
            name.as_str(),
            "__esDecorate" | "__runInitializers" | "__esDecorate$1"
        )));
    }

    #[test]
    fn decorator_free_program_keeps_the_owned_arena_in_place() {
        let mut program = lower("class Plain{method(){return 1}}", SourceType::Script);
        let nodes = program.nodes().as_ptr();
        let lists = program.lists().as_ptr();
        let names = program.names().as_ptr();

        let report = materialize_decorators(&mut program).expect("no-op decorator lowering");

        assert_eq!(report, DecoratorLoweringReport::default());
        assert_eq!(program.nodes().as_ptr(), nodes);
        assert_eq!(program.lists().as_ptr(), lists);
        assert_eq!(program.names().as_ptr(), names);
    }
}
