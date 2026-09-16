use std::io::Write;
use std::process::{Command, Stdio};

#[test]
#[ignore = "requires repository's installed native TypeScript 7.0.2 and platform package"]
fn lint_types_preserve_cli_diagnostics_cache_and_analysis_exit_codes() {
    let parent =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.tmp/lint-validation");
    std::fs::create_dir_all(&parent).unwrap();
    let root = tempfile::Builder::new()
        .prefix("typed-cli-")
        .tempdir_in(parent)
        .unwrap();
    std::fs::write(root.path().join("wake.config.toml"), "[lint]\nrecommended=false\n[lint.types]\ncompiler='@typescript/native'\nprojects=['tsconfig.json']\n[lint.rules]\n 'ts/no-unsafe-call'='error'").unwrap();
    std::fs::write(
        root.path().join("tsconfig.json"),
        r#"{"compilerOptions":{"strict":true,"types":[]},"files":["a.ts"]}"#,
    )
    .unwrap();
    std::fs::write(
        root.path().join("a.ts"),
        "declare const value:any; value();",
    )
    .unwrap();
    let run = || {
        Command::new(env!("CARGO_BIN_EXE_wake"))
            .args(["lint", "--root"])
            .arg(root.path())
            .args(["a.ts", "--cache", "--format", "json"])
            .output()
            .unwrap()
    };
    let output = run();
    assert_eq!(
        output.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        result["files"][0]["diagnostics"][0]["messageId"],
        "unsafeCall"
    );
    assert_eq!(result["cache"]["bypassed"], 1);
    std::fs::write(
        root.path().join("a.ts"),
        "declare const value:()=>void; value();",
    )
    .unwrap();
    assert_eq!(run().status.code(), Some(0));
    std::fs::write(root.path().join("tsconfig.json"), "{broken").unwrap();
    let failed = run();
    assert_eq!(failed.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&failed.stderr).contains("WAKE_LINT_ANALYSIS"));
    assert!(!root.path().join(".wake").exists());
}

#[test]
fn lint_modules_share_project_graph_cache_bypass_and_authoritative_failures() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("a.ts"), "import './b';").unwrap();
    std::fs::write(root.path().join("b.ts"), "import './a';").unwrap();
    let run = || {
        Command::new(env!("CARGO_BIN_EXE_wake"))
            .args(["lint", "--root"])
            .arg(root.path())
            .args([
                "a.ts",
                "--rule",
                "import/no-cycle=error",
                "--rule",
                "import/no-unresolved=error",
                "--cache",
                "--format",
                "json",
            ])
            .output()
            .unwrap()
    };
    let cycle = run();
    assert_eq!(cycle.status.code(), Some(1));
    let result: serde_json::Value = serde_json::from_slice(&cycle.stdout).unwrap();
    assert_eq!(result["files"][0]["diagnostics"][0]["messageId"], "cycle");
    assert_eq!(result["cache"]["bypassed"], 1);
    assert_eq!(result["cache"]["hits"], 0);
    std::fs::write(root.path().join("b.ts"), "export {};").unwrap();
    assert_eq!(run().status.code(), Some(0));
    std::fs::write(root.path().join(".pnp.cjs"), "broken").unwrap();
    let failed = run();
    assert_eq!(failed.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&failed.stderr).contains("WAKE_LINT_ANALYSIS"));
    assert!(!root.path().join(".wake").exists());
}

#[test]
fn lint_hooks_emit_source_diagnostics_and_analysis_failure_exit_two() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("app.tsx");
    std::fs::write(
        &path,
        "import {useState as h} from 'react'; function App(x) { if(x) h(0); }",
    )
    .unwrap();
    let run = || {
        Command::new(env!("CARGO_BIN_EXE_wake"))
            .args(["lint", "--root"])
            .arg(root.path())
            .args([
                "--rule",
                "react-hooks/rules-of-hooks=error",
                "--cache",
                "--format",
                "json",
                "app.tsx",
            ])
            .output()
            .unwrap()
    };
    let output = run();
    assert_eq!(output.status.code(), Some(1));
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        result["files"][0]["diagnostics"][0]["messageId"],
        "conditional"
    );
    let text = format!("function App() {{ {} }}", "ordinary();".repeat(60_000));
    std::fs::write(&path, &text).unwrap();
    let failed = run();
    assert_eq!(failed.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&failed.stderr).contains("WAKE_LINT_ANALYSIS"));
    assert_eq!(std::fs::read_to_string(path).unwrap(), text);
}

