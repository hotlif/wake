use std::fs;
use wake_app::{CancellationToken, LintProjectOptions, LintStdin, lint_project};

#[test]
fn hook_analysis_failures_preserve_sources_and_never_cache_partial_results() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("app.tsx");
    let source = format!("function App() {{ {} }}", "ordinary();".repeat(60_000));
    fs::write(&path, &source).unwrap();
    fs::write(
        dir.path().join("wake.config.toml"),
        "[lint]\nrecommended=false\n[lint.rules]\n'react-hooks/rules-of-hooks'='error'",
    )
    .unwrap();
    let options = LintProjectOptions {
        root: dir.path().into(),
        paths: vec!["app.tsx".into()],
        cache: true,
        ..Default::default()
    };
    for fix in [
        wake_app::LintFixMode::Off,
        wake_app::LintFixMode::Off,
        wake_app::LintFixMode::DryRun,
        wake_app::LintFixMode::Write,
    ] {
        let error = lint_project(
            LintProjectOptions {
                fix,
                ..options.clone()
            },
            &CancellationToken::default(),
        )
        .unwrap_err();
        assert_eq!(error.code, "WAKE_LINT_ANALYSIS", "{error}");
        assert_eq!(fs::read_to_string(&path).unwrap(), source);
    }
    fs::write(
        &path,
        "import {useState as h} from 'react'; function App(x) { if(x) h(0); }",
    )
    .unwrap();
    let cold = lint_project(options.clone(), &CancellationToken::default()).unwrap();
    assert_eq!(cold.error_count, 1);
    assert_eq!(cold.cache.unwrap().hits, 0);
    let warm = lint_project(options, &CancellationToken::default()).unwrap();
    assert_eq!(warm.error_count, 1);
    assert_eq!(warm.cache.unwrap().hits, 1);
}

#[test]
fn rule_catalog_is_project_independent_and_rejects_analysis_options() {
    let dir = tempfile::tempdir().unwrap();
    let options = LintProjectOptions {
        root: dir.path().join("missing"),
        list_rules: true,
        ..Default::default()
    };
    let result = lint_project(options.clone(), &CancellationToken::default()).unwrap();
    let json = serde_json::to_value(result).unwrap();
    assert_eq!(json["catalog"]["schema"], "wake.lint.rules.v1");
    assert!(json["catalog"]["rules"].as_array().unwrap().len() >= 32);
    assert_eq!(json["files"], serde_json::json!([]));
    assert_eq!(json["exitCode"], 0);
    assert!(
        lint_project(
            LintProjectOptions {
                paths: vec!["x.js".into()],
                ..options.clone()
            },
            &CancellationToken::default()
        )
        .is_err()
    );
    assert!(
        lint_project(
            LintProjectOptions {
                print_config: Some("x.js".into()),
                ..options
            },
            &CancellationToken::default()
        )
        .is_err()
    );
}

#[test]
fn lint_explains_virtual_files_and_whole_rule_replacement_in_effective_order() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("wake.config.toml"),
        r#"
