use std::{fs, path::Path};
use wake_app::{
    CancellationToken, LintContext, LintDocument, LintFixMode, LintProjectOptions, LintStdin,
    lint_project,
};

fn fixture() -> tempfile::TempDir {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.tmp/lint-validation");
    fs::create_dir_all(&root).unwrap();
    let fixture = tempfile::Builder::new()
        .prefix("typed-public-")
        .tempdir_in(root)
        .unwrap();
    fs::write(fixture.path().join("wake.config.toml"), "[lint]\nrecommended=false\n[lint.types]\ncompiler='@typescript/native'\nprojects=['tsconfig.json']\n[lint.rules]\n 'ts/no-unsafe-call'='error'\n 'style/eol-last'='error'\n'import/no-unresolved'='error'").unwrap();
    fs::write(fixture.path().join("tsconfig.json"), r#"{"compilerOptions":{"strict":true,"target":"es2022","module":"esnext","moduleResolution":"bundler","types":[]},"files":["a.ts","b.ts"]}"#).unwrap();
    fs::write(
        fixture.path().join("a.ts"),
        "import {value} from './b'; value();",
    )
    .unwrap();
    fs::write(fixture.path().join("b.ts"), "export const value:any = 1;\n").unwrap();
    fixture
}

#[test]
#[ignore = "requires repository's installed native TypeScript 7.0.2 and platform package"]
fn typed_project_stdin_context_fix_and_cache_share_final_source_facts() {
    let root = fixture();
    let mut options = LintProjectOptions {
        root: root.path().into(),
        paths: vec!["a.ts".into()],
        cache: true,
        ..Default::default()
    };
    let result = lint_project(options.clone(), &CancellationToken::default()).unwrap();
    assert_eq!(result.error_count, 2, "{result:?}");
    assert_eq!(result.cache.unwrap().hits, 0);
    options.fix = LintFixMode::DryRun;
    let result = lint_project(options.clone(), &CancellationToken::default()).unwrap();
    assert_eq!(result.error_count, 1, "{result:?}");
    assert!(result.files[0].output.as_ref().unwrap().ends_with('\n'));
    options.fix = LintFixMode::Write;
    lint_project(options.clone(), &CancellationToken::default()).unwrap();
    assert!(
        fs::read_to_string(root.path().join("a.ts"))
            .unwrap()
            .ends_with('\n')
    );
    options.fix = LintFixMode::Off;
    options.paths.clear();
    options.stdin = Some(LintStdin {
        filename: "a.ts".into(),
        text: "import {value} from './b'; value();\n".into(),
    });
    assert_eq!(
        lint_project(options.clone(), &CancellationToken::default())
            .unwrap()
            .error_count,
        1
    );
    options.stdin = None;
    let context = LintContext::create(options).unwrap();
    context
        .update_document(LintDocument {
            filename: "b.ts".into(),
            version: 1,
            text: "export const value = () => 1;\n".into(),
        })
        .unwrap();
    let checked = context.check(CancellationToken::default()).unwrap();
    assert_eq!(checked.result.error_count, 0, "{checked:?}");
    context.close();
}

#[test]
fn type_settings_are_explained_without_starting_the_backend_and_invalid_data_fails() {
    let root = fixture();
    let options = LintProjectOptions {
        root: root.path().into(),
        print_config: Some("a.ts".into()),
        ..Default::default()
    };
    let result = lint_project(options.clone(), &CancellationToken::default()).unwrap();
    let config = serde_json::to_value(result.config.unwrap()).unwrap();
    assert_eq!(config["types"]["compiler"], "@typescript/native");
    assert_eq!(
        config["types"]["projects"],
        serde_json::json!(["tsconfig.json"])
    );
    for setting in [
        "projects=[]",
        "projects=['../escape.json']",
        "projects=['*.json']",
        "projects=['tsconfig.json']\ncompiler=''",
        "projects=['tsconfig.json','./tsconfig.json']",
    ] {
        fs::write(
            root.path().join("wake.config.toml"),
            format!("[lint.types]\n{setting}"),
        )
        .unwrap();
        assert_eq!(
            lint_project(options.clone(), &CancellationToken::default())
                .unwrap_err()
                .code,
            "WAKE_LINT_CONFIG"
        );
    }
    fs::write(
        root.path().join("wake.config.toml"),
        "[lint]\nrecommended=false\n[lint.rules]\n 'ts/no-unsafe-call'='error'",
    )
    .unwrap();
    let mut options = options;
    options.print_config = None;
    assert_eq!(
        lint_project(options, &CancellationToken::default())
            .unwrap_err()
            .code,
        "WAKE_LINT_ANALYSIS"
    );
}

#[test]
#[ignore = "requires repository's installed native TypeScript 7.0.2 and platform package"]
fn typed_member_access_uses_unsaved_receiver_types_and_closed_optional_settings() {
    let root = fixture();
    fs::write(
        root.path().join("a.ts"),
        "import {value} from './b'; value.field; value?.field;\n",
    )
    .unwrap();
    let options = LintProjectOptions {
        root: root.path().into(),
        paths: vec!["a.ts".into()],
        cache: true,
        rules: [(
            "ts/no-unsafe-member-access".into(),
            serde_json::json!("error"),
        )]
        .into(),
        ..Default::default()
    };
    let first = lint_project(options.clone(), &CancellationToken::default()).unwrap();
    assert_eq!(first.error_count, 2);
    assert_eq!(first.cache.unwrap().bypassed, 1);
    let context = LintContext::create(options.clone()).unwrap();
    context
        .update_document(LintDocument {
            filename: "b.ts".into(),
            version: 1,
            text: "export const value = {field:1};\n".into(),
        })
        .unwrap();
    assert_eq!(
        context
            .check(CancellationToken::default())
            .unwrap()
            .result
            .error_count,
        0
    );
    context.close();
    let mut options = options;
    options.rules.insert(
        "ts/no-unsafe-member-access".into(),
        serde_json::json!({"level":"error","options":{"allow_optional":true}}),
    );
    assert_eq!(
        lint_project(options, &CancellationToken::default())
            .unwrap()
            .error_count,
        1
    );
}

#[test]
#[ignore = "requires repository's installed native TypeScript 7.0.2 and platform package"]
fn typed_templates_follow_unsaved_dependency_types_and_report_original_unicode_ranges() {
    let root = fixture();
    let source = "// 😀\nimport {value} from './b'; const message = `${value}`;\n";
    fs::write(root.path().join("a.ts"), source).unwrap();
    fs::write(root.path().join("b.ts"), "export const value = {};\n").unwrap();
    let context = LintContext::create(LintProjectOptions {
        root: root.path().into(),
        paths: vec!["a.ts".into()],
        rules: [(
            "ts/restrict-template-expressions".into(),
            serde_json::json!({"level":"error","options":{"allow_any":false}}),
        )]
        .into(),
        ..Default::default()
    })
    .unwrap();
    let first = context.check(CancellationToken::default()).unwrap();
    assert_eq!(first.result.error_count, 1, "{first:?}");
    assert_eq!(
        first.result.files[0].diagnostics[0]
            .diagnostic
            .code
            .as_deref(),
        Some("ts/restrict-template-expressions")
    );
    context
        .update_document(LintDocument {
            filename: "b.ts".into(),
            version: 1,
            text: "export const value = 'text';\n".into(),
        })
        .unwrap();
    assert_eq!(
        context
            .check(CancellationToken::default())
            .unwrap()
            .result
            .error_count,
        0
    );
    context.close();
}

#[test]
#[ignore = "requires repository's installed native TypeScript 7.0.2 and platform package"]
fn typed_baselines_recover_with_dependency_changes_and_disabled_rules_need_no_compiler() {
    use wake_app::{LintBaselineMode, LintBaselineOptions};
    let root = fixture();
    fs::write(root.path().join("a.ts"), "// wake-lint-disable-next-line ts/no-unsafe-call\nimport {value} from './b'; value();\nvalue();\n").unwrap();
    let mut options = LintProjectOptions {
        root: root.path().into(),
        baseline: Some(LintBaselineOptions {
            path: "baseline.json".into(),
            mode: LintBaselineMode::Generate,
        }),
        ..Default::default()
    };
    let generated = lint_project(options.clone(), &CancellationToken::default()).unwrap();
    assert_eq!(generated.baseline.unwrap().entries, 1);
    options.baseline.as_mut().unwrap().mode = LintBaselineMode::Check;
    let checked = lint_project(options.clone(), &CancellationToken::default()).unwrap();
    assert_eq!(checked.error_count, 0);
    assert_eq!(checked.baseline.unwrap().suppressed, 1);
    fs::write(root.path().join("b.ts"), "export const value = () => 1;\n").unwrap();
    let checked = lint_project(options, &CancellationToken::default()).unwrap();
    assert_eq!(checked.error_count, 0);
    assert!(
        checked
            .files
            .iter()
            .flat_map(|file| &file.diagnostics)
            .any(|diagnostic| diagnostic.diagnostic.code.as_deref() == Some("wake/unused-disable"))
    );
    fs::write(root.path().join("wake.config.toml"), "[lint]\nrecommended=false\n[lint.types]\ncompiler='missing-compiler'\nprojects=['missing.json']").unwrap();
    let options = LintProjectOptions {
        root: root.path().into(),
        cache: true,
        ..Default::default()
    };
    lint_project(options.clone(), &CancellationToken::default()).unwrap();
    let warm = lint_project(options, &CancellationToken::default()).unwrap();
    assert_eq!(warm.cache.unwrap().hits, 2);
}

#[test]
#[ignore = "requires repository's installed native TypeScript 7.0.2 and platform package"]
fn typed_context_recovers_configuration_errors_and_rejects_excluded_even_empty_sources() {
    let root = fixture();
    let context = LintContext::create(LintProjectOptions {
        root: root.path().into(),
        paths: vec!["a.ts".into()],
        ..Default::default()
    })
    .unwrap();
    let valid = fs::read_to_string(root.path().join("tsconfig.json")).unwrap();
    fs::write(root.path().join("tsconfig.json"), "{broken").unwrap();
    assert_eq!(
        context
            .check(CancellationToken::default())
            .unwrap_err()
            .code,
        "WAKE_LINT_ANALYSIS"
    );
    fs::write(root.path().join("tsconfig.json"), &valid).unwrap();
    assert_eq!(
        context
            .check(CancellationToken::default())
            .unwrap()
            .result
            .error_count,
        2
    );
    fs::write(
        root.path().join("tsconfig.json"),
        r#"{"compilerOptions":{"types":[]},"files":["b.ts"]}"#,
    )
    .unwrap();
    context
        .update_document(LintDocument {
            filename: "a.ts".into(),
            version: 1,
            text: "export {};\n".into(),
        })
        .unwrap();
    assert!(
        context
            .check(CancellationToken::default())
            .unwrap_err()
            .message
            .contains("program")
    );
    fs::write(root.path().join("tsconfig.json"), valid).unwrap();
    assert_eq!(
        context
            .check(CancellationToken::default())
            .unwrap()
            .result
            .error_count,
        0
    );
    context.close();
}

#[test]
#[ignore = "requires installed native TypeScript 7.0.2 and permission to watch dependency ancestors"]
fn typed_watch_observes_ignored_declarations_and_recovers_failed_project_configuration() {
    use std::{
        thread,
        time::{Duration, Instant},
    };
    use wake_app::{LintWatchEvent, LintWatcher};
    let root = fixture();
    fs::write(root.path().join("wake.config.toml"), "[lint]\nrecommended=false\n[lint.types]\ncompiler='@typescript/native'\nprojects=['tsconfig.json']\n[lint.rules]\n 'ts/no-unsafe-call'='error'").unwrap();
    fs::write(
        root.path().join("a.ts"),
        "import {value} from './types'; value();\n",
    )
    .unwrap();
    fs::write(
        root.path().join("types.d.ts"),
        "export declare const value:any;\n",
    )
    .unwrap();
    let context = LintContext::create(LintProjectOptions {
        root: root.path().into(),
        paths: vec!["a.ts".into()],
        ..Default::default()
    })
    .unwrap();
    let watcher = LintWatcher::start(context.clone()).unwrap();
    let wait = |wanted: Option<usize>| {
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut recent = Vec::new();
        loop {
            for event in watcher.drain_events() {
                if match &event {
                    LintWatchEvent::Checked { snapshot } => {
                        wanted == Some(snapshot.result.error_count)
                    }
                    LintWatchEvent::Diagnostic { error, .. } => {
                        wanted.is_none() && error.code == "WAKE_LINT_ANALYSIS"
                    }
                    _ => false,
                } {
                    return;
                }
                recent.push(format!("{event:?}"));
            }
            assert!(
                Instant::now() < deadline,
                "watch result missing: {recent:?}"
            );
            thread::sleep(Duration::from_millis(20));
        }
    };
    wait(Some(1));
    fs::write(
        root.path().join("types.d.ts"),
        "export declare const value:()=>number;\n",
    )
    .unwrap();
    wait(Some(0));
    let config = fs::read_to_string(root.path().join("tsconfig.json")).unwrap();
    fs::write(root.path().join("tsconfig.json"), "{broken").unwrap();
    wait(None);
    fs::write(root.path().join("tsconfig.json"), config).unwrap();
    wait(Some(0));
    watcher.stop();
    context.close();
}
