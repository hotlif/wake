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
        let directory = tempfile::tempdir().unwrap();
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
