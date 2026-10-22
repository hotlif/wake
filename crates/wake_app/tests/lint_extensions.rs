use std::fs;

use wake_app::{
    CancellationToken, LintBaselineMode, LintBaselineOptions, LintContext, LintFixMode,
    LintProjectOptions, lint_project,
};

fn package_file(root: &tempfile::TempDir, file: &str) -> std::path::PathBuf {
    root.path()
        .join("node_modules/@demo/wake-lint-plugin")
        .join(file)
}

fn extension_project() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    let package = root.path().join("node_modules/@demo/wake-lint-plugin");
    fs::create_dir_all(&package).unwrap();
    fs::write(
        root.path().join("package.json"),
        r#"{"name":"extension-project","private":true,"dependencies":{"@demo/wake-lint-plugin":"1.0.0"}}"#,
    )
    .unwrap();
    fs::write(
        root.path().join("wake.config.toml"),
        "[lint]\nrecommended=false\nextensions=['@demo/wake-lint-plugin']\n[lint.rules]\n'demo/no-debugger'='error'\n",
    )
    .unwrap();
    fs::write(root.path().join("a.js"), "// 🙂\ndebugger;").unwrap();
    fs::write(
        package.join("package.json"),
        r#"{"name":"@demo/wake-lint-plugin","version":"1.0.0","type":"module","main":"./index.mjs","wakeLint":{"manifest":"./wake-lint.manifest.json","module":"./index.mjs"}}"#,
    )
    .unwrap();
    fs::write(
        package.join("wake-lint.manifest.json"),
        r#"{"schema":"wake.lint.package.v1","name":"@demo/wake-lint-plugin","version":"1.0.0","protocol":"wake.lint.extension.v1","sdkMajor":1,"rules":[{"id":"demo/no-debugger","meta":{"description":"find debugger","category":"problem","fixable":false},"messageIds":["found"],"optionsSchema":{"type":"object","additionalProperties":false,"properties":{}}}]}"#,
    )
    .unwrap();
    fs::write(
        package.join("index.mjs"),
        r#"import { writeFileSync } from 'node:fs'
writeFileSync(new URL('./loaded', import.meta.url), 'loaded')
export default {
  protocol: 'wake.lint.extension.v1', sdkMajor: 1,
  name: '@demo/wake-lint-plugin', version: '1.0.0',
  rules: [{
    protocol: 'wake.lint.extension.v1', id: 'demo/no-debugger',
    meta: { description: 'find debugger', category: 'problem', fixable: false },
    create(context) {
      const index = context.source.indexOf('debugger')
      if (index >= 0) {
        const start = Buffer.byteLength(context.source.slice(0, index))
        context.report({ message: 'debugger is not allowed', messageId: 'found', start, end: start + 8 })
      }
    },
  }],
}
"#,
    )
    .unwrap();
    root
}

#[test]
fn untrusted_extension_configuration_fails_without_importing_package_code() {
    let root = extension_project();
    let error = lint_project(
        LintProjectOptions {
            root: root.path().into(),
            paths: vec!["a.js".into()],
            ..Default::default()
        },
        &CancellationToken::default(),
    )
    .unwrap_err();
    assert_eq!(error.code, "WAKE_LINT_CONFIG");
    assert!(error.message.contains("allow"), "{error}");
    assert!(
        !root
            .path()
            .join("node_modules/@demo/wake-lint-plugin/loaded")
            .exists()
    );
}

#[test]
fn authorized_config_explanation_uses_static_manifest_without_importing_rules() {
    let root = extension_project();
    let explained = lint_project(
        LintProjectOptions {
            root: root.path().into(),
            allow_extensions: true,
            print_config: Some("a.js".into()),
            ..Default::default()
        },
        &CancellationToken::default(),
    )
    .unwrap();
    let config = explained.config.unwrap();
    assert_eq!(config.rules["demo/no-debugger"].level, "error");
    assert!(
        !root
            .path()
            .join("node_modules/@demo/wake-lint-plugin/loaded")
            .exists()
    );
}

#[test]
fn authorized_extension_reports_utf8_diagnostics_and_bypasses_cache() {
    let root = extension_project();
    let result = lint_project(
        LintProjectOptions {
            root: root.path().into(),
            allow_extensions: true,
            paths: vec!["a.js".into()],
            cache: true,
            ..Default::default()
        },
        &CancellationToken::default(),
    )
    .unwrap();
    assert_eq!(result.error_count, 1);
    assert_eq!(result.files[0].diagnostics.len(), 1);
    let diagnostic = &result.files[0].diagnostics[0];
    assert_eq!(
        diagnostic.diagnostic.code.as_deref(),
        Some("demo/no-debugger")
    );
    assert_eq!(diagnostic.diagnostic.path.as_deref(), Some("a.js"));
    assert_eq!(diagnostic.diagnostic.start, Some("// 🙂\n".len() as u32));
    assert_eq!(diagnostic.message_id.as_deref(), Some("found"));
    assert_eq!(result.cache.unwrap().bypassed, 1);
    assert!(
        root.path()
            .join("node_modules/@demo/wake-lint-plugin/loaded")
            .exists()
    );
}

