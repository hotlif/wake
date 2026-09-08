use std::path::PathBuf;
use std::sync::Arc;

use wake_bundler::{BuildOptions, BuildRequest, BuildSession};
use wake_common::MemoryFileSystem;

fn candidate_fixture(value: &str) -> Arc<MemoryFileSystem> {
    let mut files = vec![(
        "index.js".to_owned(),
        (0..128)
            .map(|id| format!("import './m{id}.js';\n"))
            .collect::<String>(),
    )];
    files.extend((0..128).map(|id| {
        (
            format!("m{id}.js"),
            format!(
                "console.log({id}, {});",
                if id == 127 { value } else { "0" }
            ),
        )
    }));
    Arc::new(MemoryFileSystem::from_files(files))
}

#[test]
fn locally_bound_require_does_not_load_a_second_bundled_runtime() {
    let sources = [
        "function local(require){return require('./missing.js')}console.log(local(x=>x));",
        "function local(){return require('./missing.js');var require;}console.log(local);",
        "{const require=x=>x;console.log((require)('./missing.js'));}",
        "try{throw x=>x}catch(require){console.log(require('./missing.js'));}",
        "function local({require}){return require('./missing.js')}console.log(local({require:x=>x}));",
    ];
    for source in sources {
        for minify in [false, true] {
            for tree_shaking in [false, true] {
                let fs = Arc::new(MemoryFileSystem::from_files([("index.js", source)]));
                let mut session = BuildSession::new(
                    fs,
                    BuildOptions {
                        minify,
                        tree_shaking,
                        ..BuildOptions::default()
                    },
                );
                let output = session.build_current(BuildRequest::new("index.js"));
                assert!(!output.has_errors(), "{source}: {:?}", output.diagnostics);
                assert_eq!(output.module_count, 1);
                assert_eq!(session.load_exec_count(), 1);
            }
        }
    }
}

#[test]
fn require_binding_edits_and_persistent_summaries_match_cold_builds() {
    let directory = tempfile::tempdir().unwrap();
    let fs = Arc::new(MemoryFileSystem::from_files([
        (
            "index.js",
            "function local(require){return require('./missing.js')}console.log(local(x=>x),require('./real.js'));",
        ),
        ("real.js", "module.exports=42;"),
    ]));
    let options = BuildOptions {
        persistent_cache: Some(directory.path().join("cache")),
        source_map: true,
        ..BuildOptions::default()
    };
    let request = BuildRequest::new("index.js");
    let first = BuildSession::new(fs.clone(), options.clone()).build_current(request.clone());
    assert!(!first.has_errors(), "{:?}", first.diagnostics);
    assert_eq!(first.module_count, 2);
    let mut reopened = BuildSession::new(fs.clone(), options.clone());
    let hot = reopened.build_current(request.clone());
    assert_same_output(&hot, &first);
    assert_eq!(
        hot.cached_module_count, 2,
        "fresh session must exercise persistent summaries"
    );

    fs.insert("index.js", "function local(){return require('./missing.js')}console.log(local(),require('./real.js'));");
    let changed = [PathBuf::from("index.js")];
    let mut edited = reopened.fork(fs.clone(), options.clone(), Some((&changed, false)));
    let failed = edited.build_current(request.clone());
    assert!(
        failed.has_errors(),
        "removing the binding must restore dependency resolution"
    );
    assert!(
        failed
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("./missing.js"))
    );
    fs.insert("missing.js", "module.exports=7;");
    let cold_options = BuildOptions {
        persistent_cache: None,
        ..options.clone()
    };
    let cold = BuildSession::new(fs.clone(), cold_options).build_current(request.clone());
    let mut recovered = reopened.fork(fs, options, None);
    assert_same_output(&recovered.build_current(request), &cold);
    assert_eq!(cold.module_count, 3);
}

fn assert_same_output(actual: &wake_bundler::BuildOutput, cold: &wake_bundler::BuildOutput) {
    assert!(!actual.has_errors(), "{:?}", actual.diagnostics);
    assert!(!cold.has_errors(), "{:?}", cold.diagnostics);
    assert_eq!(actual.bundle, cold.bundle);
    assert_eq!(actual.module_count, cold.module_count);
    assert_eq!(
        actual
            .chunks
            .iter()
            .map(|c| (&c.file_name, &c.code, &c.source_map))
            .collect::<Vec<_>>(),
        cold.chunks
            .iter()
            .map(|c| (&c.file_name, &c.code, &c.source_map))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        actual
            .assets
            .iter()
            .map(|a| (&a.file_name, &a.bytes))
            .collect::<Vec<_>>(),
        cold.assets
            .iter()
            .map(|a| (&a.file_name, &a.bytes))
            .collect::<Vec<_>>()
    );
}

