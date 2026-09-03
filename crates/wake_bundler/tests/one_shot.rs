use std::process::Command;
use std::sync::Arc;

use tempfile::tempdir;
use wake_bundler::{
    BuildOptions, BuildOutput, BuildPlatform, BuildRequest, BuildSession, ModuleFormat,
};
use wake_common::MemoryFileSystem;

fn production_fixture() -> Arc<MemoryFileSystem> {
    Arc::new(MemoryFileSystem::from_files([
        (
            "src/index.js",
            br#"
                import './styles.css';
                import { compute } from './live.js';
                if (process.env.NODE_ENV !== 'production') require('./dead.js');
                const result = compute(20);
                globalThis.__oneShotResult = result;
                console.log('one-shot=' + result);
            "#
            .as_slice(),
        ),
        (
            "src/live.js",
            br#"
                export function compute(value) {
                    const doubled = value * 2;
                    return doubled + 1;
                }
                export function unused() { return 'removed'; }
            "#
            .as_slice(),
        ),
        (
            "src/dead.js",
            b"globalThis.__unexpectedDeadModule = true;".as_slice(),
        ),
        (
            "src/styles.css",
            b"body { color: #123456; background-image: url('./pixel.png'); }".as_slice(),
        ),
        ("src/pixel.png", [0x89, b'P', b'N', b'G'].as_slice()),
    ]))
}

fn production_options(source_map: bool) -> BuildOptions {
    BuildOptions {
        platform: BuildPlatform::Browser,
        module_format: ModuleFormat::Iife,
        extract_css: true,
        asset_inline_limit: 0,
        minify: true,
        dead_module_elimination: true,
        source_map,
        tree_shaking: true,
        ..BuildOptions::default()
    }
}

fn assert_outputs_equal(regular: &BuildOutput, one_shot: &BuildOutput) {
    assert_eq!(regular.bundle, one_shot.bundle, "entry bundle bytes differ");
    assert_eq!(regular.module_count, one_shot.module_count);
    assert_eq!(regular.updated_module_count, one_shot.updated_module_count);
    assert_eq!(regular.cached_module_count, one_shot.cached_module_count);
    assert_eq!(regular.entry_chunk, one_shot.entry_chunk);

    assert_eq!(regular.chunks.len(), one_shot.chunks.len());
    for (regular, one_shot) in regular.chunks.iter().zip(&one_shot.chunks) {
        assert_eq!(regular.name, one_shot.name);
        assert_eq!(regular.file_name, one_shot.file_name);
        assert_eq!(regular.code, one_shot.code, "chunk bytes differ");
        assert_eq!(regular.kind, one_shot.kind);
        assert_eq!(regular.is_entry, one_shot.is_entry);
        assert_eq!(regular.chunk_id, one_shot.chunk_id);
        assert_eq!(regular.module_ids, one_shot.module_ids);
        assert_eq!(regular.imports, one_shot.imports);
        assert_eq!(regular.dynamic_imports, one_shot.dynamic_imports);
        assert_eq!(regular.styles, one_shot.styles);
        assert_eq!(regular.source_map, one_shot.source_map);
    }

    assert_eq!(regular.assets.len(), one_shot.assets.len());
    for (regular, one_shot) in regular.assets.iter().zip(&one_shot.assets) {
        assert_eq!(regular.file_name, one_shot.file_name);
        assert_eq!(regular.bytes, one_shot.bytes, "asset bytes differ");
        assert_eq!(regular.is_css, one_shot.is_css);
        assert_eq!(regular.owner_module_ids, one_shot.owner_module_ids);
        assert_eq!(
            regular.unscoped_css_owner_module_ids,
            one_shot.unscoped_css_owner_module_ids
        );
    }

    assert_eq!(regular.diagnostics.len(), one_shot.diagnostics.len());
    for (regular, one_shot) in regular.diagnostics.iter().zip(&one_shot.diagnostics) {
        assert_eq!(regular.severity, one_shot.severity);
        assert_eq!(regular.code.as_deref(), one_shot.code.as_deref());
        assert_eq!(regular.message, one_shot.message);
        assert_eq!(regular.path, one_shot.path);
        assert_eq!(regular.notes, one_shot.notes);
        assert_eq!(regular.labels.len(), one_shot.labels.len());
        for (regular, one_shot) in regular.labels.iter().zip(&one_shot.labels) {
            assert_eq!(regular.span, one_shot.span);
            assert_eq!(regular.message, one_shot.message);
            assert_eq!(regular.primary, one_shot.primary);
        }
    }
}

fn assert_runtime_semantics(output: &BuildOutput, sourcemap: bool) {
    let directory = tempdir().unwrap();
    let mode = if sourcemap { "mapped" } else { "unmapped" };
    let bundle = directory.path().join(format!("bundle-{mode}.cjs"));
    std::fs::write(&bundle, output.bundle.as_bytes()).unwrap();
    let executed = Command::new("node")
        .arg(&bundle)
        .output()
        .expect("Node.js is required for the one-shot runtime equivalence regression");
    assert!(
        executed.status.success(),
        "mode={mode} stderr={}",
        String::from_utf8_lossy(&executed.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&executed.stdout).trim(),
        "one-shot=41",
        "mode={mode}"
    );
}

fn assert_production_equivalence(sourcemap: bool) {
    let fs = production_fixture();
    let options = production_options(sourcemap);
    let request = BuildRequest::new("src/index.js");
    let mut regular = BuildSession::new(fs.clone(), options.clone());

    let regular = regular.build_current(request.clone());
    let one_shot = BuildSession::new_one_shot(fs, options).build_once(request);

    assert!(!regular.has_errors(), "{:?}", regular.diagnostics);
    assert!(!one_shot.has_errors(), "{:?}", one_shot.diagnostics);
    assert_outputs_equal(&regular, &one_shot);
    assert_eq!(regular.entry().source_map.is_some(), sourcemap);
    assert_eq!(one_shot.entry().source_map.is_some(), sourcemap);
    assert!(
        !regular.assets.is_empty(),
        "fixture must exercise asset emission"
    );
    assert!(
        regular.assets.iter().any(|asset| asset.is_css),
        "fixture must exercise extracted CSS"
    );
    assert!(
        !regular.bundle.contains("__unexpectedDeadModule"),
        "define folding + DME must remove the development-only module"
    );
    assert_runtime_semantics(&one_shot, sourcemap);
}

#[test]
fn one_shot_matches_regular_mapped_and_unmapped_production_builds() {
    for sourcemap in [false, true] {
        assert_production_equivalence(sourcemap);
    }
}

#[test]
fn one_shot_build_once_consumes_the_session_and_produces_executable_output() {
    let fs = production_fixture();
    let session = BuildSession::new_one_shot(fs, production_options(false));
    let consume_once: fn(BuildSession, BuildRequest) -> BuildOutput = BuildSession::build_once;

    let output = consume_once(session, BuildRequest::new("src/index.js"));

    assert!(!output.has_errors(), "{:?}", output.diagnostics);
    assert_eq!(output.cached_module_count, 0);
    assert_eq!(output.updated_module_count, output.module_count);
    assert_runtime_semantics(&output, false);
}
