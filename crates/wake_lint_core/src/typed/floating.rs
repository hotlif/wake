use super::TypedSource;
use crate::{EffectiveRule, LintDiagnostic, RuleLevel};

pub(super) const RULE: &str = "ts/no-floating-promises";

pub(super) fn check(
    input: &TypedSource,
    rule: &EffectiveRule,
    diagnostics: &mut Vec<LintDiagnostic>,
) {
    let level = rule.configuration.level;
    if level == RuleLevel::Off {
        return;
    }
    let Some(expressions) = input.expression_statements.as_ref() else {
        return;
    };
    for (statement, type_id) in input.input.expression_statements().iter().zip(expressions) {
        if input.resolved_promise[type_id.0] {
            diagnostics.push(LintDiagnostic {
                rule_id: RULE.into(),
                level,
                message_id: "floating".into(),
                message: "Promise returned from an expression is not handled.".into(),
                start: statement.expression.lo,
                end: statement.expression.hi,
                fix: None,
            });
        }
    }
}
