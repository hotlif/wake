use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{LintError, LintOptions, RULES, RuleLevel};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RuleSetting {
    Level(RuleLevel),
    Options(RuleConfiguration),
}

impl From<RuleLevel> for RuleSetting {
    fn from(value: RuleLevel) -> Self {
        Self::Level(value)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleConfiguration {
    pub level: RuleLevel,
    #[serde(default)]
    pub options: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Serialize)]
pub struct EffectiveRule {
    #[serde(flatten)]
    pub configuration: RuleConfiguration,
    pub source: String,
}

/// A closed parameter schema. The default is also the materialized canonical value.
pub enum ParameterKind {
    Boolean(bool),
    /// A boolean shorthand or a closed object of boolean switches. The materialized default is
    /// the boolean shorthand; consumers expand omitted object members to the same default.
    BooleanOrObject {
        default: bool,
        keys: &'static [&'static str],
    },
    Integer {
        default: u32,
        minimum: u32,
        maximum: u32,
    },
    /// Bounded Rust regex text; an empty value disables the optional name filter.
    Regex(&'static str),
    RegexList,
    PathZones,
    /// Every allowed name exactly once, in the caller's chosen order.
    Permutation(&'static [&'static str]),
    Choice {
        default: &'static str,
        values: &'static [&'static str],
    },
}

pub struct Parameter {
    pub name: &'static str,
    pub kind: ParameterKind,
}

impl Parameter {
    pub const fn boolean(name: &'static str, default: bool) -> Self {
        Self {
            name,
            kind: ParameterKind::Boolean(default),
        }
    }

    pub const fn boolean_or_object(
        name: &'static str,
        default: bool,
        keys: &'static [&'static str],
    ) -> Self {
        Self {
            name,
            kind: ParameterKind::BooleanOrObject { default, keys },
        }
    }

    fn default_value(&self) -> Value {
        match self.kind {
            ParameterKind::Boolean(value) => Value::Bool(value),
            ParameterKind::BooleanOrObject { default, .. } => Value::Bool(default),
            ParameterKind::Integer { default, .. } => Value::from(default),
            ParameterKind::Regex(value) => Value::String(value.into()),
            ParameterKind::RegexList | ParameterKind::PathZones => Value::Array(Vec::new()),
            ParameterKind::Permutation(values) => values
                .iter()
                .map(|value| Value::String((*value).into()))
                .collect(),
            ParameterKind::Choice { default, .. } => Value::String(default.into()),
        }
    }

    fn accepts(&self, value: &Value) -> bool {
        match self.kind {
            ParameterKind::Boolean(_) => value.is_boolean(),
            ParameterKind::BooleanOrObject { keys, .. } => {
                value.is_boolean()
                    || value.as_object().is_some_and(|object| {
                        object.iter().all(|(name, value)| {
                            keys.contains(&name.as_str()) && value.is_boolean()
                        })
                    })
            }
            ParameterKind::Integer {
                minimum, maximum, ..
            } => value
                .as_u64()
                .is_some_and(|value| (u64::from(minimum)..=u64::from(maximum)).contains(&value)),
            ParameterKind::Regex(_) => value.as_str().is_some_and(|pattern| {
                pattern.chars().count() <= 4096 && regex::Regex::new(pattern).is_ok()
            }),
            ParameterKind::RegexList => crate::module_graph::parameters::regex_list(value),
            ParameterKind::PathZones => crate::module_graph::parameters::zones(value),
            ParameterKind::Permutation(values) => value.as_array().is_some_and(|list| {
                list.len() == values.len()
                    && values.iter().all(|name| {
                        list.iter()
                            .filter(|item| item.as_str() == Some(name))
                            .count()
                            == 1
                    })
            }),
            ParameterKind::Choice { values, .. } => {
                value.as_str().is_some_and(|value| values.contains(&value))
            }
        }
    }
}

pub struct Preset {
    pub id: &'static str,
    pub alias: &'static str,
    pub rules: &'static [(&'static str, RuleLevel)],
}

