use super::*;
use wake_common::Atom;

type Result<T> = std::result::Result<T, CallExecutionError>;

#[derive(Clone)]
struct Target {
    label: Option<Atom>,
    break_to: NodeId,
    continue_to: Option<NodeId>,
}
#[derive(Clone)]
struct Environment {
    return_to: NodeId,
    throw_to: NodeId,
    targets: Vec<Target>,
    flags: Flags,
}

pub(super) struct Builder<'b> {
    budget: &'b mut Budget,
    nodes: Vec<Node>,
}

impl<'b> Builder<'b> {
    pub(super) fn region<'a>(
        budget: &'b mut Budget,
        kind: ExecutionRegionKind,
        parameters: &[Pattern<'a>],
        body: Body<'a, '_>,
    ) -> Result<Graph> {
        let mut builder = Self {
            budget,
            nodes: Vec::new(),
        };
        let thrown = builder.node(vec![], None)?;
        let normal = builder.node(vec![], None)?;
        let mut environment = Environment {
            return_to: normal,
            throw_to: thrown,
            targets: Vec::new(),
            flags: Flags {
                in_class: matches!(
                    kind,
                    ExecutionRegionKind::ClassInitializer | ExecutionRegionKind::StaticBlock
                ),
                ..Default::default()
            },
        };
        let mut entry = match body {
            Body::Statements(statements) => builder.sequence(statements, normal, &environment)?,
            Body::Expression(expression) => builder.expression(expression, normal, &environment)?,
        };
        environment.flags.in_parameters = true;
        for parameter in parameters.iter().rev() {
            entry = builder.pattern(parameter, entry, &environment)?;
        }
        Ok(Graph {
            nodes: builder.nodes,
            entry,
            normal,
        })
    }

