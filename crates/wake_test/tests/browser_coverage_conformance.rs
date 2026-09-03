use std::fs;
use std::path::Path;

use wake_test::{CoverageResult, TestOptions, run_tests};

fn repository_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("wake_test is located under <repository>/crates")
}

fn browser_options(root: &Path) -> TestOptions {
    TestOptions {
        root: Some(root.to_path_buf()),
        patterns: vec!["subject.browser.test.tsx".to_string()],
        environment: Some("browser".to_string()),
        coverage: true,
        serial: true,
        browser_path: std::env::var_os("WAKE_SYSTEM_BROWSER_PATH")
            .filter(|path| !path.is_empty())
            .map(Into::into),
        ..TestOptions::default()
    }
}

fn assert_coverage_artifacts(coverage: &CoverageResult, artifacts: &[wake_test::TestArtifact]) {
    assert!(coverage.summary.lines.total > 0, "{coverage:#?}");
    assert!(coverage.summary.lines.covered > 0, "{coverage:#?}");
    assert!(coverage.summary.functions.total > 0, "{coverage:#?}");
    assert!(coverage.summary.functions.covered > 0, "{coverage:#?}");
    assert!(coverage.summary.blocks.total > 0, "{coverage:#?}");
    assert!(
        coverage.summary.blocks.covered < coverage.summary.blocks.total,
        "{coverage:#?}"
    );

    let files = &coverage.files;
    assert_eq!(files.len(), 1, "{coverage:#?}");
    let file = &files[0];
    assert_eq!(file.path, "subject.tsx");
    assert!(file.metrics.lines.total > 0, "{file:#?}");
    assert!(file.metrics.functions.total > 0, "{file:#?}");
    assert!(file.metrics.blocks.total > 0, "{file:#?}");

    let artifact = |kind: &str| {
        artifacts
            .iter()
            .find(|artifact| artifact.kind == kind)
            .unwrap_or_else(|| panic!("missing {kind} artifact: {artifacts:#?}"))
    };
    assert_eq!(artifacts.len(), 4, "{artifacts:#?}");

    let text = artifact("coverage-text");
    assert!(text.path.ends_with("/coverage/coverage.txt"), "{text:#?}");
    let text = fs::read_to_string(&text.path).unwrap();
    assert!(text.contains("Wake coverage\nFile\tLines"), "{text}");
    assert!(text.contains("subject.tsx"), "{text}");

    let json = artifact("coverage-json");
    assert!(
        json.path.ends_with("/coverage/wake-coverage.json"),
        "{json:#?}"
    );
    let json_coverage: CoverageResult =
        serde_json::from_slice(&fs::read(&json.path).unwrap()).unwrap();
    // The JSON report is written before the result gains the artifact IDs that point back to that
    // report, so compare the public coverage payload rather than creating a self-reference.
    assert_eq!(json_coverage.summary, coverage.summary);
    assert_eq!(json_coverage.files, coverage.files);
    assert!(json_coverage.report_artifact_ids.is_empty());

    let lcov = artifact("coverage-lcov");
    assert!(lcov.path.ends_with("/coverage/lcov.info"), "{lcov:#?}");
    let lcov = fs::read_to_string(&lcov.path).unwrap();
    assert!(lcov.contains("TN:Wake Test\nSF:subject.tsx\n"), "{lcov}");
    assert!(lcov.contains("FNDA:"), "{lcov}");
    assert!(lcov.contains("BRDA:"), "{lcov}");
    assert!(lcov.contains("DA:"), "{lcov}");

    let html = artifact("coverage-html");
    assert!(html.path.ends_with("/coverage/index.html"), "{html:#?}");
    let html = fs::read_to_string(&html.path).unwrap();
    assert!(html.contains("<title>Wake coverage</title>"), "{html}");
    assert!(html.contains("subject.tsx"), "{html}");
    assert!(html.contains("covered branch"), "{html}");
    assert!(html.contains("class=\"line uncovered\""), "{html}");
}

#[test]
#[ignore = "requires an installed system Chromium browser"]
fn chromium_tsx_coverage_maps_original_sources_emits_reports_and_enforces_thresholds() {
    let fixture = tempfile::Builder::new()
        .prefix("wake-browser-coverage-conformance-")
        .tempdir_in(repository_root().join("target"))
        .unwrap();
    let root = fixture.path();
    fs::write(root.join("package.json"), "{}").unwrap();
    fs::write(
        root.join("subject.tsx"),
        r#"
            import React from 'react'

            export function choose(flag: boolean) {
              if (flag) {
                return <strong>covered branch</strong>
              }
              return <em>uncovered branch</em>
            }
        "#,
    )
    .unwrap();
    fs::write(
        root.join("subject.browser.test.tsx"),
        r#"
            import {expect, test} from '@crab-dev/wake/test'
            import {choose} from './subject'

            test('covers one TSX branch in Chromium', () => {
              expect(choose(true).props.children).toBe('covered branch')
            })
        "#,
    )
    .unwrap();
    fs::write(
        root.join("wake.config.toml"),
        r#"
            [test]
            environment = "browser"

            [test.coverage]
            enabled = true
            reporters = ["text", "json", "lcov", "html"]

            [test.coverage.threshold]
            lines = 1
            functions = 1
            blocks = 0

            [[test.coverage.per_file]]
            pattern = "subject.tsx"
            lines = 1
            functions = 1
            blocks = 0
        "#,
    )
    .unwrap();

    let passed = run_tests(browser_options(root)).unwrap();
    assert!(passed.success, "{passed:#?}");
    let coverage = passed.coverage.as_ref().expect("coverage result");
    assert_coverage_artifacts(coverage, &passed.artifacts);

    fs::write(
        root.join("wake.config.toml"),
        r#"
            [test]
            environment = "browser"

            [test.coverage]
            enabled = true
            reporters = ["text", "json", "lcov", "html"]

            [test.coverage.threshold]
            blocks = 100

            [[test.coverage.per_file]]
            pattern = "subject.tsx"
            blocks = 100
        "#,
    )
    .unwrap();

    let failed = run_tests(browser_options(root)).unwrap();
    assert!(!failed.success, "{failed:#?}");
    let failed_coverage = failed
        .coverage
        .as_ref()
        .expect("coverage despite threshold failure");
    assert_coverage_artifacts(failed_coverage, &failed.artifacts);
    let threshold_diagnostics = failed
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code == "WAKE_TEST_COVERAGE")
        .collect::<Vec<_>>();
    assert!(
        threshold_diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("global blocks coverage")),
        "{threshold_diagnostics:#?}"
    );
    assert!(
        threshold_diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("subject.tsx blocks coverage")),
        "{threshold_diagnostics:#?}"
    );
}