[lint]
recommended = false
presets = ["react@1", "recommended"]
ignore = ["ignored/**"]
[lint.rules]
"js/eqeqeq" = { level = "warn", options = { allow_null = true } }
[[lint.overrides]]
files = ["**/*.ts"]
rules = { "js/eqeqeq" = "error" }
[[lint.overrides]]
files = ["src/**"]
rules = { "js/no-debugger" = "off" }
"#,
    )
    .unwrap();
    let options = LintProjectOptions {
        root: dir.path().into(),
        print_config: Some("src/virtual.ts".into()),
        ..Default::default()
    };
    let result = lint_project(options.clone(), &CancellationToken::default()).unwrap();
    assert!(result.files.is_empty());
    let json = serde_json::to_value(&result).unwrap();
    let config = &json["config"];
    assert_eq!(config["schema"], "wake.lint.config.v1");
    assert_eq!(config["language"], "typescript");
    assert_eq!(config["ignored"], false);
    assert_eq!(config["rules"]["js/eqeqeq"]["options"]["allow_null"], false);
    assert_eq!(config["rules"]["js/eqeqeq"]["source"], "override:0");
    assert_eq!(config["rules"]["js/no-debugger"]["source"], "override:1");
    assert_eq!(
        config["rules"]["react/no-danger"]["source"],
        "preset:react@1"
    );
    assert_eq!(config["rules"]["style/eol-last"]["source"], "default");
    assert!(!dir.path().join("src").exists());
    let result = lint_project(
        LintProjectOptions {
            print_config: Some("ignored/a.ts".into()),
            rules: [(
                "js/eqeqeq".into(),
                serde_json::json!({"level":"off","options":{"allow_null":true}}),
            )]
            .into(),
            ..options.clone()
        },
        &CancellationToken::default(),
    )
    .unwrap();
    let json = serde_json::to_value(result).unwrap();
    assert_eq!(json["config"]["ignored"], true);
    assert_eq!(json["config"]["rules"]["js/eqeqeq"]["source"], "request");
    assert_eq!(
        json["config"]["rules"]["js/eqeqeq"]["options"]["allow_null"],
        true
    );
    for bad in [
        LintProjectOptions {
            fix: wake_app::LintFixMode::DryRun,
            ..options.clone()
        },
        LintProjectOptions {
            paths: vec!["a.ts".into()],
            ..options.clone()
        },
        LintProjectOptions {
            max_warnings: Some(0),
            ..options.clone()
        },
        LintProjectOptions {
            stdin: Some(LintStdin {
                filename: "a.ts".into(),
                text: String::new(),
            }),
            ..options.clone()
        },
    ] {
        assert_eq!(
            lint_project(bad, &CancellationToken::default())
                .unwrap_err()
                .code,
            "WAKE_LINT_CONFIG"
        );
    }
}

#[test]
fn lint_environment_sets_feed_undef_and_effective_config() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("wake.config.toml"),
        r#"
