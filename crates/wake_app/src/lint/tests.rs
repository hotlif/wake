use super::*;
use std::fs;

#[test]
fn markdown_virtual_source_preserves_offsets_and_only_exposes_javascript_fences() {
    let source = "# 标题\n\n```js\ndebugger;\n```\n\n```ts\nconst value: string = 'x';\n```\n";
    let virtual_source = markdown_virtual_source(source);
    assert_eq!(virtual_source.len(), source.len());
    let debugger_start = source.find("debugger").unwrap();
    assert_eq!(
        &virtual_source[debugger_start..debugger_start + "debugger".len()],
        "debugger"
    );
    let typescript_start = source.find("const value").unwrap();
    assert!(virtual_source[typescript_start..].starts_with("          "));
    let heading_start = source.find('#').unwrap();
    assert_eq!(&virtual_source[heading_start..heading_start + 1], " ");
}

fn execution_matrix(workers: usize) -> serde_json::Value {
    let root = tempfile::tempdir().unwrap();
    let scheduler = execution::Scheduler::new(Arc::new(wake_turbo::Executor::new(workers)));
    fs::write(root.path().join("wake.config.toml"), "[lint.rules]\n'style/quotes'='warn'\n'style/eol-last'='error'\n'react/self-closing-comp'='error'\n[[lint.overrides]]\nfiles=['**/*.ts']\n[lint.overrides.rules]\n'js/no-debugger'='warn'\n").unwrap();
    let sources = [
        ("a.js", "debugger;\nconst greeting = \"🙂\";"),
        ("b.ts", "const value: string = \"hello\";\ndebugger;"),
        ("c.jsx", "const el = <Panel title=\"keep\"></Panel>;"),
        ("d.js", "x == y;"),
        (
            "e.js",
            "// wake-lint-disable-next-line js/no-debugger\ndebugger;",
        ),
        ("f.cjs", "module.exports = 1;"),
        ("g.tsx", "const el = <main></main>;"),
        ("h.mjs", "export const n = 1;"),
        ("i.js", "switch (x) {case 1:break;case 1:break;}"),
    ];
    for (path, text) in sources {
        fs::write(root.path().join(path), text).unwrap();
    }
    let mut results = Vec::new();
    let mut check = |options: &LintProjectOptions,
                     documents: &BTreeMap<String, Arc<LintDocument>>| {
        let result = lint_project_scheduled(
            options.clone(),
            &CancellationToken::default(),
            documents,
            None,
            Some(&scheduler),
        )
        .unwrap();
        let result = serde_json::to_value(result).unwrap();
        results.push(result.clone());
        result
    };
    let mut options = LintProjectOptions {
        root: root.path().into(),
        cache: true,
        ..Default::default()
    };
    let documents = BTreeMap::new();
    let cold = check(&options, &documents);
    assert_eq!(cold["cache"]["misses"], 9);
    assert_eq!(check(&options, &documents)["cache"]["hits"], 9);
    fs::write(
        root.path().join("b.ts"),
        "debugger;\nconst edited: number = 2;\n",
    )
    .unwrap();
    assert_eq!(check(&options, &documents)["cache"]["misses"], 1);
    let overlays = BTreeMap::from([
        (
            "a.js".into(),
            Arc::new(LintDocument {
                filename: "a.js".into(),
                version: 1,
                text: "let changed = 1;".into(),
            }),
        ),
        (
            "unsaved.ts".into(),
            Arc::new(LintDocument {
                filename: "unsaved.ts".into(),
                version: 2,
                text: "debugger;".into(),
            }),
        ),
    ]);
    assert_eq!(check(&options, &overlays)["cache"]["bypassed"], 2);
    options.baseline = Some(LintBaselineOptions {
        path: "baseline.json".into(),
        mode: LintBaselineMode::Generate,
    });
    assert_eq!(check(&options, &documents)["errorCount"], 0);
    options.baseline.as_mut().unwrap().mode = LintBaselineMode::Check;
    check(&options, &documents);
    check(&options, &documents);
    fs::write(root.path().join("d.js"), "run();\n").unwrap();
    options.baseline.as_mut().unwrap().mode = LintBaselineMode::Prune;
    check(&options, &documents);
    options.baseline.as_mut().unwrap().mode = LintBaselineMode::Check;
    options.fix = LintFixMode::DryRun;
    // Suppressed diagnostics must not leak into fixes in any round or batch.
    assert!(
        check(&options, &documents)["files"]
            .as_array()
            .unwrap()
            .iter()
            .all(|file| file["changed"] == false)
    );
    options.baseline = None;
    let preview = check(&options, &documents);
    assert!(
        preview["files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|file| file["changed"] == true)
    );
    options.fix = LintFixMode::Write;
    let written = check(&options, &documents);
    for file in written["files"].as_array().unwrap() {
        if file["written"] == true {
            assert_eq!(
                fs::read_to_string(root.path().join(file["path"].as_str().unwrap())).unwrap(),
                file["output"].as_str().unwrap()
            );
        }
    }
    options.fix = LintFixMode::Off;
    check(&options, &documents);
    let output: BTreeMap<_, _> = sources
        .into_iter()
        .map(|(path, _)| (path, fs::read_to_string(root.path().join(path)).unwrap()))
        .collect();
    serde_json::json!({"results":results,"source":output,"baseline":fs::read_to_string(root.path().join("baseline.json")).unwrap()})
}

#[test]
fn serial_and_parallel_projects_have_identical_diagnostics_fixes_cache_and_baseline() {
    assert_eq!(execution_matrix(1), execution_matrix(4));
}
