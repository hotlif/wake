//! Structured completion analysis. Calls and exceptions are conservative; this is not a type
//! analysis or an external CFG ABI. Each function and static block owns an independent region.

use wake_common::{Atom, Span};
use wake_ecma_ast::*;

#[derive(Clone, Debug, Default)]
pub struct ControlFlowFacts {
    pub functions: Vec<FunctionFlow>,
    pub unreachable: Vec<Span>,
    pub fallthrough: Vec<Fallthrough>,
    pub finally_exits: Vec<Span>,
}

#[derive(Clone, Debug)]
pub struct FunctionFlow {
    pub body: Span,
    pub returns_value: bool,
    pub returns_void: bool,
    pub end_reachable: bool,
}

#[derive(Clone, Debug)]
pub struct Fallthrough {
    pub case: Span,
    /// Real comments entirely inside this gap may explain the intentional transition.
    pub comment_gap: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Normal,
    ReturnValue,
    ReturnVoid,
    Throw,
    Break(Option<Atom>),
    Continue(Option<Atom>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Completion {
    kind: Kind,
    span: Span,
}

type Completions = Vec<Completion>;

fn normal() -> Completions {
    vec![Completion {
        kind: Kind::Normal,
        span: Span::DUMMY,
    }]
}
fn has_normal(flow: &[Completion]) -> bool {
    flow.iter()
        .any(|completion| completion.kind == Kind::Normal)
}
fn merge(into: &mut Completions, other: impl IntoIterator<Item = Completion>) {
    for completion in other {
        if !into.contains(&completion) {
            into.push(completion);
        }
    }
}

/// Analyze every execution region once without transferring AST borrows to the result.
pub fn analyze_control_flow(program: &Program<'_>) -> ControlFlowFacts {
    let mut analyzer = Analyzer {
        facts: ControlFlowFacts::default(),
    };
    analyzer.visit_program(program);
    for spans in [
        &mut analyzer.facts.unreachable,
        &mut analyzer.facts.finally_exits,
    ] {
        spans.sort_by_key(|span| (span.lo, span.hi));
        spans.dedup();
    }
    analyzer.facts
}

struct Analyzer {
    facts: ControlFlowFacts,
}

impl Analyzer {
    fn sequence(&mut self, statements: &[Statement<'_>]) -> Completions {
        let mut result = normal();
        for statement in statements {
            if has_normal(&result) {
                result.retain(|completion| completion.kind != Kind::Normal);
                merge(&mut result, self.statement(statement, &[]));
            } else if !matches!(
                statement,
                Statement::Empty(_) | Statement::FunctionDeclaration(_)
            ) && !matches!(statement, Statement::VariableDeclaration(declaration) if declaration.kind == VarKind::Var && declaration.declarations.iter().all(|declaration| declaration.init.is_none()))
                && !statement.span().is_dummy()
            {
                self.facts.unreachable.push(statement.span());
            }
        }
        result
    }

    fn function(&mut self, body: &FunctionBody<'_>) {
        let flow = self.sequence(&body.statements);
        self.facts.functions.push(FunctionFlow {
            body: body.span,
            returns_value: flow
                .iter()
                .any(|completion| completion.kind == Kind::ReturnValue),
            returns_void: flow
                .iter()
                .any(|completion| completion.kind == Kind::ReturnVoid),
            end_reachable: has_normal(&flow),
        });
    }

    fn loop_flow(
        &mut self,
        body: &Statement<'_>,
        test: Option<bool>,
        at_least_once: bool,
        labels: &[Atom],
    ) -> Completions {
        let flow = self.statement(body, &[]);
        if !at_least_once && test == Some(false) {
            return normal();
        }
        let mut result = Vec::new();
        if !at_least_once && test != Some(true) {
            merge(&mut result, normal());
        }
        for completion in flow {
            match completion.kind {
                Kind::Break(None) => merge(&mut result, normal()),
                Kind::Continue(None) | Kind::Normal => {
                    if test != Some(true) {
                        merge(&mut result, normal());
                    }
                }
                Kind::Continue(Some(label)) if labels.contains(&label) => {
                    if test != Some(true) {
                        merge(&mut result, normal());
                    }
                }
                _ => merge(&mut result, [completion]),
            }
        }
        result
    }

    fn statement(&mut self, statement: &Statement<'_>, labels: &[Atom]) -> Completions {
        match statement {
            Statement::Block(block) => self.sequence(&block.body),
            Statement::Return(ret) => vec![Completion {
                kind: if ret.argument.is_none()
                    || matches!(ret.argument, Some(Expression::Unary(unary)) if unary.operator == UnaryOperator::Void)
                {
                    Kind::ReturnVoid
                } else {
                    Kind::ReturnValue
                },
                span: ret.span,
            }],
            Statement::Throw(throw) => vec![Completion {
                kind: Kind::Throw,
                span: throw.span,
            }],
            Statement::Break(jump) => vec![Completion {
                kind: Kind::Break(jump.label.map(|id| id.name)),
                span: jump.span,
            }],
            Statement::Continue(jump) => vec![Completion {
                kind: Kind::Continue(jump.label.map(|id| id.name)),
                span: jump.span,
            }],
            Statement::If(branch) => {
                let mut flow = self.statement(&branch.consequent, &[]);
                merge(
                    &mut flow,
                    branch
                        .alternate
                        .as_ref()
                        .map_or_else(normal, |alternate| self.statement(alternate, &[])),
                );
                flow
            }
            Statement::For(loop_) => self.loop_flow(
                &loop_.body,
                loop_.test.as_ref().map_or(Some(true), truthiness),
                false,
                labels,
            ),
            Statement::While(loop_) => {
                self.loop_flow(&loop_.body, truthiness(&loop_.test), false, labels)
            }
            Statement::DoWhile(loop_) => {
                self.loop_flow(&loop_.body, truthiness(&loop_.test), true, labels)
            }
            Statement::ForIn(loop_) => self.loop_flow(&loop_.body, None, false, labels),
            Statement::ForOf(loop_) => self.loop_flow(&loop_.body, None, false, labels),
            Statement::Labeled(labeled) => {
                let mut nested_labels = labels.to_vec();
                nested_labels.push(labeled.label.name);
                let flow = self.statement(&labeled.body, &nested_labels);
                let mut result = Vec::new();
                for completion in flow {
                    if completion.kind == Kind::Break(Some(labeled.label.name)) {
                        merge(&mut result, normal());
                    } else {
                        merge(&mut result, [completion]);
                    }
                }
                result
            }
            Statement::Switch(switch) => {
                let mut branches = Vec::new();
                for (index, case) in switch.cases.iter().enumerate() {
                    let flow = self.sequence(&case.consequent);
                    if has_normal(&flow)
                        && let Some(last) = case.consequent.last()
                        && let Some(next) = switch.cases.get(index + 1)
                    {
                        self.facts.fallthrough.push(Fallthrough {
                            case: case.span,
                            comment_gap: Span::new(last.span().hi, next.span.lo),
                        });
                    }
                    branches.push(flow);
                }
                let mut tail = normal();
                let mut result = if switch.cases.iter().any(|case| case.test.is_none()) {
                    Vec::new()
                } else {
                    normal()
                };
                for mut branch in branches.into_iter().rev() {
                    if has_normal(&branch) {
                        branch.retain(|completion| completion.kind != Kind::Normal);
                        merge(&mut branch, tail.iter().copied());
                    }
                    merge(&mut result, branch.iter().copied());
                    tail = branch;
                }
                let breaks = result
                    .iter()
                    .any(|completion| completion.kind == Kind::Break(None));
                result.retain(|completion| completion.kind != Kind::Break(None));
                if breaks {
                    merge(&mut result, normal());
                }
                result
            }
            Statement::Try(try_) => {
                let mut incoming = self.sequence(&try_.block.body);
                if let Some(catch) = try_.handler {
                    incoming.retain(|completion| completion.kind != Kind::Throw);
                    // Calls, property access, coercion and other expressions can throw even when
                    // there is no explicit ThrowStatement. Preserve the catch entry conservatively.
                    merge(&mut incoming, self.sequence(&catch.body.body));
                }
                if let Some(finally) = try_.finalizer {
                    let outgoing = self.sequence(&finally.body);
                    if incoming.is_empty() {
                        return incoming;
                    }
                    let mut result = Vec::new();
                    for completion in outgoing {
                        if completion.kind == Kind::Normal {
                            merge(&mut result, incoming.iter().copied());
                        } else {
                            if !completion.span.is_dummy() {
                                self.facts.finally_exits.push(completion.span);
                            }
                            merge(&mut result, [completion]);
                        }
                    }
                    result
                } else {
                    incoming
                }
            }
            Statement::With(with) => self.statement(&with.body, &[]),
            Statement::ExportNamed(export) => export
                .declaration
                .as_ref()
                .map_or_else(normal, |declaration| self.statement(declaration, &[])),
            _ => normal(),
        }
    }
}

pub(crate) fn truthiness(expression: &Expression<'_>) -> Option<bool> {
    match expression {
        Expression::BooleanLiteral(boolean) => Some(boolean.value),
        Expression::NumberLiteral(number) => Some(number.value != 0.0 && !number.value.is_nan()),
        Expression::NullLiteral(_) => Some(false),
        Expression::Unary(unary) if unary.operator == UnaryOperator::LogicalNot => {
            truthiness(&unary.argument).map(|value| !value)
        }
        _ => None,
    }
}

impl<'a> Visit<'a> for Analyzer {
    fn visit_program(&mut self, program: &Program<'a>) {
        self.sequence(&program.body);
        walk_program(self, program);
    }
    fn visit_function(&mut self, function: &Function<'a>) {
        if let Some(body) = function.body {
            self.function(body);
        }
        walk_function(self, function);
    }
    fn visit_expression(&mut self, expression: &Expression<'a>) {
        if let Expression::Arrow(arrow) = expression
            && let ArrowBody::Block(body) = arrow.body
        {
            self.function(body);
        }
        walk_expression(self, expression);
    }
    fn visit_class(&mut self, class: &Class<'a>) {
        for member in &class.body {
            if let ClassMember::StaticBlock(block) = member {
                self.sequence(&block.body);
            }
        }
        walk_class(self, class);
    }
}