[lint]
recommended = false
environments = ["browser", "node"]
[lint.rules]
"js/no-undef" = "error"
"#,
    )
    .unwrap();
    fs::write(dir.path().join("input.js"), "window; process;\n").unwrap();

    let result = lint_project(
        LintProjectOptions {
            root: dir.path().into(),
            ..Default::default()
        },
        &CancellationToken::default(),
    )
    .unwrap();
    assert_eq!(result.error_count, 0);

    let explained = lint_project(
        LintProjectOptions {
            root: dir.path().into(),
            print_config: Some("input.js".into()),
            ..Default::default()
        },
        &CancellationToken::default(),
    )
    .unwrap();
    let json = serde_json::to_value(explained).unwrap();
    assert_eq!(
        json["config"]["environments"],
        serde_json::json!(["browser", "node"])
    );
    assert_eq!(
        json["config"]["globals"]["window"]["source"],
        "environment:browser@1"
    );
    assert_eq!(
        json["config"]["globals"]["process"]["source"],
        "environment:node@1"
    );

    fs::write(
        dir.path().join("wake.config.toml"),
        r#"
[lint]
recommended = false
environments = ["browser"]
[lint.rules]
"js/no-undef" = "error"
[[lint.overrides]]
files = ["input.js"]
environments = ["node"]
[lint.overrides.globals]
process = "writable"
window = "writable"
"#,
    )
    .unwrap();
    let explained = lint_project(
        LintProjectOptions {
            root: dir.path().into(),
            print_config: Some("input.js".into()),
            environments: vec!["browser".into()],
            globals: [(
                "window".into(),
                serde_json::Value::String("writable".into()),
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        },
        &CancellationToken::default(),
    )
    .unwrap();
    let json = serde_json::to_value(explained).unwrap();
    assert_eq!(json["config"]["globals"]["process"]["mode"], "writable");
    assert_eq!(json["config"]["globals"]["process"]["source"], "override:0");
    assert_eq!(
        json["config"]["globals"]["Promise"]["source"],
        "standard:es2024@1"
    );
    assert_eq!(json["config"]["globals"]["window"]["mode"], "writable");
    assert_eq!(json["config"]["globals"]["window"]["source"], "request");
    let checked = lint_project(
        LintProjectOptions {
            root: dir.path().into(),
            environments: vec!["browser".into()],
            globals: [(
                "window".into(),
                serde_json::Value::String("writable".into()),
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        },
        &CancellationToken::default(),
    )
    .unwrap();
    assert_eq!(checked.error_count, 0);

    fs::write(
        dir.path().join("wake.config.toml"),
        "[lint]\nenvironments=['deno']\n",
    )
    .unwrap();
    let error = lint_project(
        LintProjectOptions {
            root: dir.path().into(),
            ..Default::default()
        },
        &CancellationToken::default(),
    )
    .unwrap_err();
    assert_eq!(error.code, "WAKE_LINT_CONFIG");
    assert!(error.message.contains("Unknown lint environment"));
}

#[test]
fn lint_validates_unmatched_rule_options_and_request_settings() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("wake.config.toml"), "[[lint.overrides]]\nfiles=['never/**']\nrules={'js/eqeqeq'={level='off',options={allow_null='yes'}}}").unwrap();
    let options = LintProjectOptions {
        root: dir.path().into(),
        ..Default::default()
    };
    let error = lint_project(options.clone(), &CancellationToken::default()).unwrap_err();
    assert_eq!(error.code, "WAKE_LINT_CONFIG");
    assert!(error.message.contains("allow_null"));
    fs::write(
        dir.path().join("wake.config.toml"),
        "[lint]\npresets=['style@1']",
    )
    .unwrap();
    fs::write(dir.path().join("input.js"), "a == null;").unwrap();
    let result = lint_project(
        LintProjectOptions {
            rules: [(
                "js/eqeqeq".into(),
                serde_json::json!({"level":"error", "options":{"allow_null":true}}),
            )]
            .into(),
            ..options.clone()
        },
        &CancellationToken::default(),
    )
    .unwrap();
    assert_eq!(result.warning_count, 1); // style preset
    assert_eq!(result.error_count, 0); // request parameter applied
    for value in [
        serde_json::json!({"level":"off", "options":[]}),
        serde_json::json!({"level":"off", "typo":true}),
        serde_json::json!(null),
    ] {
        assert!(
            lint_project(
                LintProjectOptions {
                    rules: [("js/eqeqeq".into(), value)].into(),
                    ..options.clone()
                },
                &CancellationToken::default()
            )
            .is_err()
        );
    }
}

#[test]
fn lint_fix_modes_share_final_diagnostics_but_only_write_mode_changes_files() {
    use wake_app::LintFixMode;
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("wake.config.toml"),
        "[lint.rules]\n'react/self-closing-comp'='error'\n'style/eol-last'='warn'",
    )
    .unwrap();
    let path = dir.path().join("input.tsx");
    fs::write(&path, "const x = <C></C>;").unwrap();
    let options = LintProjectOptions {
        root: dir.path().to_path_buf(),
        ..Default::default()
    };
    let checked = lint_project(options.clone(), &CancellationToken::default()).unwrap();
    assert_eq!(checked.error_count, 1);
    assert!(checked.files[0].diagnostics[0].fix.is_some());
    let preview = lint_project(
        LintProjectOptions {
            fix: LintFixMode::DryRun,
            ..options.clone()
        },
        &CancellationToken::default(),
    )
    .unwrap();
    assert_eq!(preview.exit_code, 0);
    assert_eq!(
        preview.files[0].output.as_deref(),
        Some("const x = <C/>;\n")
    );
    assert!(preview.files[0].changed);
    assert!(!preview.files[0].written);
    assert_eq!(fs::read_to_string(&path).unwrap(), "const x = <C></C>;");
    let written = lint_project(
        LintProjectOptions {
            fix: LintFixMode::Write,
            ..options.clone()
        },
        &CancellationToken::default(),
    )
    .unwrap();
    assert!(written.files[0].written);
    assert_eq!(fs::read_to_string(&path).unwrap(), "const x = <C/>;\n");
    let unchanged = lint_project(
        LintProjectOptions {
            fix: LintFixMode::Write,
            ..options
        },
        &CancellationToken::default(),
    )
    .unwrap();
    assert!(!unchanged.files[0].changed);
    assert!(!unchanged.files[0].written);
    assert_eq!(unchanged.files[0].fix_passes, 0);
}