#[test]
fn lint_watch_streams_configuration_recovery_and_file_changes() {
    use std::io::{BufRead, BufReader};
    use std::time::Duration;
    struct ChildGuard(std::process::Child);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("wake.config.toml"), "[lint\n").unwrap();
    let mut child = ChildGuard(
        Command::new(env!("CARGO_BIN_EXE_wake"))
            .args(["lint", "--watch", "--format", "json", "--root"])
            .arg(root.path())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let output = child.0.stdout.take().unwrap();
    let (sender, events) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(output).lines().map_while(Result::ok) {
            if sender.send(line).is_err() {
                break;
            }
        }
    });
    let next = |kind: &str| loop {
        let line = events
            .recv_timeout(Duration::from_secs(10))
            .expect("lint watch event");
        let value: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value["schema"], "wake.lint.watch.v1");
        if value["event"]["type"] == kind {
            break value["event"].clone();
        }
    };
    assert_eq!(next("diagnostic")["error"]["code"], "WAKE_LINT_CONFIG");
    std::fs::write(root.path().join("a.js"), "debugger;").unwrap();
    std::fs::write(root.path().join("wake.config.toml"), "[lint]\n").unwrap();
    assert_eq!(next("checked")["snapshot"]["result"]["errorCount"], 1);
    std::fs::write(root.path().join("a.js"), "run();").unwrap();
    assert_eq!(next("checked")["snapshot"]["result"]["errorCount"], 0);
    drop(child);
    reader.join().unwrap();
}

#[test]
fn lint_baseline_cli_supports_generation_check_prune_and_mode_conflicts() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("a.js"), "debugger;\n").unwrap();
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_wake"))
            .args(["lint", "--root"])
            .arg(root.path())
            .args(["--format", "json"])
            .args(args)
            .output()
            .unwrap()
    };
    for mode in ["--generate-baseline", "--baseline"] {
        let output = run(&[mode, "baseline.json"]);
        assert_eq!(
            output.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result["baseline"]["suppressed"], 1);
    }
    std::fs::write(root.path().join("a.js"), "run();\n").unwrap();
    let output = run(&["--prune-baseline", "baseline.json"]);
    assert_eq!(output.status.code(), Some(0));
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["baseline"]["stale"], 1);
    for args in [
        vec!["--baseline", "x.json", "--generate-baseline", "x.json"],
        vec!["--generate-baseline", "x.json", "--fix"],
        vec!["--prune-baseline", "x.json", "a.js"],
    ] {
        assert_eq!(run(&args).status.code(), Some(2));
    }
}

#[test]
fn lint_cache_is_shared_across_cold_processes_and_fix_always_reanalyzes() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("a.js"), "debugger;").unwrap();
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_wake"))
            .args(["lint", "--root"])
            .arg(root.path())
            .args(args)
            .output()
            .unwrap()
    };
    let cold = run(&["--cache", "--format", "json"]);
    assert_eq!(
        cold.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&cold.stderr)
    );
    let cold: serde_json::Value = serde_json::from_slice(&cold.stdout).unwrap();
    assert_eq!(cold["cache"]["writes"], 1);
    let warm: serde_json::Value =
        serde_json::from_slice(&run(&["--cache", "--format", "json"]).stdout).unwrap();
    assert_eq!(warm["cache"]["hits"], 1);
    assert_eq!(cold["files"], warm["files"]);
    let fix: serde_json::Value =
        serde_json::from_slice(&run(&["--cache", "--fix-dry-run", "--format", "json"]).stdout)
            .unwrap();
    assert_eq!(fix["cache"]["bypassed"], 1);
    for args in [
        vec!["--cache", "--print-config", "x.js"],
        vec!["--cache", "--list-rules"],
    ] {
        assert_eq!(run(&args).status.code(), Some(2));
    }
}

#[test]
fn concurrent_cli_cache_writers_keep_each_others_entries() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("a.js"), "debugger;").unwrap();
    std::fs::write(root.path().join("b.js"), "run();").unwrap();
    let spawn = |path: &str| {
        Command::new(env!("CARGO_BIN_EXE_wake"))
            .args(["lint", "--root"])
            .arg(root.path())
            .args(["--cache", "--format", "json", path])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    };
    let first = spawn("a.js");
    let second = spawn("b.js");
    assert_eq!(first.wait_with_output().unwrap().status.code(), Some(1));
    assert_eq!(second.wait_with_output().unwrap().status.code(), Some(0));
    let warm = Command::new(env!("CARGO_BIN_EXE_wake"))
        .args(["lint", "--root"])
        .arg(root.path())
        .args(["--cache", "--format", "json"])
        .output()
        .unwrap();
    let warm: serde_json::Value = serde_json::from_slice(&warm.stdout).unwrap();
    assert_eq!(warm["cache"]["hits"], 2);
    assert_eq!(warm["cache"]["writes"], 0);
}

#[test]
fn lint_catalog_is_json_and_independent_of_project_configuration() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("wake.config.toml"), "not valid toml").unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_wake"))
        .args(["lint", "--list-rules", "--format", "human", "--root"])
        .arg(root.path())
        .output()
        .unwrap();
    assert_eq!(
        result.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(result["catalog"]["schema"], "wake.lint.rules.v1");
    assert!(
        result["catalog"]["rules"]
            .as_array()
            .unwrap()
            .iter()
            .any(|rule| rule["id"] == "js/no-console")
    );
    for args in [
        vec!["x.js"],
        vec!["--fix"],
        vec!["--print-config", "x.js"],
        vec!["--rule", "js/no-debugger=off"],
    ] {
        assert_eq!(
            Command::new(env!("CARGO_BIN_EXE_wake"))
                .args(["lint", "--list-rules"])
                .args(args)
                .output()
                .unwrap()
                .status
                .code(),
            Some(2)
        );
    }
}