// Versioned membership is explicit: growing RULES must never silently change an old preset.
pub static PRESETS: &[Preset] = &[
    Preset {
        id: "recommended@1",
        alias: "recommended",
        rules: &[
            ("js/no-debugger", RuleLevel::Error),
            ("js/eqeqeq", RuleLevel::Warn),
            ("js/no-empty", RuleLevel::Error),
            ("js/no-duplicate-case", RuleLevel::Error),
            ("js/no-dupe-keys", RuleLevel::Error),
            ("js/no-constant-condition", RuleLevel::Warn),
        ],
    },
    Preset {
        id: "react@1",
        alias: "react",
        rules: &[
            ("react/jsx-no-duplicate-props", RuleLevel::Error),
            ("react/no-danger", RuleLevel::Warn),
            ("react/no-children-prop", RuleLevel::Error),
            ("react/self-closing-comp", RuleLevel::Warn),
        ],
    },
    Preset {
        id: "style@1",
        alias: "style",
        rules: &[("style/eol-last", RuleLevel::Warn)],
    },
    Preset {
        id: "all@1",
        alias: "all",
        rules: &[
            ("js/no-debugger", RuleLevel::Error),
            ("js/eqeqeq", RuleLevel::Warn),
            ("js/no-empty", RuleLevel::Error),
            ("js/no-duplicate-case", RuleLevel::Error),
            ("js/no-dupe-keys", RuleLevel::Error),
            ("js/no-constant-condition", RuleLevel::Warn),
            ("react/jsx-no-duplicate-props", RuleLevel::Error),
            ("react/no-danger", RuleLevel::Warn),
            ("react/no-children-prop", RuleLevel::Error),
            ("react/self-closing-comp", RuleLevel::Warn),
            ("style/eol-last", RuleLevel::Warn),
        ],
    },
];

/// Validate even disabled settings and materialize defaults. No paths or environment access.
pub fn resolve_setting(id: &str, setting: &RuleSetting) -> Result<RuleConfiguration, LintError> {
    let metadata = RULES
        .iter()
        .find(|rule| rule.id == id)
        .ok_or_else(|| LintError::Configuration(format!("Unknown lint rule: {id}")))?;
    let (level, supplied) = match setting {
        RuleSetting::Level(level) => (*level, None),
        RuleSetting::Options(value) => (value.level, Some(&value.options)),
    };
    let mut options: BTreeMap<_, _> = metadata
        .parameters
        .iter()
        .map(|parameter| (parameter.name.into(), parameter.default_value()))
        .collect();
    if let Some(supplied) = supplied {
        for (name, value) in supplied {
            let parameter = metadata
                .parameters
                .iter()
                .find(|parameter| parameter.name == name)
                .ok_or_else(|| {
                    LintError::Configuration(format!("Unknown option {name} for lint rule {id}"))
                })?;
            if !parameter.accepts(value) {
                return Err(LintError::Configuration(format!(
                    "Invalid option {name} for lint rule {id}: {value}"
                )));
            }
            options.insert(name.clone(), value.clone());
        }
    }
    Ok(RuleConfiguration { level, options })
}

pub fn effective_configuration(
    options: &LintOptions,
) -> Result<BTreeMap<String, EffectiveRule>, LintError> {
    crate::validate_globals(&options.globals)?;
    let mut effective = RULES
        .iter()
        .map(|rule| {
            Ok((
                rule.id.into(),
                EffectiveRule {
                    configuration: resolve_setting(rule.id, &RuleLevel::Off.into())?,
                    source: "default".into(),
                },
            ))
        })
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    let names = options
        .recommended
        .then_some("recommended@1")
        .into_iter()
        .chain(options.presets.iter().map(String::as_str));
    for name in names {
        let preset = PRESETS
            .iter()
            .find(|preset| preset.id == name || preset.alias == name)
            .ok_or_else(|| LintError::Configuration(format!("Unknown lint preset: {name}")))?;
        for (id, level) in preset.rules {
            effective.insert(
                (*id).into(),
                EffectiveRule {
                    configuration: resolve_setting(id, &(*level).into())?,
                    source: format!("preset:{}", preset.id),
                },
            );
        }
    }
    for (id, setting) in &options.rules {
        effective.insert(
            id.clone(),
            EffectiveRule {
                configuration: resolve_setting(id, setting)?,
                source: "rules".into(),
            },
        );
    }
    Ok(effective)
}