#[test]
fn candidate_forks_reuse_compilation_and_isolate_failure() {
    let options = BuildOptions {
        source_map: true,
        code_splitting: true,
        css_in_js: true,
        ..BuildOptions::default()
    };
    let request = BuildRequest::new("index.js");
    let mut accepted = BuildSession::new(candidate_fixture("1"), options.clone());
    let first = accepted.build_current(request.clone());
    assert!(!first.has_errors());
    let mut rescan = accepted.fork(candidate_fixture("1"), options.clone(), None);
    let scanned = rescan.build_current(request.clone());
    assert_eq!(scanned.updated_module_count, 0);
    assert_eq!(scanned.cached_module_count, 129);
    assert_eq!(
        rescan.load_exec_count(),
        129,
        "Rescan must still reread authoritative inputs"
    );
    assert_same_output(&scanned, &first);

    let changed = [PathBuf::from("m127.js")];
    let mut edited = accepted.fork(
        candidate_fixture("2"),
        options.clone(),
        Some((&changed, false)),
    );
    let output = edited.build_current(request.clone());
    assert_eq!(output.updated_module_count, 1);
    assert_eq!(output.cached_module_count, 128);
    assert_eq!(edited.load_exec_count(), 1);
    let cold =
        BuildSession::new(candidate_fixture("2"), options.clone()).build_current(request.clone());
    assert_same_output(&output, &cold);
    assert_same_output(&accepted.build_current(request.clone()), &first);

    let mut broken = accepted.fork(
        candidate_fixture("("),
        options.clone(),
        Some((&changed, false)),
    );
    assert!(broken.build_current(request.clone()).has_errors());
    assert_same_output(&accepted.build_current(request.clone()), &first);
    let mut recovered = accepted.fork(candidate_fixture("3"), options.clone(), None);
    let output = recovered.build_current(request.clone());
    assert_eq!(output.updated_module_count, 1);
    let cold = BuildSession::new(candidate_fixture("3"), options).build_current(request);
    assert_same_output(&output, &cold);
}

#[test]
fn candidate_forks_revalidate_structure_and_changed_options() {
    let options = BuildOptions {
        code_splitting: true,
        source_map: true,
        ..BuildOptions::default()
    };
    let request = BuildRequest::new("index.js");
    let mut accepted = BuildSession::new(candidate_fixture("1"), options.clone());
    assert!(!accepted.build_current(request.clone()).has_errors());
    let fs = candidate_fixture("2");
    fs.insert("m127.js", "import('./added.js').then(console.log);");
    fs.insert("added.js", "export const added = 123;");
    let changed = [PathBuf::from("m127.js"), PathBuf::from("added.js")];
    let mut candidate = accepted.fork(fs.clone(), options.clone(), Some((&changed, true)));
    let output = candidate.build_current(request.clone());
    let cold = BuildSession::new(fs.clone(), options.clone()).build_current(request.clone());
    assert_same_output(&output, &cold);
    let removed = candidate_fixture("2");
    let mut without_chunk =
        candidate.fork(removed.clone(), options.clone(), Some((&changed, true)));
    let output = without_chunk.build_current(request.clone());
    let cold = BuildSession::new(removed, options.clone()).build_current(request.clone());
    assert_same_output(&output, &cold);
    let changed_options = BuildOptions {
        define: vec![("console.log".into(), "console.warn".into())],
        ..options
    };
    let mut reconfigured = accepted.fork(fs.clone(), changed_options.clone(), Some((&[], false)));
    let output = reconfigured.build_current(request.clone());
    assert_eq!(
        output.cached_module_count, 0,
        "changed semantic options must not inherit tasks"
    );
    let cold = BuildSession::new(fs, changed_options).build_current(request);
    assert_same_output(&output, &cold);
}