#[test]
fn markdown_processor_lints_fenced_code_without_writing_host_document() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("docs/README.md");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let source = "# Demo\n\n说明\n\n```js\ndebugger;\n```\n";
    fs::write(&path, source).unwrap();
    fs::write(
        dir.path().join("wake.config.toml"),
        r#"
[lint]
files = ["docs/**/*.md"]
[lint.processors]
"docs/**/*.md" = "markdown"
[lint.rules]
"js/no-debugger" = "error"
"#,
    )
    .unwrap();

    let checked = lint_project(
        LintProjectOptions {
            root: dir.path().into(),
            cache: true,
            ..Default::default()
        },
        &CancellationToken::default(),
    )
    .unwrap();
    assert_eq!(checked.files.len(), 1);
    assert_eq!(checked.files[0].diagnostics.len(), 1);
    assert_eq!(
        checked.files[0].diagnostics[0].diagnostic.code.as_deref(),
        Some("js/no-debugger")
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), source);
    assert_eq!(checked.cache.as_ref().unwrap().bypassed, 1);

    let explained = lint_project(
        LintProjectOptions {
            root: dir.path().into(),
            print_config: Some("docs/README.md".into()),
            ..Default::default()
        },
        &CancellationToken::default(),
    )
    .unwrap();
    let explained = serde_json::to_value(explained).unwrap();
    assert_eq!(explained["config"]["processor"], "markdown");
    assert_eq!(explained["config"]["language"], "module");

    for fix in [wake_app::LintFixMode::DryRun, wake_app::LintFixMode::Write] {
        let error = lint_project(
            LintProjectOptions {
                root: dir.path().into(),
                fix,
                ..Default::default()
            },
            &CancellationToken::default(),
        )
        .unwrap_err();
        assert_eq!(error.code, "WAKE_LINT_CONFIG");
        assert_eq!(fs::read_to_string(&path).unwrap(), source);
    }
}

#[test]
fn markdown_processor_rejects_project_rules_without_a_host_module_identity() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("docs")).unwrap();
    fs::write(
        dir.path().join("wake.config.toml"),
        r#"
[lint]
files = ["docs/**/*.md"]
[lint.processors]
"docs/**/*.md" = "markdown"
[lint.rules]
"import/no-unresolved" = "error"
"#,
    )
    .unwrap();
    fs::write(
        dir.path().join("docs/readme.md"),
        "```js\nimport 'missing';\n```\n",
    )
    .unwrap();
    let error = lint_project(
        LintProjectOptions {
            root: dir.path().into(),
            ..Default::default()
        },
        &CancellationToken::default(),
    )
    .unwrap_err();
    assert_eq!(error.code, "WAKE_LINT_ANALYSIS");
    assert!(error.message.contains("module rules"));
}

