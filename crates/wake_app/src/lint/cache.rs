//! Product-owned lint identity and cache DTO; storage/publication belongs to wake_cache.
use crate::{CancellationToken, WakeError};
use ring::digest::{Context, SHA256};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::PathBuf;
use wake_cache::{BlobCache, BlobLoadOutcome};
use wake_lint_core::{LintDiagnostic, LintOptions, LintResult, SourceType};

const MAX_VALUE: usize = 4 * 1024 * 1024;
const MAX_PENDING: usize = 256 * 1024 * 1024;

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LintCacheStats {
    pub hits: usize,
    pub misses: usize,
    pub writes: usize,
    pub bypassed: usize,
    pub warnings: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    schema: u32,
    diagnostics: Vec<LintDiagnostic>,
}

#[derive(Serialize)]
struct RecordView<'a> {
    schema: u32,
    diagnostics: &'a [LintDiagnostic],
}

pub(super) struct LintCacheSession {
    storage: BlobCache,
    path: PathBuf,
    eligible: bool,
    pending: Vec<([u8; 32], Vec<u8>)>,
    pending_bytes: usize,
    stats: LintCacheStats,
}

impl LintCacheSession {
    pub fn new(path: PathBuf, eligible: bool) -> Self {
        Self {
            storage: BlobCache::new(path.clone()),
            path,
            eligible,
            pending: Vec::new(),
            pending_bytes: 0,
            stats: LintCacheStats::default(),
        }
    }

    pub fn bypass(&mut self) {
        self.stats.bypassed += 1;
    }

    pub fn fork(&self) -> Self {
        Self::new(self.path.clone(), self.eligible)
    }

    /// Merge in canonical path order. Only the project session is allowed to publish.
    pub fn merge(&mut self, file: Self) {
        self.stats.hits += file.stats.hits;
        self.stats.misses += file.stats.misses;
        self.stats.bypassed += file.stats.bypassed;
        for warning in file.stats.warnings {
            let message = warning
                .strip_prefix("Lint cache unavailable or invalid; checking source normally: ")
                .unwrap_or(&warning);
            self.warn(message);
        }
        for (key, bytes) in file.pending {
            if self.pending.len() < 20_000 && self.pending_bytes + bytes.len() + 64 <= MAX_PENDING {
                self.pending_bytes += bytes.len() + 64;
                self.pending.push((key, bytes));
            }
        }
    }

    fn warn(&mut self, message: impl Into<String>) {
        let message: String = message.into().chars().take(512).collect();
        if self.stats.warnings.is_empty() {
            self.stats.warnings.push(format!(
                "Lint cache unavailable or invalid; checking source normally: {message}"
            ));
        } else if self.stats.warnings[0].chars().count() < 1536
            && !self.stats.warnings[0].contains(&message)
        {
            self.stats.warnings[0].push_str("; ");
            self.stats.warnings[0].push_str(&message);
        }
    }

    pub fn check(
        &mut self,
        path: &str,
        source: &str,
        language: SourceType,
        options: &LintOptions,
        baseline: &[wake_lint_core::BaselineEntry],
    ) -> Result<LintResult, WakeError> {
        let fresh = || {
            wake_lint_core::lint_text(source, language, options)
                .map_err(|error| super::analysis_failure(error, path, false))
        };
        if !self.eligible || source.len() > MAX_VALUE {
            self.bypass();
            return fresh();
        }
        let key = content_key_with_baseline(path, source, language, options, baseline)?;
        match self.storage.load(&key) {
            BlobLoadOutcome::Loaded(bytes) => match serde_json::from_slice::<Record>(&bytes) {
                Ok(record)
                    if record.schema == 1
                        && wake_lint_core::validate_cached_diagnostics(
                            source,
                            options,
                            &record.diagnostics,
                        )
                        .is_ok() =>
                {
                    self.stats.hits += 1;
                    return Ok(LintResult {
                        diagnostics: record.diagnostics,
                        parse_diagnostics: Vec::new(),
                    });
                }
                _ => self.warn("cached diagnostic schema or ranges failed validation"),
            },
            BlobLoadOutcome::Corrupt(error) => self.warn(error.to_string()),
            BlobLoadOutcome::Io(error) => self.warn(error.to_string()),
            BlobLoadOutcome::Missing | BlobLoadOutcome::Incompatible { .. } => {}
        }
        self.stats.misses += 1;
        let result = fresh()?;
        if result.parse_diagnostics.is_empty() {
            let mut buffer = LimitedBuffer(Vec::new());
            if serde_json::to_writer(
                &mut buffer,
                &RecordView {
                    schema: 1,
                    diagnostics: &result.diagnostics,
                },
            )
            .is_ok()
                && self.pending.len() < 20_000
                && self.pending_bytes + buffer.0.len() + 64 <= MAX_PENDING
            {
                self.pending_bytes += buffer.0.len() + 64;
                self.pending.push((key, buffer.0));
            }
        }
        Ok(result)
    }

    pub fn finish(mut self, cancellation: &CancellationToken) -> Result<LintCacheStats, WakeError> {
        cancellation.check()?;
        if !self.pending.is_empty() {
            match cancellation.commit(|| Ok(self.storage.store(&self.pending)))? {
                Ok(report) => {
                    self.stats.writes = report.written;
                    if report.conflicts > 0 {
                        self.warn("conflicting immutable diagnostic entries were discarded");
                    }
                    if report.repaired > 0 {
                        self.warn("corrupt cache entries were replaced");
                    }
                }
                Err(error) => self.warn(error.to_string()),
            }
        }
        Ok(self.stats)
    }
}

