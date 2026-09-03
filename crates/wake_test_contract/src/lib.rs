//! Stable serializable models and the versioned host protocol for Wake Test.
//!
//! This crate owns no discovery, execution, filesystem, browser, VM, or product lifecycle.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub mod protocol;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct TestOptions {
    pub root: Option<PathBuf>,
    pub patterns: Vec<String>,
    pub name_pattern: Option<String>,
    pub projects: Vec<String>,
    pub environment: Option<String>,
    pub watch: bool,
    pub changed: bool,
    pub related: Vec<PathBuf>,
    pub coverage: bool,
    pub update_snapshots: Option<String>,
    pub serial: bool,
    pub workers: Option<WorkerOverride>,
    pub bail: Option<u32>,
    pub shard: Option<String>,
    pub seed: Option<String>,
    pub shuffle: bool,
    pub reporter: Option<String>,
    pub output: Option<PathBuf>,
    pub allow_no_tests: bool,
    pub browser_path: Option<PathBuf>,
    pub headful: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum WorkerOverride {
    Count(usize),
    Text(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestLocation {
    pub path: String,
    pub line: usize,
    pub column: usize,
    pub end_line: Option<usize>,
    pub end_column: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestDiff {
    pub expected: Option<String>,
    pub received: Option<String>,
    pub unified: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestFailure {
    pub message: String,
    pub code: Option<String>,
    pub stack: Option<String>,
    pub location: Option<TestLocation>,
    pub diff: Option<TestDiff>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TestStatus {
    Passed,
    Failed,
    Skipped,
    Todo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TestSuiteStatus {
    Passed,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestCaseResult {
    pub id: String,
    pub name: String,
    pub full_name: String,
    pub status: TestStatus,
    pub duration_ms: u64,
    pub assertions: usize,
    pub attempts: usize,
    pub location: Option<TestLocation>,
    pub failures: Vec<TestFailure>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SnapshotSummary {
    pub added: usize,
    pub matched: usize,
    pub unmatched: usize,
    pub updated: usize,
    pub obsolete: usize,
    pub files_removed: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CoverageMetric {
    pub covered: usize,
    pub total: usize,
    pub percent: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CoverageMetrics {
    pub lines: CoverageMetric,
    pub functions: CoverageMetric,
    pub blocks: CoverageMetric,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CoverageFile {
    pub path: String,
    #[serde(flatten)]
    pub metrics: CoverageMetrics,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CoverageResult {
    pub summary: CoverageMetrics,
    pub files: Vec<CoverageFile>,
    pub report_artifact_ids: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiagnosticSeverity {
    Error,
    Warning,
    Note,
    Help,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestDiagnostic {
    pub severity: DiagnosticSeverity,
    pub code: String,
    pub message: String,
    pub path: Option<String>,
    pub location: Option<TestLocation>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserEnvironmentInfo {
    pub name: String,
    pub version: String,
    pub headless: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TestEnvironmentKind {
    Dom,
    Browser,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestEnvironmentInfo {
    pub kind: TestEnvironmentKind,
    pub react: Option<String>,
    pub react_dom: Option<String>,
    pub v8: String,
    pub browser: Option<BrowserEnvironmentInfo>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TestLeakKind {
    Timer,
    Listener,
    Task,
    Socket,
    Network,
    Other,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestArtifact {
    pub id: String,
    pub kind: String,
    pub path: String,
    pub suite_id: Option<String>,
    pub test_id: Option<String>,
    pub metadata: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestLeak {
    pub kind: TestLeakKind,
    pub description: String,
    pub location: Option<TestLocation>,
    pub stack: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestSuiteResult {
    pub id: String,
    pub path: String,
    pub name: Option<String>,
    pub project: Option<String>,
    pub environment: Option<TestEnvironmentInfo>,
    pub status: TestSuiteStatus,
    pub duration_ms: u64,
    pub tests: Vec<TestCaseResult>,
    pub failures: Vec<TestFailure>,
    pub snapshot: Option<SnapshotSummary>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TestStatusCounts {
    pub total: usize,
    pub passed: usize,
    pub failed: usize,
    pub skipped: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TestCaseStatusCounts {
    pub total: usize,
    pub passed: usize,
    pub failed: usize,
    pub skipped: usize,
    pub todo: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TestRunCounts {
    pub suites: TestStatusCounts,
    pub tests: TestCaseStatusCounts,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TestTerminationReason {
    Completed,
    Cancelled,
    Bail,
    WatchRestart,
    HostCrash,
    Timeout,
    Oom,
    InternalError,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestRunResult {
    pub schema_version: String,
    pub run_id: String,
    pub success: bool,
    pub seed: String,
    pub duration_ms: u64,
    pub termination_reason: TestTerminationReason,
    pub environment: TestEnvironmentInfo,
    pub suites: Vec<TestSuiteResult>,
    pub counts: TestRunCounts,
    pub snapshot: SnapshotSummary,
    pub coverage: Option<CoverageResult>,
    pub leaks: Vec<TestLeak>,
    pub artifacts: Vec<TestArtifact>,
    pub diagnostics: Vec<TestDiagnostic>,
}

impl TestRunResult {
    pub fn empty(run_id: String, seed: String, environment: TestEnvironmentInfo) -> Self {
        Self {
            schema_version: "wake.test.v1".to_string(),
            run_id,
            success: true,
            seed,
            duration_ms: 0,
            termination_reason: TestTerminationReason::Completed,
            environment,
            suites: Vec::new(),
            counts: TestRunCounts::default(),
            snapshot: SnapshotSummary::default(),
            coverage: None,
            leaks: Vec::new(),
            artifacts: Vec::new(),
            diagnostics: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn result_v1_uses_the_stable_camel_case_shape() {
        let result = TestRunResult::empty(
            "run-1".to_string(),
            "seed-1".to_string(),
            TestEnvironmentInfo {
                kind: TestEnvironmentKind::Dom,
                react: Some("19.2.8".to_string()),
                react_dom: Some("19.2.8".to_string()),
                v8: "150.4.0".to_string(),
                browser: None,
            },
        );
        let value = serde_json::to_value(result).unwrap();
        assert_eq!(value["schemaVersion"], "wake.test.v1");
        assert_eq!(value["runId"], "run-1");
        assert_eq!(value["seed"], "seed-1");
        assert_eq!(value["environment"]["reactDom"], "19.2.8");
        assert!(value.get("schema_version").is_none());
        assert!(value.get("run_id").is_none());
    }

    #[test]
    fn result_enums_match_the_public_typescript_sets() {
        assert_eq!(
            serde_json::to_value(TestSuiteStatus::Skipped).unwrap(),
            "skipped"
        );
        assert_eq!(
            serde_json::to_value(TestEnvironmentKind::Browser).unwrap(),
            "browser"
        );
        assert_eq!(
            serde_json::to_value(TestLeakKind::Network).unwrap(),
            "network"
        );
        assert!(serde_json::from_str::<TestSuiteStatus>(r#""todo""#).is_err());
        assert!(serde_json::from_str::<TestEnvironmentKind>(r#""node""#).is_err());
        assert!(serde_json::from_str::<TestLeakKind>(r#""handle""#).is_err());
    }

    #[test]
    fn options_reject_unknown_fields_without_a_legacy_shape() {
        let options: TestOptions = serde_json::from_value(serde_json::json!({
            "namePattern": "button",
            "updateSnapshots": "all",
            "allowNoTests": true
        }))
        .unwrap();
        assert_eq!(options.name_pattern.as_deref(), Some("button"));
        assert_eq!(options.update_snapshots.as_deref(), Some("all"));
        assert!(options.allow_no_tests);
        assert!(
            serde_json::from_value::<TestOptions>(serde_json::json!({
                "name_pattern": "legacy"
            }))
            .is_err()
        );
    }
}