#[test]
fn lint_fix_validates_entire_selection_before_any_writes_and_rejects_stdin_write() {
    use wake_app::LintFixMode;
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("wake.config.toml"),
        "[lint.rules]\n'style/eol-last'='error'",
    )
    .unwrap();
    fs::write(dir.path().join("a.js"), "run();").unwrap();
    let options = LintProjectOptions {
        root: dir.path().to_path_buf(),
        fix: LintFixMode::Write,
        ..Default::default()
    };
    assert!(
        lint_project(
            LintProjectOptions {
                paths: vec!["a.js".into(), "missing/**/*.js".into()],
                ..options.clone()
            },
            &CancellationToken::default()
        )
        .is_err()
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("a.js")).unwrap(),
        "run();"
    );
    let input = LintStdin {
        filename: "virtual.js".into(),
        text: "run();".into(),
    };
    assert!(
        lint_project(
            LintProjectOptions {
                stdin: Some(input.clone()),
                ..options.clone()
            },
            &CancellationToken::default()
        )
        .is_err()
    );
    let preview = lint_project(
        LintProjectOptions {
            fix: LintFixMode::DryRun,
            stdin: Some(input),
            ..options.clone()
        },
        &CancellationToken::default(),
    )
    .unwrap();
    assert_eq!(preview.files[0].output.as_deref(), Some("run();\n"));
    assert!(!dir.path().join("virtual.js").exists());
    let cancelled = CancellationToken::default();
    cancelled.cancel();
    assert_eq!(
        lint_project(options, &cancelled).unwrap_err().code,
        "WAKE_CANCELLED"
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("a.js")).unwrap(),
        "run();"
    );
}

#[test]
fn project_config_controls_unused_disable_severity() {
    let dir = tempfile::tempdir().unwrap();
    for (level, errors, warnings) in [("off", 0, 0), ("warn", 0, 1), ("error", 1, 0)] {
        fs::write(
            dir.path().join("wake.config.toml"),
            format!("[lint]\nreport_unused_disable='{level}'"),
        )
        .unwrap();
        let result = lint_project(
            LintProjectOptions {
                root: dir.path().to_path_buf(),
                stdin: Some(LintStdin {
                    filename: "input.js".into(),
                    text: "// wake-lint-disable-next-line js/no-debugger\nrun();".into(),
                }),
                ..Default::default()
            },
            &CancellationToken::default(),
        )
        .unwrap();
        assert_eq!(
            (result.error_count, result.warning_count),
            (errors, warnings)
        );
    }
}

#[test]
fn snapshot_locations_recognize_all_ecmascript_line_terminators() {
    let dir = tempfile::tempdir().unwrap();
    for newline in ["\n", "\r\n", "\r", "\u{2028}", "\u{2029}"] {
        let result = lint_project(
            LintProjectOptions {
                root: dir.path().to_path_buf(),
                stdin: Some(LintStdin {
                    filename: "unsaved.js".into(),
                    text: format!("// 😀{newline}debugger;{newline}"),
                }),
                ..Default::default()
            },
            &CancellationToken::default(),
        )
        .unwrap();
        let location = result.files[0].diagnostics[0]
            .diagnostic
            .location
            .as_ref()
            .unwrap();
        assert_eq!((location.line, location.column), (2, 1), "{newline:?}");
        assert_eq!(location.line_text, "debugger;");
    }
}

#[test]
fn project_discovery_config_overrides_and_counts_share_one_result() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("src")).unwrap();
    fs::create_dir(dir.path().join("dist")).unwrap();
    fs::write(dir.path().join("src/a.js"), "// 😀\r\ndebugger;").unwrap();
    fs::write(dir.path().join("src/a.test.js"), "debugger;").unwrap();
    fs::write(dir.path().join("dist/generated.js"), "debugger;").unwrap();
    fs::write(
        dir.path().join("wake.config.toml"),
        "[[lint.overrides]]\nfiles=['**/*.test.js']\nrules={'js/no-debugger'='off'}",
    )
    .unwrap();
    let result = lint_project(
        LintProjectOptions {
            root: dir.path().to_path_buf(),
            ..Default::default()
        },
        &CancellationToken::default(),
    )
    .unwrap();
    assert_eq!(result.files.len(), 2);
    assert_eq!(result.error_count, 1);
    assert_eq!(result.exit_code, 1);
    let diagnostic = &result.files[0].diagnostics[0].diagnostic;
    assert_eq!(diagnostic.code.as_deref(), Some("js/no-debugger"));
    assert_eq!(diagnostic.location.as_ref().unwrap().line, 2);
}

