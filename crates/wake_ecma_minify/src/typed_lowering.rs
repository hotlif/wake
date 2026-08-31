//! Materialize parser runtime-helper metadata as ordinary owned typed-IR syntax.
//!
//! Parser lowering records which runtime helpers a program needs. Keeping those helpers as
//! emitter-side string templates would give code generation a second semantic lowering path, so
//! this pass turns the metadata into regular function/statement subtrees before emission.

use std::cell::RefCell;
use std::collections::HashSet;

use wake_ecma_ast::{
    AssignmentOperator, BinaryOperator, LogicalOperator, PropertyKind, UnaryOperator,
    UpdateOperator, VarKind,
};
use wake_ecma_semantic::{DeclKind, SymbolId};

use crate::typed_ir::{
    ChildRole, DerivedOriginKind, ForInitializerKind, FunctionContext, IrNodeData, IrOrigin,
    IrPropertyKey, NameId, NameRole, NameSyntax, NodeId, PropertyKeyKind, SyntheticOriginKind,
    TypedIrError, TypedProgram,
};

/// Names assigned to materialized runtime helpers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RuntimeHelperReport {
    pub spread_name: Option<String>,
    pub object_spread_name: Option<String>,
    pub for_of_name: Option<String>,
}

/// Replace all runtime-helper metadata on `program` with owned typed-IR statements.
pub fn materialize_runtime_helpers(
    program: &mut TypedProgram,
) -> Result<RuntimeHelperReport, TypedIrError> {
    if matches!(
        program.node(program.root()).map(|node| node.data()),
        Some(IrNodeData::Program {
            spread_helper: None,
            object_spread_helper: None,
            for_of_helper: None,
            ..
        })
    ) {
        return Ok(RuntimeHelperReport::default());
    }
    let mut working = program.clone();
    let report = materialize_runtime_helpers_inner(&mut working)?;
    *program = working;
    Ok(report)
}

fn materialize_runtime_helpers_inner(
    program: &mut TypedProgram,
) -> Result<RuntimeHelperReport, TypedIrError> {
    let root = program.root();
    let IrNodeData::Program {
        source_type,
        strict,
        spread_helper,
        object_spread_helper,
        for_of_helper,
        body,
    } = program
        .node(root)
        .ok_or_else(|| ir_error(Some(root), "typed program root is missing"))?
        .data()
        .clone()
    else {
        return Err(ir_error(
            Some(root),
            "typed program root is not Program syntax",
        ));
    };

    if spread_helper.is_none() && object_spread_helper.is_none() && for_of_helper.is_none() {
        return Ok(RuntimeHelperReport::default());
    }

    let mut reserved = program
        .names()
        .iter()
        .flat_map(|name| [name.original().to_owned(), name.emitted().to_owned()])
        .collect::<HashSet<_>>();
    let mut inserted = Vec::new();
    let mut report = RuntimeHelperReport::default();

    if let Some(parser_name) = spread_helper.as_deref() {
        let binding = allocate_helper_binding(program, parser_name, &mut reserved)?;
        retarget_parser_helper_references(program, parser_name, &binding)?;
        inserted.push(SyntheticFactory::new(program).spread_helper(&binding)?);
        report.spread_name = Some(binding.name);
    }
    if let Some(parser_name) = object_spread_helper.as_deref() {
        let binding = allocate_helper_binding(program, parser_name, &mut reserved)?;
        retarget_parser_helper_references(program, parser_name, &binding)?;
        inserted.extend(SyntheticFactory::new(program).object_spread_helper(&binding)?);
        report.object_spread_name = Some(binding.name);
    }
    if let Some(parser_name) = for_of_helper.as_deref() {
        let binding = allocate_helper_binding(program, parser_name, &mut reserved)?;
        retarget_parser_helper_references(program, parser_name, &binding)?;
        inserted.push(SyntheticFactory::new(program).for_of_helper(&binding)?);
        report.for_of_name = Some(binding.name);
    }

    let body_items = program
        .list(body)
        .ok_or_else(|| ir_error(Some(root), "program body list is missing"))?
        .items();
    let directive_count = body_items
        .iter()
        .take_while(|&&statement| is_directive(program, statement))
        .count();
    program.splice_list(body, directive_count..directive_count, &inserted)?;
    program.replace_node_data(
        root,
        IrNodeData::Program {
            source_type,
            strict,
            spread_helper: None,
            object_spread_helper: None,
            for_of_helper: None,
            body,
        },
    )?;
    program.validate()?;
    Ok(report)
}

fn ir_error(node: Option<NodeId>, message: impl Into<String>) -> TypedIrError {
    TypedIrError {
        node,
        message: message.into(),
    }
}

fn is_directive(program: &TypedProgram, statement: NodeId) -> bool {
    matches!(
        program.node(statement).map(|node| node.data()),
        Some(IrNodeData::ExpressionStatement {
            directive: true,
            ..
        })
    )
}

#[derive(Clone, Debug)]
pub(crate) struct Binding {
    pub(crate) name: String,
    pub(crate) symbol: SymbolId,
}

fn allocate_helper_binding(
    program: &mut TypedProgram,
    requested: &str,
    reserved: &mut HashSet<String>,
) -> Result<Binding, TypedIrError> {
    let mut candidate = requested.to_owned();
    let mut suffix = 1_u32;
    while reserved.contains(&candidate) {
        candidate = format!("{requested}${suffix}");
        suffix = suffix
            .checked_add(1)
            .ok_or_else(|| ir_error(None, "runtime helper name suffix overflow"))?;
    }
    reserved.insert(candidate.clone());
    let symbol = program.allocate_symbol(candidate.clone(), DeclKind::Function)?;
    Ok(Binding {
        name: candidate,
        symbol,
    })
}

fn retarget_parser_helper_references(
    program: &mut TypedProgram,
    parser_name: &str,
    binding: &Binding,
) -> Result<(), TypedIrError> {
    let occurrences = program
        .nodes()
        .iter()
        .filter_map(|node| {
            let IrNodeData::Name { name } = node.data() else {
                return None;
            };
            let record = program.name(*name)?;
            (record.original() == parser_name
                && record.role() == NameRole::Reference
                && record.syntax() == NameSyntax::Identifier
                // Parser helper names are fresh spellings absent from source. Their generated
                // references remain unresolved in the initial semantic model, even where the
                // transform anchors the identifier to a non-dummy source expression span.
                && record.symbol().is_none())
            .then_some((node.id(), node.origin(), *name))
        })
        .collect::<Vec<(NodeId, IrOrigin, NameId)>>();
    for (node, origin, occurrence) in occurrences {
        program.set_emitted_name(occurrence, binding.name.clone())?;
        program.set_name_symbol(occurrence, Some(binding.symbol))?;
        let anchor = match origin {
            IrOrigin::Source(span) => Some(span),
            IrOrigin::Derived { anchor, .. } | IrOrigin::Synthetic { anchor, .. } => anchor,
        };
        program.set_origin(
            node,
            IrOrigin::Derived {
                anchor,
                kind: DerivedOriginKind::ParserLowering,
            },
        )?;
    }
    Ok(())
}

pub(crate) const HELPER_ORIGIN: IrOrigin = IrOrigin::Synthetic {
    anchor: None,
    kind: SyntheticOriginKind::Optimization,
};

/// Small structural constructor used only for built-in helper syntax. Every operation appends an
/// owned node; it never parses or splices JavaScript text.
pub(crate) struct SyntheticFactory<'a> {
    pub(crate) program: RefCell<&'a mut TypedProgram>,
}

impl<'a> SyntheticFactory<'a> {
    pub(crate) fn new(program: &'a mut TypedProgram) -> Self {
        Self {
            program: RefCell::new(program),
        }
    }

    pub(crate) fn symbol(&self, name: &str, kind: DeclKind) -> Result<Binding, TypedIrError> {
        let symbol = self
            .program
            .borrow_mut()
            .allocate_symbol(name.to_owned(), kind)?;
        Ok(Binding {
            name: name.to_owned(),
            symbol,
        })
    }

    pub(crate) fn leaf(&self, data: IrNodeData) -> Result<NodeId, TypedIrError> {
        self.program
            .borrow_mut()
            .append_detached_leaf(data, HELPER_ORIGIN)
    }

