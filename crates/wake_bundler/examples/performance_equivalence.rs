//! Stable performance-report probe, compiled from the same source against base and head.
//! Keep the success graph aligned with `benches/bundle.rs::gen_project`.

use std::sync::Arc;

use serde_json::{Value, json};
use wake_bundler::{BuildOptions, BuildOutput, BuildRequest, BuildSession};
use wake_common::{Diagnostic, MemoryFileSystem};

fn bundle_project(n: usize) -> MemoryFileSystem {
    let util = n - 1;
    let mut files = Vec::with_capacity(n);
    for i in 0..n {
        let mut source = String::new();
        if i != util {
            source.push_str(&format!("import u from './m{util}.js';\n"));
        }
        let (left, right) = (2 * i + 1, 2 * i + 2);
        if left < util {
            source.push_str(&format!("import a from './m{left}.js';\n"));
        }
        if right < util {
            source.push_str(&format!("import b from './m{right}.js';\n"));
        }
        source.push_str(&format!("export default {i};\n"));
        files.push((format!("m{i}.js"), source));
    }
    MemoryFileSystem::from_files(files)
}

fn diagnostics(items: &[Diagnostic]) -> Vec<Value> {
    items
        .iter()
        .map(|item| {
            json!({
                "severity": item.severity.as_str(),
                "code": item.code.as_deref(),
                "path": item.path,
                "message": item.message,
                "labels": item.labels.iter().map(|label| json!({
                    "start": label.span.lo,
                    "end": label.span.hi,
                    "message": label.message,
                    "primary": label.primary,
                })).collect::<Vec<_>>(),
                "notes": item.notes,
            })
        })
        .collect()
}

fn output(value: &BuildOutput) -> Value {
    json!({
        "bundle": value.bundle,
        "chunks": value.chunks.iter().map(|chunk| json!({
            "name": chunk.name,
            "fileName": chunk.file_name,
            "code": chunk.code,
            "kind": chunk.kind.as_str(),
            "isEntry": chunk.is_entry,
            "imports": chunk.imports,
            "dynamicImports": chunk.dynamic_imports,
            "styles": chunk.styles,
            "sourceMap": chunk.source_map,
        })).collect::<Vec<_>>(),
        "assets": value.assets.iter().map(|asset| json!({
            "fileName": asset.file_name,
            "bytes": asset.bytes,
            "isCss": asset.is_css,
        })).collect::<Vec<_>>(),
    })
}

fn main() {
    let success = BuildSession::new(Arc::new(bundle_project(2000)), BuildOptions::default())
        .build_current(BuildRequest::new("m0.js"));
    assert!(!success.has_errors(), "success probe returned errors");
    assert_eq!(success.module_count, 2000, "success probe graph changed");

    let failure_fs = Arc::new(MemoryFileSystem::from_files([(
        "index.js",
        "import './missing.js';\nconsole.log('unreachable');\n",
    )]));
    let failure = BuildSession::new(failure_fs, BuildOptions::default())
        .build_current(BuildRequest::new("index.js"));
    assert!(
        failure.has_errors(),
        "missing dependency probe unexpectedly succeeded"
    );

    let result = json!({
        "schemaVersion": "wake.performance-equivalence.v1",
        "cases": [
            {
                "id": "bundle_2k",
                "output": output(&success),
                "diagnostics": diagnostics(&success.diagnostics),
            },
            {
                "id": "missing_dependency",
                "output": output(&failure),
                "diagnostics": diagnostics(&failure.diagnostics),
            },
        ],
    });
    println!("{result}");
}
