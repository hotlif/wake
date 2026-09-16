use std::collections::BTreeMap;
use wake_common::{FxHashMap, FxHashSet, Interner, Span};
use wake_ecma_ast::*;
use wake_ecma_parser::SourceParseOutput;
use wake_ecma_semantic::{DeclKind, ReferenceAccess, SourceSemanticModel};

use crate::source_helpers::pattern_names;
use crate::{EffectiveRule, LintDiagnostic, LintFix, RuleLevel, TextEdit};

struct Group {
    names: Vec<Ident>,
    initialized: bool,
}
struct Declaration {
    span: Span,
    groups: Vec<Group>,
    for_initializer: bool,
}

#[derive(Default)]
struct Candidates {
    declarations: Vec<Declaration>,
    late: Vec<(Span, Vec<Ident>)>,
    eval: Vec<Span>,
    with: bool,
    list: bool,
}

impl Candidates {
    fn declaration(
        &mut self,
        declaration: &VariableDeclaration<'_>,
        iteration: bool,
        for_initializer: bool,
    ) {
        if declaration.kind != VarKind::Let {
            return;
        }
        self.declarations.push(Declaration {
            span: declaration.span,
            for_initializer,
            groups: declaration
                .declarations
                .iter()
                .map(|declarator| {
                    let mut names = Vec::new();
                    pattern_names(declarator.id, &mut names);
                    Group {
                        names,
                        initialized: iteration || declarator.init.is_some(),
                    }
                })
                .collect(),
        });
    }
}

impl<'a> Visit<'a> for Candidates {
    fn visit_statement(&mut self, statement: &Statement<'a>) {
        let previous = self.list;
        match statement {
            Statement::VariableDeclaration(declaration) => {
                self.declaration(declaration, false, false)
            }
            Statement::For(value) => {
                if let Some(ForInit::Variable(declaration)) = value.init {
                    self.declaration(declaration, false, true);
                }
            }
            Statement::ForIn(value) => {
                if let ForLeft::Variable(declaration) = value.left {
                    self.declaration(declaration, true, false);
                }
            }
            Statement::ForOf(value) => {
                if let ForLeft::Variable(declaration) = value.left {
                    self.declaration(declaration, true, false);
                }
            }
            Statement::Expression(value) if self.list => {
                if let Expression::Assignment(assignment) = value.expression
                    && assignment.operator == AssignmentOperator::Assign
                {
                    let mut names = Vec::new();
                    if target_names(assignment.left, &mut names) && !names.is_empty() {
                        self.late.push((assignment.span, names));
                    }
                }
            }
            Statement::With(_) => self.with = true,
            _ => {}
        }
        self.list = matches!(
            statement,
            Statement::Block(_)
                | Statement::Try(_)
                | Statement::Switch(_)
                | Statement::ExportNamed(_)
        );
        walk_statement(self, statement);
        self.list = previous;
    }

    fn visit_expression(&mut self, expression: &Expression<'a>) {
        if let Expression::Call(call) = expression
            && !call.optional
            && let Expression::Identifier(id) = call.callee
        {
            self.eval.push(id.span);
        }
        let previous = self.list;
        if matches!(expression, Expression::Arrow(_)) {
            self.list = true;
        }
        walk_expression(self, expression);
        self.list = previous;
    }

    fn visit_function(&mut self, function: &Function<'a>) {
        let previous = self.list;
        self.list = true;
        walk_function(self, function);
        self.list = previous;
    }

    fn visit_class(&mut self, class: &Class<'a>) {
        let previous = self.list;
        self.list = true;
        walk_class(self, class);
        self.list = previous;
    }
}

fn target_names(expression: Expression<'_>, names: &mut Vec<Ident>) -> bool {
    match expression {
        Expression::Identifier(id) => {
            names.push(*id);
            true
        }
        Expression::Array(array) => array
            .elements
            .iter()
            .flatten()
            .all(|element| target_names(*element, names)),
        Expression::Object(object) => object.properties.iter().all(|member| match member {
            ObjectMember::Property(property) => {
                !property.method && target_names(property.value, names)
            }
            ObjectMember::Spread(spread) => target_names(spread.argument, names),
        }),
        Expression::Assignment(value) if value.operator == AssignmentOperator::Assign => {
            target_names(value.left, names)
        }
        Expression::Spread(value) => target_names(value.argument, names),
        _ => false,
    }
}

