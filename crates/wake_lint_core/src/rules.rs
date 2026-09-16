use std::collections::BTreeMap;

use wake_common::{Atom, Interner, JsString, Span};
use wake_ecma_ast::*;
use wake_ecma_parser::{Comment, SourceToken};

use crate::{EffectiveRule, LintDiagnostic, RuleLevel};

pub(crate) struct RuleVisitor<'s> {
    source: &'s str,
    interner: &'s Interner,
    comments: &'s [Comment],
    tokens: &'s [SourceToken],
    levels: &'s BTreeMap<String, RuleLevel>,
    configuration: &'s BTreeMap<String, EffectiveRule>,
    pub diagnostics: Vec<LintDiagnostic>,
}

impl<'s> RuleVisitor<'s> {
    pub fn new(
        source: &'s str,
        interner: &'s Interner,
        comments: &'s [Comment],
        tokens: &'s [SourceToken],
        levels: &'s BTreeMap<String, RuleLevel>,
        configuration: &'s BTreeMap<String, EffectiveRule>,
    ) -> Self {
        Self {
            source,
            interner,
            comments,
            tokens,
            levels,
            configuration,
            diagnostics: Vec::new(),
        }
    }

    fn report(&mut self, id: &str, span: Span, message_id: &str, message: &str) {
        let level = self.levels[id];
        if level != RuleLevel::Off {
            self.diagnostics.push(LintDiagnostic {
                rule_id: id.into(),
                level,
                message_id: message_id.into(),
                message: message.into(),
                start: span.lo,
                end: span.hi,
                fix: None,
            });
        }
    }

    fn boolean_option(&self, id: &str, name: &str) -> bool {
        self.configuration[id].configuration.options[name]
            .as_bool()
            .expect("validated boolean option")
    }

    fn text(&self, atom: Atom) -> String {
        self.interner.with_resolved(atom, str::to_owned)
    }

    fn check_block(&mut self, block: &BlockStatement<'_>) {
        if block.body.is_empty()
            && !self
                .comments
                .iter()
                .any(|c| c.span.lo >= block.span.lo && c.span.hi <= block.span.hi)
        {
            self.report(
                "js/no-empty",
                block.span,
                "empty",
                "Empty block without an explanatory comment.",
            );
        }
    }

    fn check_condition(&mut self, expression: Expression<'_>) {
        if self.truthiness(expression).is_some() {
            self.report(
                "js/no-constant-condition",
                expression.span(),
                "constant",
                "Condition has a constant truthiness.",
            );
        }
    }

    fn check_condition_assignments(&mut self, expression: Expression<'_>) {
        if self.levels["js/no-cond-assign"] == RuleLevel::Off {
            return;
        }
        let mut assignments = ConditionAssignments(Vec::new());
        assignments.visit_expression(&expression);
        for span in assignments.0 {
            self.report(
                "js/no-cond-assign",
                span,
                "assignment",
                "Unexpected assignment in a condition.",
            );
        }
    }

    fn check_var(&mut self, declaration: &VariableDeclaration<'_>) {
        if declaration.kind == VarKind::Var
            && self
                .tokens
                .binary_search_by_key(&declaration.span.lo, |token| token.span.lo)
                .ok()
                .is_some_and(|index| {
                    self.source
                        [self.tokens[index].span.lo as usize..self.tokens[index].span.hi as usize]
                        == *"var"
                })
        {
            self.report(
                "js/no-var",
                Span::new(declaration.span.lo, declaration.span.lo + 3),
                "var",
                "Use a lexical declaration instead of var.",
            );
        }
    }

    fn check_typeof(&mut self, binary: &BinaryExpression<'_>) {
        if !matches!(
            binary.operator,
            BinaryOperator::Eq
                | BinaryOperator::NotEq
                | BinaryOperator::StrictEq
                | BinaryOperator::StrictNotEq
        ) {
            return;
        }
        for (query, literal) in [(binary.left, binary.right), (binary.right, binary.left)] {
            if let Expression::Unary(query) = query
                && query.operator == UnaryOperator::Typeof
                && let Expression::StringLiteral(literal) = literal
                && ![
                    "undefined",
                    "object",
                    "boolean",
                    "number",
                    "string",
                    "function",
                    "symbol",
                    "bigint",
                ]
                .iter()
                .any(|expected| self.interner.resolve_js(literal.value) == *expected)
            {
                self.report(
                    "js/valid-typeof",
                    literal.span,
                    "invalid",
                    "Invalid literal typeof result.",
                );
            }
        }
    }

