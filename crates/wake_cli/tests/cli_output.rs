use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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
fn removed_test_flags_report_the_wake_test_config_category() {
    let output = Command::new(env!("CARGO_BIN_EXE_wake"))
        .args(["test", "--testNamePattern", "renders"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("WAKE_TEST_CONFIG"), "{stderr}");
    assert!(stderr.contains("--testNamePattern"), "{stderr}");
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

    let mapped_output = root.join("mapped.js");
    let mapped = Command::new(env!("CARGO_BIN_EXE_wake"))
        .args([
            "--ui",
            "plain",
            "bundle",
            source_dir.join("index.ts").to_str().unwrap(),
            "--outfile",
            mapped_output.to_str().unwrap(),
            "--minify",
            "--sourcemap",
        ])
        .output()
        .unwrap();
    assert!(
        mapped.status.success(),
        "{}",
        String::from_utf8_lossy(&mapped.stderr)
    );
    let mapped_code = std::fs::read_to_string(&mapped_output).unwrap();
    assert!(
        mapped_code.ends_with("//# sourceMappingURL=mapped.js.map\n"),
        "{mapped_code}"
    );
    assert!(root.join("mapped.js.map").is_file());

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn build_watch_recovers_invalid_toml_without_exiting_or_writing_early() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "wake_cli_build_watch_recovery_{}_{}",
        std::process::id(),
        unique
    ));
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("package.json"), r#"{"private":true}"#).unwrap();
    std::fs::write(root.join("wake.config.toml"), "[html\n").unwrap();
    std::fs::write(
        root.join("src/index.js"),
        "export const recovered = true;\n",
    )
    .unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_wake"))
        .current_dir(&root)
        .args(["--ui", "plain", "build", "--watch", "--outdir", "dist"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    thread::sleep(Duration::from_millis(500));
    let stayed_alive = child.try_wait().unwrap().is_none();
    let untouched_before_recovery = !root.join(".wake").exists() && !root.join("dist").exists();

    std::fs::write(
        root.join("wake.config.toml"),
        "[html]\nentry = \"src/index.js\"\n",
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut early_status = None;
    while Instant::now() < deadline && !root.join("dist/index.html").is_file() {
        if let Some(status) = child.try_wait().unwrap() {
            early_status = Some(status);
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    let recovered = root.join("dist/index.html").is_file();
    let _ = child.kill();
    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let _ = std::fs::remove_dir_all(&root);

    assert!(stayed_alive, "watch exited on recoverable TOML: {stderr}");
    assert!(
        untouched_before_recovery,
        "watch touched generated/output state before coverage: {stderr}"
    );
    assert!(
        recovered,
        "watch did not recover after valid TOML (early status: {early_status:?}): {stderr}"
    );
    assert!(stderr.contains("WAKE_CONFIG"), "{stderr}");
    assert!(stderr.contains("Initial build completed"), "{stderr}");
}

#[test]
fn unavailable_persistent_cache_is_one_non_fatal_cli_warning() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "wake_cli_cache_warning_{}_{}",
        std::process::id(),
        unique
    ));
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("package.json"), r#"{"private":true}"#).unwrap();
    std::fs::write(root.join("src/index.js"), "export const answer = 42;\n").unwrap();
    // A directory at the optional cache-file path forces both load and store I/O failures without
    // making the required `.wake` generation directory itself invalid. The real build must still
    // complete, and both details must be collapsed into one cache warning.
    std::fs::create_dir_all(root.join(".wake/bundle-cache.bin")).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_wake"))
        .current_dir(&root)
        .args([
            "--ui",
            "plain",
            "bundle",
            "src/index.js",
            "--outfile",
            "dist/bundle.js",
            "--cache",
        ])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(root.join("dist/bundle.js").is_file());
    assert_eq!(stderr.matches("WAKE_CACHE").count(), 1, "{stderr}");
    assert!(stderr.contains("WARNING"), "{stderr}");

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

#[test]
fn build_error_prints_numbered_source_code_frame() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "wake_cli_diagnostic_{}_{}",
        std::process::id(),
        unique
    ));
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("wake.config.toml"),
        "[html]\nentry = 'src/index.js'\n",
    )
    .unwrap();
    std::fs::write(
        root.join("src/index.js"),
        "const first = 1;\nconst second = 2;\nconst broken = ;\n",
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_wake"))
        .current_dir(&root)
        .args(["--ui", "plain", "build"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{stderr}");
    assert!(stderr.contains("src/index.js:3:"), "{stderr}");
    assert!(stderr.contains("3 | const broken = ;"), "{stderr}");
    assert!(stderr.contains('^'), "{stderr}");
    std::fs::remove_dir_all(root).unwrap();
}

fn federation_fixture(name: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "wake_cli_federation_{name}_{}_{}",
        std::process::id(),
        unique
    ));
    std::fs::create_dir_all(&root).unwrap();
    root
}

#[test]
fn federation_init_discovers_root_and_is_idempotent() {
    let root = federation_fixture("init");
    let nested = root.join("packages/catalog/src");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(
        root.join("wake.config.toml"),
        "[federation]\nenabled = true\nname = 'shell'\n",
    )
    .unwrap();

    let invoke = || {
        Command::new(env!("CARGO_BIN_EXE_wake"))
            .args([
                "--ui",
                "plain",
                "federation",
                "init",
                nested.to_str().unwrap(),
            ])
            .output()
            .unwrap()
    };
    let first = invoke();
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(root.join("wake-federation.d.ts")).unwrap(),
        "/// <reference path=\"./.wake/federation/types/index.d.ts\" />\n"
    );
    let index = std::fs::read_to_string(root.join(".wake/federation/types/index.d.ts")).unwrap();
    assert_eq!(
        index,
        "// Managed by `wake dev`; remote federation declarations are synchronized here.\n"
    );
    assert!(!index.contains(": any"), "{index}");

    let second = invoke();
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert!(
        String::from_utf8_lossy(&second.stderr).contains("Already initialized"),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn federation_init_fails_closed_on_conflicting_generated_file() {
    let root = federation_fixture("conflict");
    let types = root.join(".wake/federation/types");
    std::fs::create_dir_all(&types).unwrap();
    std::fs::write(
        root.join("wake.config.toml"),
        "[federation]\nenabled = true\nname = 'shell'\n",
    )
    .unwrap();
    let index = types.join("index.d.ts");
    std::fs::write(&index, "declare module 'owned-by-user';\n").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_wake"))
        .args([
            "--ui",
            "plain",
            "federation",
            "init",
            root.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("WAKE_FED_INIT_CONFLICT"), "{stderr}");
    assert_eq!(
        std::fs::read_to_string(index).unwrap(),
        "declare module 'owned-by-user';\n"
    );
    assert!(!root.join("wake-federation.d.ts").exists());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn federation_init_requires_an_enabled_project_config() {
    let root = federation_fixture("disabled");
    std::fs::write(root.join("wake.config.toml"), "[federation]\n").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_wake"))
        .args([
            "--ui",
            "plain",
            "federation",
            "init",
            root.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("WAKE_FED_INIT_CONFIG"), "{stderr}");
    assert!(stderr.contains("federation.enabled = true"), "{stderr}");
    assert!(!root.join("wake-federation.d.ts").exists());
    assert!(!root.join(".wake").exists());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn federation_init_help_describes_the_stable_types_entry() {
    let output = Command::new(env!("CARGO_BIN_EXE_wake"))
        .args(["federation", "init", "--help"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("stable TypeScript entry"), "{stdout}");
    assert!(stdout.contains("[ROOT]"), "{stdout}");
}