    fn node(&mut self, mut edges: Vec<NodeId>, site: Option<Site>) -> Result<NodeId> {
        edges.sort_unstable();
        edges.dedup();
        self.budget.nodes()?;
        self.budget.edges(edges.len())?;
        let id = self.nodes.len();
        self.nodes.push(Node { edges, site });
        Ok(id)
    }
    fn connect(&mut self, id: NodeId, mut edges: Vec<NodeId>) -> Result<()> {
        edges.sort_unstable();
        edges.dedup();
        debug_assert!(self.nodes[id].edges.is_empty());
        self.budget.edges(edges.len())?;
        self.nodes[id].edges = edges;
        Ok(())
    }
    fn effect(&mut self, next: NodeId, environment: &Environment) -> Result<NodeId> {
        self.node(vec![next, environment.throw_to], None)
    }
    fn call(&mut self, span: Span, next: NodeId, environment: &Environment) -> Result<NodeId> {
        let site = (!span.is_dummy() && !environment.flags.suppress_calls).then_some(Site {
            span,
            flags: environment.flags,
        });
        self.node(vec![next, environment.throw_to], site)
    }
    fn sequence(
        &mut self,
        statements: &[Statement<'_>],
        mut next: NodeId,
        environment: &Environment,
    ) -> Result<NodeId> {
        for statement in statements.iter().rev() {
            next = self.statement(statement, next, environment, &[])?;
        }
        Ok(next)
    }
    fn variables(
        &mut self,
        variables: &VariableDeclaration<'_>,
        mut next: NodeId,
        environment: &Environment,
    ) -> Result<NodeId> {
        for declaration in variables.declarations.iter().rev() {
            next = self.pattern(&declaration.id, next, environment)?;
            if let Some(init) = declaration.init {
                next = self.expression(init, next, environment)?;
            }
        }
        Ok(next)
    }
    fn loop_environment(
        &self,
        environment: &Environment,
        next: NodeId,
        continuation: NodeId,
        labels: &[Atom],
    ) -> Environment {
        let mut nested = environment.clone();
        nested.flags.inside_loop = true;
        for target in &mut nested.targets {
            if target.label.is_some_and(|label| labels.contains(&label)) {
                target.continue_to = Some(continuation);
            }
        }
        nested.targets.push(Target {
            label: None,
            break_to: next,
            continue_to: Some(continuation),
        });
        nested
    }
    fn condition_edges(test: Option<&Expression<'_>>, yes: NodeId, no: NodeId) -> Vec<NodeId> {
        let truth = test.map_or(Some(true), super::super::control_flow::truthiness);
        match truth {
            Some(true) => vec![yes],
            Some(false) => vec![no],
            None => vec![yes, no],
        }
    }

    fn statement(
        &mut self,
        statement: &Statement<'_>,
        next: NodeId,
        environment: &Environment,
        labels: &[Atom],
    ) -> Result<NodeId> {
        self.budget.step()?;
        match statement {
            Statement::Empty(_)
            | Statement::Debugger(_)
            | Statement::FunctionDeclaration(_)
            | Statement::Import(_)
            | Statement::ExportAll(_) => Ok(next),
            Statement::VariableDeclaration(variables) => {
                self.variables(variables, next, environment)
            }
            Statement::Expression(expression) => {
                self.expression(expression.expression, next, environment)
            }
            Statement::ClassDeclaration(class) => self.class(class, next, environment),
            Statement::Block(block) => self.sequence(&block.body, next, environment),
            Statement::Return(return_) => return_
                .argument
                .map_or(Ok(environment.return_to), |argument| {
                    self.expression(argument, environment.return_to, environment)
                }),
            Statement::Throw(throw) => {
                self.expression(throw.argument, environment.throw_to, environment)
            }
            Statement::Break(jump) => Ok(environment
                .targets
                .iter()
                .rev()
                .find(|target| target.label == jump.label.map(|label| label.name))
                .map_or(next, |target| target.break_to)),
            Statement::Continue(jump) => Ok(environment
                .targets
                .iter()
                .rev()
                .find(|target| {
                    target.label == jump.label.map(|label| label.name)
                        && target.continue_to.is_some()
                })
                .and_then(|target| target.continue_to)
                .unwrap_or(next)),
            Statement::If(branch) => {
                let yes = self.statement(&branch.consequent, next, environment, &[])?;
                let no = if let Some(alternate) = branch.alternate {
                    self.statement(&alternate, next, environment, &[])?
                } else {
                    next
                };
                let branch_node =
                    self.node(Self::condition_edges(Some(&branch.test), yes, no), None)?;
                self.expression(branch.test, branch_node, environment)
            }
            Statement::While(loop_) => {
                let branch = self.node(vec![], None)?;
                let mut in_test = environment.clone();
                in_test.flags.inside_loop = true;
                let test = self.expression(loop_.test, branch, &in_test)?;
                let nested = self.loop_environment(environment, next, test, labels);
                let body = self.statement(&loop_.body, test, &nested, &[])?;
                self.connect(branch, Self::condition_edges(Some(&loop_.test), body, next))?;
                Ok(test)
            }
            Statement::DoWhile(loop_) => {
                let branch = self.node(vec![], None)?;
                let mut in_test = environment.clone();
                in_test.flags.inside_loop = true;
                let test = self.expression(loop_.test, branch, &in_test)?;
                let nested = self.loop_environment(environment, next, test, labels);
                let body = self.statement(&loop_.body, test, &nested, &[])?;
                self.connect(branch, Self::condition_edges(Some(&loop_.test), body, next))?;
                Ok(body)
            }
            Statement::For(loop_) => {
                let branch = self.node(vec![], None)?;
                let mut in_loop = environment.clone();
                in_loop.flags.inside_loop = true;
                let test = if let Some(test) = loop_.test {
                    self.expression(test, branch, &in_loop)?
                } else {
                    branch
                };
                let update = if let Some(update) = loop_.update {
                    self.expression(update, test, &in_loop)?
                } else {
                    test
                };
                let nested = self.loop_environment(environment, next, update, labels);
                let body = self.statement(&loop_.body, update, &nested, &[])?;
                self.connect(
                    branch,
                    Self::condition_edges(loop_.test.as_ref(), body, next),
                )?;
                match loop_.init {
                    Some(ForInit::Variable(variables)) => {
                        self.variables(variables, test, environment)
                    }
                    Some(ForInit::Expression(expression)) => {
                        self.expression(expression, test, environment)
                    }
                    None => Ok(test),
                }
            }
            Statement::ForIn(loop_) => self.iteration(
                &loop_.left,
                loop_.right,
                &loop_.body,
                next,
                environment,
                labels,
            ),
            Statement::ForOf(loop_) => self.iteration(
                &loop_.left,
                loop_.right,
                &loop_.body,
                next,
                environment,
                labels,
            ),
            Statement::Labeled(labeled) => {
                let mut nested = environment.clone();
                nested.targets.push(Target {
                    label: Some(labeled.label.name),
                    break_to: next,
                    continue_to: None,
                });
                let mut nested_labels = labels.to_vec();
                nested_labels.push(labeled.label.name);
                self.statement(&labeled.body, next, &nested, &nested_labels)
            }
            Statement::Switch(switch) => {
                let mut nested = environment.clone();
                nested.targets.push(Target {
                    label: None,
                    break_to: next,
                    continue_to: None,
                });
                let mut entries = vec![next; switch.cases.len()];
                let mut fallthrough = next;
                let mut default = next;
                for (index, case) in switch.cases.iter().enumerate().rev() {
                    fallthrough = self.sequence(&case.consequent, fallthrough, &nested)?;
                    entries[index] = fallthrough;
                    if case.test.is_none() {
                        default = fallthrough;
                    }
                }
                let mut dispatch = default;
                for (index, case) in switch.cases.iter().enumerate().rev() {
                    if let Some(test) = case.test {
                        let branch = self.node(vec![entries[index], dispatch], None)?;
                        dispatch = self.expression(test, branch, environment)?;
                    }
                }
                self.expression(switch.discriminant, dispatch, environment)
            }
            Statement::Try(try_) => self.try_statement(try_, next, environment),
            Statement::With(with) => {
                let body = self.statement(&with.body, next, environment, &[])?;
                self.expression(with.object, body, environment)
            }
            Statement::ExportNamed(export) => {
                if let Some(declaration) = export.declaration {
                    self.statement(&declaration, next, environment, &[])
                } else {
                    Ok(next)
                }
            }
            Statement::ExportDefault(export) => match export.declaration {
                ExportDefaultKind::Expression(expression) => {
                    self.expression(expression, next, environment)
                }
                ExportDefaultKind::Class(class) => self.class(class, next, environment),
                ExportDefaultKind::Function(_) => Ok(next),
            },
        }
    }

    fn iteration(
        &mut self,
        left: &ForLeft<'_>,
        right: Expression<'_>,
        body: &Statement<'_>,
        next: NodeId,
        environment: &Environment,
        labels: &[Atom],
    ) -> Result<NodeId> {
        let step = self.node(vec![], None)?;
        let nested = self.loop_environment(environment, next, step, labels);
        let body = self.statement(body, step, &nested, &[])?;
        let assigned = match left {
            ForLeft::Variable(variables) => self.variables(variables, body, &nested)?,
            ForLeft::Target(expression) => self.destructure(*expression, body, &nested)?,
        };
        self.connect(step, vec![assigned, next, environment.throw_to])?;
        self.expression(right, step, environment)
    }

    fn try_statement(
        &mut self,
        try_: &TryStatement<'_>,
        next: NodeId,
        environment: &Environment,
    ) -> Result<NodeId> {
        let mut protected = environment.clone();
        protected.flags.inside_exception = true;
        let mut incoming = protected.clone();
        let mut normal = next;
        if let Some(finalizer) = try_.finalizer {
            let mut destinations: FxHashMap<NodeId, NodeId> = FxHashMap::default();
            let mut wrap = |destination: NodeId| -> Result<NodeId> {
                if let Some(&entry) = destinations.get(&destination) {
                    return Ok(entry);
                }
                let entry = self.sequence(&finalizer.body, destination, &protected)?;
                destinations.insert(destination, entry);
                Ok(entry)
            };
            normal = wrap(next)?;
            incoming.return_to = wrap(environment.return_to)?;
            incoming.throw_to = wrap(environment.throw_to)?;
            for target in &mut incoming.targets {
                target.break_to = wrap(target.break_to)?;
                if let Some(continuation) = target.continue_to {
                    target.continue_to = Some(wrap(continuation)?);
                }
            }
        }
        if let Some(catch) = try_.handler {
            let mut entry = self.sequence(&catch.body.body, normal, &incoming)?;
            if let Some(parameter) = catch.param {
                entry = self.pattern(&parameter, entry, &incoming)?;
            }
            incoming.throw_to = entry;
        }
        self.sequence(&try_.block.body, normal, &incoming)
    }

    fn expression(
        &mut self,
        expression: Expression<'_>,
        next: NodeId,
        environment: &Environment,
    ) -> Result<NodeId> {
        self.chain(expression, next, next, environment)
    }
    fn expressions(
        &mut self,
        expressions: &[Expression<'_>],
        mut next: NodeId,
        environment: &Environment,
    ) -> Result<NodeId> {
        for &expression in expressions.iter().rev() {
            next = self.expression(expression, next, environment)?;
        }
        Ok(next)
    }
    fn key(
        &mut self,
        key: PropertyKey<'_>,
        next: NodeId,
        environment: &Environment,
    ) -> Result<NodeId> {
        if let PropertyKey::Computed(expression) = key {
            self.expression(expression, next, environment)
        } else {
            Ok(next)
        }
    }

    fn chain(
        &mut self,
        expression: Expression<'_>,
        next: NodeId,
        short: NodeId,
        environment: &Environment,
    ) -> Result<NodeId> {
        self.budget.step()?;
        match expression {
            Expression::Call(call) => {
                let invoked = self.call(call.span, next, environment)?;
                let arguments = self.expressions(&call.arguments, invoked, environment)?;
                let selected = if call.optional {
                    self.node(vec![short, arguments], None)?
                } else {
                    arguments
                };
                self.chain(call.callee, selected, short, environment)
            }
            Expression::Member(member) => {
                let accessed = self.effect(next, environment)?;
                let property = if let MemberProperty::Computed(key) = member.property {
                    self.expression(key, accessed, environment)?
                } else {
                    accessed
                };
                let selected = if member.optional {
                    self.node(vec![short, property], None)?
                } else {
                    property
                };
                self.chain(member.object, selected, short, environment)
            }
            Expression::Identifier(_) | Expression::This(_) | Expression::Super(_) => {
                self.effect(next, environment)
            }
            Expression::NumberLiteral(_)
            | Expression::StringLiteral(_)
            | Expression::BooleanLiteral(_)
            | Expression::NullLiteral(_)
            | Expression::BigIntLiteral(_)
            | Expression::RegExpLiteral(_)
            | Expression::MetaProperty(_)
            | Expression::Function(_)
            | Expression::Arrow(_) => Ok(next),
            Expression::Class(class) => self.class(class, next, environment),
            Expression::Logical(logical) => {
                let right = self.expression(logical.right, next, environment)?;
                let branch = self.node(vec![right, next], None)?;
                self.expression(logical.left, branch, environment)
            }
            Expression::Conditional(conditional) => {
                let yes = self.expression(conditional.consequent, next, environment)?;
                let no = self.expression(conditional.alternate, next, environment)?;
                let branch = self.node(
                    Self::condition_edges(Some(&conditional.test), yes, no),
                    None,
                )?;
                self.expression(conditional.test, branch, environment)
            }
            Expression::Assignment(assignment) => {
                let assigned = self.effect(next, environment)?;
                if matches!(
                    assignment.left,
                    Expression::Array(_) | Expression::Object(_)
                ) {
                    let target = self.destructure(assignment.left, assigned, environment)?;
                    self.expression(assignment.right, target, environment)
                } else {
                    let right = self.expression(assignment.right, assigned, environment)?;
                    let selected = if matches!(
                        assignment.operator,
                        AssignmentOperator::And
                            | AssignmentOperator::Or
                            | AssignmentOperator::Coalesce
                    ) {
                        self.node(vec![assigned, right], None)?
                    } else {
                        right
                    };
                    self.expression(assignment.left, selected, environment)
                }
            }
            Expression::Binary(binary) => {
                let result = self.effect(next, environment)?;
                let right = self.expression(binary.right, result, environment)?;
                self.expression(binary.left, right, environment)
            }
            Expression::PrivateIn(private) => {
                let result = self.effect(next, environment)?;
                self.expression(private.right, result, environment)
            }
            Expression::Unary(unary) => {
                let result = self.effect(next, environment)?;
                self.expression(unary.argument, result, environment)
            }
            Expression::Update(update) => {
                let result = self.effect(next, environment)?;
                self.expression(update.argument, result, environment)
            }
            Expression::Sequence(sequence) => {
                self.expressions(&sequence.expressions, next, environment)
            }
            Expression::Array(array) => {
                let mut entry = next;
                for expression in array.elements.iter().rev().flatten() {
                    entry = self.expression(*expression, entry, environment)?;
                }
                Ok(entry)
            }
            Expression::Object(object) => {
                let mut entry = next;
                for member in object.properties.iter().rev() {
                    match member {
                        ObjectMember::Property(property) => {
                            entry = self.expression(property.value, entry, environment)?;
                            entry = self.key(property.key, entry, environment)?;
                        }
                        ObjectMember::Spread(spread) => {
                            entry = self.effect(entry, environment)?;
                            entry = self.expression(spread.argument, entry, environment)?;
                        }
                    }
                }
                Ok(entry)
            }
            Expression::New(new) => {
                let invoked = self.call(new.span, next, environment)?;
                let arguments = self.expressions(&new.arguments, invoked, environment)?;
                self.expression(new.callee, arguments, environment)
            }
            Expression::TemplateLiteral(template) => {
                self.expressions(&template.expressions, next, environment)
            }
            Expression::TaggedTemplate(tagged) => {
                let invoked = self.call(tagged.span, next, environment)?;
                let arguments =
                    self.expressions(&tagged.quasi.expressions, invoked, environment)?;
                self.expression(tagged.tag, arguments, environment)
            }
            Expression::Spread(spread) => {
                let spread_end = self.effect(next, environment)?;
                self.expression(spread.argument, spread_end, environment)
            }
            Expression::Await(await_) => {
                let resumed = self.effect(next, environment)?;
                self.expression(await_.argument, resumed, environment)
            }
            Expression::Yield(yield_) => {
                let resumed = self.node(
                    vec![next, environment.throw_to, environment.return_to],
                    None,
                )?;
                if let Some(argument) = yield_.argument {
                    self.expression(argument, resumed, environment)
                } else {
                    Ok(resumed)
                }
            }
            Expression::Import(import) => {
                let imported = self.effect(next, environment)?;
                let options = if let Some(options) = import.options {
                    self.expression(options, imported, environment)?
                } else {
                    imported
                };
                self.expression(import.source, options, environment)
            }
        }
    }

    fn pattern(
        &mut self,
        pattern: &Pattern<'_>,
        next: NodeId,
        environment: &Environment,
    ) -> Result<NodeId> {
        self.budget.step()?;
        match pattern {
            Pattern::Ident(_) => Ok(next),
            Pattern::Rest(rest) => self.pattern(&rest.argument, next, environment),
            Pattern::Assignment(assignment) => {
                let target = self.pattern(&assignment.left, next, environment)?;
                let fallback = self.expression(assignment.right, target, environment)?;
                self.node(vec![target, fallback], None)
            }
            Pattern::Array(array) => {
                let mut entry = next;
                for pattern in array.elements.iter().rev().flatten() {
                    entry = self.pattern(pattern, entry, environment)?;
                    entry = self.effect(entry, environment)?;
                }
                self.effect(entry, environment)
            }
            Pattern::Object(object) => {
                let mut entry = if let Some(rest) = object.rest {
                    self.pattern(&rest.argument, next, environment)?
                } else {
                    next
                };
                for property in object.properties.iter().rev() {
                    entry = self.pattern(&property.value, entry, environment)?;
                    entry = self.effect(entry, environment)?;
                    entry = self.key(property.key, entry, environment)?;
                }
                self.effect(entry, environment)
            }
        }
    }
    fn destructure(
        &mut self,
        expression: Expression<'_>,
        next: NodeId,
        environment: &Environment,
    ) -> Result<NodeId> {
        self.budget.step()?;
        match expression {
            Expression::Identifier(_) => Ok(next),
            Expression::Assignment(assignment) => {
                let target = self.destructure(assignment.left, next, environment)?;
                let fallback = self.expression(assignment.right, target, environment)?;
                self.node(vec![target, fallback], None)
            }
            Expression::Spread(spread) => self.destructure(spread.argument, next, environment),
            Expression::Array(array) => {
                let mut entry = next;
                for expression in array.elements.iter().rev().flatten() {
                    entry = self.destructure(*expression, entry, environment)?;
                    entry = self.effect(entry, environment)?;
                }
                self.effect(entry, environment)
            }
            Expression::Object(object) => {
                let mut entry = next;
                for member in object.properties.iter().rev() {
                    match member {
                        ObjectMember::Property(property) => {
                            entry = self.destructure(property.value, entry, environment)?;
                            entry = self.effect(entry, environment)?;
                            entry = self.key(property.key, entry, environment)?;
                        }
                        ObjectMember::Spread(spread) => {
                            entry = self.destructure(spread.argument, entry, environment)?;
                        }
                    }
                }
                self.effect(entry, environment)
            }
            _ => self.expression(expression, next, environment),
        }
    }

    fn class(
        &mut self,
        class: &Class<'_>,
        next: NodeId,
        environment: &Environment,
    ) -> Result<NodeId> {
        let mut in_class = environment.clone();
        in_class.flags.in_class = true;
        let mut initialization = in_class.clone();
        initialization.flags.suppress_calls = true;
        let mut entry = next;
        // Static initialization executes now but owns independent call facts. Include its actual
        // control transfers here without publishing those same source calls as outer-region calls.
        for member in class.body.iter().rev() {
            match member {
                ClassMember::StaticBlock(block) => {
                    entry = self.sequence(&block.body, entry, &initialization)?
                }
                ClassMember::Property(property) if property.is_static => {
                    if let Some(value) = property.value {
                        entry = self.expression(value, entry, &initialization)?;
                    }
                }
                _ => {}
            }
        }
        for member in class.body.iter().rev() {
            match member {
                ClassMember::Method(method) => {
                    entry = self.key(method.key, entry, &in_class)?;
                    entry = self.expressions(&method.decorators, entry, &in_class)?;
                }
                ClassMember::Property(property) => {
                    entry = self.key(property.key, entry, &in_class)?;
                    entry = self.expressions(&property.decorators, entry, &in_class)?;
                }
                _ => {}
            }
        }
        if let Some(super_class) = class.super_class {
            entry = self.expression(super_class, entry, &in_class)?;
        }
        self.expressions(&class.decorators, entry, &in_class)
    }
}
