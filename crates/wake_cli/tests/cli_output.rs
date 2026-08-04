use std::path::PathBuf;
use std::process::Command;

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
