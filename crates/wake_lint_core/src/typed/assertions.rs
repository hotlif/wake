use super::{TypeKind, TypedSource};
use crate::{EffectiveRule, LintDiagnostic, RuleLevel, SourceAssertionKind, TypeId};
use std::collections::HashSet;

pub(super) const RULE: &str = "ts/no-unnecessary-type-assertion";

/// A cycle assumption is valid only on the active path. Failed candidates must be removed before
/// another union member is tried; they are not memoized positive proofs.
struct Equivalence<'a> {
    input: &'a TypedSource,
    active: HashSet<(usize, usize)>,
    remaining: usize,
}

impl Equivalence<'_> {
    fn equivalent(&mut self, left: TypeId, right: TypeId) -> bool {
        if self.remaining == 0 {
            return false;
        }
        self.remaining -= 1;
        if left == right {
            return true;
        }
        if self.active.contains(&(left.0, right.0)) {
            return true;
        }
        if self.active.len() >= 128 {
            return false;
        }
        self.active.insert((left.0, right.0));
        let result = self.nodes(left, right);
        self.active.remove(&(left.0, right.0));
        result
    }

    fn ordered(&mut self, left: &[TypeId], right: &[TypeId]) -> bool {
        left.len() == right.len()
            && left
                .iter()
                .zip(right)
                .all(|(left, right)| self.equivalent(*left, *right))
    }

    fn unordered(&mut self, left: &[TypeId], right: &[TypeId]) -> bool {
        if left.len() != right.len() {
            return false;
        }
        let mut used = vec![false; right.len()];
        for left in left {
            let mut matched = false;
            for (index, right) in right.iter().enumerate() {
                if self.remaining == 0 {
                    return false;
                }
                if !used[index] && self.equivalent(*left, *right) {
                    used[index] = true;
                    matched = true;
                    break;
                }
            }
            if !matched {
                return false;
            }
        }
        true
    }

    fn shape(&mut self, left: &super::TypeNode, right: &super::TypeNode) -> bool {
        if !left.structural_complete
            || !right.structural_complete
            || left.readonly_properties != right.readonly_properties
            || left.structural_properties.len() != right.structural_properties.len()
            || left.index_signatures.len() != right.index_signatures.len()
        {
            return false;
        }
        // Type validation rejects duplicate names. Sort references so property matching is bounded
        // by O(n log n), independently of backend enumeration order.
        let mut a: Vec<_> = left.structural_properties.iter().collect();
        let mut b: Vec<_> = right.structural_properties.iter().collect();
        a.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        b.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        if !a
            .iter()
            .zip(&b)
            .all(|(a, b)| a.0 == b.0 && a.1 == b.1 && self.equivalent(a.2, b.2))
        {
            return false;
        }
        let mut used = vec![false; right.index_signatures.len()];
        for (key, value, readonly) in &left.index_signatures {
            let mut matched = false;
            for (index, (other_key, other_value, other_readonly)) in
                right.index_signatures.iter().enumerate()
            {
                if self.remaining == 0 {
                    return false;
                }
                // Charge each candidate, including those rejected before a recursive comparison.
                self.remaining -= 1;
                if !used[index]
                    && readonly == other_readonly
                    && self.equivalent(*key, *other_key)
                    && self.equivalent(*value, *other_value)
                {
                    used[index] = true;
                    matched = true;
                    break;
                }
            }
            if !matched {
                return false;
            }
        }
        true
    }

    fn nodes(&mut self, left: TypeId, right: TypeId) -> bool {
        let a = &self.input.nodes[left.0];
        let b = &self.input.nodes[right.0];
        if a.kind != b.kind
            || a.literal != b.literal
            || a.enum_identity != b.enum_identity
            || a.unique_symbol_identity != b.unique_symbol_identity
            || a.class_identity != b.class_identity
            || a.standard_function != b.standard_function
            || a.standard_promise != b.standard_promise
            || a.standard_thenable != b.standard_thenable
            || a.standard_regexp != b.standard_regexp
            || matches!(a.kind, TypeKind::Parameter | TypeKind::Other)
        {
            return false;
        }
        if a.kind == TypeKind::Object {
            if !self.ordered(&a.type_arguments, &b.type_arguments) {
                return false;
            }
            // Same target means the same source declaration, not structurally similar targets.
            if a.reference_target.is_some() || b.reference_target.is_some() {
                return a.reference_target == b.reference_target;
            }
            if a.standard_function || a.standard_promise || a.standard_thenable || a.standard_regexp
            {
                return true;
            }
            // Properties already include inherited members. Nominal classes and incompletely
            // modeled callable objects cannot advertise a complete structural shape.
            return self.shape(a, b);
        }
        self.unordered(&a.parts, &b.parts)
    }
}

fn equivalent(input: &TypedSource, left: TypeId, right: TypeId) -> bool {
    Equivalence {
        input,
        active: HashSet::new(),
        remaining: TypedSource::MAX_RELATIONS,
    }
    .equivalent(left, right)
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
    let facts = input
        .assertions
        .as_ref()
        .expect("validated type assertion facts");
    for (site, (operand, asserted)) in input.input.assertions().iter().zip(facts) {
        if matches!(site.kind, SourceAssertionKind::NonNull) || site.is_const {
            continue;
        }
        let left = input.resolved[operand.0].0;
        if !equivalent(input, *operand, *asserted)
            || input.resolved_uncertain[operand.0]
            || input.resolved_uncertain[asserted.0]
            || matches!(left, TypeKind::Any | TypeKind::Unknown | TypeKind::Error)
        {
            continue;
        }
        diagnostics.push(LintDiagnostic {
            rule_id: RULE.into(),
            level,
            message_id: "unnecessary".into(),
            message: "This type assertion does not change the expression type.".into(),
            start: site.span.lo,
            end: site.span.hi,
            fix: None,
        });
    }
}