    pub(crate) fn name_node(
        &self,
        spelling: &str,
        role: NameRole,
        symbol: Option<SymbolId>,
    ) -> Result<NodeId, TypedIrError> {
        self.program.borrow_mut().append_detached_name(
            spelling,
            role,
            NameSyntax::Identifier,
            symbol,
            HELPER_ORIGIN,
        )
    }

    pub(crate) fn identifier(
        &self,
        spelling: &str,
        role: NameRole,
        symbol: Option<SymbolId>,
    ) -> Result<NodeId, TypedIrError> {
        let name = self.name_node(spelling, role, symbol)?;
        self.program
            .borrow_mut()
            .append_detached_node_with(HELPER_ORIGIN, |_| Ok(IrNodeData::Identifier { name }))
    }

    pub(crate) fn reference(&self, binding: &Binding) -> Result<NodeId, TypedIrError> {
        self.identifier(&binding.name, NameRole::Reference, Some(binding.symbol))
    }

    pub(crate) fn target(&self, binding: &Binding) -> Result<NodeId, TypedIrError> {
        self.identifier(
            &binding.name,
            NameRole::AssignmentTarget,
            Some(binding.symbol),
        )
    }

    pub(crate) fn global(&self, spelling: &str) -> Result<NodeId, TypedIrError> {
        self.identifier(spelling, NameRole::Reference, None)
    }

    pub(crate) fn binding_pattern(&self, binding: &Binding) -> Result<NodeId, TypedIrError> {
        self.identifier(&binding.name, NameRole::Binding, Some(binding.symbol))
    }

    pub(crate) fn number(&self, value: f64) -> Result<NodeId, TypedIrError> {
        self.leaf(IrNodeData::NumberLiteral { value })
    }

    pub(crate) fn string(&self, value: &str) -> Result<NodeId, TypedIrError> {
        self.leaf(IrNodeData::StringLiteral {
            value: value.to_owned(),
        })
    }

    pub(crate) fn boolean(&self, value: bool) -> Result<NodeId, TypedIrError> {
        self.leaf(IrNodeData::BooleanLiteral { value })
    }

    pub(crate) fn null(&self) -> Result<NodeId, TypedIrError> {
        self.leaf(IrNodeData::NullLiteral)
    }

    pub(crate) fn array(&self, elements: Vec<NodeId>) -> Result<NodeId, TypedIrError> {
        self.program
            .borrow_mut()
            .append_detached_node_with(HELPER_ORIGIN, |builder| {
                let elements = builder.list(ChildRole::ArrayElements, elements)?;
                Ok(IrNodeData::ArrayExpression { elements })
            })
    }

    pub(crate) fn object(&self, members: Vec<NodeId>) -> Result<NodeId, TypedIrError> {
        self.program
            .borrow_mut()
            .append_detached_node_with(HELPER_ORIGIN, |builder| {
                let members = builder.list(ChildRole::ObjectMembers, members)?;
                Ok(IrNodeData::ObjectExpression { members })
            })
    }

    pub(crate) fn data_property(&self, key: &str, value: NodeId) -> Result<NodeId, TypedIrError> {
        let key_node = self.name_node(key, NameRole::Property, None)?;
        self.program
            .borrow_mut()
            .append_detached_node_with(HELPER_ORIGIN, |_| {
                Ok(IrNodeData::ObjectProperty {
                    key: IrPropertyKey {
                        kind: PropertyKeyKind::Identifier,
                        value: key_node,
                    },
                    value,
                    kind: PropertyKind::Init,
                    method: false,
                    shorthand: false,
                    computed: false,
                    prototype_setter: false,
                })
            })
    }

    pub(crate) fn member(&self, object: NodeId, property: &str) -> Result<NodeId, TypedIrError> {
        let property = self.name_node(property, NameRole::Property, None)?;
        self.program
            .borrow_mut()
            .append_detached_node_with(HELPER_ORIGIN, |_| {
                Ok(IrNodeData::MemberExpression {
                    object,
                    property,
                    property_kind: PropertyKeyKind::Identifier,
                    optional: false,
                })
            })
    }

    pub(crate) fn computed_member(
        &self,
        object: NodeId,
        property: NodeId,
    ) -> Result<NodeId, TypedIrError> {
        self.program
            .borrow_mut()
            .append_detached_node_with(HELPER_ORIGIN, |_| {
                Ok(IrNodeData::MemberExpression {
                    object,
                    property,
                    property_kind: PropertyKeyKind::Computed,
                    optional: false,
                })
            })
    }

    pub(crate) fn call(
        &self,
        callee: NodeId,
        arguments: Vec<NodeId>,
    ) -> Result<NodeId, TypedIrError> {
        self.program
            .borrow_mut()
            .append_detached_node_with(HELPER_ORIGIN, |builder| {
                let arguments = builder.list(ChildRole::Arguments, arguments)?;
                Ok(IrNodeData::CallExpression {
                    callee,
                    arguments,
                    optional: false,
                })
            })
    }

    pub(crate) fn new_expression(
        &self,
        callee: NodeId,
        arguments: Vec<NodeId>,
    ) -> Result<NodeId, TypedIrError> {
        self.program
            .borrow_mut()
            .append_detached_node_with(HELPER_ORIGIN, |builder| {
                let arguments = builder.list(ChildRole::Arguments, arguments)?;
                Ok(IrNodeData::NewExpression { callee, arguments })
            })
    }

    pub(crate) fn unary(
        &self,
        operator: UnaryOperator,
        argument: NodeId,
    ) -> Result<NodeId, TypedIrError> {
        self.program
            .borrow_mut()
            .append_detached_node_with(HELPER_ORIGIN, |_| {
                Ok(IrNodeData::UnaryExpression { operator, argument })
            })
    }

    pub(crate) fn binary(
        &self,
        operator: BinaryOperator,
        left: NodeId,
        right: NodeId,
    ) -> Result<NodeId, TypedIrError> {
        self.program
            .borrow_mut()
            .append_detached_node_with(HELPER_ORIGIN, |_| {
                Ok(IrNodeData::BinaryExpression {
                    operator,
                    left,
                    right,
                })
            })
    }

    pub(crate) fn logical(
        &self,
        operator: LogicalOperator,
        left: NodeId,
        right: NodeId,
    ) -> Result<NodeId, TypedIrError> {
        self.program
            .borrow_mut()
            .append_detached_node_with(HELPER_ORIGIN, |_| {
                Ok(IrNodeData::LogicalExpression {
                    operator,
                    left,
                    right,
                })
            })
    }

    pub(crate) fn conditional(
        &self,
        test: NodeId,
        consequent: NodeId,
        alternate: NodeId,
    ) -> Result<NodeId, TypedIrError> {
        self.program
            .borrow_mut()
            .append_detached_node_with(HELPER_ORIGIN, |_| {
                Ok(IrNodeData::ConditionalExpression {
                    test,
                    consequent,
                    alternate,
                })
            })
    }

    pub(crate) fn assignment(&self, left: NodeId, right: NodeId) -> Result<NodeId, TypedIrError> {
        self.program
            .borrow_mut()
            .append_detached_node_with(HELPER_ORIGIN, |_| {
                Ok(IrNodeData::AssignmentExpression {
                    operator: AssignmentOperator::Assign,
                    left,
                    right,
                })
            })
    }

    pub(crate) fn update(&self, argument: NodeId) -> Result<NodeId, TypedIrError> {
        self.program
            .borrow_mut()
            .append_detached_node_with(HELPER_ORIGIN, |_| {
                Ok(IrNodeData::UpdateExpression {
                    operator: UpdateOperator::Increment,
                    prefix: false,
                    argument,
                })
            })
    }

    pub(crate) fn prefix_update(&self, argument: NodeId) -> Result<NodeId, TypedIrError> {
        self.program
            .borrow_mut()
            .append_detached_node_with(HELPER_ORIGIN, |_| {
                Ok(IrNodeData::UpdateExpression {
                    operator: UpdateOperator::Increment,
                    prefix: true,
                    argument,
                })
            })
    }

    pub(crate) fn expression_statement(&self, expression: NodeId) -> Result<NodeId, TypedIrError> {
        self.program
            .borrow_mut()
            .append_detached_node_with(HELPER_ORIGIN, |_| {
                Ok(IrNodeData::ExpressionStatement {
                    expression,
                    directive: false,
                })
            })
    }