pub(crate) fn check(
    source: &str,
    program: &Program<'_>,
    interner: &Interner,
    parsed: &SourceParseOutput,
    semantic: &SourceSemanticModel,
    configuration: &BTreeMap<String, EffectiveRule>,
    diagnostics: &mut Vec<LintDiagnostic>,
) {
    const ID: &str = "js/prefer-const";
    let rule = &configuration[ID].configuration;
    if rule.level == RuleLevel::Off {
        return;
    }
    let mut candidates = Candidates {
        list: true,
        ..Default::default()
    };
    candidates.visit_program(program);
    let eval: FxHashSet<_> = candidates.eval.into_iter().collect();
    if crate::source_helpers::dynamic_access(semantic, interner, &eval, candidates.with) {
        return;
    }

    let original: FxHashSet<_> = parsed
        .identifiers
        .iter()
        .filter(|id| {
            matches!(
                id.role,
                SourceIdentifierRole::ValueBinding | SourceIdentifierRole::ValueReference
            )
        })
        .map(|id| (id.span, interner.intern(&id.name)))
        .collect();
    let declarations: FxHashMap<_, _> = semantic
        .model
        .binding_occurrences
        .iter()
        .filter(|binding| {
            binding.decl_kind == DeclKind::Let
                && original.contains(&(binding.span, binding.name))
                && (!semantic.incomplete_value_names.contains(&binding.name)
                    || semantic.source_symbols.contains(&binding.symbol))
        })
        .map(|binding| ((binding.span, binding.name), binding))
        .collect();
    let mut references: FxHashMap<_, Vec<_>> = FxHashMap::default();
    let mut by_span = FxHashMap::default();
    for &index in &semantic.references {
        let reference = &semantic.model.references[index];
        if let Some(symbol) = reference.resolved {
            references.entry(symbol).or_default().push(reference);
        }
        by_span.insert(reference.span, reference);
    }
    let uninitialized: FxHashSet<_> = candidates
        .declarations
        .iter()
        .flat_map(|declaration| &declaration.groups)
        .filter(|group| !group.initialized)
        .flat_map(|group| &group.names)
        .filter_map(|id| {
            declarations
                .get(&(id.span, id.name))
                .map(|binding| binding.symbol)
        })
        .collect();
    let mut late = FxHashMap::default();
    for (span, names) in &candidates.late {
        if rule.options["destructuring"] == "all"
            && !names.iter().all(|id| {
                let Some(write) = by_span.get(&id.span) else {
                    return false;
                };
                let Some(symbol) = write.resolved else {
                    return false;
                };
                let binding = &semantic.model.symbols[symbol as usize];
                let uses = references.get(&symbol).map_or(&[][..], Vec::as_slice);
                write.access == ReferenceAccess::Write
                    && write.scope == binding.scope
                    && write.span.lo > binding.span.lo
                    && uses
                        .iter()
                        .filter(|reference| reference.access.is_write())
                        .count()
                        == 1
                    && (rule.options["ignore_read_before_assign"] == false
                        || !uses.iter().any(|reference| {
                            reference.access.is_read() && reference.span.lo < span.hi
                        }))
            })
        {
            continue;
        }
        if names.iter().all(|id| {
            by_span
                .get(&id.span)
                .and_then(|reference| reference.resolved)
                .is_some_and(|symbol| uninitialized.contains(&symbol))
        }) {
            for id in names {
                late.insert(id.span, *span);
            }
        }
    }

    for declaration in candidates.declarations {
        let mut eligible = Vec::new();
        let mut all_safe = true;
        let mut fixable = true;
        for group in declaration.groups {
            let mut group_eligible = Vec::new();
            for id in &group.names {
                let Some(binding) = declarations.get(&(id.span, id.name)) else {
                    continue;
                };
                let uses = references
                    .get(&binding.symbol)
                    .map_or(&[][..], Vec::as_slice);
                let mut writes = uses.iter().filter(|reference| reference.access.is_write());
                let safe = if group.initialized {
                    writes.next().is_none()
                } else if let Some(write) = writes.next() {
                    writes.next().is_none()
                        && write.access == ReferenceAccess::Write
                        && write.scope == binding.scope
                        && write.span.lo > binding.span.lo
                        && late.get(&write.span).is_some_and(|assignment| {
                            rule.options["ignore_read_before_assign"] == false
                                || !uses.iter().any(|reference| {
                                    reference.access.is_read() && reference.span.lo < assignment.hi
                                })
                        })
                } else {
                    false
                };
                if safe {
                    group_eligible.push(*id);
                }
            }
            let complete = !group.names.is_empty() && group_eligible.len() == group.names.len();
            all_safe &= complete;
            fixable &= group.initialized;
            if rule.options["destructuring"] != "all" || complete {
                eligible.extend(group_eligible);
            }
        }
        if declaration.for_initializer && !all_safe {
            continue;
        }
        let fix = (all_safe
            && fixable
            && source.get(declaration.span.lo as usize..declaration.span.lo as usize + 3)
                == Some("let"))
        .then(|| LintFix {
            edits: vec![TextEdit {
                start: declaration.span.lo,
                end: declaration.span.lo + 3,
                text: "const".into(),
            }],
        });
        for (index, id) in eligible.into_iter().enumerate() {
            diagnostics.push(LintDiagnostic {
                rule_id: ID.into(),
                level: rule.level,
                message_id: "prefer".into(),
                message: format!(
                    "'{}' is never reassigned; use const.",
                    interner.resolve(id.name)
                ),
                start: id.span.lo,
                end: id.span.hi,
                fix: if index == 0 { fix.clone() } else { None },
            });
        }
    }
}
