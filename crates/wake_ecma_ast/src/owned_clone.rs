//! Deep cloning from a borrowed arena AST into a new owning [`ModuleAst`].
//!
//! The clone is intentionally implemented in the AST crate.  Every recursive
//! match is exhaustive, so adding an AST variant makes this module fail to
//! compile until the ownership transfer is defined for that variant.

use bumpalo::Bump;
use std::sync::Arc;
use wake_common::Interner;

use crate::*;

/// Copy `program` and every arena-backed child into a fresh owning [`ModuleAst`].
///
/// Spans, interned atoms, flags, ordering, elisions, import attributes and
/// per-program helper metadata are preserved exactly.  The returned holder has
/// no lifetime relationship with `program`; the source holder may be dropped
/// immediately after this function returns.
pub fn clone_program_owned(program: &Program<'_>) -> ModuleAst {
    let structure_hash = crate::structure_hash(program);
    ModuleAst::from_builder_prehashed(structure_hash, |arena| {
        ProgramCloner { arena }.program(program)
    })
}

/// Copy a borrowed program while attaching the exact source bytes backing its spans.
///
/// This is primarily useful for tests and adapters that only received a borrowed `Program`; the
/// normal parser path already creates a source-owning `ModuleAst` without cloning the tree.
pub fn clone_program_owned_with_source(
    program: &Program<'_>,
    source: Arc<str>,
    interner: &Interner,
) -> ModuleAst {
    let structure_hash = crate::structure_hash(program);
    ModuleAst::from_source_builder_prehashed(source, interner.identity(), structure_hash, |arena| {
        ProgramCloner { arena }.program(program)
    })
}

struct ProgramCloner<'dst> {
    arena: &'dst Bump,
}

