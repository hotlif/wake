use std::fs;
use wake_app::{
    CancellationToken, LintContext, LintDocument, LintFixMode, LintProjectOptions, LintStdin,
    lint_project,
};

fn project(config: &str, files: &[(&str, &str)]) -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("wake.config.toml"), config).unwrap();
    for (path, source) in files {
        let path = root.path().join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, source).unwrap();
    }
    root
}

#[test]
fn surrogate_type_and_dynamic_specifiers_are_unresolved_without_becoming_unknown() {
    let root = project(
        "[lint]\nrecommended=false\n[lint.rules]\n'import/no-unresolved'={level='error',options={include_types=true,commonjs=true,dynamic_imports=true,report_unknown=false}}",
        &[(
            "entry.ts",
            r#"import type A from '\ud800'; type B=import('\ud801').B; import('\ud802'); require('\ud803');"#,
        )],
    );
    let result = lint_project(
        LintProjectOptions {
            root: root.path().into(),
            ..Default::default()
        },
        &CancellationToken::default(),
    )
    .unwrap();
    assert_eq!(result.error_count, 4, "{result:?}");
    let diagnostics = &result.files[0].diagnostics;
    assert_eq!(diagnostics.len(), 4);
    assert!(
        diagnostics.iter().all(
            |diagnostic| diagnostic.message_id.as_deref() == Some("unresolved")
                && diagnostic.diagnostic.message.contains("UTF-16")
        )
    );
}

#[test]
fn utf16_attributes_remain_distinct_in_cached_lint_and_resolved_module_graphs() {
    let source = r#"
import './dep' with {key:'\ud800'};
import './dep' with {key:'\udfff'};
import './dep' with {key:'\ufffd'};
import './dep' with {key:'\u{d800}'};
void import('./dep', {with:{'\ud800':'\udfff'}});
"#;
    let root = project(
        "[lint]\nrecommended=false\n[lint.rules]\n'js/no-duplicate-imports'='error'",
        &[("entry.js", source), ("dep.js", "export const value=1;\n")],
    );
    let options = LintProjectOptions {
        root: root.path().into(),
        cache: true,
        ..Default::default()
    };
    let run = || {
        serde_json::to_value(lint_project(options.clone(), &CancellationToken::default()).unwrap())
            .unwrap()
    };
    let cold = run();
    assert_eq!(cold["errorCount"], 1);
    assert_eq!(cold["cache"]["misses"], 2);
    let warm = run();
    assert_eq!(warm["files"], cold["files"]);
    assert_eq!(warm["cache"]["hits"], 2);
    let changed = source.replace("u{d800}", "u{d801}");
    fs::write(root.path().join("entry.js"), &changed).unwrap();
    let refreshed = run();
    assert_eq!(refreshed["errorCount"], 0);
    assert_eq!(refreshed["cache"]["misses"], 1);

    fs::write(root.path().join("wake.config.toml"), "[lint]\nrecommended=false\n[lint.rules]\n'import/no-duplicates'='error'\n'import/no-unresolved'={level='error',options={report_unknown=true}}").unwrap();
    assert_eq!(run()["errorCount"], 0);
    fs::write(
        root.path().join("entry.js"),
        source.replacen("'./dep'", "'./dep.js'", 1),
    )
    .unwrap();
    let resolved = run();
    assert_eq!(resolved["errorCount"], 1, "{resolved}");
    assert_eq!(resolved["cache"]["bypassed"], 2);
}

#[test]
fn module_diagnostics_apply_real_directives_before_baselines_and_fix_previews() {
    use wake_app::{LintBaselineMode, LintBaselineOptions};
    let root = project(
        "[lint]\nrecommended=false\n[lint.rules]\n'import/no-cycle'='error'\n'style/eol-last'='error'",
        &[
            (
                "a.ts",
                "// wake-lint-disable-next-line import/no-cycle\nimport './b';\n",
            ),
            ("b.ts", "import './a';"),
        ],
    );
    let mut options = LintProjectOptions {
        root: root.path().into(),
        baseline: Some(LintBaselineOptions {
            path: "baseline.json".into(),
            mode: LintBaselineMode::Generate,
        }),
        ..Default::default()
    };
    let generated = lint_project(options.clone(), &CancellationToken::default()).unwrap();
    assert_eq!(generated.baseline.unwrap().entries, 2);
    let baseline = fs::read_to_string(root.path().join("baseline.json")).unwrap();
    assert!(
        !baseline.contains("a.ts"),
        "directive suppressed finding must not enter baseline"
    );
    options.baseline.as_mut().unwrap().mode = LintBaselineMode::Check;
    options.fix = LintFixMode::DryRun;
    let checked = lint_project(options.clone(), &CancellationToken::default()).unwrap();
    assert_eq!(checked.error_count, 0, "{checked:?}");
    assert_eq!(checked.baseline.unwrap().suppressed, 2);
    assert!(checked.files.iter().all(|file| !file.changed));
    fs::write(root.path().join("b.ts"), "export {};\n").unwrap();
    options.fix = LintFixMode::Off;
    let changed = lint_project(options, &CancellationToken::default()).unwrap();
    assert!(
        changed.files[0]
            .diagnostics
            .iter()
            .any(
                |d| d.diagnostic.code.as_deref() == Some("wake/unused-disable")
                    && d.message_id.as_deref() == Some("unused")
            ),
        "{changed:?}"
    );
}