#[test]
fn discovery_ignores_generated_pnp_runtime_manifests() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("src")).unwrap();
    fs::write(dir.path().join("src/index.js"), "debugger;").unwrap();
    fs::write(dir.path().join(".pnp.cjs"), "debugger;").unwrap();
    fs::write(dir.path().join(".pnp.mjs"), "debugger;").unwrap();
    fs::write(dir.path().join(".pnp.loader.mjs"), "debugger;").unwrap();
    fs::write(
        dir.path().join("wake.config.toml"),
        "[lint]\nfiles=['**/*.js','**/*.mjs']\n[lint.rules]\n'js/no-debugger'='error'",
    )
    .unwrap();

    let result = lint_project(
        LintProjectOptions {
            root: dir.path().to_path_buf(),
            ..Default::default()
        },
        &CancellationToken::default(),
    )
    .unwrap();
    assert_eq!(
        result
            .files
            .iter()
            .map(|file| file.path.as_str())
            .collect::<Vec<_>>(),
        ["src/index.js"]
    );
    assert_eq!(result.error_count, 1);
    let explicit = lint_project(
        LintProjectOptions {
            root: dir.path().to_path_buf(),
            paths: vec![".pnp.cjs".into()],
            ..Default::default()
        },
        &CancellationToken::default(),
    )
    .unwrap_err();
    assert!(explicit.to_string().contains("ignored"));
}

#[test]
fn stdin_uses_virtual_file_config_and_never_reads_disk_source() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("wake.config.toml"),
        "[lint.rules]\n'js/no-debugger'='warn'",
    )
    .unwrap();
    let result = lint_project(
        LintProjectOptions {
            root: dir.path().to_path_buf(),
            stdin: Some(LintStdin {
                filename: "unsaved.ts".into(),
                text: "debugger;".into(),
            }),
            max_warnings: Some(0),
            ..Default::default()
        },
        &CancellationToken::default(),
    )
    .unwrap();
    assert_eq!(result.error_count, 0);
    assert_eq!(result.warning_count, 1);
    assert_eq!(result.exit_code, 1);
    assert!(!dir.path().join("unsaved.ts").exists());
}

#[test]
fn invalid_configs_paths_ignores_and_unmatched_patterns_fail_explicitly() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("dist")).unwrap();
    fs::write(dir.path().join("dist/x.js"), "debugger;").unwrap();
    for pattern in ["missing.js", "dist/x.js", "src/**/*.ts", "../escape.js"] {
        assert!(
            lint_project(
                LintProjectOptions {
                    root: dir.path().to_path_buf(),
                    paths: vec![pattern.into()],
                    ..Default::default()
                },
                &CancellationToken::default()
            )
            .is_err(),
            "{pattern}"
        );
    }
    fs::write(
        dir.path().join("wake.config.toml"),
        "[[lint.overrides]]\nfiles=['never/*.ts']\nrules={'js/typo'='off'}",
    )
    .unwrap();
    assert!(
        lint_project(
            LintProjectOptions {
                root: dir.path().to_path_buf(),
                ..Default::default()
            },
            &CancellationToken::default()
        )
        .is_err()
    );
}

#[test]
fn explicit_paths_are_deduplicated_and_cancelled_runs_fail() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("a.js"), "debugger;").unwrap();
    let options = LintProjectOptions {
        root: dir.path().to_path_buf(),
        paths: vec![".".into(), "a.js".into()],
        ..Default::default()
    };
    let result = lint_project(options.clone(), &CancellationToken::default()).unwrap();
    assert_eq!(result.files.len(), 1);
    let cancellation = CancellationToken::default();
    cancellation.cancel();
    assert!(lint_project(options, &cancellation).is_err());
}
