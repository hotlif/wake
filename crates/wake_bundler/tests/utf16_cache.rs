use std::process::Command;
use std::sync::Arc;

use wake_bundler::{BuildOptions, BuildRequest, BuildSession};
use wake_common::MemoryFileSystem;

fn execute(code: &str) -> String {
    let script = format!("{code}\nprocess.stdout.write(JSON.stringify(globalThis.__wake_result));");
    let output = Command::new("node")
        .arg("-e")
        .arg(script)
        .output()
        .expect("Node runtime required");
    assert!(
        output.status.success(),
        "{}\n{code}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn utf16_values_survive_fresh_session_cache_hits_and_content_invalidation() {
    for minify in [false, true] {
        let directory = tempfile::Builder::new()
            .prefix(".tmp-utf16-cache-")
            .tempdir_in(env!("CARGO_MANIFEST_DIR"))
            .unwrap();
        let fs = Arc::new(MemoryFileSystem::new());
        fs.insert(
            "src/entry.js",
            br#"
import {high,pair,low} from './values.js';
const keys={"\ud800":1,"\udfff":2};
globalThis.__wake_result=[high,pair,low,keys["\ud800"],keys[low]];
"#
            .to_vec(),
        );
        let values = r#"export const high="\ud800",pair="\ud83d"+"\udc4d",low="\udfff";"#;
        fs.insert("src/values.js", values.as_bytes().to_vec());
        let options = BuildOptions {
            minify,
            source_map: true,
            persistent_cache: Some(directory.path().join("cache.bin")),
            ..BuildOptions::default()
        };
        let cold = BuildSession::new_one_shot(fs.clone(), options.clone())
            .build_once(BuildRequest::new("src/entry.js"));
        assert!(!cold.has_errors(), "{:?}", cold.diagnostics);
        let warm = BuildSession::new_one_shot(fs.clone(), options.clone())
            .build_once(BuildRequest::new("src/entry.js"));
        assert!(!warm.has_errors(), "{:?}", warm.diagnostics);
        assert_eq!(warm.cached_module_count, 2);
        assert_eq!(cold.bundle, warm.bundle);
        assert_eq!(execute(&cold.bundle), r#"["\ud800","👍","\udfff",1,2]"#);
        assert_eq!(execute(&warm.bundle), execute(&cold.bundle));

        fs.insert(
            "src/values.js",
            values.replace("ud800", "ud801").into_bytes(),
        );
        let changed = BuildSession::new_one_shot(fs.clone(), options)
            .build_once(BuildRequest::new("src/entry.js"));
        assert!(!changed.has_errors(), "{:?}", changed.diagnostics);
        assert!(changed.cached_module_count < warm.cached_module_count);
        assert_eq!(execute(&changed.bundle), r#"["\ud801","👍","\udfff",1,2]"#);
    }
}

#[test]
fn static_unicode_request_cache_identity_survives_rejected_surrogates() {
    for minify in [false, true] {
        let directory = tempfile::Builder::new()
            .prefix(".tmp-utf16-module-cache-")
            .tempdir_in(env!("CARGO_MANIFEST_DIR"))
            .unwrap();
        let fs = Arc::new(MemoryFileSystem::new());
        let valid = "import {value} from './é.js'; globalThis.__wake_result=value;";
        fs.insert("src/entry.js", valid.as_bytes().to_vec());
        fs.insert("src/é.js", b"export const value=1;".to_vec());
        fs.insert("src/�.js", b"export const value=99;".to_vec());
        let options = BuildOptions {
            minify,
            persistent_cache: Some(directory.path().join("cache.bin")),
            ..BuildOptions::default()
        };
        let build = || {
            BuildSession::new_one_shot(fs.clone(), options.clone())
                .build_once(BuildRequest::new("src/entry.js"))
        };
        let cold = build();
        assert!(!cold.has_errors(), "{:?}", cold.diagnostics);
        let warm = build();
        assert!(!warm.has_errors(), "{:?}", warm.diagnostics);
        assert_eq!(warm.cached_module_count, 2);
        assert_eq!(cold.bundle, warm.bundle);
        assert_eq!(execute(&warm.bundle), "1");

        fs.insert(
            "src/entry.js",
            br"import {value} from './\ud800.js'; globalThis.__wake_result=value;".to_vec(),
        );
        let rejected = build();
        assert!(rejected.has_errors(), "{:?}", rejected.diagnostics);
        assert!(
            rejected
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains("UTF-8")),
            "{:?}",
            rejected.diagnostics
        );

        fs.insert("src/entry.js", valid.as_bytes().to_vec());
        let restored = build();
        assert!(!restored.has_errors(), "{:?}", restored.diagnostics);
        assert_eq!(restored.cached_module_count, 2);
        assert_eq!(restored.bundle, warm.bundle);
        assert_eq!(execute(&restored.bundle), "1");

        fs.insert("src/é.js", b"export const value=2;".to_vec());
        let changed = build();
        assert!(!changed.has_errors(), "{:?}", changed.diagnostics);
        assert!(changed.cached_module_count < restored.cached_module_count);
        assert_eq!(execute(&changed.bundle), "2");
    }
}