#[test]
fn lint_print_config_and_request_rules_share_parameter_validation() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("wake.config.toml"),
        "[lint]\npresets=['react']",
    )
    .unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_wake"))
        .args(["lint", "--root"])
        .arg(root.path())
        .args([
            "--print-config",
            "src/missing.tsx",
            "--rule",
            "js/eqeqeq=off",
            "--rule",
            r#"js/eqeqeq={"level":"error","options":{"allow_null":true}}"#,
        ])
        .output()
        .unwrap();
    assert_eq!(
        result.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(value["config"]["rules"]["js/eqeqeq"]["source"], "request");
    assert_eq!(
        value["config"]["rules"]["js/eqeqeq"]["options"]["allow_null"],
        true
    );
    assert_eq!(value["config"]["rules"]["react/no-danger"]["level"], "warn");
    assert!(!root.path().join("src").exists());
    for args in [
        vec!["--rule", "js/eqeqeq"],
        vec!["--rule", "js/eqeqeq=fatal"],
        vec!["--print-config", "x.js", "--fix"],
        vec!["--print-config", "x.js", "--max-warnings", "0"],
        vec![
            "--print-config",
            "x.js",
            "--rule",
            r#"js/eqeqeq={"level":"off","options":{"allow_null":0}}"#,
        ],
    ] {
        let result = Command::new(env!("CARGO_BIN_EXE_wake"))
            .args(["lint", "--root"])
            .arg(root.path())
            .args(args)
            .output()
            .unwrap();
        assert_eq!(result.status.code(), Some(2));
    }
}

#[test]
fn lint_environment_cli_override_controls_host_globals() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("input.js"), "window;\n").unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_wake"))
        .args(["lint", "--root"])
        .arg(root.path())
        .args([
            "input.js",
            "--rule",
            "js/no-undef=error",
            "--env",
            "browser",
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    assert_eq!(
        result.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(value["errorCount"], 0);

    let invalid = Command::new(env!("CARGO_BIN_EXE_wake"))
        .args(["lint", "--root"])
        .arg(root.path())
        .args(["input.js", "--env", "deno", "--format", "json"])
        .output()
        .unwrap();
    assert_eq!(invalid.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("Unknown lint environment"));
}

#[test]
fn lint_fix_preview_and_write_use_final_diagnostics_and_reject_conflicting_modes() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("wake.config.toml"),
        "[lint.rules]\n'style/eol-last'='error'",
    )
    .unwrap();
    let path = root.path().join("a.js");
    std::fs::write(&path, "run();").unwrap();
    for (mode, written) in [("--fix-dry-run", false), ("--fix", true)] {
        let result = Command::new(env!("CARGO_BIN_EXE_wake"))
            .args(["lint", "--root"])
            .arg(root.path())
            .args([mode, "--format", "json"])
            .output()
            .unwrap();
        assert_eq!(
            result.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let value: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!(value["files"][0]["written"], written);
        assert_eq!(value["files"][0]["output"], "run();\n");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            if written { "run();\n" } else { "run();" }
        );
    }
    for args in [
        vec!["--fix", "--fix-dry-run"],
        vec!["--fix", "--stdin", "--stdin-filename", "a.js"],
    ] {
        let result = Command::new(env!("CARGO_BIN_EXE_wake"))
            .arg("lint")
            .args(args)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert_eq!(result.status.code(), Some(2));
    }
}

#[test]
fn lint_json_uses_rule_exit_status_and_snapshot_locations() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("a.js"), "// 😀\r\ndebugger;").unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_wake"))
        .args(["lint", "--root"])
        .arg(root.path())
        .args(["--format", "json"])
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(1));
    assert!(
        result.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(
        value["files"][0]["diagnostics"][0]["code"],
        "js/no-debugger"
    );
    assert_eq!(value["files"][0]["diagnostics"][0]["location"]["line"], 2);
}

#[test]
fn stdin_and_warning_threshold_share_the_native_project_path() {
    let root = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_wake"))
        .args(["lint", "--root"])
        .arg(root.path())
        .args([
            "--stdin",
            "--stdin-filename",
            "unsaved.ts",
            "--max-warnings",
            "0",
            "--format",
            "json",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"a == b;").unwrap();
    let result = child.wait_with_output().unwrap();
    assert_eq!(result.status.code(), Some(1));
    let value: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(value["warningCount"], 1);
    assert!(!root.path().join("unsaved.ts").exists());
}

#[test]
fn missing_stdin_filename_and_invalid_rules_are_usage_errors() {
    let root = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_wake"))
        .args(["lint", "--stdin"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    std::fs::write(
        root.path().join("wake.config.toml"),
        "[lint.rules]\n'js/unknown'='off'",
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_wake"))
        .args(["lint", "--root"])
        .arg(root.path())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
}
