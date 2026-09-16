//! Native lint project orchestration. Discovery/configuration and snapshot diagnostics are
//! shared by Rust and Node frontends; rules stay in wake_lint_core, source publication in output.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use regex::Regex;
use serde::{Deserialize, Serialize};
use wake_common::{Diagnostic, Severity, SourceFile, Span};
use wake_config::{Lint, LintRuleLevel, LintRuleSetting};
use wake_lint_core::{LintOptions, RuleLevel, SourceType};

use crate::output::{LintSourceSnapshot, replace_lint_source};
use crate::{CancellationToken, DiagnosticInfo, WakeError};

mod baseline;
mod cache;
mod context;
mod execution;
mod module_snapshot;
mod modules;
#[cfg(test)]
mod tests;
mod type_service;
mod watch;
pub use baseline::{LintBaselineMode, LintBaselineOptions, LintBaselineStats};
pub use cache::LintCacheStats;
pub use context::{LintCheck, LintContext, LintDocument, LintSnapshot};
pub use watch::{LintWatchEvent, LintWatcher};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LintFixMode {
    #[default]
    Off,
    DryRun,
    Write,
}

#[derive(Clone, Debug)]
pub struct LintProjectOptions {
    pub root: PathBuf,
    pub paths: Vec<String>,
    pub stdin: Option<LintStdin>,
    pub max_warnings: Option<usize>,
    pub fix: LintFixMode,
    /// Validate and explain a virtual filename without reading source files.
    pub print_config: Option<String>,
    /// Inspect native rules without reading the filesystem or project configuration.
    pub list_rules: bool,
    pub cache: bool,
    pub baseline: Option<LintBaselineOptions>,
    /// Closed rule settings, validated by the native registry; applied after project overrides.
    pub rules: BTreeMap<String, serde_json::Value>,
    /// Explicit globals, applied by name after matching configuration overrides.
    pub globals: BTreeMap<String, serde_json::Value>,
    /// Explicit built-in host global sets, applied after matching configuration overrides.
    pub environments: Vec<String>,
}

