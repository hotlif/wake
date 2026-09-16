use super::{TypeKind, TypeLiteral, TypedSource};
use crate::{EffectiveRule, LintDiagnostic, RuleLevel};
use wake_ecma_ast::SourcePrimitiveValue;

pub(super) const RULE: &str = "ts/switch-exhaustiveness-check";

fn matches_literal(case: &SourcePrimitiveValue, literal: &TypeLiteral) -> bool {
    match (case, literal) {
        (SourcePrimitiveValue::String(case), TypeLiteral::String(literal)) => case == literal,
        (SourcePrimitiveValue::Number(case), TypeLiteral::Number(literal)) => case == literal,
        (SourcePrimitiveValue::Boolean(case), TypeLiteral::Boolean(literal)) => case == literal,
        (SourcePrimitiveValue::BigInt(case), TypeLiteral::BigInt(literal)) => case == literal,
        (SourcePrimitiveValue::Null, TypeLiteral::Null)
        | (SourcePrimitiveValue::Undefined, TypeLiteral::Undefined) => true,
        _ => false,
    }
}

fn matches_type_literal(case: &TypeLiteral, literal: &TypeLiteral) -> bool {
    match (case, literal) {
        (TypeLiteral::String(case), TypeLiteral::String(literal)) => case == literal,
        (TypeLiteral::Number(case), TypeLiteral::Number(literal)) => case == literal,
        (TypeLiteral::Boolean(case), TypeLiteral::Boolean(literal)) => case == literal,
        (TypeLiteral::BigInt(case), TypeLiteral::BigInt(literal)) => case == literal,
        (TypeLiteral::Null, TypeLiteral::Null)
        | (TypeLiteral::Undefined, TypeLiteral::Undefined) => true,
        _ => false,
    }
}

pub(super) fn check(
    input: &TypedSource,
    rule: &EffectiveRule,
    diagnostics: &mut Vec<LintDiagnostic>,
) {
    let level = rule.configuration.level;
    if level == RuleLevel::Off {
        return;
    }
    for (index, (site, id)) in input
        .input
        .switches()
        .iter()
        .zip(
            input
                .switch_discriminants
                .as_ref()
                .expect("validated switch facts"),
        )
        .enumerate()
    {
        if site.has_default || input.resolved[id.0].0 != TypeKind::Union {
            continue;
        }
        let case_types = input
            .switch_case_types
            .as_ref()
            .and_then(|cases| cases.get(index));
        let exhaustive = input.nodes[id.0].parts.iter().all(|part| {
            input.nodes[part.0].literal.as_ref().is_some_and(|literal| {
                site.cases.iter().any(|case| matches_literal(case, literal))
                    || case_types.is_some_and(|cases| {
                        cases.iter().any(|case| {
                            input.nodes[case.0]
                                .literal
                                .as_ref()
                                .is_some_and(|case| matches_type_literal(case, literal))
                        })
                    })
            })
        });
        if exhaustive {
            continue;
        }
        diagnostics.push(LintDiagnostic {
            rule_id: RULE.into(),
            level,
            message_id: "missingDefault".into(),
            message: "Switch over a union type has no default or exhaustiveness proof.".into(),
            start: site.span.lo,
            end: site.span.hi,
            fix: None,
        });
    }
}