// A small valid stored ZIP fixture generated from our own text, including CRC32. No vendored bytes.
fn stored_zip(files: &[(&str, &str)]) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut central = Vec::new();
    for (name, text) in files {
        let crc = !text.bytes().fold(!0u32, |crc, byte| {
            (0..8).fold(crc ^ byte as u32, |crc, _| {
                (crc >> 1) ^ (0xedb8_8320 & (0u32.wrapping_sub(crc & 1)))
            })
        });
        let offset = bytes.len() as u32;
        let mut local = vec![0u8; 30];
        local[0..4].copy_from_slice(&0x0403_4b50u32.to_le_bytes());
        local[4..6].copy_from_slice(&20u16.to_le_bytes());
        local[14..18].copy_from_slice(&crc.to_le_bytes());
        local[18..22].copy_from_slice(&(text.len() as u32).to_le_bytes());
        local[22..26].copy_from_slice(&(text.len() as u32).to_le_bytes());
        local[26..28].copy_from_slice(&(name.len() as u16).to_le_bytes());
        bytes.extend(local);
        bytes.extend_from_slice(name.as_bytes());
        bytes.extend_from_slice(text.as_bytes());
        let mut header = vec![0u8; 46];
        header[0..4].copy_from_slice(&0x0201_4b50u32.to_le_bytes());
        header[4..6].copy_from_slice(&20u16.to_le_bytes());
        header[6..8].copy_from_slice(&20u16.to_le_bytes());
        header[16..20].copy_from_slice(&crc.to_le_bytes());
        header[20..24].copy_from_slice(&(text.len() as u32).to_le_bytes());
        header[24..28].copy_from_slice(&(text.len() as u32).to_le_bytes());
        header[28..30].copy_from_slice(&(name.len() as u16).to_le_bytes());
        header[42..46].copy_from_slice(&offset.to_le_bytes());
        central.extend(header);
        central.extend_from_slice(name.as_bytes());
    }
    let mut end = vec![0u8; 22];
    end[0..4].copy_from_slice(&0x0605_4b50u32.to_le_bytes());
    end[8..10].copy_from_slice(&(files.len() as u16).to_le_bytes());
    end[10..12].copy_from_slice(&(files.len() as u16).to_le_bytes());
    end[12..16].copy_from_slice(&(central.len() as u32).to_le_bytes());
    end[16..20].copy_from_slice(&(bytes.len() as u32).to_le_bytes());
    bytes.extend(central);
    bytes.extend(end);
    bytes
}

