use super::{CallArgumentType, TypeKind, TypedSource, void_return_checks};
use crate::{EffectiveRule, LintDiagnostic, RuleLevel, SourceCallbackKind};

pub(super) const RULE: &str = "ts/no-misused-promises";

fn is_promise_callback(input: &TypedSource, argument: &CallArgumentType) -> bool {
    let Some(actual) = argument.actual.as_ref() else {
        return false;
    };
    let Some(contextual) = argument.contextual.as_ref() else {
        return false;
    };
    let actual_kind = input.resolved[actual.type_id.0].0;
    let contextual_kind = input.resolved[contextual.type_id.0].0;
    if matches!(
        actual_kind,
        TypeKind::Any | TypeKind::Unknown | TypeKind::Error
    ) || matches!(
        contextual_kind,
        TypeKind::Any | TypeKind::Unknown | TypeKind::Error
    ) {
        return false;
    }
    actual
        .return_types
        .iter()
        .any(|id| input.resolved_promise[id.0])
        && !contextual.returns.is_empty()
        && contextual
            .returns
            .iter()
            .all(|kind| *kind == TypeKind::Void)
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
    let checks_conditionals = rule.configuration.options["checks_conditionals"]
        .as_bool()
        .expect("validated checks_conditionals option");
    let checks_void_return = void_return_checks(&rule.configuration.options["checks_void_return"]);
    if checks_conditionals {
        for (site, id) in input.input.conditions().iter().zip(
            input
                .conditions
                .as_ref()
                .expect("validated condition facts"),
        ) {
            if !input.resolved_promise[id.0] {
                continue;
            }
            diagnostics.push(LintDiagnostic {
                rule_id: RULE.into(),
                level,
                message_id: "promiseCondition".into(),
                message: "Promise used where a synchronous boolean condition is expected.".into(),
                start: site.test.lo,
                end: site.test.hi,
                fix: None,
            });
        }
    }
    if checks_void_return.arguments
        && let Some(arguments) = input.call_arguments.as_ref()
    {
        for (call_site, call_arguments) in input.input.calls().iter().zip(arguments) {
            for (span, argument) in call_site.arguments.iter().zip(call_arguments) {
                if !is_promise_callback(input, argument) {
                    continue;
                }
                diagnostics.push(LintDiagnostic {
                    rule_id: RULE.into(),
                    level,
                    message_id: "promiseCallback".into(),
                    message: "Promise-returning callback passed to a void-returning parameter."
                        .into(),
                    start: span.lo,
                    end: span.hi,
                    fix: None,
                });
            }
        }
    }
    if checks_void_return.variables
        && let Some(assignments) = input.assignment_call_types.as_ref()
    {
        for (site, assignment) in input.input.assignments().iter().zip(assignments) {
            if !is_promise_callback(input, assignment) {
                continue;
            }
            diagnostics.push(LintDiagnostic {
                rule_id: RULE.into(),
                level,
                message_id: "promiseCallback".into(),
                message: "Promise-returning callback passed to a void-returning parameter.".into(),
                start: site.value.lo,
                end: site.value.hi,
                fix: None,
            });
        }
    }
    if checks_void_return.returns
        && let Some(returns) = input.return_call_types.as_ref()
    {
        for (site, returned) in input.input.returns().iter().zip(returns) {
            if !is_promise_callback(input, returned) {
                continue;
            }
            diagnostics.push(LintDiagnostic {
                rule_id: RULE.into(),
                level,
                message_id: "promiseCallback".into(),
                message: "Promise-returning callback passed to a void-returning parameter.".into(),
                start: site.argument.lo,
                end: site.argument.hi,
                fix: None,
            });
        }
    }
    if (checks_void_return.properties || checks_void_return.attributes)
        && let Some(callbacks) = input.callback_types.as_ref()
    {
        for (site, callback) in input.input.callbacks().iter().zip(callbacks) {
            let enabled = match site.kind {
                SourceCallbackKind::ObjectProperty => checks_void_return.properties,
                SourceCallbackKind::JsxAttribute => checks_void_return.attributes,
            };
            if !enabled {
                continue;
            }
            if !is_promise_callback(input, callback) {
                continue;
            }
            diagnostics.push(LintDiagnostic {
                rule_id: RULE.into(),
                level,
                message_id: "promiseCallback".into(),
                message: "Promise-returning callback passed to a void-returning parameter.".into(),
                start: site.value.lo,
                end: site.value.hi,
                fix: None,
            });
        }
    }
}
