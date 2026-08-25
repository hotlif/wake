use std::path::Path;

use regex::Regex;

use super::{
    CoverageMetric, CoverageMetrics, CoverageResult, DiagnosticSeverity, TestDiagnostic, TestError,
    normalize_path, wake_glob_regex,
};

pub(super) fn evaluate(
    root: &Path,
    config: &wake_config::TestCoverage,
    coverage: &CoverageResult,
) -> Result<Vec<TestDiagnostic>, TestError> {
    let mut diagnostics = Vec::new();
    check_metrics(
        "global",
        Some(normalize_path(root)),
        config.threshold.lines,
        config.threshold.functions,
        config.threshold.blocks,
        &coverage.summary,
        &mut diagnostics,
    );

    for configured in &config.per_file {
        let expression = wake_glob_regex(&configured.pattern)?;
        let matcher = Regex::new(&expression).map_err(|error| {
            TestError::new(
                "WAKE_TEST_CONFIG",
                format!(
                    "invalid coverage per-file pattern {:?}: {error}",
                    configured.pattern
                ),
            )
        })?;
        for file in &coverage.files {
            if !matcher.is_match(&file.path) {
                continue;
            }
            check_metrics(
                &file.path,
                Some(file.path.clone()),
                configured.lines,
                configured.functions,
                configured.blocks,
                &file.metrics,
                &mut diagnostics,
            );
        }
    }
    Ok(diagnostics)
}

#[allow(clippy::too_many_arguments)]
fn check_metrics(
    scope: &str,
    path: Option<String>,
    lines: Option<f64>,
    functions: Option<f64>,
    blocks: Option<f64>,
    actual: &CoverageMetrics,
    diagnostics: &mut Vec<TestDiagnostic>,
) {
    for (name, required, metric) in [
        ("lines", lines, &actual.lines),
        ("functions", functions, &actual.functions),
        ("blocks", blocks, &actual.blocks),
    ] {
        let Some(required) = required else {
            continue;
        };
        if passes(metric, required) {
            continue;
        }
        diagnostics.push(TestDiagnostic {
            severity: DiagnosticSeverity::Error,
            code: "WAKE_TEST_COVERAGE".to_string(),
            message: format!(
                "{scope} {name} coverage {:.2}% is below the configured {:.2}% threshold",
                metric.percent, required
            ),
            path: path.clone(),
            location: None,
            notes: vec![format!(
                "covered {} of {} measured {name}",
                metric.covered, metric.total
            )],
        });
    }
}

fn passes(metric: &CoverageMetric, required: f64) -> bool {
    metric.percent + f64::EPSILON >= required
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CoverageFile;

    fn metric(covered: usize, total: usize) -> CoverageMetric {
        CoverageMetric {
            covered,
            total,
            percent: if total == 0 {
                100.0
            } else {
                covered as f64 * 100.0 / total as f64
            },
        }
    }

    fn metrics(lines: (usize, usize), functions: (usize, usize)) -> CoverageMetrics {
        CoverageMetrics {
            lines: metric(lines.0, lines.1),
            functions: metric(functions.0, functions.1),
            blocks: metric(1, 1),
        }
    }

    #[test]
    fn reports_global_and_matching_file_thresholds() {
        let config = wake_config::TestCoverage {
            threshold: wake_config::TestCoverageThreshold {
                lines: Some(80.0),
                ..wake_config::TestCoverageThreshold::default()
            },
            per_file: vec![wake_config::TestCoverageFileThreshold {
                pattern: "src/**".to_string(),
                lines: None,
                functions: Some(90.0),
                blocks: None,
            }],
            ..wake_config::TestCoverage::default()
        };
        let coverage = CoverageResult {
            summary: metrics((3, 4), (2, 2)),
            files: vec![
                CoverageFile {
                    path: "src/component.tsx".to_string(),
                    metrics: metrics((3, 4), (4, 5)),
                },
                CoverageFile {
                    path: "vendor/ignored.ts".to_string(),
                    metrics: metrics((0, 1), (0, 1)),
                },
            ],
            report_artifact_ids: Vec::new(),
        };

        let diagnostics = evaluate(Path::new("project"), &config, &coverage).unwrap();
        assert_eq!(diagnostics.len(), 2, "{diagnostics:#?}");
        assert!(
            diagnostics[0]
                .message
                .contains("global lines coverage 75.00%")
        );
        assert!(
            diagnostics[1]
                .message
                .contains("src/component.tsx functions")
        );
        assert_eq!(diagnostics[1].path.as_deref(), Some("src/component.tsx"));
    }

    #[test]
    fn ignores_unconfigured_metrics_and_nonmatching_files() {
        let config = wake_config::TestCoverage {
            per_file: vec![wake_config::TestCoverageFileThreshold {
                pattern: "src/**".to_string(),
                lines: None,
                functions: None,
                blocks: Some(100.0),
            }],
            ..wake_config::TestCoverage::default()
        };
        let coverage = CoverageResult {
            summary: metrics((0, 1), (0, 1)),
            files: vec![CoverageFile {
                path: "vendor/value.ts".to_string(),
                metrics: metrics((0, 1), (0, 1)),
            }],
            report_artifact_ids: Vec::new(),
        };

        assert!(
            evaluate(Path::new("project"), &config, &coverage)
                .unwrap()
                .is_empty()
        );
    }
}
