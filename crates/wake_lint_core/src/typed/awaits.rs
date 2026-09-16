use super::{TypeKind, TypedSource};
use crate::{EffectiveRule, LintDiagnostic, RuleLevel};

pub(super) const RULE: &str = "ts/await-thenable";

pub(super) fn check(
    input: &TypedSource,
    rule: &EffectiveRule,
    diagnostics: &mut Vec<LintDiagnostic>,
) {
    let level = rule.configuration.level;
    if level == RuleLevel::Off {
        return;
    }
    let facts = input
        .awaits
        .as_ref()
        .expect("validated await operand types");
    for (site, id) in input.input.awaits().iter().zip(facts) {
        let node = &input.nodes[id.0];
        let thenable = input.resolved_thenable[id.0];
        if thenable
            || matches!(
                node.kind,
                TypeKind::Any | TypeKind::Error | TypeKind::Unknown
            )
        {
            continue;
        }
        diagnostics.push(LintDiagnostic {
            rule_id: RULE.into(),
            level,
            message_id: "notThenable".into(),
            message: "Awaited value is not a thenable type.".into(),
            start: site.argument.lo,
            end: site.argument.hi,
            fix: None,
        });
    }
}