impl Default for LintProjectOptions {
    fn default() -> Self {
        Self {
            root: PathBuf::from("."),
            paths: Vec::new(),
            stdin: None,
            max_warnings: None,
            fix: LintFixMode::Off,
            print_config: None,
            list_rules: false,
            cache: false,
            baseline: None,
            rules: BTreeMap::new(),
            globals: BTreeMap::new(),
            environments: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LintStdin {
    pub filename: String,
    pub text: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LintFileResult {
    pub path: String,
    pub diagnostics: Vec<LintDiagnosticInfo>,
    pub changed: bool,
    pub written: bool,
    pub fix_passes: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LintDiagnosticInfo {
    #[serde(flatten)]
    pub diagnostic: DiagnosticInfo,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix: Option<LintFixInfo>,
}
#[derive(Debug, Serialize)]
pub struct LintFixInfo {
    pub edits: Vec<LintTextEdit>,
}
#[derive(Debug, Serialize)]
pub struct LintTextEdit {
    pub start: u32,
    pub end: u32,
    pub text: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LintProjectResult {
    pub files: Vec<LintFileResult>,
    pub error_count: usize,
    pub warning_count: usize,
    pub exit_code: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<LintEffectiveConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub catalog: Option<LintRuleCatalog>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache: Option<LintCacheStats>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline: Option<LintBaselineStats>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LintRuleCatalog {
    pub schema: &'static str,
    pub pipeline_version: &'static str,
    pub rules: Vec<wake_lint_core::RuleInfo>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LintEffectiveConfig {
    pub schema: &'static str,
    pub pipeline_version: &'static str,
    pub path: String,
    pub language: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub processor: Option<String>,
    pub ignored: bool,
    pub report_unused_disable: &'static str,
    pub rules: BTreeMap<String, LintEffectiveRule>,
    pub globals: BTreeMap<String, wake_lint_core::EffectiveGlobal>,
    pub environments: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub types: Option<wake_config::LintTypes>,
}

#[derive(Debug, Serialize)]
pub struct LintEffectiveRule {
    pub level: &'static str,
    pub options: BTreeMap<String, serde_json::Value>,
    pub source: String,
}

fn level_name(level: RuleLevel) -> &'static str {
    match level {
        RuleLevel::Off => "off",
        RuleLevel::Warn => "warn",
        RuleLevel::Error => "error",
    }
}

fn failure(message: impl Into<String>) -> WakeError {
    WakeError::new("WAKE_LINT_CONFIG", message)
}

fn analysis_failure(error: wake_lint_core::LintError, path: &str, fixing: bool) -> WakeError {
    let code = match &error {
        wake_lint_core::LintError::Analysis(_) => "WAKE_LINT_ANALYSIS",
        _ if fixing => "WAKE_LINT_FIX",
        _ => "WAKE_LINT_CONFIG",
    };
    WakeError::new(code, error.to_string()).at(Path::new(path))
}

/// Decode the CLI's repeatable assignment syntax. Semantic validation remains registry-owned.
pub fn parse_lint_rule_argument(input: &str) -> Result<(String, serde_json::Value), WakeError> {
    let (id, value) = input
        .split_once('=')
        .ok_or_else(|| failure("--rule requires id=level or id=JSON"))?;
    if id.is_empty() {
        return Err(failure("--rule requires a rule ID"));
    }
    let value = if matches!(value, "off" | "warn" | "error") {
        serde_json::Value::String(value.into())
    } else {
        serde_json::from_str(value)
            .map_err(|error| failure(format!("Invalid --rule value for {id}: {error}")))?
    };
    Ok((id.into(), value))
}

fn io_error(path: &Path, error: std::io::Error) -> WakeError {
    WakeError::new("WAKE_LINT_IO", error.to_string()).at(path)
}

/// Decode a repeatable CLI global assignment; core validation runs on the request map.
pub fn parse_lint_global_argument(input: &str) -> Result<(String, serde_json::Value), WakeError> {
    let (name, mode) = input
        .split_once('=')
        .filter(|(name, _)| !name.is_empty())
        .ok_or_else(|| failure("--global requires name=readonly|writable|off"))?;
    Ok((name.into(), serde_json::Value::String(mode.into())))
}

/// Decode a repeatable CLI environment override (`browser` or `node`).
pub fn parse_lint_environment_argument(input: &str) -> Result<String, WakeError> {
    if input.is_empty() {
        return Err(failure("--env requires browser or node"));
    }
    let environment = input.to_owned();
    wake_lint_core::validate_environments(std::slice::from_ref(&environment))
        .map_err(|error| failure(error.to_string()))?;
    Ok(environment)
}

fn core_level(level: LintRuleLevel) -> RuleLevel {
    match level {
        LintRuleLevel::Off => RuleLevel::Off,
        LintRuleLevel::Warn => RuleLevel::Warn,
        LintRuleLevel::Error => RuleLevel::Error,
    }
}

fn rule_options(recommended: bool, rules: &BTreeMap<String, LintRuleSetting>) -> LintOptions {
    LintOptions {
        recommended,
        rules: rules
            .iter()
            .map(|(id, setting)| {
                (
                    id.clone(),
                    match setting {
                        LintRuleSetting::Level(level) => core_level(*level).into(),
                        LintRuleSetting::Options(value) => wake_lint_core::RuleSetting::Options(
                            wake_lint_core::RuleConfiguration {
                                level: core_level(value.level),
                                options: value.options.clone(),
                            },
                        ),
                    },
                )
            })
            .collect(),
        ..Default::default()
    }
}

struct ConfiguredLint {
    types: Option<wake_config::LintTypes>,
    files: Vec<Regex>,
    ignore: Vec<Regex>,
    processors: Vec<(Regex, String)>,
    base: LintOptions,
    overrides: Vec<(Vec<Regex>, LintOptions)>,
    request: BTreeMap<String, wake_lint_core::RuleSetting>,
    request_globals: BTreeMap<String, wake_lint_core::GlobalMode>,
    request_environments: Vec<String>,
}

fn global_options(
    raw: BTreeMap<String, serde_json::Value>,
) -> Result<BTreeMap<String, wake_lint_core::GlobalMode>, WakeError> {
    let globals = raw
        .into_iter()
        .map(|(name, value)| {
            let mode = serde_json::from_value(value).map_err(|error| {
                failure(format!("Invalid lint global mode for {name}: {error}"))
            })?;
            Ok((name, mode))
        })
        .collect::<Result<_, WakeError>>()?;
    wake_lint_core::validate_globals(&globals).map_err(|error| failure(error.to_string()))?;
    Ok(globals)
}

fn environment_options(raw: Vec<String>) -> Result<Vec<String>, WakeError> {
    raw.into_iter()
        .map(|environment| parse_lint_environment_argument(&environment))
        .collect()
}

impl ConfiguredLint {
    fn new(
        config: Lint,
        request: BTreeMap<String, serde_json::Value>,
        globals: BTreeMap<String, serde_json::Value>,
        environments: Vec<String>,
    ) -> Result<Self, WakeError> {
        let types = config.types.map(type_service::validate).transpose()?;
        let mut base = rule_options(config.recommended, &config.rules);
        base.globals = global_options(config.globals)?;
        base.environments = environment_options(config.environments)?;
        base.presets = config.presets;
        base.report_unused_disable = match config.report_unused_disable {
            LintRuleLevel::Off => RuleLevel::Off,
            LintRuleLevel::Warn => RuleLevel::Warn,
            LintRuleLevel::Error => RuleLevel::Error,
        };
        wake_lint_core::effective_rules(&base).map_err(|error| failure(error.to_string()))?;
        let files = config
            .files
            .iter()
            .map(|pattern| glob(pattern))
            .collect::<Result<_, _>>()?;
        let ignore = config
            .ignore
            .iter()
            .map(|pattern| glob(pattern))
            .collect::<Result<_, _>>()?;
        let processors = config
            .processors
            .into_iter()
            .map(|(pattern, processor)| {
                if processor != "markdown" {
                    return Err(failure(format!(
                        "Unsupported lint processor `{processor}`; only `markdown` is built in"
                    )));
                }
                Ok((glob(&pattern)?, processor))
            })
            .collect::<Result<Vec<_>, WakeError>>()?;
        let mut overrides = Vec::new();
        for entry in config.overrides {
            if entry.files.is_empty() {
                return Err(failure("lint override requires at least one files pattern"));
            }
            let mut options = rule_options(config.recommended, &entry.rules);
            options.globals = global_options(entry.globals)?;
            options.environments = environment_options(entry.environments)?;
            wake_lint_core::effective_rules(&options)
                .map_err(|error| failure(error.to_string()))?;
            let patterns = entry
                .files
                .iter()
                .map(|pattern| glob(pattern))
                .collect::<Result<_, _>>()?;
            overrides.push((patterns, options));
        }
        let request = request
            .into_iter()
            .map(|(id, value)| {
                let setting = serde_json::from_value::<wake_lint_core::RuleSetting>(value)
                    .map_err(|error| failure(format!("Invalid lint setting for {id}: {error}")))?;
                wake_lint_core::resolve_setting(&id, &setting)
                    .map_err(|error| failure(error.to_string()))?;
                Ok((id, setting))
            })
            .collect::<Result<_, WakeError>>()?;
        Ok(Self {
            types,
            files,
            ignore,
            processors,
            base,
            overrides,
            request,
            request_globals: global_options(globals)?,
            request_environments: environment_options(environments)?,
        })
    }

    fn includes(&self, path: &str) -> bool {
        !default_ignored(path)
            && self.files.iter().any(|pattern| pattern.is_match(path))
            && !self.ignore.iter().any(|pattern| pattern.is_match(path))
    }

    fn processor_for(&self, path: &str) -> Option<&str> {
        self.processors
            .iter()
            .find(|(pattern, _)| pattern.is_match(path))
            .map(|(_, processor)| processor.as_str())
    }

    fn options(&self, path: &str) -> LintOptions {
        let mut options = self.base.clone();
        for (patterns, override_options) in &self.overrides {
            if patterns.iter().any(|pattern| pattern.is_match(path)) {
                options.rules.extend(override_options.rules.clone());
                options.globals.extend(override_options.globals.clone());
            }
        }
        options.rules.extend(self.request.clone());
        options.globals.extend(self.request_globals.clone());
        options
            .environments
            .extend(self.request_environments.clone());
        options.environments.sort();
        options.environments.dedup();
        options
    }

    fn explain(&self, path: String) -> Result<LintEffectiveConfig, WakeError> {
        let language = match self.processor_for(&path) {
            Some("markdown") => "module",
            _ => match source_type(&path)? {
                SourceType::Module => "module",
                SourceType::Script => "script",
                SourceType::TypeScript => "typescript",
                SourceType::Tsx => "tsx",
                SourceType::Jsx => "jsx",
            },
        };
        let mut effective = wake_lint_core::effective_configuration(&self.base)
            .map_err(|error| failure(error.to_string()))?;
        let mut globals = wake_lint_core::effective_globals(&self.base)
            .map_err(|error| failure(error.to_string()))?;
        for value in globals.values_mut() {
            if value.source == "globals" {
                value.source = "config:globals".into();
            }
        }
        for value in effective.values_mut() {
            if value.source == "rules" {
                value.source = "config:rules".into();
            }
        }
        for (index, (patterns, options)) in self.overrides.iter().enumerate() {
            if patterns.iter().any(|pattern| pattern.is_match(&path)) {
                let environment_globals = wake_lint_core::effective_globals(&LintOptions {
                    environments: options.environments.clone(),
                    ..LintOptions::default()
                })
                .map_err(|error| failure(error.to_string()))?;
                globals.extend(
                    environment_globals
                        .into_iter()
                        .filter(|(_, value)| value.source.starts_with("environment:"))
                        .map(|(name, value)| {
                            (
                                name,
                                wake_lint_core::EffectiveGlobal {
                                    mode: value.mode,
                                    source: format!("override:{index}:{}", value.source),
                                },
                            )
                        }),
                );
                globals.extend(options.globals.iter().map(|(name, &mode)| {
                    (
                        name.clone(),
                        wake_lint_core::EffectiveGlobal {
                            mode,
                            source: format!("override:{index}"),
                        },
                    )
                }));
                for (id, setting) in &options.rules {
                    effective.insert(
                        id.clone(),
                        wake_lint_core::EffectiveRule {
                            configuration: wake_lint_core::resolve_setting(id, setting)
                                .map_err(|error| failure(error.to_string()))?,
                            source: format!("override:{index}"),
                        },
                    );
                }
            }
        }
        for (id, setting) in &self.request {
            effective.insert(
                id.clone(),
                wake_lint_core::EffectiveRule {
                    configuration: wake_lint_core::resolve_setting(id, setting)
                        .map_err(|error| failure(error.to_string()))?,
                    source: "request".into(),
                },
            );
        }
        let request_environment_globals = wake_lint_core::effective_globals(&LintOptions {
            environments: self.request_environments.clone(),
            ..LintOptions::default()
        })
        .map_err(|error| failure(error.to_string()))?;
        globals.extend(
            request_environment_globals
                .into_iter()
                .filter(|(_, value)| value.source.starts_with("environment:"))
                .map(|(name, value)| {
                    (
                        name,
                        wake_lint_core::EffectiveGlobal {
                            mode: value.mode,
                            source: format!("request:{}", value.source),
                        },
                    )
                }),
        );
        globals.extend(self.request_globals.iter().map(|(name, &mode)| {
            (
                name.clone(),
                wake_lint_core::EffectiveGlobal {
                    mode,
                    source: "request".into(),
                },
            )
        }));
        let environments = self.options(&path).environments;
        Ok(LintEffectiveConfig {
            types: self.types.clone(),
            globals,
            environments,
            schema: "wake.lint.config.v1",
            pipeline_version: wake_lint_core::PIPELINE_VERSION,
            language,
            processor: self.processor_for(&path).map(str::to_owned),
            ignored: !self.includes(&path),
            path,
            report_unused_disable: level_name(self.base.report_unused_disable),
            rules: effective
                .into_iter()
                .map(|(id, value)| {
                    (
                        id,
                        LintEffectiveRule {
                            level: level_name(value.configuration.level),
                            options: value.configuration.options,
                            source: value.source,
                        },
                    )
                })
                .collect(),
        })
    }
}

fn default_ignored(path: &str) -> bool {
    path.split('/').any(|part| {
        matches!(
            part,
            "node_modules"
                | ".git"
                | ".yarn"
                | ".wake"
                | "dist"
                | "docs-dist"
                | ".pnp.cjs"
                | ".pnp.mjs"
                | ".pnp.loader.mjs"
        )
    }) || [".d.ts", ".d.mts", ".d.cts"]
        .iter()
        .any(|extension| path.ends_with(extension))
}

fn normalize_relative(input: &str) -> Result<String, WakeError> {
    let normalized = input.replace('\\', "/");
    if input.is_empty()
        || Path::new(&normalized).is_absolute()
        || normalized.contains(':')
        || Path::new(&normalized).components().any(|part| {
            matches!(
                part,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(failure(format!(
            "lint path must stay relative to root: {input}"
        )));
    }
    Ok(normalized
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .collect::<Vec<_>>()
        .join("/"))
}

fn glob(pattern: &str) -> Result<Regex, WakeError> {
    let pattern = normalize_relative(pattern)?;
    if pattern.is_empty()
        || pattern
            .chars()
            .any(|ch| matches!(ch, '!' | '[' | ']' | '{' | '}'))
    {
        return Err(failure(format!("Unsupported lint glob: {pattern}")));
    }
    let mut expression = String::from("^");
    let segments: Vec<_> = pattern.split('/').collect();
    for (index, segment) in segments.iter().enumerate() {
        let last = index + 1 == segments.len();
        if *segment == "**" {
            expression.push_str(if last { ".*" } else { "(?:[^/]+/)*" });
            continue;
        }
        if segment.contains("**") {
            return Err(failure("** must be a complete path segment"));
        }
        for ch in segment.chars() {
            match ch {
                '*' => expression.push_str("[^/]*"),
                '?' => expression.push_str("[^/]"),
                _ => expression.push_str(&regex::escape(&ch.to_string())),
            }
        }
        if !last {
            expression.push('/');
        }
    }
    expression.push('$');
    Regex::new(&expression).map_err(|error| failure(error.to_string()))
}

enum Selection {
    File(String),
    Directory(String),
    Glob(Regex),
}

impl Selection {
    fn matches(&self, path: &str) -> bool {
        match self {
            Self::File(file) => file == path,
            Self::Directory(directory) => {
                directory.is_empty()
                    || path
                        .strip_prefix(directory)
                        .is_some_and(|rest| rest.starts_with('/'))
            }
            Self::Glob(pattern) => pattern.is_match(path),
        }
    }
}

fn discover(
    root: &Path,
    directory: &Path,
    files: &mut Vec<(String, PathBuf)>,
    cancellation: &CancellationToken,
) -> Result<(), WakeError> {
    cancellation.check()?;
    for entry in std::fs::read_dir(directory).map_err(|error| io_error(directory, error))? {
        let entry = entry.map_err(|error| io_error(directory, error))?;
        let path = entry.path();
        let relative = path
            .strip_prefix(root)
            .expect("discovery remains below root");
        let relative = relative
            .to_str()
            .ok_or_else(|| failure("lint requires UTF-8 paths"))?
            .replace('\\', "/");
        if default_ignored(&relative) {
            continue;
        }
        let kind = entry.file_type().map_err(|error| io_error(&path, error))?;
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            discover(root, &path, files, cancellation)?;
        } else if kind.is_file() {
            files.push((relative, path));
        }
    }
    Ok(())
}

fn source_type(path: &str) -> Result<SourceType, WakeError> {
    match Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
    {
        Some("js" | "mjs") => Ok(SourceType::Module),
        Some("cjs") => Ok(SourceType::Script),
        Some("ts" | "mts" | "cts") => Ok(SourceType::TypeScript),
        Some("tsx") => Ok(SourceType::Tsx),
        Some("jsx") => Ok(SourceType::Jsx),
        _ => Err(failure(format!(
            "Unsupported lint source extension: {path}"
        ))),
    }
}

fn markdown_line_end(line: &str) -> usize {
    line.trim_end_matches(['\r', '\n']).len()
}

fn markdown_fence(line: &str) -> Option<(u8, usize, &str)> {
    let content = &line[..markdown_line_end(line)];
    let bytes = content.as_bytes();
    let mut offset = 0;
    while offset < bytes.len() && offset < 3 && bytes[offset] == b' ' {
        offset += 1;
    }
    let marker = *bytes.get(offset)?;
    if marker != b'`' && marker != b'~' {
        return None;
    }
    let start = offset;
    while offset < bytes.len() && bytes[offset] == marker {
        offset += 1;
    }
    let length = offset - start;
    (length >= 3).then(|| (marker, length, content[offset..].trim()))
}

fn mask_markdown_line(line: &str) -> String {
    line.chars()
        .map(|ch| {
            if matches!(ch, '\r' | '\n') {
                ch.to_string()
            } else {
                " ".repeat(ch.len_utf8())
            }
        })
        .collect()
}

fn markdown_virtual_source(source: &str) -> String {
    let mut output = String::with_capacity(source.len());
    let mut position = 0;
    let mut active: Option<(u8, usize)> = None;
    while position < source.len() {
        let start = position;
        while position < source.len() {
            let byte = source.as_bytes()[position];
            position += 1;
            if byte == b'\n' {
                break;
            }
            if byte == b'\r' {
                if position < source.len() && source.as_bytes()[position] == b'\n' {
                    position += 1;
                }
                break;
            }
        }
        let line = &source[start..position];
        if let Some((marker, length, info)) = markdown_fence(line) {
            if let Some((active_marker, active_length)) = active {
                let closing = marker == active_marker && length >= active_length && info.is_empty();
                if closing {
                    output.push_str(&mask_markdown_line(line));
                } else {
                    output.push_str(line);
                }
                if closing {
                    active = None;
                }
            } else {
                let language = info.split_whitespace().next().unwrap_or_default();
                let javascript = matches!(
                    language.to_ascii_lowercase().as_str(),
                    "js" | "javascript" | "mjs" | "cjs"
                );
                output.push_str(&mask_markdown_line(line));
                if javascript {
                    active = Some((marker, length));
                }
            }
        } else if active.is_some() {
            output.push_str(line);
        } else {
            output.push_str(&mask_markdown_line(line));
        }
    }
    output
}

fn analyze(
    path: String,
    text: String,
    config: &ConfiguredLint,
    fix: LintFixMode,
    mut cache: Option<&mut cache::LintCacheSession>,
    mut baseline: Option<&mut baseline::FileBaseline>,
) -> Result<LintFileResult, WakeError> {
    let processor = config.processor_for(&path);
    if processor.is_some() && fix != LintFixMode::Off {
        if let Some(cache) = cache.as_deref_mut() {
            cache.bypass();
        }
        return Err(failure(
            "lint processors only support diagnostics; fix modes require a reversible source mapping",
        ));
    }
    let (language, lint_text) = if processor == Some("markdown") {
        (SourceType::Module, markdown_virtual_source(&text))
    } else {
        (source_type(&path)?, text.clone())
    };
    if processor.is_some()
        && let Some(cache) = cache.as_deref_mut()
    {
        cache.bypass();
    }
    let options = config.options(&path);
    let entries = baseline
        .as_ref()
        .map(|session| session.entries())
        .unwrap_or_default();
    let (mut result, final_text, fix_passes) = if fix == LintFixMode::Off {
        (
            if let Some(cache) = cache {
                cache.check(&path, &lint_text, language, &options, &entries)?
            } else {
                wake_lint_core::lint_text(&lint_text, language, &options)
                    .map_err(|error| analysis_failure(error, &path, false))?
            },
            text.clone(),
            0,
        )
    } else {
        if let Some(cache) = cache {
            cache.bypass();
        }
        let (fixed, matched) = if baseline.is_some() {
            wake_lint_core::fix_text_with_baseline(&text, language, &options, &entries)
        } else {
            wake_lint_core::fix_text(&text, language, &options)
                .map(|fixed| (fixed, wake_lint_core::BaselineMatch::default()))
        }
        .map_err(|error| analysis_failure(error, &path, true))?;
        if let Some(session) = baseline.as_mut() {
            session.record(&fixed.result, matched);
        }
        (fixed.result, fixed.output, fixed.passes)
    };
    if fix == LintFixMode::Off
        && let Some(session) = baseline
    {
        session.apply(&final_text, &mut result)?;
    }
    Ok(present_result(path, &text, final_text, fix_passes, result))
}

fn present_result(
    path: String,
    text: &str,
    final_text: String,
    fix_passes: u8,
    result: wake_lint_core::LintResult,
) -> LintFileResult {
    let changed = text != final_text;
    let output = changed.then(|| final_text.clone());
    let source = SourceFile::new_ecmascript(path.clone(), final_text);
    let present = |mut diagnostic: Diagnostic| {
        diagnostic.path = Some(path.clone());
        DiagnosticInfo::from_diagnostic(&diagnostic, Some(&source))
    };
    let mut diagnostics: Vec<_> = result
        .parse_diagnostics
        .into_iter()
        .map(|diagnostic| LintDiagnosticInfo {
            diagnostic: present(diagnostic),
            message_id: None,
            fix: None,
        })
        .collect();
    diagnostics.extend(result.diagnostics.into_iter().map(|diagnostic| {
        let value = Diagnostic::new(
            if diagnostic.level == RuleLevel::Error {
                Severity::Error
            } else {
                Severity::Warning
            },
            diagnostic.message,
        )
        .with_code(diagnostic.rule_id)
        .with_primary(
            Span::new(diagnostic.start, diagnostic.end),
            diagnostic.message_id.clone(),
        );
        LintDiagnosticInfo {
            diagnostic: present(value),
            message_id: Some(diagnostic.message_id),
            fix: diagnostic.fix.map(|fix| LintFixInfo {
                edits: fix
                    .edits
                    .into_iter()
                    .map(|edit| LintTextEdit {
                        start: edit.start,
                        end: edit.end,
                        text: edit.text,
                    })
                    .collect(),
            }),
        }
    }));
    LintFileResult {
        path,
        diagnostics,
        changed,
        written: false,
        fix_passes,
        output,
    }
}

struct FileTask {
    path: String,
    text: String,
    config: Arc<ConfiguredLint>,
    fix: LintFixMode,
    cache: Option<cache::LintCacheSession>,
    baseline: Option<baseline::FileBaseline>,
    snapshot: Option<LintSourceSnapshot>,
}

struct CompletedFile {
    result: LintFileResult,
    cache: Option<cache::LintCacheSession>,
    baseline: Option<baseline::FileBaseline>,
    snapshot: Option<LintSourceSnapshot>,
}

impl FileTask {
    fn run(mut self) -> Result<CompletedFile, WakeError> {
        let result = analyze(
            self.path,
            self.text,
            &self.config,
            self.fix,
            self.cache.as_mut(),
            self.baseline.as_mut(),
        )?;
        Ok(CompletedFile {
            result,
            cache: self.cache,
            baseline: self.baseline,
            snapshot: self.snapshot,
        })
    }
}

fn merge_files(
    results: Vec<Result<CompletedFile, WakeError>>,
    files: &mut Vec<LintFileResult>,
    pending: &mut Vec<(usize, LintSourceSnapshot)>,
    mut cache: Option<&mut cache::LintCacheSession>,
    mut baseline: Option<&mut baseline::BaselineSession>,
) -> Result<(), WakeError> {
    for result in results {
        let completed = result?;
        if let (Some(session), Some(file)) = (cache.as_mut(), completed.cache) {
            session.merge(file);
        }
        if let (Some(session), Some(file)) = (baseline.as_mut(), completed.baseline) {
            session.merge(&completed.result.path, file)?;
        }
        if completed.result.changed
            && let Some(snapshot) = completed.snapshot
        {
            pending.push((files.len(), snapshot));
        }
        files.push(completed.result);
    }
    Ok(())
}

pub fn lint_project(
    options: LintProjectOptions,
    cancellation: &CancellationToken,
) -> Result<LintProjectResult, WakeError> {
    lint_project_with_documents(options, cancellation, &BTreeMap::new(), None)
}

fn lint_project_with_documents(
    options: LintProjectOptions,
    cancellation: &CancellationToken,
    documents: &BTreeMap<String, std::sync::Arc<LintDocument>>,
    expected_root: Option<&Path>,
) -> Result<LintProjectResult, WakeError> {
    lint_project_scheduled(options, cancellation, documents, expected_root, None)
}

fn lint_project_scheduled(
    options: LintProjectOptions,
    cancellation: &CancellationToken,
    documents: &BTreeMap<String, Arc<LintDocument>>,
    expected_root: Option<&Path>,
    scheduler: Option<&execution::Scheduler>,
) -> Result<LintProjectResult, WakeError> {
    lint_project_observed(
        options,
        cancellation,
        documents,
        expected_root,
        scheduler,
        &mut BTreeSet::new(),
    )
}

fn lint_project_observed(
    options: LintProjectOptions,
    cancellation: &CancellationToken,
    documents: &BTreeMap<String, Arc<LintDocument>>,
    expected_root: Option<&Path>,
    scheduler: Option<&execution::Scheduler>,
    dependencies: &mut BTreeSet<PathBuf>,
) -> Result<LintProjectResult, WakeError> {
    dependencies.clear();
    cancellation.check()?;
    baseline::validate_request(&options)?;
    if options.list_rules {
        if options.cache
            || !options.paths.is_empty()
            || options.stdin.is_some()
            || options.print_config.is_some()
            || !options.rules.is_empty()
            || !options.globals.is_empty()
            || !options.environments.is_empty()
            || options.max_warnings.is_some()
            || options.fix != LintFixMode::Off
        {
            return Err(failure(
                "listRules cannot be combined with paths, stdin, printConfig, rules, globals, environments, maxWarnings, fix, or cache",
            ));
        }
        return Ok(LintProjectResult {
            files: Vec::new(),
            error_count: 0,
            warning_count: 0,
            exit_code: 0,
            config: None,
            catalog: Some(LintRuleCatalog {
                schema: "wake.lint.rules.v1",
                pipeline_version: wake_lint_core::PIPELINE_VERSION,
                rules: wake_lint_core::rule_catalog(),
            }),
            cache: None,
            baseline: None,
        });
    }
    let root = options
        .root
        .canonicalize()
        .map_err(|error| io_error(&options.root, error))?;
    if let Some(expected) = expected_root {
        context::validate_root_identity(expected, &root)?;
    }
    if !root.is_dir() {
        return Err(failure("lint root must be a directory"));
    }
    let config = wake_config::load(&root).map_err(|error| failure(error.to_string()))?;
    let resolve = wake_resolver::ResolveOptions {
        alias: config.resolver_aliases(&root),
        ..Default::default()
    };
    let config = Arc::new(ConfiguredLint::new(
        config.lint,
        options.rules,
        options.globals,
        options.environments,
    )?);
    if let Some(path) = options.print_config {
        if options.cache
            || !options.paths.is_empty()
            || options.stdin.is_some()
            || options.fix != LintFixMode::Off
            || options.max_warnings.is_some()
        {
            return Err(failure(
                "print-config cannot be combined with paths, stdin, fix modes, max-warnings, or cache",
            ));
        }
        cancellation.check()?;
        return Ok(LintProjectResult {
            files: Vec::new(),
            error_count: 0,
            warning_count: 0,
            exit_code: 0,
            config: Some(config.explain(normalize_relative(&path)?)?),
            catalog: None,
            cache: None,
            baseline: None,
        });
    }
    let mut baseline = options
        .baseline
        .map(|request| {
            baseline::BaselineSession::new(
                &root,
                request,
                options.paths.is_empty() && options.stdin.is_none(),
            )
        })
        .transpose()?;
    let mut cache = options
        .cache
        .then(|| cache::LintCacheSession::new(root.join(".wake/lint/v1"), options.stdin.is_none()));
    let mut files = Vec::new();
    let mut pending = Vec::new();
    let scheduler = scheduler.unwrap_or_else(|| execution::global());
    if let Some(input) = options.stdin {
        if options.fix == LintFixMode::Write {
            return Err(failure(
                "stdin cannot be combined with write fixes; use dry-run",
            ));
        }
        if !options.paths.is_empty() {
            return Err(failure("stdin cannot be combined with lint paths"));
        }
        let path = normalize_relative(&input.filename)?;
        if !config.includes(&path) {
            return Err(failure(format!("Lint stdin file is ignored: {path}")));
        }
        let task = FileTask {
            baseline: baseline.as_ref().map(|session| session.file(&path)),
            cache: cache.as_ref().map(cache::LintCacheSession::fork),
            path,
            text: input.text,
            config: config.clone(),
            fix: options.fix,
            snapshot: None,
        };
        let results = if modules::needs_project(&config, &task.path) {
            let batch = scheduler.admit(cancellation)?;
            modules::analyze(
                vec![task],
                &batch,
                cancellation,
                modules::Project {
                    root: &root,
                    documents,
                    resolve: &resolve,
                    dependencies,
                },
            )?
        } else {
            scheduler.run(vec![move || task.run()], cancellation)?
        };
        merge_files(
            results,
            &mut files,
            &mut pending,
            cache.as_mut(),
            baseline.as_mut(),
        )?;
    } else {
        let mut selectors = Vec::new();
        for input in &options.paths {
            let relative = normalize_relative(input)?;
            if relative.contains(['*', '?']) {
                selectors.push(Selection::Glob(glob(&relative)?));
            } else if documents.contains_key(&relative) {
                selectors.push(Selection::File(relative));
            } else {
                let path = root.join(&relative);
                let metadata =
                    std::fs::symlink_metadata(&path).map_err(|error| io_error(&path, error))?;
                if metadata.file_type().is_symlink() {
                    return Err(failure(format!(
                        "Lint does not follow symlinks: {relative}"
                    )));
                }
                selectors.push(if metadata.is_dir() {
                    Selection::Directory(relative)
                } else {
                    Selection::File(relative)
                });
            }
        }
        let mut candidates = Vec::new();
        discover(&root, &root, &mut candidates, cancellation)?;
        let disk_paths: HashSet<_> = candidates.iter().map(|(path, _)| path.clone()).collect();
        for path in documents.keys() {
            if !disk_paths.contains(path) {
                candidates.push((path.clone(), root.join(path)));
            }
        }
        candidates.sort_by(|a, b| a.0.cmp(&b.0));
        let module_analysis = candidates.iter().any(|(relative, _)| {
            config.includes(relative)
                && (selectors.is_empty()
                    || selectors.iter().any(|selector| selector.matches(relative)))
                && modules::needs_project(&config, relative)
        });
        let mut matched = vec![false; selectors.len()];
        let mut identities = HashSet::new();
        let mut tasks = Vec::new();
        let mut batch = None;
        let mut selected_bytes = 0usize;
        for (relative, path) in candidates {
            cancellation.check()?;
            if !config.includes(&relative) {
                continue;
            }
            let mut selected = selectors.is_empty();
            for (index, selector) in selectors.iter().enumerate() {
                if selector.matches(&relative) {
                    matched[index] = true;
                    selected = true;
                }
            }
            if !selected {
                continue;
            }
            let document = documents.get(&relative);
            if document.is_none() {
                let identity =
                    same_file::Handle::from_path(&path).map_err(|error| io_error(&path, error))?;
                if !identities.insert(identity) {
                    continue;
                }
            }
            if batch.is_none() {
                batch = Some(scheduler.admit(cancellation)?);
            }
            if module_analysis && tasks.len() >= wake_lint_core::ModuleGraph::MAX_FILES {
                return Err(WakeError::new(
                    "WAKE_LINT_ANALYSIS",
                    "Module selected file budget exceeded",
                ));
            }
            let snapshot = if options.fix == LintFixMode::Write {
                Some(LintSourceSnapshot::read(&path)?)
            } else {
                None
            };
            let text = match (document, &snapshot) {
                (Some(document), _) => document.text.clone(),
                (_, Some(snapshot)) => snapshot.text.clone(),
                _ => std::fs::read_to_string(&path).map_err(|error| io_error(&path, error))?,
            };
            if module_analysis {
                if text.len()
                    > wake_lint_core::ModuleGraph::MAX_SOURCE_BYTES.saturating_sub(selected_bytes)
                {
                    return Err(WakeError::new(
                        "WAKE_LINT_ANALYSIS",
                        "Module selected source byte budget exceeded",
                    ));
                }
                selected_bytes += text.len();
            }
            let file_cache = if document.is_some() {
                if let Some(cache) = cache.as_mut() {
                    cache.bypass();
                }
                None
            } else {
                cache.as_ref().map(cache::LintCacheSession::fork)
            };
            tasks.push(FileTask {
                baseline: baseline.as_ref().map(|session| session.file(&relative)),
                cache: file_cache,
                path: relative,
                text,
                config: config.clone(),
                fix: options.fix,
                snapshot,
            });
            if !module_analysis && tasks.len() == scheduler.batch_size() {
                let jobs = std::mem::take(&mut tasks)
                    .into_iter()
                    .map(|task| move || task.run())
                    .collect();
                let results = batch
                    .take()
                    .expect("prepared files hold admission")
                    .run(jobs, cancellation)?;
                merge_files(
                    results,
                    &mut files,
                    &mut pending,
                    cache.as_mut(),
                    baseline.as_mut(),
                )?;
            }
        }
        if let Some(index) = matched.iter().position(|matched| !matched) {
            return Err(failure(format!(
                "Lint path is ignored or matches no included files: {}",
                options.paths[index]
            )));
        }
        if let Some(batch) = batch {
            let results = if module_analysis {
                modules::analyze(
                    tasks,
                    &batch,
                    cancellation,
                    modules::Project {
                        root: &root,
                        documents,
                        resolve: &resolve,
                        dependencies,
                    },
                )?
            } else {
                let jobs = tasks.into_iter().map(|task| move || task.run()).collect();
                batch.run(jobs, cancellation)?
            };
            merge_files(
                results,
                &mut files,
                &mut pending,
                cache.as_mut(),
                baseline.as_mut(),
            )?;
        }
    }
    cancellation.check()?;
    for (index, snapshot) in pending {
        cancellation.commit(|| {
            replace_lint_source(
                snapshot,
                files[index]
                    .output
                    .as_deref()
                    .expect("changed file has final source"),
            )
        })?;
        files[index].written = true;
    }
    let error_count = files
        .iter()
        .flat_map(|file| &file.diagnostics)
        .filter(|diagnostic| diagnostic.diagnostic.severity == "error")
        .count();
    let warning_count = files
        .iter()
        .flat_map(|file| &file.diagnostics)
        .filter(|diagnostic| diagnostic.diagnostic.severity == "warning")
        .count();
    let exit_code = u8::from(
        error_count > 0
            || options
                .max_warnings
                .is_some_and(|limit| warning_count > limit),
    );
    let cache = cache.map(|cache| cache.finish(cancellation)).transpose()?;
    let baseline = baseline
        .map(|session| session.finish(&config, cancellation))
        .transpose()?;
    Ok(LintProjectResult {
        files,
        error_count,
        warning_count,
        exit_code,
        config: None,
        catalog: None,
        cache,
        baseline,
    })
}
