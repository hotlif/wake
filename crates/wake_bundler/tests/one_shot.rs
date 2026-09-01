use std::any::Any;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use tempfile::tempdir;
use wake_bundler::{BuildOutput, BuildPlatform, IncrementalBundler, ModuleFormat};
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

fn configure_production(bundler: &mut IncrementalBundler, sourcemap: bool) {
    bundler
        .set_platform(BuildPlatform::Browser)
        .set_module_format(ModuleFormat::Iife)
        .enable_minify()
        .enable_tree_shaking()
        .enable_dead_module_elimination()
        .enable_css_extraction()
        .set_asset_inline_limit(0);
    if sourcemap {
        bundler.enable_sourcemap();
    }
}

fn assert_outputs_equal(regular: &BuildOutput, one_shot: &BuildOutput) {
    assert_eq!(regular.bundle, one_shot.bundle, "entry bundle bytes differ");
    assert_eq!(regular.module_count, one_shot.module_count);
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
        assert_eq!(regular.styles, one_shot.styles);
        assert_eq!(regular.source_map, one_shot.source_map);
    }

    assert_eq!(regular.assets.len(), one_shot.assets.len());
    for (regular, one_shot) in regular.assets.iter().zip(&one_shot.assets) {
        assert_eq!(regular.file_name, one_shot.file_name);
        assert_eq!(regular.bytes, one_shot.bytes, "asset bytes differ");
        assert_eq!(regular.is_css, one_shot.is_css);
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
    let mut regular = IncrementalBundler::new(fs.clone());
    let mut one_shot = IncrementalBundler::new_one_shot(fs);
    configure_production(&mut regular, sourcemap);
    configure_production(&mut one_shot, sourcemap);

    let regular = regular.build(Path::new("src/index.js"));
    let one_shot = one_shot.build(Path::new("src/index.js"));

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

fn panic_message(payload: Box<dyn Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "non-string panic payload".to_string()
    }
}

#[test]
fn one_shot_rejects_a_second_build() {
    let fs = production_fixture();
    let mut bundler = IncrementalBundler::new_one_shot(fs);
    configure_production(&mut bundler, false);
    let first = bundler.build(Path::new("src/index.js"));
    assert!(!first.has_errors(), "{:?}", first.diagnostics);
    let task_exec_count = bundler.task_exec_count();
    assert!(
        task_exec_count > 0,
        "one-shot engine release must preserve its observable task count"
    );

    let second = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = bundler.build(Path::new("src/index.js"));
    }));
    let panic = second.expect_err("one-shot bundler must reject a second build");
    assert_eq!(
        panic_message(panic),
        "one-shot IncrementalBundler may only build once"
    );
    assert_eq!(bundler.task_exec_count(), task_exec_count);
}
