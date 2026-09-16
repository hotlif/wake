use super::{TypeKind, TypedSource};
use crate::{EffectiveRule, LintDiagnostic, RuleLevel};

pub(super) const RULE: &str = "ts/no-unsafe-return";

pub(super) fn check(
    input: &TypedSource,
    rule: &EffectiveRule,
    diagnostics: &mut Vec<LintDiagnostic>,
) {
    let level = rule.configuration.level;
    if level == RuleLevel::Off {
        return;
    }
    for (site, id) in input
        .input
        .returns()
        .iter()
        .zip(input.returns.as_ref().expect("validated return facts"))
    {
        let kind = input.resolved_unsafe[id.0].unwrap_or(input.resolved[id.0].0);
        let message_id = match kind {
            TypeKind::Error => "errorReturn",
            TypeKind::Any => "unsafeReturn",
            _ => continue,
        };
        let description = if kind == TypeKind::Error {
            "an unresolved type"
        } else {
            "any"
        };
        diagnostics.push(LintDiagnostic {
            rule_id: RULE.into(),
            level,
            message_id: message_id.into(),
            message: format!("Unsafe return of {description}."),
            start: site.argument.lo,
            end: site.argument.hi,
            fix: None,
        });
    }
}