    pub(crate) fn return_statement(
        &self,
        argument: Option<NodeId>,
    ) -> Result<NodeId, TypedIrError> {
        self.program
            .borrow_mut()
            .append_detached_node_with(HELPER_ORIGIN, |_| {
                Ok(IrNodeData::ReturnStatement { argument })
            })
    }

    pub(crate) fn throw_type_error(&self, message: &str) -> Result<NodeId, TypedIrError> {
        let error = self.global("TypeError")?;
        let message = self.string(message)?;
        let argument = self.new_expression(error, vec![message])?;
        self.program
            .borrow_mut()
            .append_detached_node_with(HELPER_ORIGIN, |_| {
                Ok(IrNodeData::ThrowStatement { argument })
            })
    }

    pub(crate) fn throw_statement(&self, argument: NodeId) -> Result<NodeId, TypedIrError> {
        self.program
            .borrow_mut()
            .append_detached_node_with(HELPER_ORIGIN, |_| {
                Ok(IrNodeData::ThrowStatement { argument })
            })
    }

    pub(crate) fn break_statement(&self) -> Result<NodeId, TypedIrError> {
        self.leaf(IrNodeData::BreakStatement { label: None })
    }

    pub(crate) fn continue_statement(&self) -> Result<NodeId, TypedIrError> {
        self.leaf(IrNodeData::ContinueStatement { label: None })
    }

    pub(crate) fn block(&self, statements: Vec<NodeId>) -> Result<NodeId, TypedIrError> {
        self.program
            .borrow_mut()
            .append_detached_node_with(HELPER_ORIGIN, |builder| {
                let body = builder.list(ChildRole::BlockBody, statements)?;
                Ok(IrNodeData::Block { body })
            })
    }

    pub(crate) fn if_statement(
        &self,
        test: NodeId,
        consequent: NodeId,
        alternate: Option<NodeId>,
    ) -> Result<NodeId, TypedIrError> {
        self.program
            .borrow_mut()
            .append_detached_node_with(HELPER_ORIGIN, |_| {
                Ok(IrNodeData::IfStatement {
                    test,
                    consequent,
                    alternate,
                })
            })
    }

    pub(crate) fn while_statement(
        &self,
        test: NodeId,
        body: NodeId,
    ) -> Result<NodeId, TypedIrError> {
        self.program
            .borrow_mut()
            .append_detached_node_with(HELPER_ORIGIN, |_| {
                Ok(IrNodeData::WhileStatement { test, body })
            })
    }

    pub(crate) fn for_statement(
        &self,
        initializer: Option<NodeId>,
        test: Option<NodeId>,
        update: Option<NodeId>,
        body: NodeId,
    ) -> Result<NodeId, TypedIrError> {
        self.program
            .borrow_mut()
            .append_detached_node_with(HELPER_ORIGIN, |_| {
                Ok(IrNodeData::ForStatement {
                    initializer,
                    initializer_kind: initializer.map(|_| ForInitializerKind::Variable),
                    test,
                    update,
                    body,
                })
            })
    }

