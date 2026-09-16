use crate::{Analysis, LintOptions, ParameterKind, RULES, RuleLevel, effective_rules};
use serde::Serialize;
use serde_json::{Value, json};

/// Owned, serializable descriptions derived from the same registry used by execution.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleInfo {
    pub id: String,
    pub description: String,
    pub category: String,
    pub languages: Vec<String>,
    pub default_level: RuleLevel,
    pub analysis: String,
    pub options_schema: Value,
    pub message_ids: Vec<String>,
    pub documentation: String,
    pub fixable: bool,
}

pub fn rule_catalog() -> Vec<RuleInfo> {
    let defaults = effective_rules(&LintOptions::default()).expect("built-in defaults are valid");
    let mut rules: Vec<_> = RULES.iter().map(|rule| {
        let properties: serde_json::Map<String, Value> = rule.parameters.iter().map(|parameter| {
            let schema = match parameter.kind {
                ParameterKind::Boolean(default) => json!({"type":"boolean", "default":default}),
                ParameterKind::BooleanOrObject { default, keys } => {
                    let properties = keys.iter().map(|key| ((*key).into(), json!({"type":"boolean"}))).collect::<serde_json::Map<String, Value>>();
                    json!({"oneOf":[{"type":"boolean"},{"type":"object","additionalProperties":false,"properties":properties}],"default":default})
                }
                ParameterKind::Integer { default, minimum, maximum } => json!({"type":"integer", "default":default, "minimum":minimum, "maximum":maximum}),
                ParameterKind::Regex(default) => json!({"type":"string", "default":default, "format":"rust-regex", "maxLength":4096}),
                ParameterKind::RegexList => json!({"type":"array","default":[],"maxItems":64,"items":{"type":"string","format":"rust-regex","maxLength":4096}}),
                ParameterKind::PathZones => crate::module_graph::parameters::zones_schema(),
                ParameterKind::Permutation(values) => json!({"type":"array","default":values,"minItems":values.len(),"maxItems":values.len(),"uniqueItems":true,"items":{"type":"string","enum":values}}),
                ParameterKind::Choice { default, values } => json!({"type":"string", "default":default, "enum":values}),
            };
            (parameter.name.into(), schema)
        }).collect();
        RuleInfo {
            id: rule.id.into(), description: rule.description.into(), category: rule.category.into(),
            languages: rule.languages.iter().map(|language| (*language).into()).collect(),
            default_level: defaults[rule.id],
            analysis: match rule.analysis { Analysis::Syntax => "syntax", Analysis::Scope => "scope", Analysis::ControlFlow => "control-flow", Analysis::ScopeControlFlow => "scope-control-flow", Analysis::ModuleGraph => "module-graph", Analysis::TypeInformation => "type-information" }.into(),
            options_schema: json!({"type":"object", "additionalProperties":false, "properties":properties}),
            message_ids: rule.message_ids.iter().map(|id| (*id).into()).collect(),
            documentation: "/reference/cli/lint".into(), fixable: rule.fixable,
        }
    }).collect();
    rules.sort_by(|a, b| a.id.cmp(&b.id));
    rules
}