#[test]
fn mixed_project_extension_files_bypass_cache_even_when_graph_path_runs() {
    let root = extension_project();
    fs::write(root.path().join("b.js"), "debugger;").unwrap();
    fs::write(
        root.path().join("wake.config.toml"),
        "[lint]\nrecommended=false\nextensions=['@demo/wake-lint-plugin']\n[lint.rules]\n'demo/no-debugger'='off'\n[[lint.overrides]]\nfiles=['a.js']\n[lint.overrides.rules]\n'import/no-cycle'='error'\n[[lint.overrides]]\nfiles=['b.js']\n[lint.overrides.rules]\n'demo/no-debugger'='error'\n",
    )
    .unwrap();
    let options = LintProjectOptions {
        root: root.path().into(),
        allow_extensions: true,
        paths: vec!["a.js".into(), "b.js".into()],
        cache: true,
        ..Default::default()
    };
    for _ in 0..2 {
        let result = lint_project(options.clone(), &CancellationToken::default()).unwrap();
        assert_eq!(result.error_count, 1);
        assert_eq!(
            result.files[1].diagnostics[0].diagnostic.code.as_deref(),
            Some("demo/no-debugger")
        );
        let cache = result.cache.unwrap();
        assert_eq!(
            (cache.hits, cache.misses, cache.writes, cache.bypassed),
            (0, 0, 0, 2),
            "active extension files must not use the native single-file cache",
        );
    }
}

