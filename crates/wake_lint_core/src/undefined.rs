use std::collections::BTreeMap;
use wake_common::{FxHashSet, Interner, Span};
use wake_ecma_ast::*;
use wake_ecma_semantic::SourceSemanticModel;

use crate::{EffectiveGlobal, EffectiveRule, GlobalMode, LintDiagnostic, RuleLevel};

pub(crate) fn check(
    program: &Program<'_>,
    interner: &Interner,
    semantic: &SourceSemanticModel,
    syntax: &[SourceNode],
    configuration: &BTreeMap<String, EffectiveRule>,
    globals: &BTreeMap<String, EffectiveGlobal>,
    diagnostics: &mut Vec<LintDiagnostic>,
) {
    let js = &configuration["js/no-undef"].configuration;
    let jsx = &configuration["react/jsx-no-undef"].configuration;
    if js.level == RuleLevel::Off && jsx.level == RuleLevel::Off {
        return;
    }
    let globals: FxHashSet<_> = globals
        .iter()
        .filter(|(_, value)| value.mode != GlobalMode::Off)
        .map(|(name, _)| interner.intern(name))
        .collect();
    // Only opening-element names count. Attributes, closing tags and generated helper calls
    // cannot supply a component reference. Semantic references already exclude intrinsic names.
    let tags = crate::source_helpers::jsx_tags(syntax);
    let mut exempt = TypeofIdentifiers::default();
    if js.level != RuleLevel::Off && js.options["typeof"] == false {
        exempt.visit_program(program);
    }
    for &index in &semantic.references {
        let reference = &semantic.model.references[index];
        if reference.resolved.is_some()
            || semantic.incomplete_value_names.contains(&reference.name)
            || globals.contains(&reference.name)
        {
            continue;
        }
        let component = tags
            .get(&reference.span.lo)
            .is_some_and(|&end| reference.span.hi <= end);
        let (id, level) = if component {
            ("react/jsx-no-undef", jsx.level)
        } else {
            if exempt.0.contains(&reference.span) {
                continue;
            }
            ("js/no-undef", js.level)
        };
        if level != RuleLevel::Off {
            diagnostics.push(LintDiagnostic {
                rule_id: id.into(),
                level,
                message_id: "undefined".into(),
                message: format!("'{}' is not defined.", interner.resolve(reference.name)),
                start: reference.span.lo,
                end: reference.span.hi,
                fix: None,
            });
        }
    }
}

#[derive(Default)]
struct TypeofIdentifiers(FxHashSet<Span>);

impl<'a> Visit<'a> for TypeofIdentifiers {
    fn visit_expression(&mut self, expression: &Expression<'a>) {
        if let Expression::Unary(unary) = expression
            && unary.operator == UnaryOperator::Typeof
            && let Expression::Identifier(id) = unary.argument
        {
            self.0.insert(id.span);
        }
        walk_expression(self, expression);
    }
}