impl<'dst> ProgramCloner<'dst> {
    fn program(&self, source: &Program<'_>) -> Program<'dst> {
        Program {
            span: source.span,
            source_type: source.source_type,
            body: self.statements(&source.body),
            strict: source.strict,
            spread_helper: source.spread_helper,
            object_spread_helper: source.object_spread_helper,
            for_of_helper: source.for_of_helper,
        }
    }

    fn statements<'src>(&self, source: &[Statement<'src>]) -> AVec<'dst, Statement<'dst>> {
        let mut cloned = AVec::with_capacity_in(source.len(), self.arena);
        cloned.extend(
            source
                .iter()
                .copied()
                .map(|statement| self.statement(statement)),
        );
        cloned
    }

    fn expressions<'src>(&self, source: &[Expression<'src>]) -> AVec<'dst, Expression<'dst>> {
        let mut cloned = AVec::with_capacity_in(source.len(), self.arena);
        cloned.extend(
            source
                .iter()
                .copied()
                .map(|expression| self.expression(expression)),
        );
        cloned
    }

    fn optional_expressions<'src>(
        &self,
        source: &[Option<Expression<'src>>],
    ) -> AVec<'dst, Option<Expression<'dst>>> {
        let mut cloned = AVec::with_capacity_in(source.len(), self.arena);
        cloned.extend(
            source
                .iter()
                .copied()
                .map(|expression| expression.map(|expression| self.expression(expression))),
        );
        cloned
    }

    fn patterns<'src>(&self, source: &[Pattern<'src>]) -> AVec<'dst, Pattern<'dst>> {
        let mut cloned = AVec::with_capacity_in(source.len(), self.arena);
        cloned.extend(source.iter().copied().map(|pattern| self.pattern(pattern)));
        cloned
    }

    fn statement<'src>(&self, source: Statement<'src>) -> Statement<'dst> {
        match source {
            Statement::VariableDeclaration(declaration) => {
                Statement::VariableDeclaration(self.variable_declaration(declaration))
            }
            Statement::FunctionDeclaration(function) => {
                Statement::FunctionDeclaration(self.function(function))
            }
            Statement::ClassDeclaration(class) => Statement::ClassDeclaration(self.class(class)),
            Statement::Block(block) => Statement::Block(self.block_statement(block)),
            Statement::Empty(span) => Statement::Empty(span),
            Statement::Expression(statement) => {
                Statement::Expression(self.arena.alloc(ExpressionStatement {
                    span: statement.span,
                    expression: self.expression(statement.expression),
                }))
            }
            Statement::If(statement) => Statement::If(
                self.arena.alloc(IfStatement {
                    span: statement.span,
                    test: self.expression(statement.test),
                    consequent: self.statement(statement.consequent),
                    alternate: statement
                        .alternate
                        .map(|alternate| self.statement(alternate)),
                }),
            ),
            Statement::For(statement) => Statement::For(self.arena.alloc(ForStatement {
                span: statement.span,
                init: statement.init.map(|init| self.for_init(init)),
                test: statement.test.map(|test| self.expression(test)),
                update: statement.update.map(|update| self.expression(update)),
                body: self.statement(statement.body),
            })),
            Statement::ForIn(statement) => Statement::ForIn(self.arena.alloc(ForInStatement {
                span: statement.span,
                left: self.for_left(statement.left),
                right: self.expression(statement.right),
                body: self.statement(statement.body),
            })),
            Statement::ForOf(statement) => Statement::ForOf(self.arena.alloc(ForOfStatement {
                span: statement.span,
                left: self.for_left(statement.left),
                right: self.expression(statement.right),
                body: self.statement(statement.body),
                is_await: statement.is_await,
            })),
            Statement::While(statement) => Statement::While(self.arena.alloc(WhileStatement {
                span: statement.span,
                test: self.expression(statement.test),
                body: self.statement(statement.body),
            })),
            Statement::DoWhile(statement) => {
                Statement::DoWhile(self.arena.alloc(DoWhileStatement {
                    span: statement.span,
                    body: self.statement(statement.body),
                    test: self.expression(statement.test),
                }))
            }
            Statement::Switch(statement) => {
                let mut cases = AVec::with_capacity_in(statement.cases.len(), self.arena);
                cases.extend(statement.cases.iter().map(|case| SwitchCase {
                    span: case.span,
                    test: case.test.map(|test| self.expression(test)),
                    consequent: self.statements(&case.consequent),
                }));
                Statement::Switch(self.arena.alloc(SwitchStatement {
                    span: statement.span,
                    discriminant: self.expression(statement.discriminant),
                    cases,
                }))
            }
            Statement::Return(statement) => Statement::Return(self.arena.alloc(ReturnStatement {
                span: statement.span,
                argument: statement.argument.map(|argument| self.expression(argument)),
            })),
            Statement::Break(statement) => Statement::Break(self.arena.alloc(*statement)),
            Statement::Continue(statement) => Statement::Continue(self.arena.alloc(*statement)),
            Statement::Throw(statement) => Statement::Throw(self.arena.alloc(ThrowStatement {
                span: statement.span,
                argument: self.expression(statement.argument),
            })),
            Statement::Try(statement) => Statement::Try(
                self.arena.alloc(TryStatement {
                    span: statement.span,
                    block: self.block_statement(statement.block),
                    handler: statement.handler.map(|handler| self.catch_clause(handler)),
                    finalizer: statement
                        .finalizer
                        .map(|finalizer| self.block_statement(finalizer)),
                }),
            ),
            Statement::Labeled(statement) => {
                Statement::Labeled(self.arena.alloc(LabeledStatement {
                    span: statement.span,
                    label: statement.label,
                    body: self.statement(statement.body),
                }))
            }
            Statement::With(statement) => Statement::With(self.arena.alloc(WithStatement {
                span: statement.span,
                object: self.expression(statement.object),
                body: self.statement(statement.body),
            })),
            Statement::Debugger(span) => Statement::Debugger(span),
            Statement::Import(declaration) => {
                let mut specifiers =
                    AVec::with_capacity_in(declaration.specifiers.len(), self.arena);
                specifiers.extend(declaration.specifiers.iter().copied());
                Statement::Import(self.arena.alloc(ImportDeclaration {
                    span: declaration.span,
                    specifiers,
                    source: declaration.source,
                    attributes: self.import_attributes(declaration.attributes),
                }))
            }
            Statement::ExportNamed(declaration) => {
                let mut specifiers =
                    AVec::with_capacity_in(declaration.specifiers.len(), self.arena);
                specifiers.extend(declaration.specifiers.iter().copied());
                Statement::ExportNamed(
                    self.arena.alloc(ExportNamedDeclaration {
                        span: declaration.span,
                        declaration: declaration
                            .declaration
                            .map(|declaration| self.statement(declaration)),
                        specifiers,
                        source: declaration.source,
                        attributes: self.import_attributes(declaration.attributes),
                    }),
                )
            }
            Statement::ExportDefault(declaration) => {
                let span = declaration.span;
                let declaration = match declaration.declaration {
                    ExportDefaultKind::Function(function) => {
                        ExportDefaultKind::Function(self.function(function))
                    }
                    ExportDefaultKind::Class(class) => ExportDefaultKind::Class(self.class(class)),
                    ExportDefaultKind::Expression(expression) => {
                        ExportDefaultKind::Expression(self.expression(expression))
                    }
                };
                Statement::ExportDefault(
                    self.arena
                        .alloc(ExportDefaultDeclaration { span, declaration }),
                )
            }
            Statement::ExportAll(declaration) => {
                Statement::ExportAll(self.arena.alloc(ExportAllDeclaration {
                    span: declaration.span,
                    exported: declaration.exported,
                    source: declaration.source,
                    attributes: self.import_attributes(declaration.attributes),
                }))
            }
        }
    }

    fn variable_declaration<'src>(
        &self,
        source: &VariableDeclaration<'src>,
    ) -> &'dst VariableDeclaration<'dst> {
        let mut declarations = AVec::with_capacity_in(source.declarations.len(), self.arena);
        declarations.extend(
            source
                .declarations
                .iter()
                .map(|declaration| VariableDeclarator {
                    span: declaration.span,
                    id: self.pattern(declaration.id),
                    init: declaration.init.map(|init| self.expression(init)),
                }),
        );
        self.arena.alloc(VariableDeclaration {
            span: source.span,
            kind: source.kind,
            declarations,
        })
    }

    fn block_statement<'src>(&self, source: &BlockStatement<'src>) -> &'dst BlockStatement<'dst> {
        self.arena.alloc(BlockStatement {
            span: source.span,
            body: self.statements(&source.body),
        })
    }

    fn catch_clause<'src>(&self, source: &CatchClause<'src>) -> &'dst CatchClause<'dst> {
        self.arena.alloc(CatchClause {
            span: source.span,
            param: source.param.map(|param| self.pattern(param)),
            body: self.block_statement(source.body),
        })
    }

    fn for_init<'src>(&self, source: ForInit<'src>) -> ForInit<'dst> {
        match source {
            ForInit::Variable(declaration) => {
                ForInit::Variable(self.variable_declaration(declaration))
            }
            ForInit::Expression(expression) => ForInit::Expression(self.expression(expression)),
        }
    }

    fn for_left<'src>(&self, source: ForLeft<'src>) -> ForLeft<'dst> {
        match source {
            ForLeft::Variable(declaration) => {
                ForLeft::Variable(self.variable_declaration(declaration))
            }
            ForLeft::Target(expression) => ForLeft::Target(self.expression(expression)),
        }
    }

    fn pattern<'src>(&self, source: Pattern<'src>) -> Pattern<'dst> {
        match source {
            Pattern::Ident(ident) => Pattern::Ident(self.arena.alloc(*ident)),
            Pattern::Array(pattern) => {
                let mut elements = AVec::with_capacity_in(pattern.elements.len(), self.arena);
                elements.extend(
                    pattern
                        .elements
                        .iter()
                        .copied()
                        .map(|element| element.map(|element| self.pattern(element))),
                );
                Pattern::Array(self.arena.alloc(ArrayPattern {
                    span: pattern.span,
                    elements,
                }))
            }
            Pattern::Object(pattern) => {
                let mut properties = AVec::with_capacity_in(pattern.properties.len(), self.arena);
                properties.extend(pattern.properties.iter().map(|property| {
                    ObjectPatternProperty {
                        span: property.span,
                        key: self.property_key(property.key),
                        value: self.pattern(property.value),
                        shorthand: property.shorthand,
                        computed: property.computed,
                    }
                }));
                Pattern::Object(self.arena.alloc(ObjectPattern {
                    span: pattern.span,
                    properties,
                    rest: pattern.rest.map(|rest| self.rest_element(rest)),
                }))
            }
            Pattern::Assignment(pattern) => {
                Pattern::Assignment(self.arena.alloc(AssignmentPattern {
                    span: pattern.span,
                    left: self.pattern(pattern.left),
                    right: self.expression(pattern.right),
                }))
            }
            Pattern::Rest(rest) => Pattern::Rest(self.rest_element(rest)),
        }
    }

    fn rest_element<'src>(&self, source: &RestElement<'src>) -> &'dst RestElement<'dst> {
        self.arena.alloc(RestElement {
            span: source.span,
            argument: self.pattern(source.argument),
        })
    }

    fn expression<'src>(&self, source: Expression<'src>) -> Expression<'dst> {
        match source {
            Expression::NumberLiteral(literal) => {
                Expression::NumberLiteral(self.arena.alloc(*literal))
            }
            Expression::StringLiteral(literal) => {
                Expression::StringLiteral(self.arena.alloc(*literal))
            }
            Expression::BooleanLiteral(literal) => {
                Expression::BooleanLiteral(self.arena.alloc(*literal))
            }
            Expression::NullLiteral(span) => Expression::NullLiteral(span),
            Expression::BigIntLiteral(literal) => {
                Expression::BigIntLiteral(self.arena.alloc(*literal))
            }
            Expression::RegExpLiteral(literal) => {
                Expression::RegExpLiteral(self.arena.alloc(*literal))
            }
            Expression::TemplateLiteral(template) => {
                Expression::TemplateLiteral(self.template_literal(template))
            }
            Expression::Identifier(ident) => Expression::Identifier(self.arena.alloc(*ident)),
            Expression::This(span) => Expression::This(span),
            Expression::Super(span) => Expression::Super(span),
            Expression::MetaProperty(property) => {
                Expression::MetaProperty(self.arena.alloc(*property))
            }
            Expression::Array(array) => Expression::Array(self.arena.alloc(ArrayExpression {
                span: array.span,
                elements: self.optional_expressions(&array.elements),
            })),
            Expression::Object(object) => {
                let mut properties = AVec::with_capacity_in(object.properties.len(), self.arena);
                properties.extend(object.properties.iter().map(|member| match member {
                    ObjectMember::Property(property) => {
                        ObjectMember::Property(self.object_property(property))
                    }
                    ObjectMember::Spread(spread) => {
                        ObjectMember::Spread(self.spread_element(spread))
                    }
                }));
                Expression::Object(self.arena.alloc(ObjectExpression {
                    span: object.span,
                    properties,
                }))
            }
            Expression::Function(function) => Expression::Function(self.function(function)),
            Expression::Arrow(function) => Expression::Arrow(self.arrow_function(function)),
            Expression::Class(class) => Expression::Class(self.class(class)),
            Expression::Unary(expression) => Expression::Unary(self.arena.alloc(UnaryExpression {
                span: expression.span,
                operator: expression.operator,
                argument: self.expression(expression.argument),
            })),
            Expression::Update(expression) => {
                Expression::Update(self.arena.alloc(UpdateExpression {
                    span: expression.span,
                    operator: expression.operator,
                    prefix: expression.prefix,
                    argument: self.expression(expression.argument),
                }))
            }
            Expression::Binary(expression) => {
                Expression::Binary(self.arena.alloc(BinaryExpression {
                    span: expression.span,
                    operator: expression.operator,
                    left: self.expression(expression.left),
                    right: self.expression(expression.right),
                }))
            }
            Expression::Logical(expression) => {
                Expression::Logical(self.arena.alloc(LogicalExpression {
                    span: expression.span,
                    operator: expression.operator,
                    left: self.expression(expression.left),
                    right: self.expression(expression.right),
                }))
            }
            Expression::Assignment(expression) => {
                Expression::Assignment(self.arena.alloc(AssignmentExpression {
                    span: expression.span,
                    operator: expression.operator,
                    left: self.expression(expression.left),
                    right: self.expression(expression.right),
                }))
            }
            Expression::Conditional(expression) => {
                Expression::Conditional(self.arena.alloc(ConditionalExpression {
                    span: expression.span,
                    test: self.expression(expression.test),
                    consequent: self.expression(expression.consequent),
                    alternate: self.expression(expression.alternate),
                }))
            }
            Expression::Call(expression) => Expression::Call(self.arena.alloc(CallExpression {
                span: expression.span,
                callee: self.expression(expression.callee),
                arguments: self.expressions(&expression.arguments),
                optional: expression.optional,
            })),
            Expression::New(expression) => Expression::New(self.arena.alloc(NewExpression {
                span: expression.span,
                callee: self.expression(expression.callee),
                arguments: self.expressions(&expression.arguments),
            })),
            Expression::Member(expression) => {
                Expression::Member(self.arena.alloc(MemberExpression {
                    span: expression.span,
                    object: self.expression(expression.object),
                    property: self.member_property(expression.property),
                    optional: expression.optional,
                }))
            }
            Expression::Sequence(expression) => {
                Expression::Sequence(self.arena.alloc(SequenceExpression {
                    span: expression.span,
                    expressions: self.expressions(&expression.expressions),
                }))
            }
            Expression::TaggedTemplate(expression) => {
                Expression::TaggedTemplate(self.arena.alloc(TaggedTemplateExpression {
                    span: expression.span,
                    tag: self.expression(expression.tag),
                    quasi: self.template_literal(expression.quasi),
                }))
            }
            Expression::Spread(spread) => Expression::Spread(self.spread_element(spread)),
            Expression::Await(expression) => Expression::Await(self.arena.alloc(AwaitExpression {
                span: expression.span,
                argument: self.expression(expression.argument),
            })),
            Expression::Yield(expression) => Expression::Yield(
                self.arena.alloc(YieldExpression {
                    span: expression.span,
                    argument: expression
                        .argument
                        .map(|argument| self.expression(argument)),
                    delegate: expression.delegate,
                }),
            ),
            Expression::Import(expression) => {
                Expression::Import(self.arena.alloc(ImportExpression {
                    span: expression.span,
                    source: self.expression(expression.source),
                    options: expression.options.map(|options| self.expression(options)),
                }))
            }
        }
    }

    fn member_property<'src>(&self, source: MemberProperty<'src>) -> MemberProperty<'dst> {
        match source {
            MemberProperty::Ident(ident) => MemberProperty::Ident(ident),
            MemberProperty::Computed(expression) => {
                MemberProperty::Computed(self.expression(expression))
            }
            MemberProperty::Private(ident) => MemberProperty::Private(ident),
        }
    }

    fn property_key<'src>(&self, source: PropertyKey<'src>) -> PropertyKey<'dst> {
        match source {
            PropertyKey::Ident(ident) => PropertyKey::Ident(ident),
            PropertyKey::String(literal) => PropertyKey::String(self.arena.alloc(*literal)),
            PropertyKey::Number(literal) => PropertyKey::Number(self.arena.alloc(*literal)),
            PropertyKey::Computed(expression) => PropertyKey::Computed(self.expression(expression)),
            PropertyKey::Private(ident) => PropertyKey::Private(ident),
        }
    }

    fn template_literal<'src>(
        &self,
        source: &TemplateLiteral<'src>,
    ) -> &'dst TemplateLiteral<'dst> {
        let mut quasis = AVec::with_capacity_in(source.quasis.len(), self.arena);
        quasis.extend(source.quasis.iter().copied());
        self.arena.alloc(TemplateLiteral {
            span: source.span,
            quasis,
            expressions: self.expressions(&source.expressions),
        })
    }

    fn object_property<'src>(&self, source: &ObjectProperty<'src>) -> &'dst ObjectProperty<'dst> {
        self.arena.alloc(ObjectProperty {
            span: source.span,
            key: self.property_key(source.key),
            value: self.expression(source.value),
            kind: source.kind,
            method: source.method,
            shorthand: source.shorthand,
            computed: source.computed,
            prototype_setter: source.prototype_setter,
        })
    }

    fn spread_element<'src>(&self, source: &SpreadElement<'src>) -> &'dst SpreadElement<'dst> {
        self.arena.alloc(SpreadElement {
            span: source.span,
            argument: self.expression(source.argument),
        })
    }

    fn function<'src>(&self, source: &Function<'src>) -> &'dst Function<'dst> {
        self.arena.alloc(Function {
            span: source.span,
            id: source.id,
            params: self.patterns(&source.params),
            body: source.body.map(|body| self.function_body(body)),
            is_async: source.is_async,
            is_generator: source.is_generator,
        })
    }

    fn function_body<'src>(&self, source: &FunctionBody<'src>) -> &'dst FunctionBody<'dst> {
        self.arena.alloc(FunctionBody {
            span: source.span,
            statements: self.statements(&source.statements),
            strict: source.strict,
        })
    }

    fn arrow_function<'src>(&self, source: &ArrowFunction<'src>) -> &'dst ArrowFunction<'dst> {
        let body = match source.body {
            ArrowBody::Block(body) => ArrowBody::Block(self.function_body(body)),
            ArrowBody::Expression(expression) => ArrowBody::Expression(self.expression(expression)),
        };
        self.arena.alloc(ArrowFunction {
            span: source.span,
            params: self.patterns(&source.params),
            body,
            is_async: source.is_async,
        })
    }

    fn class<'src>(&self, source: &Class<'src>) -> &'dst Class<'dst> {
        let mut body = AVec::with_capacity_in(source.body.len(), self.arena);
        body.extend(source.body.iter().map(|member| match member {
            ClassMember::Method(method) => {
                ClassMember::Method(self.arena.alloc(MethodDefinition {
                    span: method.span,
                    key: self.property_key(method.key),
                    value: self.function(method.value),
                    kind: method.kind,
                    is_static: method.is_static,
                    computed: method.computed,
                    decorators: self.expressions(&method.decorators),
                }))
            }
            ClassMember::Property(property) => {
                ClassMember::Property(self.arena.alloc(PropertyDefinition {
                    span: property.span,
                    key: self.property_key(property.key),
                    value: property.value.map(|value| self.expression(value)),
                    is_static: property.is_static,
                    computed: property.computed,
                    decorators: self.expressions(&property.decorators),
                    accessor: property.accessor,
                }))
            }
            ClassMember::StaticBlock(block) => {
                ClassMember::StaticBlock(self.arena.alloc(StaticBlock {
                    span: block.span,
                    body: self.statements(&block.body),
                }))
            }
        }));
        self.arena.alloc(Class {
            span: source.span,
            id: source.id,
            super_class: source
                .super_class
                .map(|super_class| self.expression(super_class)),
            body,
            decorators: self.expressions(&source.decorators),
        })
    }

    fn import_attributes<'src>(
        &self,
        source: Option<&ImportAttributes<'src>>,
    ) -> Option<&'dst ImportAttributes<'dst>> {
        source.map(|attributes| {
            let items: &'dst [ImportAttribute] = self.arena.alloc_slice_copy(attributes.items);
            &*self.arena.alloc(ImportAttributes {
                span: attributes.span,
                keyword: attributes.keyword,
                items,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use wake_common::{Interner, Span};

    use super::*;

    #[test]
    fn cloned_program_outlives_its_source_arena() {
        let interner = Interner::new();
        let source = ModuleAst::build_sample(&interner, 32);
        let expected_hash = source.structure_hash();

        let cloned = source.with_ast(clone_program_owned);
        assert_eq!(cloned.structure_hash(), expected_hash);
        cloned.with_ast(|program| {
            assert_eq!(crate::structure_hash(program), expected_hash);
        });

        drop(source);

        cloned.with_ast(|program| {
            assert_eq!(program.body.len(), 1);
            let Statement::VariableDeclaration(declaration) = program.body[0] else {
                panic!("sample clone lost its variable declaration");
            };
            assert_eq!(declaration.declarations.len(), 1);
            assert!(matches!(declaration.declarations[0].id, Pattern::Ident(_)));
            assert!(matches!(
                declaration.declarations[0].init,
                Some(Expression::Binary(_))
            ));
        });
    }

    #[test]
    fn cloned_program_owns_import_attribute_slices_and_preserves_metadata() {
        let interner = Interner::new();
        let type_name = interner.intern("type");
        let json = interner.intern("json");
        let source_name = interner.intern("./data.json");
        let local_name = interner.intern("data");
        let spread_helper = interner.intern("__spread");
        let object_spread_helper = interner.intern("__object_spread");
        let for_of_helper = interner.intern("__for_of");
        let program_span = Span::new(3, 97);
        let attribute_span = Span::new(35, 55);

        let source = ModuleAst::from_builder(move |arena| {
            let items = arena.alloc_slice_copy(&[ImportAttribute {
                span: attribute_span,
                key: ModuleExportName::Ident(Ident::new(attribute_span, type_name)),
                value: json,
            }]);
            let attributes = arena.alloc(ImportAttributes {
                span: attribute_span,
                keyword: AttributesKeyword::With,
                items,
            });
            let mut specifiers = AVec::new_in(arena);
            specifiers.push(ImportSpecifier::Default {
                span: Span::new(10, 14),
                local: Ident::new(Span::new(10, 14), local_name),
            });
            let import = arena.alloc(ImportDeclaration {
                span: program_span,
                specifiers,
                source: source_name,
                attributes: Some(attributes),
            });
            let mut program = Program::new_in(arena, SourceType::TypeScript);
            program.span = program_span;
            program.strict = false;
            program.spread_helper = Some(spread_helper);
            program.object_spread_helper = Some(object_spread_helper);
            program.for_of_helper = Some(for_of_helper);
            program.body.push(Statement::Import(import));
            program
        });

        let source_items_address = source.with_ast(|program| {
            let Statement::Import(declaration) = program.body[0] else {
                unreachable!();
            };
            declaration.attributes.unwrap().items.as_ptr() as usize
        });
        let cloned = source.with_ast(clone_program_owned);
        let cloned_items_address = cloned.with_ast(|program| {
            let Statement::Import(declaration) = program.body[0] else {
                unreachable!();
            };
            declaration.attributes.unwrap().items.as_ptr() as usize
        });
        assert_ne!(source_items_address, cloned_items_address);
        assert_eq!(source.structure_hash(), cloned.structure_hash());

        drop(source);

        cloned.with_ast(|program| {
            assert_eq!(program.span, program_span);
            assert_eq!(program.source_type, SourceType::TypeScript);
            assert!(!program.strict);
            assert_eq!(program.spread_helper, Some(spread_helper));
            assert_eq!(program.object_spread_helper, Some(object_spread_helper));
            assert_eq!(program.for_of_helper, Some(for_of_helper));
            assert_eq!(crate::structure_hash(program), cloned.structure_hash());

            let Statement::Import(declaration) = program.body[0] else {
                panic!("clone lost its import declaration");
            };
            assert_eq!(declaration.span, program_span);
            assert_eq!(declaration.source, source_name);
            let attributes = declaration
                .attributes
                .expect("attributes must survive clone");
            assert_eq!(attributes.span, attribute_span);
            assert_eq!(attributes.keyword, AttributesKeyword::With);
            assert_eq!(attributes.items.len(), 1);
            assert_eq!(attributes.items[0].span, attribute_span);
            assert_eq!(attributes.items[0].value, json);
            let ModuleExportName::Ident(key) = attributes.items[0].key else {
                panic!("attribute key kind changed during clone");
            };
            assert_eq!(key.name, type_name);
        });
    }
}
