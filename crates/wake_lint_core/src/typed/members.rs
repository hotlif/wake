use super::{TypeKind, TypedSource};
use crate::{EffectiveRule, LintDiagnostic, RuleLevel};

pub(super) const RULE: &str = "ts/no-unsafe-member-access";

pub(super) fn check(
    input: &TypedSource,
    rule: &EffectiveRule,
    diagnostics: &mut Vec<LintDiagnostic>,
) {
    let level = rule.configuration.level;
    if level == RuleLevel::Off {
        return;
    }
    let allow_optional = rule.configuration.options["allow_optional"]
        .as_bool()
        .expect("validated boolean");
    for (site, facts) in input
        .input
        .members()
        .iter()
        .zip(input.members.as_ref().expect("validated member types"))
    {
        for (id, computed) in [(Some(facts.object), false), (facts.property, true)] {
            if !computed && site.optional && allow_optional {
                continue;
            }
            let Some(id) = id else {
                continue;
            };
            let kind = input.resolved[id.0].0;
            if !matches!(kind, TypeKind::Any | TypeKind::Error) {
                continue;
            }
            let message_id = match (computed, kind == TypeKind::Error) {
                (false, false) => "unsafeMember",
                (false, true) => "errorMember",
                (true, false) => "unsafeKey",
                (true, true) => "errorKey",
            };
            let position = if computed {
                "computed member key"
            } else {
                "member receiver"
            };
            let kind = if kind == TypeKind::Error {
                "an unresolved type"
            } else {
                "any"
            };
            diagnostics.push(LintDiagnostic {
                rule_id: RULE.into(),
                level,
                message_id: message_id.into(),
                message: format!("Unsafe {position} of {kind}."),
                start: site.property.lo,
                end: site.property.hi,
                fix: None,
            });
        }
    }
}
