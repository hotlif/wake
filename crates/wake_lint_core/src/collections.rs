//! JSX list checks combine original parser facts with native expression and binding identities.

use crate::{LintDiagnostic, RuleLevel};
use std::collections::{BTreeMap, BTreeSet};
use wake_common::{Atom, Interner, Span};
use wake_ecma_ast::*;
use wake_ecma_parser::SourceParseOutput;
use wake_ecma_semantic::{DeclKind, SourceSemanticModel, SymbolId};

type Range = (u32, u32);
fn range(span: Span) -> Range {
    (span.lo, span.hi)
}

struct Keys {
    sites: BTreeMap<Range, (Span, bool)>,
    expressions: BTreeSet<Range>,
}

impl Keys {
    fn new(source: &str, parsed: &SourceParseOutput) -> Self {
        let nodes = &parsed.syntax;
        let mut children = vec![Vec::new(); nodes.len()];
        let mut values = vec![None; nodes.len()];
        for (index, node) in nodes.iter().enumerate() {
            if let Some(parent) = node.parent {
                children[parent].push(index);
            }
        }
        for value in &parsed.jsx_values {
            values[value.node] = Some(value);
        }
        let mut keys = Self {
            sites: BTreeMap::new(),
            expressions: BTreeSet::new(),
        };
        for (index, node) in nodes.iter().enumerate() {
            if node.kind == SourceNodeKind::JsxFragment {
                keys.sites.insert(range(node.span), (node.span, true));
                continue;
            }
            if node.kind != SourceNodeKind::JsxElement {
                continue;
            }
            let Some(&opening) = children[index]
                .iter()
                .find(|&&child| nodes[child].kind == SourceNodeKind::JsxOpeningElement)
            else {
                continue;
            };
            let mut missing = true;
            let mut expression = None;
            for &attribute in &children[opening] {
                if nodes[attribute].kind == SourceNodeKind::JsxSpreadAttribute {
                    missing = false;
                    expression = None;
                } else if nodes[attribute].kind == SourceNodeKind::JsxAttribute
                    && children[attribute].iter().any(|&child| {
                        nodes[child].kind == SourceNodeKind::JsxName
                            && &source[nodes[child].span.lo as usize..nodes[child].span.hi as usize]
                                == "key"
                    })
                {
                    missing = values[attribute]
                        .is_some_and(|value| value.value == SourcePrimitiveValue::Undefined);
                    expression = values[attribute].and_then(|value| value.expression);
                }
            }
            keys.sites
                .insert(range(node.span), (nodes[opening].span, missing));
            if let Some(expression) = expression {
                keys.expressions.insert(range(expression));
            }
        }
        keys
    }
}

fn callback<'a>(expression: &Expression<'a>, interner: &Interner) -> Option<Expression<'a>> {
    let Expression::Call(call) = expression else {
        return None;
    };
    let Expression::Member(member) = call.callee else {
        return None;
    };
    let method = match member.property {
        MemberProperty::Ident(id) => interner.resolve(id.name),
        MemberProperty::Computed(Expression::StringLiteral(literal)) => {
            interner.resolve_js(literal.value).as_str()?.to_owned()
        }
        _ => return None,
    };
    if !matches!(method.as_str(), "map" | "flatMap") {
        return None;
    }
    match call.arguments.first()? {
        expression @ (Expression::Arrow(_) | Expression::Function(_)) => Some(*expression),
        _ => None,
    }
}

fn rendered(expression: &Expression<'_>, keys: &Keys, output: &mut BTreeSet<Range>) {
    let span = range(expression.span());
    if keys.sites.contains_key(&span) {
        output.insert(span);
        return;
    }
    match expression {
        Expression::Conditional(conditional) => {
            rendered(&conditional.consequent, keys, output);
            rendered(&conditional.alternate, keys, output);
        }
        Expression::Logical(logical) => {
            if logical.operator != LogicalOperator::And {
                rendered(&logical.left, keys, output);
            }
            rendered(&logical.right, keys, output);
        }
        Expression::Sequence(sequence) => {
            if let Some(last) = sequence.expressions.last() {
                rendered(last, keys, output);
            }
        }
        Expression::Assignment(assignment) => rendered(&assignment.right, keys, output),
        _ => {}
    }
}

struct Returns<'s> {
    keys: &'s Keys,
    output: &'s mut BTreeSet<Range>,
}
impl<'a> Visit<'a> for Returns<'_> {
    fn visit_statement(&mut self, statement: &Statement<'a>) {
        match statement {
            Statement::FunctionDeclaration(_) | Statement::ClassDeclaration(_) => {}
            Statement::Return(statement) => {
                if let Some(argument) = statement.argument {
                    rendered(&argument, self.keys, self.output);
                }
            }
            _ => walk_statement(self, statement),
        }
    }
    fn visit_expression(&mut self, _expression: &Expression<'a>) {}
}

fn callback_results(expression: Expression<'_>, keys: &Keys, output: &mut BTreeSet<Range>) {
    let body = match expression {
        Expression::Arrow(arrow) => match arrow.body {
            ArrowBody::Expression(expression) => {
                rendered(&expression, keys, output);
                return;
            }
            ArrowBody::Block(body) => Some(body),
        },
        Expression::Function(function) => function.body,
        _ => None,
    };
    if let Some(body) = body {
        let mut returns = Returns { keys, output };
        for statement in &body.statements {
            returns.visit_statement(statement);
        }
    }
}

