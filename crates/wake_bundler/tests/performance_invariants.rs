use std::path::PathBuf;
use std::sync::Arc;

use wake_bundler::{BuildOptions, BuildRequest, BuildSession};
use wake_common::MemoryFileSystem;

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