    pub(crate) fn variable_declaration(
        &self,
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
                Ok(IrNodeData::VariableDeclaration {
                    kind: VarKind::Var,
                    declarations,
                })
            })
    }

    pub(crate) fn function_body(&self, statements: Vec<NodeId>) -> Result<NodeId, TypedIrError> {
        self.program
            .borrow_mut()
            .append_detached_node_with(HELPER_ORIGIN, |builder| {
                let statements = builder.list(ChildRole::FunctionStatements, statements)?;
                Ok(IrNodeData::FunctionBody {
                    statements,
                    strict: false,
                })
            })
    }

    pub(crate) fn function(
        &self,
        context: FunctionContext,
        name: Option<&Binding>,
        parameters: &[Binding],
        statements: Vec<NodeId>,
    ) -> Result<NodeId, TypedIrError> {
        let name = name
            .map(|binding| {
                self.name_node(&binding.name, NameRole::FunctionName, Some(binding.symbol))
            })
            .transpose()?;
        let parameters = parameters
            .iter()
            .map(|binding| self.binding_pattern(binding))
            .collect::<Result<Vec<_>, _>>()?;
        let body = self.function_body(statements)?;
        self.program
            .borrow_mut()
            .append_detached_node_with(HELPER_ORIGIN, |builder| {
                let parameters = builder.list(ChildRole::FunctionParameters, parameters)?;
                Ok(IrNodeData::Function {
                    context,
                    name,
                    parameters,
                    body: Some(body),
                    is_async: false,
                    is_generator: false,
                })
            })
    }

    pub(crate) fn function_expression(
        &self,
        parameters: &[Binding],
        statements: Vec<NodeId>,
    ) -> Result<NodeId, TypedIrError> {
        self.function(FunctionContext::Expression, None, parameters, statements)
    }

    pub(crate) fn function_declaration(
        &self,
        name: &Binding,
        parameters: &[Binding],
        statements: Vec<NodeId>,
    ) -> Result<NodeId, TypedIrError> {
        self.function(
            FunctionContext::Declaration,
            Some(name),
            parameters,
            statements,
        )
    }

    pub(crate) fn void_zero(&self) -> Result<NodeId, TypedIrError> {
        let zero = self.number(0.0)?;
        self.unary(UnaryOperator::Void, zero)
    }

    pub(crate) fn typeof_expression(&self, argument: NodeId) -> Result<NodeId, TypedIrError> {
        self.unary(UnaryOperator::Typeof, argument)
    }

    fn spread_helper(&self, helper: &Binding) -> Result<NodeId, TypedIrError> {
        let value = self.symbol("value", DeclKind::Param)?;
        let limit = self.symbol("limit", DeclKind::Param)?;
        let method = self.symbol("method", DeclKind::Var)?;
        let iterator = self.symbol("iterator", DeclKind::Var)?;
        let result = self.symbol("result", DeclKind::Var)?;
        let step = self.symbol("step", DeclKind::Var)?;
        let error = self.symbol("error", DeclKind::Var)?;
        let done = self.symbol("done", DeclKind::Var)?;
        let caught = self.symbol("caught", DeclKind::CatchParam)?;
        let chars = self.symbol("chars", DeclKind::Var)?;
        let index = self.symbol("index", DeclKind::Var)?;
        let first = self.symbol("first", DeclKind::Var)?;
        let second = self.symbol("second", DeclKind::Var)?;

        let array_is_array = self.member(self.global("Array")?, "isArray")?;
        let is_array = self.call(array_is_array, vec![self.reference(&value)?])?;
        let slice_no_limit =
            self.call(self.member(self.reference(&value)?, "slice")?, Vec::new())?;
        let slice_limit = self.call(
            self.member(self.reference(&value)?, "slice")?,
            vec![self.number(0.0)?, self.reference(&limit)?],
        )?;
        let limit_undefined = self.binary(
            BinaryOperator::StrictEq,
            self.reference(&limit)?,
            self.void_zero()?,
        )?;
        let array_result = self.conditional(limit_undefined, slice_no_limit, slice_limit)?;
        let return_array = self.return_statement(Some(array_result))?;
        let array_branch = self.if_statement(is_array, return_array, None)?;

        let nullish = self.binary(BinaryOperator::Eq, self.reference(&value)?, self.null()?)?;
        let throw_null = self.throw_type_error("Cannot spread null or undefined")?;
        let null_branch = self.if_statement(nullish, throw_null, None)?;

        let symbol_defined = self.binary(
            BinaryOperator::StrictNotEq,
            self.typeof_expression(self.global("Symbol")?)?,
            self.string("undefined")?,
        )?;
        let iterator_property = self.member(self.global("Symbol")?, "iterator")?;
        let value_iterator = self.computed_member(self.reference(&value)?, iterator_property)?;
        let method_initializer =
            self.logical(LogicalOperator::And, symbol_defined, value_iterator)?;
        let method_declaration =
            self.variable_declaration(vec![(method.clone(), Some(method_initializer))])?;

        let method_call = self.call(
            self.member(self.reference(&method)?, "call")?,
            vec![self.reference(&value)?],
        )?;
        let iterable_declarations = self.variable_declaration(vec![
            (iterator.clone(), Some(method_call)),
            (result.clone(), Some(self.array(Vec::new())?)),
            (step.clone(), None),
            (error.clone(), None),
            (done.clone(), Some(self.boolean(false)?)),
        ])?;

        let while_limit_undefined = self.binary(
            BinaryOperator::StrictEq,
            self.reference(&limit)?,
            self.void_zero()?,
        )?;
        let result_length = self.member(self.reference(&result)?, "length")?;
        let below_limit =
            self.binary(BinaryOperator::Lt, result_length, self.reference(&limit)?)?;
        let while_test = self.logical(LogicalOperator::Or, while_limit_undefined, below_limit)?;
        let next_call = self.call(self.member(self.reference(&iterator)?, "next")?, Vec::new())?;
        let set_step =
            self.expression_statement(self.assignment(self.target(&step)?, next_call)?)?;
        let done_test = self.member(self.reference(&step)?, "done")?;
        let set_done =
            self.expression_statement(self.assignment(self.target(&done)?, self.boolean(true)?)?)?;
        let done_block = self.block(vec![set_done, self.break_statement()?])?;
        let stop = self.if_statement(done_test, done_block, None)?;
        let push_value = self.call(
            self.member(self.reference(&result)?, "push")?,
            vec![self.member(self.reference(&step)?, "value")?],
        )?;
        let push_value = self.expression_statement(push_value)?;
        let while_body = self.block(vec![set_step, stop, push_value])?;
        let while_loop = self.while_statement(while_test, while_body)?;
        let try_block = self.block(vec![while_loop])?;

        let catch_assignment = self.expression_statement(
            self.assignment(self.target(&error)?, self.reference(&caught)?)?,
        )?;
        let catch_body = self.block(vec![catch_assignment])?;
        let catch_parameter = self.binding_pattern(&caught)?;
        let handler = self
            .program
            .borrow_mut()
            .append_detached_node_with(HELPER_ORIGIN, |_| {
                Ok(IrNodeData::CatchClause {
                    parameter: Some(catch_parameter),
                    body: catch_body,
                })
            })?;

        let not_done = self.unary(UnaryOperator::LogicalNot, self.reference(&done)?)?;
        let return_member = self.member(self.reference(&iterator)?, "return")?;
        let has_return = self.logical(LogicalOperator::And, not_done, return_member)?;
        let call_return = self.call(
            self.member(self.reference(&iterator)?, "return")?,
            Vec::new(),
        )?;
        let call_return = self.expression_statement(call_return)?;
        let close_if = self.if_statement(has_return, call_return, None)?;
        let close_try = self.block(vec![close_if])?;
        let rethrow_argument = self.reference(&error)?;
        let rethrow = self
            .program
            .borrow_mut()
            .append_detached_node_with(HELPER_ORIGIN, |_| {
                Ok(IrNodeData::ThrowStatement {
                    argument: rethrow_argument,
                })
            })?;
        let rethrow_if = self.if_statement(self.reference(&error)?, rethrow, None)?;
        let inner_finally = self.block(vec![rethrow_if])?;
        let nested_finally =
            self.program
                .borrow_mut()
                .append_detached_node_with(HELPER_ORIGIN, |_| {
                    Ok(IrNodeData::TryStatement {
                        block: close_try,
                        handler: None,
                        finalizer: Some(inner_finally),
                    })
                })?;
        let outer_finally = self.block(vec![nested_finally])?;
        let iteration_try =
            self.program
                .borrow_mut()
                .append_detached_node_with(HELPER_ORIGIN, |_| {
                    Ok(IrNodeData::TryStatement {
                        block: try_block,
                        handler: Some(handler),
                        finalizer: Some(outer_finally),
                    })
                })?;
        let return_result = self.return_statement(Some(self.reference(&result)?))?;
        let iterable_body =
            self.block(vec![iterable_declarations, iteration_try, return_result])?;
        let iterable_branch = self.if_statement(self.reference(&method)?, iterable_body, None)?;

        let is_string = self.binary(
            BinaryOperator::StrictEq,
            self.typeof_expression(self.reference(&value)?)?,
            self.string("string")?,
        )?;
        let string_declarations = self.variable_declaration(vec![
            (chars.clone(), Some(self.array(Vec::new())?)),
            (index.clone(), Some(self.number(0.0)?)),
            (first.clone(), None),
            (second.clone(), None),
        ])?;
        let index_below_length = self.binary(
            BinaryOperator::Lt,
            self.reference(&index)?,
            self.member(self.reference(&value)?, "length")?,
        )?;
        let no_string_limit = self.binary(
            BinaryOperator::StrictEq,
            self.reference(&limit)?,
            self.void_zero()?,
        )?;
        let chars_below_limit = self.binary(
            BinaryOperator::Lt,
            self.member(self.reference(&chars)?, "length")?,
            self.reference(&limit)?,
        )?;
        let string_limit = self.logical(LogicalOperator::Or, no_string_limit, chars_below_limit)?;
        let string_while_test =
            self.logical(LogicalOperator::And, index_below_length, string_limit)?;
        let current_index = self.update(self.target(&index)?)?;
        let char_code = self.call(
            self.member(self.reference(&value)?, "charCodeAt")?,
            vec![current_index],
        )?;
        let set_first =
            self.expression_statement(self.assignment(self.target(&first)?, char_code)?)?;
        let high_start = self.binary(
            BinaryOperator::GtEq,
            self.reference(&first)?,
            self.number(55_296.0)?,
        )?;
        let high_end = self.binary(
            BinaryOperator::LtEq,
            self.reference(&first)?,
            self.number(56_319.0)?,
        )?;
        let high_pair = self.logical(LogicalOperator::And, high_start, high_end)?;
        let more_input = self.binary(
            BinaryOperator::Lt,
            self.reference(&index)?,
            self.member(self.reference(&value)?, "length")?,
        )?;
        let possible_pair = self.logical(LogicalOperator::And, high_pair, more_input)?;
        let read_second = self.call(
            self.member(self.reference(&value)?, "charCodeAt")?,
            vec![self.reference(&index)?],
        )?;
        let set_second =
            self.expression_statement(self.assignment(self.target(&second)?, read_second)?)?;
        let low_start = self.binary(
            BinaryOperator::GtEq,
            self.reference(&second)?,
            self.number(56_320.0)?,
        )?;
        let low_end = self.binary(
            BinaryOperator::LtEq,
            self.reference(&second)?,
            self.number(57_343.0)?,
        )?;
        let valid_low = self.logical(LogicalOperator::And, low_start, low_end)?;
        let index_minus_one = self.binary(
            BinaryOperator::Sub,
            self.reference(&index)?,
            self.number(1.0)?,
        )?;
        // `index` already points at the low surrogate. The slice end must observe the incremented
        // value; postfix `index++` returns the old value and would emit only the high surrogate.
        let next_index = self.prefix_update(self.target(&index)?)?;
        let pair_slice = self.call(
            self.member(self.reference(&value)?, "slice")?,
            vec![index_minus_one, next_index],
        )?;
        let push_pair = self.expression_statement(self.call(
            self.member(self.reference(&chars)?, "push")?,
            vec![pair_slice],
        )?)?;
        let low_block = self.block(vec![push_pair, self.continue_statement()?])?;
        let low_if = self.if_statement(valid_low, low_block, None)?;
        let pair_block = self.block(vec![set_second, low_if])?;
        let pair_if = self.if_statement(possible_pair, pair_block, None)?;
        let from_char_code = self.call(
            self.member(self.global("String")?, "fromCharCode")?,
            vec![self.reference(&first)?],
        )?;
        let push_char = self.expression_statement(self.call(
            self.member(self.reference(&chars)?, "push")?,
            vec![from_char_code],
        )?)?;
        let string_loop_body = self.block(vec![set_first, pair_if, push_char])?;
        let string_loop = self.while_statement(string_while_test, string_loop_body)?;
        let return_chars = self.return_statement(Some(self.reference(&chars)?))?;
        let string_body = self.block(vec![string_declarations, string_loop, return_chars])?;
        let string_branch = self.if_statement(is_string, string_body, None)?;

        let throw_iterable = self.throw_type_error("Value is not iterable")?;
        self.function_declaration(
            helper,
            &[value, limit],
            vec![
                array_branch,
                null_branch,
                method_declaration,
                iterable_branch,
                string_branch,
                throw_iterable,
            ],
        )
    }

    fn object_spread_helper(&self, helper: &Binding) -> Result<Vec<NodeId>, TypedIrError> {
        let main = self.object_spread_main(helper)?;
        let define = self.object_spread_define(helper)?;
        let proto = self.object_spread_proto(helper)?;
        let rest = self.object_spread_rest(helper)?;
        Ok(vec![main, define, proto, rest])
    }

    fn object_spread_main(&self, helper: &Binding) -> Result<NodeId, TypedIrError> {
        let target = self.symbol("target", DeclKind::Param)?;
        let source_index = self.symbol("sourceIndex", DeclKind::Var)?;
        let source = self.symbol("source", DeclKind::Var)?;
        let keys = self.symbol("keys", DeclKind::Var)?;
        let symbols = self.symbol("symbols", DeclKind::Var)?;
        let symbol_index = self.symbol("symbolIndex", DeclKind::Var)?;
        let key_index = self.symbol("keyIndex", DeclKind::Var)?;
        let key = self.symbol("key", DeclKind::Var)?;

        let source_lookup =
            self.computed_member(self.global("arguments")?, self.reference(&source_index)?)?;
        let source_declaration =
            self.variable_declaration(vec![(source.clone(), Some(source_lookup))])?;
        let skip_null = self.if_statement(
            self.binary(BinaryOperator::Eq, self.reference(&source)?, self.null()?)?,
            self.continue_statement()?,
            None,
        )?;
        let mut source_body = vec![source_declaration, skip_null];
        source_body.extend(self.own_key_collection(
            &source,
            &keys,
            &symbols,
            &symbol_index,
            true,
        )?);

        let key_lookup =
            self.computed_member(self.reference(&keys)?, self.reference(&key_index)?)?;
        let key_declaration = self.variable_declaration(vec![(key.clone(), Some(key_lookup))])?;
        let source_value = self.computed_member(self.reference(&source)?, self.reference(&key)?)?;
        let descriptor = self.data_descriptor(source_value)?;
        let define_property = self.call(
            self.member(self.global("Object")?, "defineProperty")?,
            vec![self.reference(&target)?, self.reference(&key)?, descriptor],
        )?;
        let define_property = self.expression_statement(define_property)?;
        let key_loop =
            self.counted_loop(&key_index, &keys, vec![key_declaration, define_property])?;
        source_body.push(key_loop);

        let source_initializer =
            self.variable_declaration(vec![(source_index.clone(), Some(self.number(1.0)?))])?;
        let source_test = self.binary(
            BinaryOperator::Lt,
            self.reference(&source_index)?,
            self.member(self.global("arguments")?, "length")?,
        )?;
        let source_update = self.update(self.target(&source_index)?)?;
        let source_loop = self.for_statement(
            Some(source_initializer),
            Some(source_test),
            Some(source_update),
            self.block(source_body)?,
        )?;
        let return_target = self.return_statement(Some(self.reference(&target)?))?;
        self.function_declaration(helper, &[target], vec![source_loop, return_target])
    }

    fn object_spread_define(&self, helper: &Binding) -> Result<NodeId, TypedIrError> {
        let target = self.symbol("target", DeclKind::Param)?;
        let source = self.symbol("source", DeclKind::Param)?;
        let keys = self.symbol("keys", DeclKind::Var)?;
        let symbols = self.symbol("symbols", DeclKind::Var)?;
        let symbol_index = self.symbol("symbolIndex", DeclKind::Var)?;
        let key_index = self.symbol("keyIndex", DeclKind::Var)?;
        let key = self.symbol("key", DeclKind::Var)?;
        let descriptor = self.symbol("descriptor", DeclKind::Var)?;
        let previous = self.symbol("previous", DeclKind::Var)?;

        let mut statements =
            self.own_key_collection(&source, &keys, &symbols, &symbol_index, false)?;
        let key_lookup =
            self.computed_member(self.reference(&keys)?, self.reference(&key_index)?)?;
        let descriptor_call = self.call(
            self.member(self.global("Object")?, "getOwnPropertyDescriptor")?,
            vec![self.reference(&source)?, self.reference(&key)?],
        )?;
        let key_and_descriptor = self.variable_declaration(vec![
            (key.clone(), Some(key_lookup)),
            (descriptor.clone(), Some(descriptor_call)),
        ])?;

        let lacks_value = self.unary(
            UnaryOperator::LogicalNot,
            self.binary(
                BinaryOperator::In,
                self.string("value")?,
                self.reference(&descriptor)?,
            )?,
        )?;
        let previous_call = self.call(
            self.member(self.global("Object")?, "getOwnPropertyDescriptor")?,
            vec![self.reference(&target)?, self.reference(&key)?],
        )?;
        let previous_declaration =
            self.variable_declaration(vec![(previous.clone(), Some(previous_call))])?;
        let previous_accessor = self.logical(
            LogicalOperator::And,
            self.reference(&previous)?,
            self.unary(
                UnaryOperator::LogicalNot,
                self.binary(
                    BinaryOperator::In,
                    self.string("value")?,
                    self.reference(&previous)?,
                )?,
            )?,
        )?;

        let descriptor_get = self.member(self.reference(&descriptor)?, "get")?;
        let get_missing =
            self.binary(BinaryOperator::StrictEq, descriptor_get, self.void_zero()?)?;
        let assign_get = self.expression_statement(self.assignment(
            self.member(self.reference(&descriptor)?, "get")?,
            self.member(self.reference(&previous)?, "get")?,
        )?)?;
        let inherit_get = self.if_statement(get_missing, assign_get, None)?;
        let descriptor_set = self.member(self.reference(&descriptor)?, "set")?;
        let set_missing =
            self.binary(BinaryOperator::StrictEq, descriptor_set, self.void_zero()?)?;
        let assign_set = self.expression_statement(self.assignment(
            self.member(self.reference(&descriptor)?, "set")?,
            self.member(self.reference(&previous)?, "set")?,
        )?)?;
        let inherit_set = self.if_statement(set_missing, assign_set, None)?;
        let inherit_accessors = self.if_statement(
            previous_accessor,
            self.block(vec![inherit_get, inherit_set])?,
            None,
        )?;
        let accessor_branch = self.if_statement(
            lacks_value,
            self.block(vec![previous_declaration, inherit_accessors])?,
            None,
        )?;
        let define_property = self.expression_statement(self.call(
            self.member(self.global("Object")?, "defineProperty")?,
            vec![
                self.reference(&target)?,
                self.reference(&key)?,
                self.reference(&descriptor)?,
            ],
        )?)?;
        let key_loop = self.counted_loop(
            &key_index,
            &keys,
            vec![key_and_descriptor, accessor_branch, define_property],
        )?;
        statements.push(key_loop);
        statements.push(self.return_statement(Some(self.reference(&target)?))?);
        let function = self.function_expression(&[target, source], statements)?;
        self.assign_helper_method(helper, "define", function)
    }

    fn object_spread_proto(&self, helper: &Binding) -> Result<NodeId, TypedIrError> {
        let target = self.symbol("target", DeclKind::Param)?;
        let value = self.symbol("value", DeclKind::Param)?;
        let type_binding = self.symbol("type", DeclKind::Var)?;
        let descriptor = self.symbol("descriptor", DeclKind::Var)?;

        let type_value = self.typeof_expression(self.reference(&value)?)?;
        let type_declaration =
            self.variable_declaration(vec![(type_binding.clone(), Some(type_value))])?;
        let not_null = self.binary(
            BinaryOperator::StrictNotEq,
            self.reference(&value)?,
            self.null()?,
        )?;
        let not_object = self.binary(
            BinaryOperator::StrictNotEq,
            self.reference(&type_binding)?,
            self.string("object")?,
        )?;
        let not_function = self.binary(
            BinaryOperator::StrictNotEq,
            self.reference(&type_binding)?,
            self.string("function")?,
        )?;
        let invalid = self.logical(
            LogicalOperator::And,
            not_null,
            self.logical(LogicalOperator::And, not_object, not_function)?,
        )?;
        let return_unchanged = self.return_statement(Some(self.reference(&target)?))?;
        let reject_primitive = self.if_statement(invalid, return_unchanged, None)?;

        let set_prototype = self.expression_statement(self.call(
            self.member(self.global("Object")?, "setPrototypeOf")?,
            vec![self.reference(&target)?, self.reference(&value)?],
        )?)?;
        let descriptor_call = self.call(
            self.member(self.global("Object")?, "getOwnPropertyDescriptor")?,
            vec![
                self.member(self.global("Object")?, "prototype")?,
                self.string("__proto__")?,
            ],
        )?;
        let descriptor_declaration =
            self.variable_declaration(vec![(descriptor.clone(), Some(descriptor_call))])?;
        let descriptor_set = self.logical(
            LogicalOperator::And,
            self.reference(&descriptor)?,
            self.member(self.reference(&descriptor)?, "set")?,
        )?;
        let call_setter = self.expression_statement(self.call(
            self.member(self.member(self.reference(&descriptor)?, "set")?, "call")?,
            vec![self.reference(&target)?, self.reference(&value)?],
        )?)?;
        let call_descriptor = self.if_statement(descriptor_set, call_setter, None)?;
        let fallback = self.block(vec![descriptor_declaration, call_descriptor])?;
        let has_native = self.member(self.global("Object")?, "setPrototypeOf")?;
        let install = self.if_statement(has_native, set_prototype, Some(fallback))?;
        let return_target = self.return_statement(Some(self.reference(&target)?))?;
        let function = self.function_expression(
            &[target, value],
            vec![type_declaration, reject_primitive, install, return_target],
        )?;
        self.assign_helper_method(helper, "proto", function)
    }

    fn object_spread_rest(&self, helper: &Binding) -> Result<NodeId, TypedIrError> {
        let source = self.symbol("source", DeclKind::Param)?;
        let excluded = self.symbol("excluded", DeclKind::Param)?;
        let target = self.symbol("target", DeclKind::Var)?;
        let keys = self.symbol("keys", DeclKind::Var)?;
        let symbols = self.symbol("symbols", DeclKind::Var)?;
        let symbol_index = self.symbol("symbolIndex", DeclKind::Var)?;
        let key_index = self.symbol("keyIndex", DeclKind::Var)?;
        let key = self.symbol("key", DeclKind::Var)?;
        let skip = self.symbol("skip", DeclKind::Var)?;
        let excluded_index = self.symbol("excludedIndex", DeclKind::Var)?;

        let nullish = self.binary(BinaryOperator::Eq, self.reference(&source)?, self.null()?)?;
        let reject_null = self.if_statement(
            nullish,
            self.throw_type_error("Cannot destructure null or undefined")?,
            None,
        )?;
        let target_declaration =
            self.variable_declaration(vec![(target.clone(), Some(self.object(Vec::new())?))])?;
        let mut statements = vec![reject_null, target_declaration];
        statements.extend(self.own_key_collection(
            &source,
            &keys,
            &symbols,
            &symbol_index,
            true,
        )?);

        let key_lookup =
            self.computed_member(self.reference(&keys)?, self.reference(&key_index)?)?;
        let key_and_skip = self.variable_declaration(vec![
            (key.clone(), Some(key_lookup)),
            (skip.clone(), Some(self.boolean(false)?)),
        ])?;
        let excluded_value =
            self.computed_member(self.reference(&excluded)?, self.reference(&excluded_index)?)?;
        let excluded_match = self.binary(
            BinaryOperator::StrictEq,
            excluded_value,
            self.reference(&key)?,
        )?;
        let set_skip =
            self.expression_statement(self.assignment(self.target(&skip)?, self.boolean(true)?)?)?;
        let match_body = self.block(vec![set_skip, self.break_statement()?])?;
        let match_if = self.if_statement(excluded_match, match_body, None)?;
        let excluded_loop = self.counted_loop(&excluded_index, &excluded, vec![match_if])?;
        let should_copy = self.unary(UnaryOperator::LogicalNot, self.reference(&skip)?)?;
        let source_value = self.computed_member(self.reference(&source)?, self.reference(&key)?)?;
        let descriptor = self.data_descriptor(source_value)?;
        let copy = self.expression_statement(self.call(
            self.member(self.global("Object")?, "defineProperty")?,
            vec![self.reference(&target)?, self.reference(&key)?, descriptor],
        )?)?;
        let copy_if = self.if_statement(should_copy, copy, None)?;
        let key_loop = self.counted_loop(
            &key_index,
            &keys,
            vec![key_and_skip, excluded_loop, copy_if],
        )?;
        statements.push(key_loop);
        statements.push(self.return_statement(Some(self.reference(&target)?))?);
        let function = self.function_expression(&[source, excluded], statements)?;
        self.assign_helper_method(helper, "rest", function)
    }

    fn assign_helper_method(
        &self,
        helper: &Binding,
        property: &str,
        function: NodeId,
    ) -> Result<NodeId, TypedIrError> {
        let left = self.member(self.reference(helper)?, property)?;
        let assignment = self.assignment(left, function)?;
        self.expression_statement(assignment)
    }

    fn data_descriptor(&self, value: NodeId) -> Result<NodeId, TypedIrError> {
        let value = self.data_property("value", value)?;
        let enumerable = self.data_property("enumerable", self.boolean(true)?)?;
        let configurable = self.data_property("configurable", self.boolean(true)?)?;
        let writable = self.data_property("writable", self.boolean(true)?)?;
        self.object(vec![value, enumerable, configurable, writable])
    }

    fn own_key_collection(
        &self,
        source: &Binding,
        keys: &Binding,
        symbols: &Binding,
        symbol_index: &Binding,
        coerce_source: bool,
    ) -> Result<Vec<NodeId>, TypedIrError> {
        let keys_argument = if coerce_source {
            self.call(self.global("Object")?, vec![self.reference(source)?])?
        } else {
            self.reference(source)?
        };
        let keys_call = self.call(
            self.member(self.global("Object")?, "keys")?,
            vec![keys_argument],
        )?;
        let keys_declaration = self.variable_declaration(vec![(keys.clone(), Some(keys_call))])?;

        let symbols_call = self.call(
            self.member(self.global("Object")?, "getOwnPropertySymbols")?,
            vec![self.reference(source)?],
        )?;
        let symbols_declaration =
            self.variable_declaration(vec![(symbols.clone(), Some(symbols_call))])?;
        let symbol_value =
            self.computed_member(self.reference(symbols)?, self.reference(symbol_index)?)?;
        let enumerable = self.call(
            self.member(
                self.member(
                    self.member(self.global("Object")?, "prototype")?,
                    "propertyIsEnumerable",
                )?,
                "call",
            )?,
            vec![self.reference(source)?, symbol_value],
        )?;
        let push_symbol = self.expression_statement(self.call(
            self.member(self.reference(keys)?, "push")?,
            vec![self.computed_member(self.reference(symbols)?, self.reference(symbol_index)?)?],
        )?)?;
        let push_if = self.if_statement(enumerable, push_symbol, None)?;
        let symbol_loop = self.counted_loop(symbol_index, symbols, vec![push_if])?;
        let symbols_body = self.block(vec![symbols_declaration, symbol_loop])?;
        let has_symbols = self.binary(
            BinaryOperator::StrictEq,
            self.typeof_expression(self.member(self.global("Object")?, "getOwnPropertySymbols")?)?,
            self.string("function")?,
        )?;
        let symbols_if = self.if_statement(has_symbols, symbols_body, None)?;
        Ok(vec![keys_declaration, symbols_if])
    }

    fn counted_loop(
        &self,
        index: &Binding,
        collection: &Binding,
        body: Vec<NodeId>,
    ) -> Result<NodeId, TypedIrError> {
        let initializer =
            self.variable_declaration(vec![(index.clone(), Some(self.number(0.0)?))])?;
        let test = self.binary(
            BinaryOperator::Lt,
            self.reference(index)?,
            self.member(self.reference(collection)?, "length")?,
        )?;
        let update = self.update(self.target(index)?)?;
        self.for_statement(
            Some(initializer),
            Some(test),
            Some(update),
            self.block(body)?,
        )
    }

    fn for_of_helper(&self, _helper: &Binding) -> Result<NodeId, TypedIrError> {
        let helper = _helper;
        let value = self.symbol("value", DeclKind::Param)?;
        let iterator = self.symbol("iterator", DeclKind::Var)?;
        let next = self.symbol("next", DeclKind::Var)?;
        let normal = self.symbol("normal", DeclKind::Var)?;
        let error = self.symbol("error", DeclKind::Var)?;
        let has_error = self.symbol("hasError", DeclKind::Var)?;
        let state = self.symbol("state", DeclKind::Var)?;

        let declarations = self.variable_declaration(vec![
            (iterator.clone(), None),
            (next.clone(), None),
            (normal.clone(), Some(self.boolean(true)?)),
            (error.clone(), None),
            (has_error.clone(), Some(self.boolean(false)?)),
            (state.clone(), None),
        ])?;

        let start = self.for_of_start_method(&value, &iterator, &next)?;
        let advance = self.for_of_next_method(&iterator, &next, &normal, &state)?;
        let capture = self.for_of_error_method(&error, &has_error)?;
        let finish = self.for_of_finish_method(&iterator, &normal, &error, &has_error)?;
        let start = self.data_property("s", start)?;
        let advance = self.data_property("n", advance)?;
        let capture = self.data_property("e", capture)?;
        let finish = self.data_property("f", finish)?;
        let initial_value = self.data_property("v", self.void_zero()?)?;
        let state_object = self.object(vec![start, advance, capture, finish, initial_value])?;
        let initialize_state =
            self.expression_statement(self.assignment(self.target(&state)?, state_object)?)?;
        let return_state = self.return_statement(Some(self.reference(&state)?))?;
        self.function_declaration(
            helper,
            &[value],
            vec![declarations, initialize_state, return_state],
        )
    }

    fn for_of_start_method(
        &self,
        value: &Binding,
        iterator: &Binding,
        next: &Binding,
    ) -> Result<NodeId, TypedIrError> {
        let iterator_symbol = self.symbol("iteratorSymbol", DeclKind::Var)?;
        let method = self.symbol("method", DeclKind::Var)?;

        let value_null = self.binary(BinaryOperator::Eq, self.reference(value)?, self.null()?)?;
        let symbol_missing = self.binary(
            BinaryOperator::StrictEq,
            self.typeof_expression(self.global("Symbol")?)?,
            self.string("undefined")?,
        )?;
        let unavailable = self.logical(LogicalOperator::Or, value_null, symbol_missing)?;
        let reject_unavailable = self.if_statement(
            unavailable,
            self.throw_type_error("Value is not iterable")?,
            None,
        )?;

        let iterator_symbol_value = self.member(self.global("Symbol")?, "iterator")?;
        let iterator_symbol_declaration = self
            .variable_declaration(vec![(iterator_symbol.clone(), Some(iterator_symbol_value))])?;
        let missing_symbol = self.binary(
            BinaryOperator::Eq,
            self.reference(&iterator_symbol)?,
            self.null()?,
        )?;
        let reject_missing_symbol = self.if_statement(
            missing_symbol,
            self.throw_type_error("Value is not iterable")?,
            None,
        )?;

        let method_value =
            self.computed_member(self.reference(value)?, self.reference(&iterator_symbol)?)?;
        let method_declaration =
            self.variable_declaration(vec![(method.clone(), Some(method_value))])?;
        let method_not_function = self.binary(
            BinaryOperator::StrictNotEq,
            self.typeof_expression(self.reference(&method)?)?,
            self.string("function")?,
        )?;
        let reject_method = self.if_statement(
            method_not_function,
            self.throw_type_error("Value is not iterable")?,
            None,
        )?;

        let call_method = self.call(
            self.member(self.reference(&method)?, "call")?,
            vec![self.reference(value)?],
        )?;
        let initialize_iterator =
            self.expression_statement(self.assignment(self.target(iterator)?, call_method)?)?;
        let iterator_null =
            self.binary(BinaryOperator::Eq, self.reference(iterator)?, self.null()?)?;
        let iterator_not_object = self.binary(
            BinaryOperator::StrictNotEq,
            self.typeof_expression(self.reference(iterator)?)?,
            self.string("object")?,
        )?;
        let iterator_not_function = self.binary(
            BinaryOperator::StrictNotEq,
            self.typeof_expression(self.reference(iterator)?)?,
            self.string("function")?,
        )?;
        let invalid_type = self.logical(
            LogicalOperator::And,
            iterator_not_object,
            iterator_not_function,
        )?;
        let invalid_iterator = self.logical(LogicalOperator::Or, iterator_null, invalid_type)?;
        let reject_iterator = self.if_statement(
            invalid_iterator,
            self.throw_type_error("Iterator is not an object")?,
            None,
        )?;

        let next_value = self.member(self.reference(iterator)?, "next")?;
        let initialize_next =
            self.expression_statement(self.assignment(self.target(next)?, next_value)?)?;
        let next_not_function = self.binary(
            BinaryOperator::StrictNotEq,
            self.typeof_expression(self.reference(next)?)?,
            self.string("function")?,
        )?;
        let reject_next = self.if_statement(
            next_not_function,
            self.throw_type_error("Iterator next is not callable")?,
            None,
        )?;
        self.function_expression(
            &[],
            vec![
                reject_unavailable,
                iterator_symbol_declaration,
                reject_missing_symbol,
                method_declaration,
                reject_method,
                initialize_iterator,
                reject_iterator,
                initialize_next,
                reject_next,
            ],
        )
    }

    fn for_of_next_method(
        &self,
        iterator: &Binding,
        next: &Binding,
        normal: &Binding,
        state: &Binding,
    ) -> Result<NodeId, TypedIrError> {
        let step = self.symbol("step", DeclKind::Var)?;
        let done = self.symbol("done", DeclKind::Var)?;
        let set_normal =
            self.expression_statement(self.assignment(self.target(normal)?, self.boolean(true)?)?)?;
        let step_call = self.call(
            self.member(self.reference(next)?, "call")?,
            vec![self.reference(iterator)?],
        )?;
        let step_declaration = self.variable_declaration(vec![(step.clone(), Some(step_call))])?;
        let step_null = self.binary(BinaryOperator::Eq, self.reference(&step)?, self.null()?)?;
        let step_not_object = self.binary(
            BinaryOperator::StrictNotEq,
            self.typeof_expression(self.reference(&step)?)?,
            self.string("object")?,
        )?;
        let step_not_function = self.binary(
            BinaryOperator::StrictNotEq,
            self.typeof_expression(self.reference(&step)?)?,
            self.string("function")?,
        )?;
        let step_invalid_type =
            self.logical(LogicalOperator::And, step_not_object, step_not_function)?;
        let invalid_step = self.logical(LogicalOperator::Or, step_null, step_invalid_type)?;
        let reject_step = self.if_statement(
            invalid_step,
            self.throw_type_error("Iterator result is not an object")?,
            None,
        )?;
        let done_value = self.member(self.reference(&step)?, "done")?;
        let done_declaration = self.variable_declaration(vec![(done.clone(), Some(done_value))])?;
        let return_done = self.if_statement(
            self.reference(&done)?,
            self.return_statement(Some(self.boolean(true)?))?,
            None,
        )?;
        let state_value = self.member(self.reference(state)?, "v")?;
        let step_value = self.member(self.reference(&step)?, "value")?;
        let set_value = self.expression_statement(self.assignment(state_value, step_value)?)?;
        let set_abrupt = self
            .expression_statement(self.assignment(self.target(normal)?, self.boolean(false)?)?)?;
        let return_false = self.return_statement(Some(self.boolean(false)?))?;
        self.function_expression(
            &[],
            vec![
                set_normal,
                step_declaration,
                reject_step,
                done_declaration,
                return_done,
                set_value,
                set_abrupt,
                return_false,
            ],
        )
    }

    fn for_of_error_method(
        &self,
        error: &Binding,
        has_error: &Binding,
    ) -> Result<NodeId, TypedIrError> {
        let caught = self.symbol("caught", DeclKind::Param)?;
        let mark_error = self
            .expression_statement(self.assignment(self.target(has_error)?, self.boolean(true)?)?)?;
        let save_error = self.expression_statement(
            self.assignment(self.target(error)?, self.reference(&caught)?)?,
        )?;
        self.function_expression(&[caught], vec![mark_error, save_error])
    }

    fn for_of_finish_method(
        &self,
        iterator: &Binding,
        normal: &Binding,
        error: &Binding,
        has_error: &Binding,
    ) -> Result<NodeId, TypedIrError> {
        let return_method = self.symbol("returnMethod", DeclKind::Var)?;
        let close_result = self.symbol("closeResult", DeclKind::Var)?;

        let return_value = self.member(self.reference(iterator)?, "return")?;
        let return_declaration =
            self.variable_declaration(vec![(return_method.clone(), Some(return_value))])?;
        let has_return = self.binary(
            BinaryOperator::NotEq,
            self.reference(&return_method)?,
            self.null()?,
        )?;
        let return_not_function = self.binary(
            BinaryOperator::StrictNotEq,
            self.typeof_expression(self.reference(&return_method)?)?,
            self.string("function")?,
        )?;
        let reject_return = self.if_statement(
            return_not_function,
            self.throw_type_error("Iterator return is not callable")?,
            None,
        )?;
        let close_call = self.call(
            self.member(self.reference(&return_method)?, "call")?,
            vec![self.reference(iterator)?],
        )?;
        let close_declaration =
            self.variable_declaration(vec![(close_result.clone(), Some(close_call))])?;
        let close_null = self.binary(
            BinaryOperator::Eq,
            self.reference(&close_result)?,
            self.null()?,
        )?;
        let close_not_object = self.binary(
            BinaryOperator::StrictNotEq,
            self.typeof_expression(self.reference(&close_result)?)?,
            self.string("object")?,
        )?;
        let close_not_function = self.binary(
            BinaryOperator::StrictNotEq,
            self.typeof_expression(self.reference(&close_result)?)?,
            self.string("function")?,
        )?;
        let close_invalid_type =
            self.logical(LogicalOperator::And, close_not_object, close_not_function)?;
        let invalid_close = self.logical(LogicalOperator::Or, close_null, close_invalid_type)?;
        let reject_close = self.if_statement(
            invalid_close,
            self.throw_type_error("Iterator return result is not an object")?,
            None,
        )?;
        let has_return_body = self.block(vec![reject_return, close_declaration, reject_close])?;
        let maybe_return = self.if_statement(has_return, has_return_body, None)?;
        let abnormal_body = self.block(vec![return_declaration, maybe_return])?;
        let not_normal = self.unary(UnaryOperator::LogicalNot, self.reference(normal)?)?;
        let maybe_close = self.if_statement(not_normal, abnormal_body, None)?;
        let try_block = self.block(vec![maybe_close])?;

        let rethrow = self.throw_statement(self.reference(error)?)?;
        let rethrow_if = self.if_statement(self.reference(has_error)?, rethrow, None)?;
        let finally_block = self.block(vec![rethrow_if])?;
        let try_statement =
            self.program
                .borrow_mut()
                .append_detached_node_with(HELPER_ORIGIN, |_| {
                    Ok(IrNodeData::TryStatement {
                        block: try_block,
                        handler: None,
                        finalizer: Some(finally_block),
                    })
                })?;
        self.function_expression(&[], vec![try_statement])
    }
}