    fn check_self_assignment(&mut self, left: Expression<'_>, right: Expression<'_>) {
        match (left, right) {
            (Expression::Identifier(a), Expression::Identifier(b)) if a.name == b.name => {
                self.report(
                    "js/no-self-assign",
                    b.span,
                    "self",
                    "Identifier is assigned to itself.",
                );
            }
            (Expression::Array(a), Expression::Array(b)) => {
                for (left, right) in a.elements.iter().zip(&b.elements) {
                    if matches!(left, Some(Expression::Spread(_)))
                        || matches!(right, Some(Expression::Spread(_)))
                    {
                        break;
                    }
                    if let (Some(left), Some(right)) = (left, right) {
                        self.check_self_assignment(*left, *right);
                    }
                }
            }
            (Expression::Object(a), Expression::Object(b)) => {
                let mut values = BTreeMap::new();
                for member in &b.properties {
                    let ObjectMember::Property(property) = member else {
                        return;
                    };
                    if property.kind != PropertyKind::Init {
                        return;
                    }
                    let Some((key, _)) = self.key(property.key) else {
                        return;
                    };
                    values.insert(key, property.value);
                }
                for member in &a.properties {
                    if let ObjectMember::Property(property) = member
                        && let Some((key, _)) = self.key(property.key)
                        && let Some(right) = values.get(&key)
                    {
                        self.check_self_assignment(property.value, *right);
                    }
                }
            }
            _ => {}
        }
    }

    fn check_self_comparison(&mut self, binary: &BinaryExpression<'_>) {
        if !is_comparison(binary.operator) {
            return;
        }
        let same = matches!((binary.left, binary.right), (Expression::Identifier(a), Expression::Identifier(b)) if a.name == b.name)
            || matches!((self.literal(binary.left), self.literal(binary.right)), (Some(a), Some(b)) if a == b);
        if same {
            self.report(
                "js/no-self-compare",
                binary.span,
                "self",
                "Both comparison operands are the same.",
            );
        }
    }

    fn truthiness(&self, expression: Expression<'_>) -> Option<bool> {
        match expression {
            Expression::BooleanLiteral(v) => Some(v.value),
            Expression::NullLiteral(_) => Some(false),
            Expression::NumberLiteral(v) => Some(v.value != 0.0 && !v.value.is_nan()),
            Expression::StringLiteral(v) => Some(!self.interner.resolve_js(v.value).is_empty()),
            Expression::Array(_)
            | Expression::Object(_)
            | Expression::Function(_)
            | Expression::Arrow(_)
            | Expression::Class(_)
            | Expression::RegExpLiteral(_) => Some(true),
            Expression::Unary(v) => match v.operator {
                UnaryOperator::LogicalNot => self.truthiness(v.argument).map(|v| !v),
                UnaryOperator::Typeof => Some(true),
                UnaryOperator::Void => Some(false),
                _ => None,
            },
            Expression::Logical(v) => {
                let left = self.truthiness(v.left);
                let right = self.truthiness(v.right);
                match v.operator {
                    LogicalOperator::And => match left {
                        Some(false) => Some(false),
                        Some(true) => right,
                        None if right == Some(false) => Some(false),
                        _ => None,
                    },
                    LogicalOperator::Or => match left {
                        Some(true) => Some(true),
                        Some(false) => right,
                        None if right == Some(true) => Some(true),
                        _ => None,
                    },
                    LogicalOperator::Coalesce => None,
                }
            }
            Expression::Sequence(v) => v.expressions.last().and_then(|e| self.truthiness(*e)),
            Expression::Assignment(v) if v.operator == AssignmentOperator::Assign => {
                self.truthiness(v.right)
            }
            _ => None,
        }
    }