#[test]
fn pnp_virtual_peers_keep_distinct_graphs_and_watch_the_shared_physical_archive() {
    use std::{
        thread,
        time::{Duration, Instant},
    };
    use wake_app::{LintWatchEvent, LintWatcher};
    let root = project(
        "[lint]\nrecommended=false\n[lint.rules]\n'import/no-cycle'='error'\n'import/no-unresolved'='error'\n'import/no-duplicates'='error'",
        &[
            ("a.ts", "import 'first'; import 'second';"),
            ("package.json", r#"{"name":"app","main":"a.ts"}"#),
            ("peers/a/index.js", "export {};"),
            ("peers/b/index.js", "import 'app';"),
            (".pnp.cjs", "module.exports = require('./.pnp.data.json');"),
        ],
    );
    let location =
        |peer| format!("./.yarn/__virtual__/pkg-{peer}/0/cache/pkg.zip/node_modules/pkg/");
    fs::write(root.path().join(".pnp.data.json"), serde_json::json!({
        "dependencyTreeRoots":[{"name":"app","reference":"workspace:."}],
        "packageRegistryData":[
            ["app",[["workspace:.",{"packageLocation":"./","packageDependencies":[["app","workspace:."],["first",["pkg","virtual:a#npm:1"]],["second",["pkg","virtual:b#npm:1"]]]}]]],
            ["pkg",[
                ["virtual:a#npm:1",{"packageLocation":location("a"),"packageDependencies":[["pkg","virtual:a#npm:1"],["peer","npm:a"]]}],
                ["virtual:b#npm:1",{"packageLocation":location("b"),"packageDependencies":[["pkg","virtual:b#npm:1"],["peer","npm:b"]]}]
            ]],
            ["peer",[
                ["npm:a",{"packageLocation":"./peers/a/","packageDependencies":[["peer","npm:a"]]}],
                ["npm:b",{"packageLocation":"./peers/b/","packageDependencies":[["peer","npm:b"],["app","workspace:."]]}]
            ]]
        ]
    }).to_string()).unwrap();
    let archive = root.path().join(".yarn/cache/pkg.zip");
    fs::create_dir_all(archive.parent().unwrap()).unwrap();
    let zipped = |source| {
        stored_zip(&[
            (
                "node_modules/pkg/package.json",
                r#"{"name":"pkg","main":"index.js"}"#,
            ),
            ("node_modules/pkg/index.js", source),
        ])
    };
    fs::write(&archive, zipped("import 'peer';")).unwrap();
    let options = LintProjectOptions {
        root: root.path().into(),
        paths: vec!["a.ts".into()],
        ..Default::default()
    };
    let checked = lint_project(options.clone(), &CancellationToken::default()).unwrap();
    assert_eq!(checked.error_count, 1, "{checked:?}");
    let diagnostic = &checked.files[0].diagnostics[0];
    assert_eq!(diagnostic.message_id.as_deref(), Some("cycle"));
    assert_eq!(diagnostic.diagnostic.start, Some(23));
    let context = LintContext::create(options).unwrap();
    let watcher = LintWatcher::start(context.clone()).unwrap();
    let await_errors = |count| {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if watcher.drain_events().iter().any(|event| matches!(event, LintWatchEvent::Checked { snapshot } if snapshot.result.error_count == count)) { break; }
            assert!(
                Instant::now() < deadline,
                "archive watch did not report {count} errors"
            );
            thread::sleep(Duration::from_millis(10));
        }
    };
    await_errors(1);
    fs::write(&archive, zipped("export {};")).unwrap();
    await_errors(0);
    watcher.stop();
    context.close();
}

#[test]
fn project_graph_uses_aliases_and_rechecks_dependencies_without_single_file_cache_hits() {
    let root = project(
        "[alias]\ncustom='src'\n[lint]\nrecommended=false\n[lint.rules]\n'import/no-cycle'='error'\n'import/no-unresolved'='error'",
        &[
            ("src/a.ts", "import 'custom/b';"),
            ("src/b.ts", "import './a';"),
        ],
    );
    let options = LintProjectOptions {
        root: root.path().into(),
        paths: vec!["src/a.ts".into()],
        cache: true,
        ..Default::default()
    };
    for _ in 0..2 {
        let result = lint_project(options.clone(), &CancellationToken::default()).unwrap();
        assert_eq!(result.files.len(), 1);
        assert_eq!(result.error_count, 1);
        let cache = result.cache.unwrap();
        assert_eq!(
            (cache.hits, cache.misses, cache.writes, cache.bypassed),
            (0, 0, 0, 1)
        );
    }
    fs::write(root.path().join("src/b.ts"), "export {};").unwrap();
    assert_eq!(
        lint_project(options.clone(), &CancellationToken::default())
            .unwrap()
            .error_count,
        0
    );
    fs::remove_file(root.path().join("src/b.ts")).unwrap();
    let missing = lint_project(options, &CancellationToken::default()).unwrap();
    assert!(
        missing.files[0]
            .diagnostics
            .iter()
            .any(|d| d.diagnostic.code.as_deref() == Some("import/no-unresolved"))
    );
    assert!(!root.path().join(".wake").exists());
}

#[test]
fn mixed_projects_preserve_single_file_caching_for_files_without_module_rules() {
    let root = project(
        "[lint]\nrecommended=false\n[lint.rules]\n'js/no-debugger'='warn'\n[[lint.overrides]]\nfiles=['a.ts']\n[lint.overrides.rules]\n'import/no-cycle'='error'",
        &[
            ("a.ts", "import './b';"),
            ("b.ts", "import './a'; debugger;"),
        ],
    );
    let options = LintProjectOptions {
        root: root.path().into(),
        cache: true,
        ..Default::default()
    };
    let cold = lint_project(options.clone(), &CancellationToken::default()).unwrap();
    assert_eq!((cold.error_count, cold.warning_count), (1, 1));
    let cache = cold.cache.unwrap();
    assert_eq!((cache.hits, cache.misses, cache.bypassed), (0, 1, 1));
    let cache = lint_project(options, &CancellationToken::default())
        .unwrap()
        .cache
        .unwrap();
    assert_eq!((cache.hits, cache.misses, cache.bypassed), (1, 0, 1));
}