#[cfg(test)]
mod tests {
    use wake_common::Interner;
    use wake_ecma_ast::SourceType;
    use wake_ecma_parser::parse;

    use super::*;
    use crate::typed_ir::{IrNodeData, IrOrigin, NameRole, SyntheticOriginKind};

    fn lower(source: &str) -> TypedProgram {
        let interner = Interner::new();
        let parsed = parse(source, &interner, SourceType::Script);
        assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);
        parsed.module.with_ast(|program| {
            let semantic = wake_ecma_semantic::analyze(program);
            TypedProgram::lower(program, &interner, Some(&semantic)).expect("typed lowering")
        })
    }

    fn set_helpers(ir: &mut TypedProgram, spread: bool, object: bool, for_of: bool) {
        let root = ir.root();
        let IrNodeData::Program {
            source_type,
            strict,
            body,
            ..
        } = ir.node(root).expect("root").data().clone()
        else {
            unreachable!()
        };
        ir.replace_node_data(
            root,
            IrNodeData::Program {
                source_type,
                strict,
                spread_helper: spread.then(|| "__wake_iter".to_owned()),
                object_spread_helper: object.then(|| "__wake_object".to_owned()),
                for_of_helper: for_of.then(|| "__wake_for_of".to_owned()),
                body,
            },
        )
        .expect("helper metadata");
    }

    fn program_body(ir: &TypedProgram) -> Vec<crate::typed_ir::NodeId> {
        let IrNodeData::Program { body, .. } = ir.node(ir.root()).expect("root").data() else {
            unreachable!()
        };
        ir.list(*body).expect("program body").items().to_vec()
    }

    #[test]
    fn materialization_clears_all_metadata_and_inserts_synthetic_functions() {
        let mut ir = lower("boot();");
        set_helpers(&mut ir, true, true, true);
        let before = program_body(&ir).len();

        let report = materialize_runtime_helpers(&mut ir).expect("materialization");

        assert!(report.spread_name.is_some());
        assert!(report.object_spread_name.is_some());
        assert!(report.for_of_name.is_some());
        let IrNodeData::Program {
            spread_helper,
            object_spread_helper,
            for_of_helper,
            ..
        } = ir.node(ir.root()).expect("root").data()
        else {
            unreachable!()
        };
        assert_eq!(
            (spread_helper, object_spread_helper, for_of_helper),
            (&None, &None, &None)
        );
        let inserted = &program_body(&ir)[..program_body(&ir).len() - before];
        assert!(
            inserted.len() >= 5,
            "object helper owns three companion assignments"
        );
        assert!(inserted.iter().all(|&node| matches!(
            ir.node(node).expect("helper statement").origin(),
            IrOrigin::Synthetic {
                anchor: None,
                kind: SyntheticOriginKind::Optimization
            }
        )));
        ir.validate().expect("materialized IR must validate");
    }

    #[test]
    fn helpers_follow_the_directive_prologue() {
        let mut ir = lower("'wake-prologue';'use strict';boot();");
        set_helpers(&mut ir, true, false, false);
        materialize_runtime_helpers(&mut ir).expect("materialization");
        let body = program_body(&ir);
        assert!(matches!(
            ir.node(body[0]).expect("directive").data(),
            IrNodeData::ExpressionStatement {
                directive: true,
                ..
            }
        ));
        assert!(matches!(
            ir.node(body[1]).expect("directive").data(),
            IrNodeData::ExpressionStatement {
                directive: true,
                ..
            }
        ));
        assert!(matches!(
            ir.node(body[2]).expect("helper").data(),
            IrNodeData::Function { .. }
        ));
    }

    #[test]
    fn helper_binding_avoids_every_existing_original_and_emitted_name() {
        let mut ir = lower("let __wake_iter=1,renamed=2;consume(__wake_iter,renamed);");
        let renamed = ir
            .nodes()
            .iter()
            .filter_map(|node| {
                let IrNodeData::Name { name } = node.data() else {
                    return None;
                };
                (ir.name(*name)?.original() == "renamed").then_some(*name)
            })
            .collect::<Vec<_>>();
        for name in renamed {
            ir.set_emitted_name(name, "__wake_iter$1").expect("rename");
        }
        set_helpers(&mut ir, true, false, false);

        let report = materialize_runtime_helpers(&mut ir).expect("materialization");
        let helper = report.spread_name.expect("spread helper name");
        assert_ne!(helper, "__wake_iter");
        assert_ne!(helper, "__wake_iter$1");

        let helper_symbols = ir
            .names()
            .iter()
            .filter(|name| name.emitted() == helper)
            .map(|name| (name.role(), name.symbol()))
            .collect::<Vec<_>>();
        assert!(
            helper_symbols
                .iter()
                .any(|(role, symbol)| { *role == NameRole::FunctionName && symbol.is_some() })
        );
        assert!(helper_symbols.iter().all(|(_, symbol)| symbol.is_some()));
        assert!(
            ir.names()
                .iter()
                .any(|name| name.original() == "__wake_iter" && name.emitted() == "__wake_iter")
        );
    }

    #[test]
    fn helper_free_program_keeps_the_owned_arena_in_place() {
        let mut ir = lower("boot();");
        let nodes = ir.nodes().as_ptr();
        let lists = ir.lists().as_ptr();
        let names = ir.names().as_ptr();

        let report = materialize_runtime_helpers(&mut ir).expect("no-op materialization");

        assert_eq!(report, RuntimeHelperReport::default());
        assert_eq!(ir.nodes().as_ptr(), nodes);
        assert_eq!(ir.lists().as_ptr(), lists);
        assert_eq!(ir.names().as_ptr(), names);
    }
}