struct Collections<'s> {
    interner: &'s Interner,
    keys: &'s Keys,
    elements: BTreeSet<Range>,
    output: BTreeSet<Range>,
}
impl<'a> Visit<'a> for Collections<'_> {
    fn visit_expression(&mut self, expression: &Expression<'a>) {
        if self.elements.contains(&range(expression.span())) {
            rendered(expression, self.keys, &mut self.output);
        }
        if let Some(callback) = callback(expression, self.interner) {
            callback_results(callback, self.keys, &mut self.output);
        }
        walk_expression(self, expression);
    }
}

pub(crate) fn check_keys(
    source: &str,
    program: &Program<'_>,
    interner: &Interner,
    parsed: &SourceParseOutput,
    levels: &BTreeMap<String, RuleLevel>,
    diagnostics: &mut Vec<LintDiagnostic>,
) {
    let level = levels["react/jsx-key"];
    if level == RuleLevel::Off {
        return;
    }
    let keys = Keys::new(source, parsed);
    let elements = parsed
        .arrays
        .iter()
        .flat_map(|array| array.elements.iter().flatten())
        .filter(|element| !element.spread)
        .map(|element| range(element.expression))
        .collect();
    let mut visitor = Collections {
        interner,
        keys: &keys,
        elements,
        output: BTreeSet::new(),
    };
    visitor.visit_program(program);
    for key in visitor.output {
        let (span, missing) = keys.sites[&key];
        if missing {
            diagnostics.push(LintDiagnostic {
                rule_id: "react/jsx-key".into(),
                level,
                message_id: "key".into(),
                message:
                    "Provide a key on this JSX list item; use an explicit Fragment when needed."
                        .into(),
                start: span.lo,
                end: span.hi,
                fix: None,
            });
        }
    }
}

struct IndexBindings<'s> {
    interner: &'s Interner,
    parameters: &'s BTreeMap<Range, Vec<(Atom, SymbolId)>>,
    symbols: BTreeSet<SymbolId>,
}
impl<'a> Visit<'a> for IndexBindings<'_> {
    fn visit_expression(&mut self, expression: &Expression<'a>) {
        let parameter = match callback(expression, self.interner) {
            Some(Expression::Arrow(arrow)) => arrow.params.get(1),
            Some(Expression::Function(function)) => function.params.get(1),
            _ => None,
        };
        let parameter = match parameter {
            Some(Pattern::Assignment(assignment)) => Some(&assignment.left),
            parameter => parameter,
        };
        if let Some(Pattern::Ident(parameter)) = parameter
            && let Some(bindings) = self.parameters.get(&range(parameter.span))
        {
            self.symbols.extend(
                bindings
                    .iter()
                    .filter(|(name, _)| *name == parameter.name)
                    .map(|(_, symbol)| *symbol),
            );
        }
        walk_expression(self, expression);
    }
}

struct Reads<'s> {
    reads: &'s BTreeSet<Range>,
    found: bool,
}
impl<'a> Visit<'a> for Reads<'_> {
    fn visit_expression(&mut self, expression: &Expression<'a>) {
        if self.found
            || matches!(
                expression,
                Expression::Arrow(_) | Expression::Function(_) | Expression::Class(_)
            )
        {
            return;
        }
        if let Expression::Identifier(identifier) = expression
            && self.reads.contains(&range(identifier.span))
        {
            self.found = true;
        }
        walk_expression(self, expression);
    }
}
struct KeyReads<'s> {
    keys: &'s Keys,
    reads: BTreeSet<Range>,
    output: BTreeSet<Range>,
}
impl<'a> Visit<'a> for KeyReads<'_> {
    fn visit_expression(&mut self, expression: &Expression<'a>) {
        let span = range(expression.span());
        if self.keys.expressions.contains(&span) {
            let mut reads = Reads {
                reads: &self.reads,
                found: false,
            };
            reads.visit_expression(expression);
            if reads.found {
                self.output.insert(span);
            }
        }
        walk_expression(self, expression);
    }
}

pub(crate) fn check_indexes(
    source: &str,
    program: &Program<'_>,
    interner: &Interner,
    parsed: &SourceParseOutput,
    semantic: &SourceSemanticModel,
    levels: &BTreeMap<String, RuleLevel>,
    diagnostics: &mut Vec<LintDiagnostic>,
) {
    let level = levels["react/no-array-index-key"];
    if level == RuleLevel::Off {
        return;
    }
    let mut parameters: BTreeMap<Range, Vec<(Atom, SymbolId)>> = BTreeMap::new();
    for binding in &semantic.model.binding_occurrences {
        if binding.decl_kind == DeclKind::Param {
            parameters
                .entry(range(binding.span))
                .or_default()
                .push((binding.name, binding.symbol));
        }
    }
    let mut bindings = IndexBindings {
        interner,
        parameters: &parameters,
        symbols: BTreeSet::new(),
    };
    bindings.visit_program(program);
    let keys = Keys::new(source, parsed);
    let reads = semantic
        .references
        .iter()
        .map(|&index| &semantic.model.references[index])
        .filter(|reference| {
            reference.access.is_read()
                && reference
                    .resolved
                    .is_some_and(|symbol| bindings.symbols.contains(&symbol))
        })
        .map(|reference| range(reference.span))
        .collect();
    let mut visitor = KeyReads {
        keys: &keys,
        reads,
        output: BTreeSet::new(),
    };
    visitor.visit_program(program);
    for (start, end) in visitor.output {
        diagnostics.push(LintDiagnostic {
            rule_id: "react/no-array-index-key".into(),
            level,
            message_id: "index".into(),
            message: "Use a stable item identity instead of the array callback index as a key."
                .into(),
            start,
            end,
            fix: None,
        });
    }
}
