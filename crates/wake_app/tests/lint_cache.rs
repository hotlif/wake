use std::fs;
use wake_app::{CancellationToken, LintFixMode, LintProjectOptions, LintStdin, lint_project};

fn run(options: LintProjectOptions) -> serde_json::Value {
    serde_json::to_value(lint_project(options, &CancellationToken::default()).unwrap()).unwrap()
}

#[test]
fn cold_and_warm_checks_match_and_content_and_effective_configuration_invalidate() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.js");
    fs::write(&path, "debugger;").unwrap();
    let options = LintProjectOptions {
        root: dir.path().into(),
        cache: true,
        ..Default::default()
    };
    let cold = run(options.clone());
    assert_eq!(cold["cache"]["misses"], 1);
    assert_eq!(cold["cache"]["writes"], 1);
    let warm = run(options.clone());
    assert_eq!(warm["cache"]["hits"], 1);
    assert_eq!(cold["files"], warm["files"]);
    assert_eq!(warm["exitCode"], 1);
    let modified = fs::metadata(&path).unwrap().modified().unwrap();
    fs::write(&path, "run(123);").unwrap();
    fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(modified))
        .unwrap();
    let changed = run(options.clone());
    assert_eq!(changed["cache"]["misses"], 1);
    assert_eq!(changed["exitCode"], 0);
    fs::write(&path, "x == y;").unwrap();
    let before = run(options.clone());
    assert_eq!(before["warningCount"], 1);
    fs::write(
        dir.path().join("wake.config.toml"),
        "[lint.rules]\n'js/eqeqeq'='off'",
    )
    .unwrap();
    let after = run(options.clone());
    assert_eq!(after["cache"]["misses"], 1);
    assert_eq!(after["warningCount"], 0);
    assert_eq!(run(options)["cache"]["hits"], 1);
}

#[test]
fn corrupted_or_unavailable_cache_is_a_visible_miss_without_affecting_lint_status() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("a.js"), "debugger;").unwrap();
    let options = LintProjectOptions {
        root: dir.path().into(),
        cache: true,
        ..Default::default()
    };
    let original = run(options.clone());
    let entries = fs::read_dir(dir.path().join(".wake/lint/v1")).unwrap();
    for entry in entries {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|extension| extension == "wlc") {
            fs::write(path, "corrupt").unwrap();
        }
    }
    let recovered = run(options.clone());
    assert_eq!(original["files"], recovered["files"]);
    assert_eq!(recovered["cache"]["misses"], 1);
    assert_eq!(recovered["cache"]["warnings"].as_array().unwrap().len(), 1);
    assert_eq!(run(options)["cache"]["hits"], 1);
    let blocked = tempfile::tempdir().unwrap();
    fs::write(blocked.path().join("a.js"), "run();").unwrap();
    fs::write(blocked.path().join(".wake"), "user data").unwrap();
    let result = run(LintProjectOptions {
        root: blocked.path().into(),
        cache: true,
        max_warnings: Some(0),
        ..Default::default()
    });
    assert_eq!(result["exitCode"], 0);
    assert_eq!(result["warningCount"], 0);
    assert_eq!(result["cache"]["warnings"].as_array().unwrap().len(), 1);
    assert_eq!(
        fs::read_to_string(blocked.path().join(".wake")).unwrap(),
        "user data"
    );
}

#[test]
fn stdin_and_fix_modes_bypass_cache_and_parse_errors_are_never_reused() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("a.js"), "run();").unwrap();
    fs::write(
        dir.path().join("wake.config.toml"),
        "[lint.rules]\n'style/eol-last'='error'",
    )
    .unwrap();
    let options = LintProjectOptions {
        root: dir.path().into(),
        cache: true,
        ..Default::default()
    };
    for fix in [LintFixMode::DryRun, LintFixMode::Write] {
        let result = run(LintProjectOptions {
            fix,
            ..options.clone()
        });
        assert_eq!(result["cache"]["bypassed"], 1);
        assert!(!dir.path().join(".wake").exists());
    }
    let result = run(LintProjectOptions {
        stdin: Some(LintStdin {
            filename: "input.js".into(),
            text: "debugger;".into(),
        }),
        ..options.clone()
    });
    assert_eq!(result["cache"]["bypassed"], 1);
    assert!(!dir.path().join(".wake").exists());
    fs::write(dir.path().join("a.js"), "const = ;").unwrap();
    for _ in 0..2 {
        let result = run(options.clone());
        assert_eq!(result["cache"]["hits"], 0);
        assert_eq!(result["cache"]["writes"], 0);
        assert_eq!(result["exitCode"], 1);
    }
}