#[cfg(test)]
fn content_key(
    path: &str,
    source: &str,
    language: SourceType,
    options: &LintOptions,
) -> Result<[u8; 32], WakeError> {
    content_key_with_baseline(path, source, language, options, &[])
}

fn content_key_with_baseline(
    path: &str,
    source: &str,
    language: SourceType,
    options: &LintOptions,
    baseline: &[wake_lint_core::BaselineEntry],
) -> Result<[u8; 32], WakeError> {
    let rules: BTreeMap<_, _> = wake_lint_core::effective_configuration(options)
        .map_err(|error| super::failure(error.to_string()))?
        .into_iter()
        .map(|(id, rule)| (id, rule.configuration))
        .collect();
    let globals: BTreeMap<_, _> = wake_lint_core::effective_globals(options)
        .map_err(|error| super::failure(error.to_string()))?
        .into_iter()
        .filter(|(_, value)| value.mode != wake_lint_core::GlobalMode::Off)
        .map(|(name, value)| (name, value.mode))
        .collect();
    let metadata = serde_json::to_vec(&serde_json::json!({
        "schema":1, "core":wake_lint_core::PIPELINE_VERSION, "parser":wake_lint_core::PARSER_PIPELINE_VERSION,
        "path":path, "language":format!("{language:?}"), "rules":rules, "globals":globals, "unused":options.report_unused_disable, "baseline":baseline,
    })).map_err(|error| super::failure(error.to_string()))?;
    let mut hash = Context::new(&SHA256);
    hash.update(&(metadata.len() as u64).to_le_bytes());
    hash.update(&metadata);
    hash.update(source.as_bytes());
    Ok(hash
        .finish()
        .as_ref()
        .try_into()
        .expect("SHA-256 has 32 bytes"))
}

struct LimitedBuffer(Vec<u8>);
impl Write for LimitedBuffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_VALUE.saturating_sub(self.0.len()) {
            return Err(io::Error::other("lint cache value exceeds budget"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globals_identity_uses_active_modes_without_provenance_or_redundant_off_entries() {
        use wake_lint_core::GlobalMode::{Off, Readonly, Writable};
        let mut options = LintOptions::default();
        let key = |options: &LintOptions| {
            content_key("a.js", "Promise; injected;", SourceType::Module, options).unwrap()
        };
        let standard = key(&options);
        options.globals.insert("Promise".into(), Readonly);
        options.globals.insert("absent".into(), Off);
        assert_eq!(standard, key(&options));
        options.globals.insert("Promise".into(), Off);
        assert_ne!(standard, key(&options));
        let removed = key(&options);
        options.globals.insert("injected".into(), Readonly);
        assert_ne!(removed, key(&options));
        let readonly = key(&options);
        options.globals.insert("injected".into(), Writable);
        assert_ne!(readonly, key(&options));
    }

    #[test]
    fn environment_sets_change_cache_identity() {
        let key = |options: &LintOptions| {
            content_key("a.js", "window; process;", SourceType::Module, options).unwrap()
        };
        let standard = key(&LintOptions::default());
        let browser = key(&LintOptions {
            environments: vec!["browser".into()],
            ..Default::default()
        });
        let node = key(&LintOptions {
            environments: vec!["node".into()],
            ..Default::default()
        });
        assert_ne!(standard, browser);
        assert_ne!(standard, node);
        assert_ne!(browser, node);
    }

    #[test]
    fn keys_use_effective_settings_and_distinguish_paths_languages_and_source() {
        let default = LintOptions::default();
        let explicit = LintOptions {
            recommended: false,
            presets: vec!["recommended@1".into()],
            ..Default::default()
        };
        let key = content_key("a.js", "x == null;", SourceType::Module, &default).unwrap();
        assert_eq!(
            key,
            content_key("a.js", "x == null;", SourceType::Module, &explicit).unwrap()
        );
        assert_ne!(
            key,
            content_key("b.js", "x == null;", SourceType::Module, &default).unwrap()
        );
        assert_ne!(
            key,
            content_key("a.js", "x == null;", SourceType::Script, &default).unwrap()
        );
        assert_ne!(
            key,
            content_key("a.js", "x == zero;", SourceType::Module, &default).unwrap()
        );
        let mut changed = default;
        changed.report_unused_disable = wake_lint_core::RuleLevel::Off;
        assert_ne!(
            key,
            content_key("a.js", "x == null;", SourceType::Module, &changed).unwrap()
        );
    }

    #[test]
    fn valid_blob_checksums_do_not_bypass_diagnostic_schema_validation() {
        let dir = tempfile::tempdir().unwrap();
        let options = LintOptions::default();
        let source = "debugger;";
        let key = content_key("a.js", source, SourceType::Module, &options).unwrap();
        let mut record = serde_json::json!({"schema":1,"diagnostics":wake_lint_core::lint_text(source, SourceType::Module, &options).unwrap().diagnostics});
        record["diagnostics"][0]["end"] = 999.into();
        BlobCache::new(dir.path().to_owned())
            .store(&[(key, serde_json::to_vec(&record).unwrap())])
            .unwrap();
        let mut session = LintCacheSession::new(dir.path().to_owned(), true);
        let result = session
            .check("a.js", source, SourceType::Module, &options, &[])
            .unwrap();
        assert_eq!(result.diagnostics[0].end, source.len() as u32);
        assert_eq!(session.stats.hits, 0);
        assert_eq!(session.stats.misses, 1);
        assert_eq!(session.stats.warnings.len(), 1);
    }
}
