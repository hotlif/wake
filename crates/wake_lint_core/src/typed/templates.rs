use super::{TypeKind, TypedSource};
use crate::{EffectiveRule, LintDiagnostic, RuleLevel};

pub(super) const RULE: &str = "ts/restrict-template-expressions";

pub(super) fn check(
    input: &TypedSource,
    rule: &EffectiveRule,
    diagnostics: &mut Vec<LintDiagnostic>,
) {
    let level = rule.configuration.level;
    if level == RuleLevel::Off {
        return;
    }
    let allow = |name: &str| {
        rule.configuration.options[name]
            .as_bool()
            .expect("validated boolean")
    };
    let mut regexp = vec![false; input.nodes.len()];
    let mut accepted = vec![false; input.nodes.len()];
    for &index in &input.order {
        let node = &input.nodes[index];
        if let Some(constraint) = node.constraint {
            regexp[index] = regexp[constraint.0];
            accepted[index] = accepted[constraint.0];
            continue;
        }
        regexp[index] = node.standard_regexp
            || node.reference_target.is_some_and(|id| regexp[id.0])
            || match node.kind {
                TypeKind::Union => node.parts.iter().all(|id| regexp[id.0]),
                TypeKind::Intersection => node.parts.iter().any(|id| regexp[id.0]),
                _ => node.bases.iter().any(|id| regexp[id.0]),
            };
        accepted[index] = match node.kind {
            TypeKind::String => true,
            TypeKind::Number | TypeKind::BigInt => allow("allow_number"),
            TypeKind::Boolean => allow("allow_boolean"),
            TypeKind::Null | TypeKind::Undefined => allow("allow_nullish"),
            TypeKind::Any | TypeKind::Error => allow("allow_any"),
            TypeKind::Never => allow("allow_never"),
            TypeKind::Object => allow("allow_regexp") && regexp[index],
            TypeKind::Union => node.parts.iter().all(|id| accepted[id.0]),
            TypeKind::Intersection => node.parts.iter().any(|id| accepted[id.0]),
            _ => false,
        };
    }
    let facts = input
        .templates
        .as_ref()
        .expect("validated interpolation facts");
    for (site, facts) in input.input.templates().iter().zip(facts) {
        let Some(facts) = facts else {
            continue;
        };
        for (span, id) in site.expressions.iter().zip(facts) {
            if accepted[id.0] {
                continue;
            }
            diagnostics.push(LintDiagnostic {rule_id:RULE.into(), level, message_id:"invalid".into(),
                message:"Template expression has a type whose implicit string conversion is not allowed.".into(),
                start:span.lo, end:span.hi, fix:None});
        }
    }
}
