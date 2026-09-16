use std::collections::BTreeMap;

use wake_common::{FxHashSet, Interner, Span};
use wake_ecma_ast::*;
use wake_ecma_semantic::SourceSemanticModel;

use crate::{LintDiagnostic, RuleLevel};

pub(crate) fn check(
    program: &Program<'_>,
    interner: &Interner,
    semantic: &SourceSemanticModel,
    levels: &BTreeMap<String, RuleLevel>,
    diagnostics: &mut Vec<LintDiagnostic>,
) {
    let globals = semantic
        .references
        .iter()
        .filter_map(|&index| {
            let reference = &semantic.model.references[index];
            (reference.resolved.is_none()
                && !semantic.incomplete_value_names.contains(&reference.name))
            .then_some(reference.span)
        })
        .collect();
    ScopeVisitor {
        interner,
        globals,
        levels,
        diagnostics,
    }
    .visit_program(program);
}

struct ScopeVisitor<'a> {
    interner: &'a Interner,
    globals: FxHashSet<Span>,
    levels: &'a BTreeMap<String, RuleLevel>,
    diagnostics: &'a mut Vec<LintDiagnostic>,
}

impl ScopeVisitor<'_> {
    fn is_nan(&self, expression: &Expression<'_>) -> bool {
        if self.is_global(expression, "NaN") {
            return true;
        }
        let Expression::Member(member) = expression else {
            return false;
        };
        self.is_global(&member.object, "Number")
            && match member.property {
                MemberProperty::Ident(id) => self.interner.resolve(id.name) == "NaN",
                MemberProperty::Computed(Expression::StringLiteral(string)) => {
                    self.interner.resolve_js(string.value) == "NaN"
                }
                _ => false,
            }
    }

    fn is_global(&self, expression: &Expression<'_>, name: &str) -> bool {
        matches!(expression, Expression::Identifier(id) if self.globals.contains(&id.span) && self.interner.resolve(id.name) == name)
    }

    fn report(&mut self, id: &str, span: Span, message: &str) {
        if self.levels[id] != RuleLevel::Off {
            self.diagnostics.push(LintDiagnostic {
                rule_id: id.into(),
                level: self.levels[id],
                message_id: "unexpected".into(),
                message: message.into(),
                start: span.lo,
                end: span.hi,
                fix: None,
            });
        }
    }

    fn executor(&mut self, executor: &Expression<'_>) {
        let mut returns = ExecutorReturns::default();
        let is_async = match executor {
            Expression::Function(function) => {
                if let Some(body) = function.body {
                    for statement in &body.statements {
                        returns.visit_statement(statement);
                    }
                }
                function.is_async
            }
            Expression::Arrow(arrow) => {
                match arrow.body {
                    ArrowBody::Block(body) => {
                        for statement in &body.statements {
                            returns.visit_statement(statement);
                        }
                    }
                    ArrowBody::Expression(expression) => {
                        if !is_void(&expression) {
                            returns.spans.push(expression.span());
                        }
                    }
                }
                arrow.is_async
            }
            _ => return,
        };
        if is_async {
            self.report(
                "js/no-async-promise-executor",
                executor.span(),
                "Promise executors must not be async.",
            );
        }
        for span in returns.spans {
            self.report(
                "js/no-promise-executor-return",
                span,
                "Promise executors must not return a value.",
            );
        }
    }
}

impl<'a> Visit<'a> for ScopeVisitor<'_> {
    fn visit_statement(&mut self, statement: &Statement<'a>) {
        if let Statement::Switch(switch) = statement {
            if self.is_nan(&switch.discriminant) {
                self.report(
                    "js/use-isnan",
                    switch.discriminant.span(),
                    "NaN cannot match a switch case.",
                );
            }
            for case in &switch.cases {
                if let Some(test) = case.test
                    && self.is_nan(&test)
                {
                    self.report(
                        "js/use-isnan",
                        test.span(),
                        "A NaN case cannot match the switch value.",
                    );
                }
            }
        }
        walk_statement(self, statement);
    }

    fn visit_expression(&mut self, expression: &Expression<'a>) {
        match expression {
            Expression::Binary(binary)
                if crate::rules::is_comparison(binary.operator)
                    && (self.is_nan(&binary.left) || self.is_nan(&binary.right)) =>
            {
                self.report(
                    "js/use-isnan",
                    binary.span,
                    "Use Number.isNaN to test for NaN.",
                );
            }
            Expression::Member(member) if self.is_global(&member.object, "console") => {
                self.report(
                    "js/no-console",
                    member.span,
                    "Unexpected global console access.",
                );
            }
            Expression::New(new) if self.is_global(&new.callee, "Promise") => {
                if let Some(executor) = new.arguments.first() {
                    self.executor(executor);
                }
            }
            _ => {}
        }
        walk_expression(self, expression);
    }
}

fn is_void(expression: &Expression<'_>) -> bool {
    matches!(expression, Expression::Unary(unary) if unary.operator == UnaryOperator::Void)
}

#[derive(Default)]
struct ExecutorReturns {
    spans: Vec<Span>,
}

impl<'a> Visit<'a> for ExecutorReturns {
    fn visit_statement(&mut self, statement: &Statement<'a>) {
        if let Statement::Return(ret) = statement
            && let Some(argument) = &ret.argument
            && !is_void(argument)
        {
            self.spans.push(ret.span);
        }
        walk_statement(self, statement);
    }

    // Expressions cannot contain returns in this execution scope; nested functions and classes
    // own their returns, including those in initializers, computed names and methods.
    fn visit_expression(&mut self, _: &Expression<'a>) {}
    fn visit_function(&mut self, _: &Function<'a>) {}
    fn visit_class(&mut self, _: &Class<'a>) {}
}