    fn literal(&self, expression: Expression<'_>) -> Option<Literal> {
        match expression {
            Expression::StringLiteral(v) => {
                Some(Literal::String(self.interner.resolve_js(v.value)))
            }
            Expression::NumberLiteral(v) => Some(Literal::Number(v.value)),
            Expression::BooleanLiteral(v) => Some(Literal::Bool(v.value)),
            Expression::NullLiteral(_) => Some(Literal::Null),
            Expression::Unary(v)
                if matches!(v.operator, UnaryOperator::Minus | UnaryOperator::Plus) =>
            {
                if let Expression::NumberLiteral(number) = v.argument {
                    Some(Literal::Number(if v.operator == UnaryOperator::Minus {
                        -number.value
                    } else {
                        number.value
                    }))
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    fn key(&self, key: PropertyKey<'_>) -> Option<(JsString, Span)> {
        match key {
            PropertyKey::Ident(v) | PropertyKey::Private(v) => {
                Some((self.text(v.name).into(), v.span))
            }
            PropertyKey::String(v) => Some((self.interner.resolve_js(v.value), v.span)),
            PropertyKey::Number(v) => Some((number_key(v.value).into(), v.span)),
            PropertyKey::Computed(expression) => self.literal(expression).map(|literal| {
                (
                    match literal {
                        Literal::String(v) => v,
                        Literal::Number(v) => number_key(v).into(),
                        Literal::Bool(v) => v.to_string().into(),
                        Literal::Null => "null".into(),
                    },
                    expression.span(),
                )
            }),
        }
    }

    fn check_object(&mut self, object: &ObjectExpression<'_>) {
        // JSX props/development objects carry synthetic or element spans. Only a real source
        // object starts with its own opening brace; nested user objects still get visited.
        if self.source.as_bytes().get(object.span.lo as usize) != Some(&b'{') {
            return;
        }
        let mut keys = BTreeMap::<JsString, u8>::new();
        for member in &object.properties {
            let ObjectMember::Property(property) = member else {
                continue;
            };
            let Some((name, span)) = self.key(property.key) else {
                continue;
            };
            let kind = match property.kind {
                PropertyKind::Init => 3,
                PropertyKind::Get => 1,
                PropertyKind::Set => 2,
            };
            let previous = keys.entry(name).or_default();
            if *previous & kind != 0 {
                self.report(
                    "js/no-dupe-keys",
                    span,
                    "duplicate",
                    "Duplicate object property.",
                );
            }
            *previous |= kind;
        }
    }
}

impl<'a> Visit<'a> for RuleVisitor<'_> {
    fn visit_statement(&mut self, statement: &Statement<'a>) {
        match statement {
            Statement::If(value) => self.check_condition_assignments(value.test),
            Statement::While(value) => self.check_condition_assignments(value.test),
            Statement::DoWhile(value) => self.check_condition_assignments(value.test),
            Statement::VariableDeclaration(value) => self.check_var(value),
            Statement::For(value) => {
                if let Some(test) = value.test {
                    self.check_condition_assignments(test);
                }
                if let Some(ForInit::Variable(declaration)) = value.init {
                    self.check_var(declaration);
                }
            }
            Statement::ForIn(value) => {
                if let ForLeft::Variable(declaration) = value.left {
                    self.check_var(declaration);
                }
            }
            Statement::ForOf(value) => {
                if let ForLeft::Variable(declaration) = value.left {
                    self.check_var(declaration);
                }
            }
            _ => {}
        }
        match statement {
            Statement::Debugger(span) => self.report(
                "js/no-debugger",
                *span,
                "unexpected",
                "Unexpected debugger statement.",
            ),
            Statement::Block(block) => self.check_block(block),
            Statement::If(v) => self.check_condition(v.test),
            Statement::While(v)
                if self.boolean_option("js/no-constant-condition", "check_loops") =>
            {
                self.check_condition(v.test)
            }
            Statement::DoWhile(v)
                if self.boolean_option("js/no-constant-condition", "check_loops") =>
            {
                self.check_condition(v.test)
            }
            Statement::For(v) => {
                if self.boolean_option("js/no-constant-condition", "check_loops")
                    && let Some(test) = v.test
                {
                    self.check_condition(test);
                }
            }
            Statement::Try(v) => {
                self.check_block(v.block);
                if !self.boolean_option("js/no-empty", "allow_catch")
                    && let Some(handler) = v.handler
                {
                    self.check_block(handler.body);
                }
                if let Some(finalizer) = v.finalizer {
                    self.check_block(finalizer);
                }
            }
            Statement::Switch(v) => {
                let mut seen = Vec::<Literal>::new();
                for case in &v.cases {
                    if let Some(test) = case.test
                        && let Some(value) = self.literal(test)
                    {
                        if seen.contains(&value) {
                            self.report(
                                "js/no-duplicate-case",
                                test.span(),
                                "duplicate",
                                "Duplicate literal case label.",
                            );
                        }
                        seen.push(value);
                    }
                }
            }
            _ => {}
        }
        walk_statement(self, statement);
    }

    fn visit_expression(&mut self, expression: &Expression<'a>) {
        if self.levels["js/no-constant-binary-expression"] != RuleLevel::Off
            && let Some(message_id) = crate::constant_binary::check(*expression, self.interner)
        {
            self.report(
                "js/no-constant-binary-expression",
                expression.span(),
                message_id,
                match message_id {
                    "constant-short-circuit" => {
                        "Left operand always makes the same short-circuit decision."
                    }
                    "constant-nullish" => {
                        "Left operand has a constant nullishness; check the fallback expression."
                    }
                    _ => "Comparison has a constant result.",
                },
            );
        }
        if let Expression::Assignment(assignment) = expression
            && assignment.operator == AssignmentOperator::Assign
            && self.levels["js/no-self-assign"] != RuleLevel::Off
        {
            self.check_self_assignment(assignment.left, assignment.right);
        }
        if let Expression::Binary(binary) = expression {
            self.check_self_comparison(binary);
        }
        match expression {
            Expression::Binary(value) => self.check_typeof(value),
            Expression::Conditional(value) => self.check_condition_assignments(value.test),
            Expression::Array(value) if value.elements.iter().any(Option::is_none) => {
                self.report(
                    "js/no-sparse-arrays",
                    value.span,
                    "sparse",
                    "Unexpected empty slot in an array expression.",
                );
            }
            _ => {}
        }
        match expression {
            Expression::Binary(v)
                if matches!(v.operator, BinaryOperator::Eq | BinaryOperator::NotEq)
                    && !(self.boolean_option("js/eqeqeq", "allow_null")
                        && (matches!(v.left, Expression::NullLiteral(_))
                            || matches!(v.right, Expression::NullLiteral(_)))) =>
            {
                self.report(
                    "js/eqeqeq",
                    v.span,
                    "strict",
                    "Use a strict equality comparison.",
                )
            }
            Expression::Conditional(v) => self.check_condition(v.test),
            Expression::Object(v) => self.check_object(v),
            _ => {}
        }
        walk_expression(self, expression);
    }
}

struct ConditionAssignments(Vec<Span>);

pub(crate) fn is_comparison(operator: BinaryOperator) -> bool {
    matches!(
        operator,
        BinaryOperator::Eq
            | BinaryOperator::NotEq
            | BinaryOperator::StrictEq
            | BinaryOperator::StrictNotEq
            | BinaryOperator::Lt
            | BinaryOperator::Gt
            | BinaryOperator::LtEq
            | BinaryOperator::GtEq
    )
}

impl<'a> Visit<'a> for ConditionAssignments {
    fn visit_expression(&mut self, expression: &Expression<'a>) {
        if matches!(
            expression,
            Expression::Arrow(_) | Expression::Function(_) | Expression::Class(_)
        ) {
            return;
        }
        if let Expression::Assignment(value) = expression {
            self.0.push(value.span);
        }
        walk_expression(self, expression);
    }
    fn visit_function(&mut self, _: &Function<'a>) {}
    fn visit_class(&mut self, _: &Class<'a>) {}
}

#[derive(PartialEq)]
enum Literal {
    String(JsString),
    Number(f64),
    Bool(bool),
    Null,
}

fn number_key(value: f64) -> String {
    if value == 0.0 {
        return "0".into();
    }
    if value.is_infinite() {
        return if value.is_sign_positive() {
            "Infinity"
        } else {
            "-Infinity"
        }
        .into();
    }
    if value.abs() >= 1e21 || value.abs() < 1e-6 {
        let text = format!("{value:e}");
        let (mantissa, exponent) = text
            .split_once('e')
            .expect("scientific number has exponent");
        let exponent: i32 = exponent.parse().expect("formatted exponent is an integer");
        format!("{mantissa}e{exponent:+}")
    } else {
        value.to_string()
    }
}
