use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn fixture_file() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/hello-esm/src/index.js")
}

#[test]
fn parse_json_keeps_stdout_machine_readable() {
    let output = Command::new(env!("CARGO_BIN_EXE_wake"))
        .args(["parse", fixture_file().to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(value["statementCount"].as_u64().unwrap() > 0);
    assert!(value["diagnostics"].is_array());

    let tsx_fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/react-docs/docs/components/demos/basic.demo.tsx");
    let output = Command::new(env!("CARGO_BIN_EXE_wake"))
        .args(["parse", tsx_fixture.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["diagnostics"], serde_json::json!([]));
}

#[test]
fn tokenize_json_keeps_stdout_machine_readable() {
    let output = Command::new(env!("CARGO_BIN_EXE_wake"))
        .args([
            "tokenize",
            fixture_file().to_str().unwrap(),
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        value["tokens"]
            .as_array()
            .is_some_and(|tokens| !tokens.is_empty())
    );
}

#[test]
fn forced_tui_is_rejected_for_static_commands_without_escape_sequences() {
    let output = Command::new(env!("CARGO_BIN_EXE_wake"))
        .args(["--ui", "tui", "parse", fixture_file().to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("only available"), "{stderr}");
    assert!(!stderr.contains('\x1b'));
}

#[test]
fn bundle_writes_exact_node_commonjs_outfile_without_web_outputs() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root =
        std::env::temp_dir().join(format!("wake_cli_bundle_{}_{}", std::process::id(), unique));
    let source_dir = root.join("src");
    let output = root.join("nested/extension.js");
    std::fs::create_dir_all(&source_dir).unwrap();
    std::fs::write(root.join("package.json"), r#"{"private":true}"#).unwrap();
    std::fs::write(
        source_dir.join("index.ts"),
        "export const answer: number = 42;",
    )
    .unwrap();

    let result = Command::new(env!("CARGO_BIN_EXE_wake"))
        .args([
            "--ui",
            "plain",
            "bundle",
            source_dir.join("index.ts").to_str().unwrap(),
            "--outfile",
            output.to_str().unwrap(),
            "--platform",
            "node",
            "--target",
            "node20",
        ])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let code = std::fs::read_to_string(&output).unwrap();
    assert!(code.contains("module.exports = __wake_entry__"), "{code}");
    assert!(!root.join("index.html").exists());
    assert!(!root.join("manifest.json").exists());

    let invalid = Command::new(env!("CARGO_BIN_EXE_wake"))
        .args([
            "--ui",
            "plain",
            "bundle",
            source_dir.join("index.ts").to_str().unwrap(),
            "--outfile",
            root.join("invalid.js").to_str().unwrap(),
            "--minify",
            "--sourcemap",
        ])
        .output()
        .unwrap();
    assert_eq!(invalid.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&invalid.stderr).contains("WAKE_CONFIG"),
        "{}",
        String::from_utf8_lossy(&invalid.stderr)
    );

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn bundle_missing_outfile_is_a_usage_error() {
    let output = Command::new(env!("CARGO_BIN_EXE_wake"))
        .args(["bundle", fixture_file().to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("--outfile"));
}