#[test]
fn candidate_css_token_changes_match_cold_output() {
    fn fixture(color: &str) -> Arc<MemoryFileSystem> {
        Arc::new(MemoryFileSystem::from_files([
            ("node_modules/@crab-dev/css/package.json".to_owned(),
                r#"{"name":"@crab-dev/css","version":"0.1.32","main":"index.js"}"#.to_owned()),
            ("node_modules/@crab-dev/css/index.js".to_owned(), "export const css = () => {};".to_owned()),
            ("tokens.js".to_owned(), format!("export const color = '{color}';")),
            ("index.js".to_owned(), "import { css } from '@crab-dev/css'; import { color } from './tokens.js'; export const box = css`color: ${color};`;".to_owned()),
        ]))
    }
    for extract_css in [false, true] {
        let options = BuildOptions {
            css_in_js: true,
            extract_css,
            source_map: true,
            ..BuildOptions::default()
        };
        let request = BuildRequest::new("index.js");
        let mut accepted = BuildSession::new(fixture("red"), options.clone());
        let first = accepted.build_current(request.clone());
        assert!(!first.has_errors());
        let mut rescan = accepted.fork(fixture("red"), options.clone(), None);
        let scanned = rescan.build_current(request.clone());
        assert_same_output(&scanned, &first);
        assert_eq!(scanned.updated_module_count, 0);
        let changed = [PathBuf::from("tokens.js")];
        let mut candidate =
            accepted.fork(fixture("blue"), options.clone(), Some((&changed, false)));
        let output = candidate.build_current(request.clone());
        let cold = BuildSession::new(fixture("blue"), options).build_current(request.clone());
        assert_same_output(&output, &cold);
        assert_same_output(&accepted.build_current(request), &first);
        let css = if extract_css {
            String::from_utf8(
                output
                    .assets
                    .iter()
                    .find(|asset| asset.is_css)
                    .unwrap()
                    .bytes
                    .clone(),
            )
            .unwrap()
        } else {
            output.bundle
        };
        assert!(css.contains("color: blue"), "{css}");
        assert!(!css.contains("color: red"), "{css}");
    }
}

/// Stable work-count gate for the edit-one path. This intentionally asserts architectural work
/// avoidance rather than wall-clock milliseconds, which are too noisy on shared CI runners.
#[test]
fn edit_one_keeps_scan_link_and_codegen_work_local() {
    const MODULES: usize = 2_000;
    let mut files = Vec::with_capacity(MODULES);
    for id in 0..MODULES {
        let left = id * 2 + 1;
        let right = id * 2 + 2;
        let mut source = String::new();
        if left < MODULES {
            source.push_str(&format!("import './m{left}.js';\n"));
        }
        if right < MODULES {
            source.push_str(&format!("import './m{right}.js';\n"));
        }
        source.push_str(&format!("export const value = {id};\n"));
        files.push((format!("m{id}.js"), source));
    }

    let fs = Arc::new(MemoryFileSystem::from_files(files));
    let mut session = BuildSession::new(
        fs.clone(),
        BuildOptions {
            tree_shaking: true,
            code_splitting: true,
            ..BuildOptions::default()
        },
    );
    let request = BuildRequest::new("m0.js");
    let first = session.build_current(request.clone());
    assert!(!first.has_errors(), "{:?}", first.diagnostics);
    assert_eq!(first.module_count, MODULES);

    let loads = session.load_exec_count();
    let resolves = session.resolve_exec_count();
    let topology_reuses = session.topology_reuse_count();
    let link_reuses = session.link_plan_reuse_count();
    let changed = PathBuf::from(format!("m{}.js", MODULES - 1));
    fs.insert(&changed, "export const value = 9999;\n");
    session.invalidate_paths(std::slice::from_ref(&changed), false);
    let rebuilt = session.build_current(request);

    assert!(!rebuilt.has_errors(), "{:?}", rebuilt.diagnostics);
    assert_eq!(rebuilt.updated_module_count, 1);
    assert_eq!(rebuilt.cached_module_count, MODULES - 1);
    assert_eq!(session.load_exec_count() - loads, 1);
    assert_eq!(session.resolve_exec_count(), resolves);
    assert_eq!(session.topology_reuse_count() - topology_reuses, 1);
    assert_eq!(session.link_plan_reuse_count() - link_reuses, 1);
}
