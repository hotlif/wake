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
fn library_token_generates_configured_typescript_and_fails_strictly() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root =
        std::env::temp_dir().join(format!("wake_cli_token_{}_{}", std::process::id(), unique));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("design.toml"),
        "[build]\noutput='./src/token.ts'\nprefix='demo'\n[token]\ncolor='red'\n",
    )
    .unwrap();

    let result = Command::new(env!("CARGO_BIN_EXE_wake"))
        .args([
            "--ui",
            "plain",
            "library",
            "token",
            root.to_str().unwrap(),
            "--config",
            "design.toml",
        ])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let generated = std::fs::read_to_string(root.join("src/token.ts")).unwrap();
    assert!(generated.contains("--demo-color"), "{generated}");

    std::fs::write(
        root.join("design.toml"),
        "[build]\noutput='./src/token.ts'\nprefix='demo'\n[token]\ncolor='$ref(missing)'\n",
    )
    .unwrap();
    let failed = Command::new(env!("CARGO_BIN_EXE_wake"))
        .args([
            "--ui",
            "plain",
            "library",
            "token",
            root.to_str().unwrap(),
            "--config",
            "design.toml",
        ])
        .output()
        .unwrap();
    assert!(!failed.status.success());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("WAKE_TOKEN_REF"));
    assert_eq!(
        std::fs::read_to_string(root.join("src/token.ts")).unwrap(),
        generated
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn library_docgen_generates_public_json_and_preserves_it_on_failure() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root =
        std::env::temp_dir().join(format!("wake_cli_docgen_{}_{}", std::process::id(), unique));
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("package.json"), r#"{"name":"demo"}"#).unwrap();
    std::fs::write(
        root.join("src/index.ts"),
        "export { default } from './button.js';",
    )
    .unwrap();
    std::fs::write(
        root.join("src/button.tsx"),
        "interface Props { label: string; }\nconst Button = (props: Props) => null;\nexport default Button;\n",
    )
    .unwrap();

    let result = Command::new(env!("CARGO_BIN_EXE_wake"))
        .args(["--ui", "plain", "library", "docgen", root.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let output = root.join("public/docgen.json");
    let generated = std::fs::read_to_string(&output).unwrap();
    assert!(generated.contains("./src/button.tsx"), "{generated}");
    assert!(
        generated.contains("\"displayName\":\"Button\""),
        "{generated}"
    );

    std::fs::write(root.join("src/index.ts"), "export default Missing;").unwrap();
    let failed = Command::new(env!("CARGO_BIN_EXE_wake"))
        .args(["--ui", "plain", "library", "docgen", root.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!failed.status.success());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("WAKE_DOCGEN_ENTRY"));
    assert_eq!(std::fs::read_to_string(output).unwrap(), generated);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn library_build_emits_packify_compatible_entry_paths() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "wake_cli_library_{}_{}",
        std::process::id(),
        unique
    ));
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("package.json"),
        r#"{"name":"@demo/button","type":"module"}"#,
    )
    .unwrap();
    std::fs::write(
        root.join("src/index.ts"),
        "import Button from './button.js';\nexport type { ButtonProps } from './button.js';\nexport default Button;\n",
    )
    .unwrap();
    std::fs::write(
        root.join("src/button.tsx"),
        "import type { FC } from 'react';\nexport interface ButtonProps { label: string; }\nconst Button: FC<ButtonProps> = (props) => <button>{props.label}</button>;\nexport default Button;\n",
    )
    .unwrap();

    let result = Command::new(env!("CARGO_BIN_EXE_wake"))
        .args(["--ui", "plain", "library", "build", root.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(root.join("esm/index.mjs").is_file());
    assert!(root.join("cjs/index.cjs").is_file());
    assert!(root.join("declarations/index.d.ts").is_file());
    assert!(!root.join("css").exists());
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
