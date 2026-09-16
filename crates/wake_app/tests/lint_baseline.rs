use std::fs;
use wake_app::{
    CancellationToken, LintBaselineMode, LintBaselineOptions, LintFixMode, LintProjectOptions,
    lint_project,
};

fn options(root: &std::path::Path, mode: LintBaselineMode) -> LintProjectOptions {
    LintProjectOptions {
        root: root.into(),
        baseline: Some(LintBaselineOptions {
            path: "lint-baseline.json".into(),
            mode,
        }),
        ..Default::default()
    }
}

#[test]
fn generates_merges_checks_and_prunes_baselines_without_absorbing_new_problems() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("a.js"), "const a = 1; debugger;\n").unwrap();
    fs::write(dir.path().join("b.js"), "const b = 2; debugger;\n").unwrap();
    let mut request = options(dir.path(), LintBaselineMode::Generate);
    request.paths = vec!["a.js".into()];
    let generated = lint_project(request.clone(), &CancellationToken::default()).unwrap();
    assert_eq!(generated.exit_code, 0);
    assert_eq!(generated.baseline.unwrap().suppressed, 1);
    request.paths = vec!["b.js".into()];
    let generated = lint_project(request, &CancellationToken::default()).unwrap();
    assert_eq!(generated.baseline.unwrap().entries, 2);
    fs::write(
        dir.path().join("a.js"),
        "// moved\nconst a = 1; debugger;\nconst c = 3; debugger;\n",
    )
    .unwrap();
    let checked = lint_project(
        options(dir.path(), LintBaselineMode::Check),
        &CancellationToken::default(),
    )
    .unwrap();
    assert_eq!(checked.error_count, 1);
    assert_eq!(checked.baseline.unwrap().suppressed, 2);
    fs::remove_file(dir.path().join("b.js")).unwrap();
    let pruned = lint_project(
        options(dir.path(), LintBaselineMode::Prune),
        &CancellationToken::default(),
    )
    .unwrap();
    assert_eq!(pruned.error_count, 1);
    let stats = pruned.baseline.unwrap();
    assert_eq!((stats.entries, stats.stale, stats.written), (1, 1, true));
    let document: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.path().join("lint-baseline.json")).unwrap())
            .unwrap();
    assert_eq!(document["schema"], "wake.lint.baseline.v1");
    assert_eq!(document["entries"][0]["path"], "a.js");
}

#[test]
fn malformed_baselines_and_conflicting_requests_cannot_write_source_or_baseline() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("lint-baseline.json");
    fs::write(&path, "{\"name\":\"package\"}").unwrap();
    fs::write(dir.path().join("a.js"), "const a = \"x\";").unwrap();
    for mode in [
        LintBaselineMode::Check,
        LintBaselineMode::Generate,
        LintBaselineMode::Prune,
    ] {
        assert!(lint_project(options(dir.path(), mode), &CancellationToken::default()).is_err());
    }
    assert_eq!(fs::read_to_string(&path).unwrap(), "{\"name\":\"package\"}");
    let mut request = options(dir.path(), LintBaselineMode::Generate);
    request.fix = LintFixMode::Write;
    assert!(lint_project(request, &CancellationToken::default()).is_err());
    assert_eq!(
        fs::read_to_string(dir.path().join("a.js")).unwrap(),
        "const a = \"x\";"
    );
}

#[test]
fn baseline_changes_invalidate_cache_and_repeated_context_stays_visible() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("a.js"), "debugger;\n").unwrap();
    lint_project(
        options(dir.path(), LintBaselineMode::Generate),
        &CancellationToken::default(),
    )
    .unwrap();
    let mut request = options(dir.path(), LintBaselineMode::Check);
    request.cache = true;
    let cold = lint_project(request.clone(), &CancellationToken::default()).unwrap();
    assert_eq!((cold.error_count, cold.cache.unwrap().misses), (0, 1));
    let warm = lint_project(request.clone(), &CancellationToken::default()).unwrap();
    assert_eq!(
        (warm.baseline.unwrap().suppressed, warm.cache.unwrap().hits),
        (1, 1)
    );
    fs::write(
        dir.path().join("lint-baseline.json"),
        "{\"schema\":\"wake.lint.baseline.v1\",\"entries\":[]}",
    )
    .unwrap();
    let changed = lint_project(request, &CancellationToken::default()).unwrap();
    assert_eq!((changed.error_count, changed.cache.unwrap().misses), (1, 1));
    lint_project(
        options(dir.path(), LintBaselineMode::Generate),
        &CancellationToken::default(),
    )
    .unwrap();
    fs::write(dir.path().join("a.js"), "debugger;\ndebugger;\n").unwrap();
    let ambiguous = lint_project(
        options(dir.path(), LintBaselineMode::Check),
        &CancellationToken::default(),
    )
    .unwrap();
    assert_eq!(
        (ambiguous.error_count, ambiguous.baseline.unwrap().ambiguous),
        (2, 2)
    );
}