#[test]
fn fix_previews_and_publication_analyze_the_final_project_graph() {
    let root = project(
        "[lint]\nrecommended=false\n[lint.rules]\n'import/no-cycle'='error'\n'js/prefer-const'='error'\n'style/eol-last'='error'",
        &[
            ("a.ts", "let marker = 1;\nimport './b';"),
            ("b.ts", "let marker = 1;\nimport './a';"),
        ],
    );
    let options = LintProjectOptions {
        root: root.path().into(),
        cache: true,
        ..Default::default()
    };
    for fix in [LintFixMode::DryRun, LintFixMode::Write] {
        let result = lint_project(
            LintProjectOptions {
                fix,
                ..options.clone()
            },
            &CancellationToken::default(),
        )
        .unwrap();
        assert_eq!(result.error_count, 2, "{result:?}");
        assert_eq!(result.files.len(), 2);
        assert!(
            result
                .files
                .iter()
                .all(|file| file.changed && file.output.as_ref().unwrap().contains("const marker"))
        );
        for file in &result.files {
            let diagnostic = &file.diagnostics[0];
            assert_eq!(diagnostic.message_id.as_deref(), Some("cycle"));
            let output = file.output.as_ref().unwrap();
            assert_eq!(
                &output[diagnostic.diagnostic.start.unwrap() as usize
                    ..diagnostic.diagnostic.end.unwrap() as usize],
                if file.path == "a.ts" {
                    "'./b'"
                } else {
                    "'./a'"
                }
            );
        }
        assert!(
            result
                .files
                .iter()
                .all(|file| file.written == (fix == LintFixMode::Write))
        );
        assert_eq!(
            fs::read_to_string(root.path().join("a.ts"))
                .unwrap()
                .contains("const marker"),
            fix == LintFixMode::Write
        );
    }
}

#[test]
fn module_failures_do_not_publish_prepared_fixes_or_cache_entries() {
    let original = "import 'pkg';";
    let root = project(
        "[lint]\nrecommended=false\n[lint.rules]\n'import/no-unresolved'='error'\n'style/eol-last'='error'",
        &[("a.ts", original), ("b.ts", "export {};")],
    );
    fs::write(root.path().join(".pnp.cjs"), "truncated").unwrap();
    let error = lint_project(
        LintProjectOptions {
            root: root.path().into(),
            fix: LintFixMode::Write,
            cache: true,
            ..Default::default()
        },
        &CancellationToken::default(),
    )
    .unwrap_err();
    assert_eq!(error.code, "WAKE_LINT_ANALYSIS");
    assert!(error.message.contains("PnP"), "{error}");
    assert_eq!(
        fs::read_to_string(root.path().join("a.ts")).unwrap(),
        original
    );
    assert_eq!(
        fs::read_to_string(root.path().join("b.ts")).unwrap(),
        "export {};"
    );
    assert!(!root.path().join(".wake").exists());
}

#[test]
fn stdin_and_context_documents_join_the_same_owned_dependency_snapshot() {
    let root = project(
        "[lint]\nrecommended=false\n[lint.rules]\n'import/no-cycle'='error'",
        &[("a.ts", "import './b';"), ("b.ts", "import './a';")],
    );
    let result = lint_project(
        LintProjectOptions {
            root: root.path().into(),
            stdin: Some(LintStdin {
                filename: "a.ts".into(),
                text: "export {};".into(),
            }),
            ..Default::default()
        },
        &CancellationToken::default(),
    )
    .unwrap();
    assert_eq!(result.error_count, 0);
    let context = LintContext::create(LintProjectOptions {
        root: root.path().into(),
        paths: vec!["a.ts".into()],
        ..Default::default()
    })
    .unwrap();
    assert_eq!(
        context
            .check(CancellationToken::default())
            .unwrap()
            .result
            .error_count,
        1
    );
    context
        .update_document(LintDocument {
            filename: "b.ts".into(),
            version: 1,
            text: "export {};".into(),
        })
        .unwrap();
    let result = context.check(CancellationToken::default()).unwrap();
    assert_eq!(result.documents["b.ts"], 1);
    assert_eq!(result.result.error_count, 0);
    assert_eq!(
        fs::read_to_string(root.path().join("b.ts")).unwrap(),
        "import './a';"
    );
    context.close();
}