#[test]
fn undeclared_package_and_invalid_manifest_fail_before_import() {
    let root = extension_project();
    fs::write(root.path().join("package.json"), r#"{"private":true}"#).unwrap();
    let options = LintProjectOptions {
        root: root.path().into(),
        allow_extensions: true,
        paths: vec!["a.js".into()],
        ..Default::default()
    };
    let error = lint_project(options.clone(), &CancellationToken::default()).unwrap_err();
    assert_eq!(error.code, "WAKE_LINT_EXTENSION");
    assert!(error.message.contains("direct dependency"), "{error}");
    assert!(
        !root
            .path()
            .join("node_modules/@demo/wake-lint-plugin/loaded")
            .exists()
    );

    fs::write(
        root.path().join("package.json"),
        r#"{"private":true,"dependencies":{"@demo/wake-lint-plugin":"1.0.0"}}"#,
    )
    .unwrap();
    fs::write(
        root.path()
            .join("node_modules/@demo/wake-lint-plugin/wake-lint.manifest.json"),
        r#"{"schema":"wake.lint.package.v2"}"#,
    )
    .unwrap();
    let error = lint_project(options, &CancellationToken::default()).unwrap_err();
    assert_eq!(error.code, "WAKE_LINT_EXTENSION");
    assert!(
        !root
            .path()
            .join("node_modules/@demo/wake-lint-plugin/loaded")
            .exists()
    );
}

#[test]
fn extension_fixes_and_baseline_fail_before_import_or_source_write() {
    let root = extension_project();
    for fix in [LintFixMode::DryRun, LintFixMode::Write] {
        let error = lint_project(
            LintProjectOptions {
                root: root.path().into(),
                allow_extensions: true,
                paths: vec!["a.js".into()],
                fix,
                ..Default::default()
            },
            &CancellationToken::default(),
        )
        .unwrap_err();
        assert_eq!(error.code, "WAKE_LINT_EXTENSION");
        assert_eq!(
            fs::read_to_string(root.path().join("a.js")).unwrap(),
            "// 🙂\ndebugger;"
        );
        assert!(
            !root
                .path()
                .join("node_modules/@demo/wake-lint-plugin/loaded")
                .exists()
        );
    }
    let error = lint_project(
        LintProjectOptions {
            root: root.path().into(),
            allow_extensions: true,
            paths: vec!["a.js".into()],
            baseline: Some(LintBaselineOptions {
                path: "baseline.json".into(),
                mode: LintBaselineMode::Generate,
            }),
            ..Default::default()
        },
        &CancellationToken::default(),
    )
    .unwrap_err();
    assert_eq!(error.code, "WAKE_LINT_EXTENSION");
    assert!(!root.path().join("baseline.json").exists());
    assert!(
        !root
            .path()
            .join("node_modules/@demo/wake-lint-plugin/loaded")
            .exists()
    );
}

#[test]
fn disabled_rule_does_not_import_and_unmatched_override_still_validates_unknown_rules() {
    let root = extension_project();
    fs::write(root.path().join("wake.config.toml"),
        "[lint]\nrecommended=false\nextensions=['@demo/wake-lint-plugin']\n[lint.rules]\n'demo/no-debugger'='off'\n").unwrap();
    let result = lint_project(
        LintProjectOptions {
            root: root.path().into(),
            allow_extensions: true,
            paths: vec!["a.js".into()],
            ..Default::default()
        },
        &CancellationToken::default(),
    )
    .unwrap();
    assert_eq!(result.error_count, 0);
    assert!(!package_file(&root, "loaded").exists());

    let error = lint_project(
        LintProjectOptions {
            root: root.path().into(),
            allow_extensions: true,
            paths: vec!["a.js".into()],
            fix: LintFixMode::DryRun,
            ..Default::default()
        },
        &CancellationToken::default(),
    )
    .unwrap_err();
    assert_eq!(error.code, "WAKE_LINT_EXTENSION");
    assert_eq!(
        LintContext::create(LintProjectOptions {
            root: root.path().into(),
            ..Default::default()
        })
        .err()
        .unwrap()
        .code,
        "WAKE_LINT_EXTENSION"
    );

    fs::write(root.path().join("wake.config.toml"),
        "[lint]\nrecommended=false\nextensions=['@demo/wake-lint-plugin']\n[lint.rules]\n'demo/no-debugger'={level='off',options={unexpected=true}}\n").unwrap();
    let error = lint_project(
        LintProjectOptions {
            root: root.path().into(),
            allow_extensions: true,
            paths: vec!["a.js".into()],
            ..Default::default()
        },
        &CancellationToken::default(),
    )
    .unwrap_err();
    assert_eq!(error.code, "WAKE_LINT_EXTENSION");
    assert!(error.message.contains("options"), "{error}");

    fs::write(root.path().join("wake.config.toml"),
        "[lint]\nrecommended=false\nextensions=['@demo/wake-lint-plugin']\n[[lint.overrides]]\nfiles=['**/*.ts']\n[lint.overrides.rules]\n'demo/not-declared'='off'\n").unwrap();
    let error = lint_project(
        LintProjectOptions {
            root: root.path().into(),
            allow_extensions: true,
            paths: vec!["a.js".into()],
            ..Default::default()
        },
        &CancellationToken::default(),
    )
    .unwrap_err();
    assert_eq!(error.code, "WAKE_LINT_CONFIG");
    assert!(error.message.contains("demo/not-declared"), "{error}");
    assert!(!package_file(&root, "loaded").exists());
}

#[test]
fn extension_comment_suppression_is_explicitly_unsupported_in_first_phase() {
    let root = extension_project();
    fs::write(
        root.path().join("a.js"),
        "// wake-lint-disable-next-line demo/no-debugger\ndebugger;",
    )
    .unwrap();
    let result = lint_project(
        LintProjectOptions {
            root: root.path().into(),
            allow_extensions: true,
            paths: vec!["a.js".into()],
            ..Default::default()
        },
        &CancellationToken::default(),
    )
    .unwrap();
    let codes = result.files[0]
        .diagnostics
        .iter()
        .filter_map(|diagnostic| diagnostic.diagnostic.code.as_deref())
        .collect::<Vec<_>>();
    assert!(codes.contains(&"wake/invalid-directive"), "{codes:?}");
    assert!(codes.contains(&"demo/no-debugger"), "{codes:?}");
}

#[test]
fn runtime_rule_drift_and_worker_crash_fail_closed() {
    let root = extension_project();
    fs::write(package_file(&root, "index.mjs"),
        "export default { protocol:'wake.lint.extension.v1', sdkMajor:1, name:'@demo/wake-lint-plugin', version:'1.0.0', rules:[{ protocol:'wake.lint.extension.v1', id:'demo/other', meta:{description:'find debugger',category:'problem',fixable:false}, create(){} }] }\n").unwrap();
    let options = LintProjectOptions {
        root: root.path().into(),
        allow_extensions: true,
        paths: vec!["a.js".into()],
        ..Default::default()
    };
    let error = lint_project(options.clone(), &CancellationToken::default()).unwrap_err();
    assert_eq!(error.code, "WAKE_LINT_EXTENSION");
    assert!(
        error.message.contains("runtime") || error.message.contains("manifest"),
        "{error}"
    );

    fs::write(package_file(&root, "index.mjs"), "process.exit(7)\n").unwrap();
    let error = lint_project(options, &CancellationToken::default()).unwrap_err();
    assert_eq!(error.code, "WAKE_LINT_EXTENSION");
}

#[test]
fn worker_timeout_and_cancellation_stop_extension_execution() {
    let root = extension_project();
    fs::write(package_file(&root, "index.mjs"), "while (true) {}\n").unwrap();
    let options = LintProjectOptions {
        root: root.path().into(),
        allow_extensions: true,
        paths: vec!["a.js".into()],
        ..Default::default()
    };
    let error = lint_project(options.clone(), &CancellationToken::default()).unwrap_err();
    assert_eq!(error.code, "WAKE_LINT_EXTENSION");
    assert!(error.message.contains("timed out"), "{error}");

    fs::write(package_file(&root, "index.mjs"),
        "import { writeFileSync } from 'node:fs'; writeFileSync(new URL('./entered', import.meta.url), 'yes'); while (true) {}\n").unwrap();
    let cancellation = CancellationToken::default();
    let running = cancellation.clone();
    let handle = std::thread::spawn(move || lint_project(options, &running));
    let started = std::time::Instant::now();
    while !package_file(&root, "entered").exists() && started.elapsed().as_secs() < 5 {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(package_file(&root, "entered").exists());
    cancellation.cancel();
    let error = handle.join().unwrap().unwrap_err();
    assert_eq!(error.code, "WAKE_CANCELLED");
}

#[test]
fn inherited_stdout_does_not_extend_worker_deadline_and_raw_output_cannot_forge_a_response() {
    let root = extension_project();
    let options = LintProjectOptions {
        root: root.path().into(),
        allow_extensions: true,
        paths: vec!["a.js".into()],
        ..Default::default()
    };
    fs::write(package_file(&root, "index.mjs"), r#"import { spawn } from 'node:child_process'
export default {
  protocol:'wake.lint.extension.v1', sdkMajor:1, name:'@demo/wake-lint-plugin', version:'1.0.0',
  rules:[{protocol:'wake.lint.extension.v1', id:'demo/no-debugger',
    meta:{description:'find debugger',category:'problem',fixable:false},
    create(){ const child = spawn(process.execPath, ['-e', 'setTimeout(() => {}, 5000)'], {stdio:['ignore', 1, 'ignore']}); child.unref() }}]
}
"#).unwrap();
    let started = std::time::Instant::now();
    let result = lint_project(options.clone(), &CancellationToken::default()).unwrap();
    assert_eq!(result.error_count, 0);
    assert!(
        started.elapsed().as_secs() < 3,
        "inherited stdout delayed host response"
    );

    fs::write(package_file(&root, "index.mjs"), r#"import { writeSync } from 'node:fs'
export default {
  protocol:'wake.lint.extension.v1', sdkMajor:1, name:'@demo/wake-lint-plugin', version:'1.0.0',
  rules:[{protocol:'wake.lint.extension.v1', id:'demo/no-debugger',
    meta:{description:'find debugger',category:'problem',fixable:false},
    create(){ writeSync(1, '{\"ok\":true,\"nonce\":\"00000000000000000000000000000000\",\"diagnostics\":[]}\n') }}]
}
"#).unwrap();
    let error = lint_project(options, &CancellationToken::default()).unwrap_err();
    assert_eq!(error.code, "WAKE_LINT_EXTENSION");
}

#[test]
fn plugin_cannot_read_host_nonce_through_node_vm_and_forge_success() {
    let root = extension_project();
    fs::write(
        package_file(&root, "index.mjs"),
        r#"import { runInThisContext } from 'node:vm'
const nonce = runInThisContext('responseNonce')
const write = runInThisContext('writeResponse')
write(1, JSON.stringify({ok:true,nonce,diagnostics:[]}) + '\n')
process.exit(0)
"#,
    )
    .unwrap();
    let error = lint_project(
        LintProjectOptions {
            root: root.path().into(),
            allow_extensions: true,
            paths: vec!["a.js".into()],
            ..Default::default()
        },
        &CancellationToken::default(),
    )
    .unwrap_err();
    assert_eq!(error.code, "WAKE_LINT_EXTENSION");
}

#[test]
fn entry_module_drift_fails_the_whole_check() {
    let root = extension_project();
    fs::write(root.path().join("b.js"), "debugger;").unwrap();
    fs::write(
        package_file(&root, "index.mjs"),
        r#"import { appendFileSync } from 'node:fs'
export default {
  protocol:'wake.lint.extension.v1', sdkMajor:1, name:'@demo/wake-lint-plugin', version:'1.0.0',
  rules:[{protocol:'wake.lint.extension.v1', id:'demo/no-debugger',
    meta:{description:'find debugger',category:'problem',fixable:false},
    create(context){
      context.report({message:'found',messageId:'found',start:0,end:0})
      if (context.path === 'a.js') appendFileSync(new URL('./index.mjs', import.meta.url), '\n// changed during lint\n')
    }}]
}
"#,
    )
    .unwrap();
    let error = lint_project(
        LintProjectOptions {
            root: root.path().into(),
            allow_extensions: true,
            paths: vec!["a.js".into(), "b.js".into()],
            ..Default::default()
        },
        &CancellationToken::default(),
    )
    .unwrap_err();
    assert_eq!(error.code, "WAKE_LINT_EXTENSION");
    assert!(
        error.message.contains("changed") || error.message.contains("identity"),
        "{error}"
    );
}

#[test]
fn manifest_and_package_locator_drift_fail_the_whole_check() {
    for mutation in ["manifest", "locator"] {
        let root = extension_project();
        fs::write(root.path().join("b.js"), "debugger;").unwrap();
        fs::write(
            package_file(&root, "other.mjs"),
            fs::read_to_string(package_file(&root, "index.mjs")).unwrap(),
        )
        .unwrap();
        let mutation_script = if mutation == "manifest" {
            "appendFileSync(new URL('./wake-lint.manifest.json', import.meta.url), '\\n')"
        } else {
            "const file = new URL('./package.json', import.meta.url); const value = JSON.parse(readFileSync(file, 'utf8')); value.wakeLint.module = './other.mjs'; writeFileSync(file, JSON.stringify(value))"
        };
        fs::write(
            package_file(&root, "index.mjs"),
            format!(
                r#"import {{ appendFileSync, readFileSync, writeFileSync }} from 'node:fs'
export default {{
  protocol:'wake.lint.extension.v1', sdkMajor:1, name:'@demo/wake-lint-plugin', version:'1.0.0',
  rules:[{{protocol:'wake.lint.extension.v1', id:'demo/no-debugger',
    meta:{{description:'find debugger',category:'problem',fixable:false}},
    create(context){{
      context.report({{message:'found',messageId:'found',start:0,end:0}})
      if (context.path === 'a.js') {{ {mutation_script} }}
    }}}}]
}}
"#
            ),
        )
        .unwrap();
        let error = lint_project(
            LintProjectOptions {
                root: root.path().into(),
                allow_extensions: true,
                paths: vec!["a.js".into(), "b.js".into()],
                ..Default::default()
            },
            &CancellationToken::default(),
        )
        .unwrap_err();
        assert_eq!(error.code, "WAKE_LINT_EXTENSION", "{mutation}: {error}");
        assert!(
            error.message.contains("changed") || error.message.contains("identity"),
            "{mutation}: {error}"
        );
    }
}

#[test]
fn extension_error_discards_diagnostics_from_earlier_files_and_rules() {
    let root = extension_project();
    fs::write(root.path().join("b.js"), "debugger;").unwrap();
    fs::write(
        package_file(&root, "index.mjs"),
        r#"export default {
  protocol:'wake.lint.extension.v1', sdkMajor:1, name:'@demo/wake-lint-plugin', version:'1.0.0',
  rules:[{protocol:'wake.lint.extension.v1', id:'demo/no-debugger',
    meta:{description:'find debugger',category:'problem',fixable:false},
    create(context){
      context.report({message:'found',messageId:'found',start:0,end:0})
      if (context.path === 'b.js') throw new Error('later failure')
    }}]
}
"#,
    )
    .unwrap();
    let error = lint_project(
        LintProjectOptions {
            root: root.path().into(),
            allow_extensions: true,
            paths: vec!["a.js".into(), "b.js".into()],
            ..Default::default()
        },
        &CancellationToken::default(),
    )
    .unwrap_err();
    assert_eq!(error.code, "WAKE_LINT_EXTENSION");
    assert!(error.message.contains("later failure"), "{error}");
}

#[test]
fn lint_context_and_watch_reject_extension_projects() {
    let root = extension_project();
    let options = LintProjectOptions {
        root: root.path().into(),
        ..Default::default()
    };
    let error = LintContext::create(options.clone()).err().unwrap();
    assert_eq!(error.code, "WAKE_LINT_EXTENSION");
    let error = LintContext::create_for_watch(options).err().unwrap();
    assert_eq!(error.code, "WAKE_LINT_EXTENSION");
    assert!(!package_file(&root, "loaded").exists());
}