#[test]
fn pruning_preserves_uninspected_or_unparseable_sources_and_check_counts_only_selected_stale_entries()
 {
    let dir = tempfile::tempdir().unwrap();
    for file in ["a.js", "b.js", "c.js"] {
        fs::write(dir.path().join(file), "debugger;\n").unwrap();
    }
    lint_project(
        options(dir.path(), LintBaselineMode::Generate),
        &CancellationToken::default(),
    )
    .unwrap();
    fs::write(dir.path().join("a.js"), "function {").unwrap();
    fs::write(dir.path().join("b.js"), "run();").unwrap();
    fs::write(dir.path().join("c.js"), "run();").unwrap();
    fs::write(
        dir.path().join("wake.config.toml"),
        "[lint]\nignore=['c.js']\n",
    )
    .unwrap();
    let mut partial = options(dir.path(), LintBaselineMode::Check);
    partial.paths = vec!["a.js".into()];
    assert_eq!(
        lint_project(partial, &CancellationToken::default())
            .unwrap()
            .baseline
            .unwrap()
            .stale,
        0
    );
    let pruned = lint_project(
        options(dir.path(), LintBaselineMode::Prune),
        &CancellationToken::default(),
    )
    .unwrap();
    assert!(pruned.error_count > 0);
    let stats = pruned.baseline.unwrap();
    assert_eq!((stats.stale, stats.entries), (1, 2));
}

#[test]
fn baseline_check_supports_unsaved_fix_preview_without_rewriting_accepted_problems() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("a.js"), "const a = \"x\";\n").unwrap();
    fs::write(dir.path().join("wake.config.toml"), "[lint]\nrecommended=false\n[lint.rules]\n'style/quotes'='error'\n'style/eol-last'='error'\n").unwrap();
    lint_project(
        options(dir.path(), LintBaselineMode::Generate),
        &CancellationToken::default(),
    )
    .unwrap();
    let mut request = options(dir.path(), LintBaselineMode::Check);
    request.stdin = Some(wake_app::LintStdin {
        filename: "a.js".into(),
        text: "const a = \"x\";".into(),
    });
    request.fix = LintFixMode::DryRun;
    let result = lint_project(request, &CancellationToken::default()).unwrap();
    assert_eq!(
        result.files[0].output.as_deref(),
        Some("const a = \"x\";\n")
    );
    assert_eq!(result.exit_code, 0);
    assert_eq!(result.baseline.unwrap().suppressed, 1);
}

#[test]
fn baseline_rejects_oversized_noncanonical_and_unknown_schema_and_respects_cancellation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("lint-baseline.json");
    for document in [
        "{\"schema\":\"wake.lint.baseline.v2\",\"entries\":[]}".to_owned(),
        serde_json::json!({"schema":"wake.lint.baseline.v1", "entries":[{"path":"../a.js","ruleId":"js/no-debugger","messageId":"unexpected","fingerprint":"0".repeat(64)}]}).to_string(),
        " ".repeat(4 * 1024 * 1024 + 1),
    ] {
        fs::write(&path, &document).unwrap();
        assert!(lint_project(options(dir.path(), LintBaselineMode::Generate), &CancellationToken::default()).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), document);
    }
    fs::remove_file(&path).unwrap();
    let cancelled = CancellationToken::default();
    cancelled.cancel();
    assert_eq!(
        lint_project(options(dir.path(), LintBaselineMode::Generate), &cancelled)
            .unwrap_err()
            .code,
        "WAKE_CANCELLED"
    );
    assert!(!path.exists());
}
