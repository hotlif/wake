use crate::{EffectiveRule, LintDiagnostic, RuleLevel};
use std::collections::{BTreeMap, BTreeSet};
use wake_ecma_ast::SourceImport;

pub(crate) fn check(
    imports: &[SourceImport],
    configuration: &BTreeMap<String, EffectiveRule>,
    diagnostics: &mut Vec<LintDiagnostic>,
) {
    let id = "js/no-duplicate-imports";
    let rule = &configuration[id].configuration;
    if rule.level == RuleLevel::Off {
        return;
    }
    let separate_types = rule.options["allow_separate_type_imports"]
        .as_bool()
        .expect("validated option");
    let mut seen = BTreeSet::new();
    for import in imports {
        if import.equals_target.is_some() {
            continue;
        }
        let Some(source) = &import.source else {
            continue;
        };
        let mut attributes = import
            .attributes
            .as_ref()
            .map(|attributes| {
                attributes
                    .entries
                    .iter()
                    .map(|entry| (&entry.key, &entry.value))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        attributes.sort();
        let type_only = import.type_only
            || (!import.bindings.is_empty()
                && import.bindings.iter().all(|binding| binding.type_only));
        if !seen.insert((
            import.parent,
            &source.value,
            attributes,
            separate_types && type_only,
        )) {
            diagnostics.push(LintDiagnostic {
                rule_id: id.into(),
                level: rule.level,
                message_id: "duplicate".into(),
                message: source.value.as_str().map_or_else(
                    || {
                        "The same module is imported more than once with the same attributes."
                            .into()
                    },
                    |value| {
                        format!(
                            "Module '{value}' is imported more than once with the same attributes."
                        )
                    },
                ),
                start: source.span.lo,
                end: source.span.hi,
                fix: None,
            });
        }
    }
}
