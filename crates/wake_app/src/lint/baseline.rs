//! Reviewed baseline inputs and operations. Fingerprints belong to core; publication to output.
use super::{ConfiguredLint, LintFixMode, LintProjectOptions, failure, normalize_relative};
use crate::output::{
    LintNewFileSnapshot, LintSourceSnapshot, create_lint_file, replace_lint_source,
};
use crate::{CancellationToken, WakeError};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use wake_lint_core::{BaselineEntry, BaselineMatch, LintResult};

const SCHEMA: &str = "wake.lint.baseline.v1";
const MAX_BYTES: usize = 4 * 1024 * 1024;
const MAX_ENTRIES: usize = 20_000;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum LintBaselineMode {
    #[default]
    Check,
    Generate,
    Prune,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LintBaselineOptions {
    pub path: String,
    #[serde(default)]
    pub mode: LintBaselineMode,
}

#[derive(Debug, Default, Serialize)]
pub struct LintBaselineStats {
    pub path: String,
    pub suppressed: usize,
    pub stale: usize,
    pub ambiguous: usize,
    pub entries: usize,
    pub written: bool,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Document {
    schema: String,
    entries: Vec<Entry>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Entry {
    path: String,
    rule_id: String,
    message_id: String,
    fingerprint: String,
}

enum OutputSnapshot {
    Existing(LintSourceSnapshot),
    New(LintNewFileSnapshot),
}

pub(super) struct BaselineSession {
    root: PathBuf,
    mode: LintBaselineMode,
    output: OutputSnapshot,
    entries: BTreeMap<String, BTreeSet<BaselineEntry>>,
    inspected: BTreeMap<String, BTreeSet<BaselineEntry>>,
    stats: LintBaselineStats,
    full_project: bool,
}

pub(super) fn validate_request(options: &LintProjectOptions) -> Result<(), WakeError> {
    let Some(baseline) = &options.baseline else {
        return Ok(());
    };
    if options.list_rules || options.print_config.is_some() {
        return Err(failure(
            "baseline cannot be combined with listRules or printConfig",
        ));
    }
    if baseline.mode != LintBaselineMode::Check
        && (options.stdin.is_some()
            || options.fix != LintFixMode::Off
            || options.max_warnings.is_some())
    {
        return Err(failure(
            "baseline generation/pruning cannot be combined with stdin, fixes or maxWarnings",
        ));
    }
    if baseline.mode == LintBaselineMode::Prune && !options.paths.is_empty() {
        return Err(failure(
            "baseline pruning requires the complete project without path selection",
        ));
    }
    Ok(())
}

impl BaselineSession {
    pub fn new(
        root: &Path,
        options: LintBaselineOptions,
        full_project: bool,
    ) -> Result<Self, WakeError> {
        let relative = normalize_relative(&options.path)?;
        if !relative.ends_with(".json") {
            return Err(failure("baseline requires a .json filename"));
        }
        let path = root.join(&relative);
        let mut current = root.to_path_buf();
        let parts: Vec<_> = relative.split('/').collect();
        for part in &parts[..parts.len() - 1] {
            current.push(part);
            if !std::fs::symlink_metadata(&current).is_ok_and(|m| m.file_type().is_dir()) {
                return Err(failure(
                    "baseline parents must be existing ordinary directories",
                ));
            }
        }
        let (output, document) = match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_file() => {
                let snapshot = LintSourceSnapshot::read_bounded(&path, MAX_BYTES)?;
                let document = serde_json::from_str::<Document>(&snapshot.text)
                    .map_err(|e| failure(format!("Invalid lint baseline: {e}")))?;
                (OutputSnapshot::Existing(snapshot), document)
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound
                    && options.mode == LintBaselineMode::Generate =>
            {
                (
                    OutputSnapshot::New(LintNewFileSnapshot::read(&path)?),
                    Document {
                        schema: SCHEMA.into(),
                        entries: Vec::new(),
                    },
                )
            }
            _ => {
                return Err(failure(
                    "baseline must be an existing ordinary file (generate can create a missing file)",
                ));
            }
        };
        if document.schema != SCHEMA || document.entries.len() > MAX_ENTRIES {
            return Err(failure("Unsupported or oversized lint baseline"));
        }
        let mut entries: BTreeMap<String, BTreeSet<BaselineEntry>> = BTreeMap::new();
        for entry in document.entries {
            if normalize_relative(&entry.path)? != entry.path
                || super::source_type(&entry.path).is_err()
            {
                return Err(failure(
                    "baseline entry requires a canonical relative source path",
                ));
            }
            let key = BaselineEntry {
                rule_id: entry.rule_id,
                message_id: entry.message_id,
                fingerprint: entry.fingerprint,
            };
            wake_lint_core::validate_baseline(std::slice::from_ref(&key))
                .map_err(|e| failure(e.to_string()))?;
            if !entries.entry(entry.path).or_default().insert(key) {
                return Err(failure("Duplicate lint baseline entry"));
            }
        }
        Ok(Self {
            root: root.into(),
            mode: options.mode,
            output,
            entries,
            inspected: BTreeMap::new(),
            stats: LintBaselineStats {
                path: relative,
                ..Default::default()
            },
            full_project,
        })
    }

    pub fn file(&self, path: &str) -> FileBaseline {
        FileBaseline {
            mode: self.mode,
            entries: self.entries.get(path).cloned().unwrap_or_default(),
            matched: BaselineMatch::default(),
            parsed: false,
        }
    }

    pub fn merge(&mut self, path: &str, file: FileBaseline) -> Result<(), WakeError> {
        self.stats.suppressed += file.matched.suppressed;
        self.stats.ambiguous += file.matched.ambiguous;
        if file.parsed {
            self.inspected
                .insert(path.into(), file.matched.matched.into_iter().collect());
            if self.mode == LintBaselineMode::Generate {
                self.entries.insert(path.into(), file.entries);
                if self.entries.values().map(BTreeSet::len).sum::<usize>() > MAX_ENTRIES {
                    return Err(failure("Generated lint baseline exceeds entry limit"));
                }
            }
        }
        Ok(())
    }

    pub fn finish(
        mut self,
        config: &ConfiguredLint,
        cancellation: &CancellationToken,
    ) -> Result<LintBaselineStats, WakeError> {
        cancellation.check()?;
        for (path, entries) in &mut self.entries {
            let matched = self.inspected.get(path);
            let deleted = self.full_project
                && config.includes(path)
                && std::fs::symlink_metadata(self.root.join(path))
                    .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound);
            if matched.is_some() || deleted {
                let stale: Vec<_> = entries
                    .iter()
                    .filter(|entry| matched.is_none_or(|set| !set.contains(*entry)))
                    .cloned()
                    .collect();
                self.stats.stale += stale.len();
                if self.mode == LintBaselineMode::Prune {
                    for entry in stale {
                        entries.remove(&entry);
                    }
                }
            }
        }
        self.stats.entries = self.entries.values().map(BTreeSet::len).sum();
        if self.mode != LintBaselineMode::Check {
            let entries = self
                .entries
                .into_iter()
                .flat_map(|(path, entries)| {
                    entries.into_iter().map(move |entry| Entry {
                        path: path.clone(),
                        rule_id: entry.rule_id,
                        message_id: entry.message_id,
                        fingerprint: entry.fingerprint,
                    })
                })
                .collect();
            let mut text = serde_json::to_string_pretty(&Document {
                schema: SCHEMA.into(),
                entries,
            })
            .map_err(|e| failure(e.to_string()))?;
            text.push('\n');
            if text.len() > MAX_BYTES {
                return Err(failure("Generated lint baseline exceeds byte limit"));
            }
            match self.output {
                OutputSnapshot::Existing(snapshot) if snapshot.text == text => {}
                OutputSnapshot::Existing(snapshot) => {
                    cancellation.commit(|| replace_lint_source(snapshot, &text))?;
                    self.stats.written = true;
                }
                OutputSnapshot::New(snapshot) => {
                    cancellation.commit(|| create_lint_file(snapshot, &text))?;
                    self.stats.written = true;
                }
            }
        }
        Ok(self.stats)
    }
}

/// Owned single-file baseline state. It has no filesystem/publication capability.
pub(super) struct FileBaseline {
    mode: LintBaselineMode,
    entries: BTreeSet<BaselineEntry>,
    matched: BaselineMatch,
    parsed: bool,
}

impl FileBaseline {
    pub fn entries(&self) -> Vec<BaselineEntry> {
        self.entries.iter().cloned().collect()
    }

    pub fn apply(&mut self, source: &str, result: &mut LintResult) -> Result<(), WakeError> {
        if self.mode == LintBaselineMode::Generate && result.parse_diagnostics.is_empty() {
            let candidates = wake_lint_core::baseline_candidates(source, &result.diagnostics)
                .map_err(|e| failure(e.to_string()))?;
            self.entries.extend(candidates.entries);
            if self.entries.len() > MAX_ENTRIES {
                return Err(failure("Generated lint baseline exceeds entry limit"));
            }
        }
        let matched =
            wake_lint_core::apply_baseline(source, &mut result.diagnostics, &self.entries())
                .map_err(|e| failure(e.to_string()))?;
        self.record(result, matched);
        Ok(())
    }

    pub fn record(&mut self, result: &LintResult, matched: BaselineMatch) {
        self.parsed = result.parse_diagnostics.is_empty();
        self.matched = matched;
    }
}
