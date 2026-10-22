use std::{fs, path::Path, process::Command};

struct VisualFixture(Option<tempfile::TempDir>);

impl VisualFixture {
    fn new(repository: &Path) -> Self {
        Self(Some(
            tempfile::Builder::new()
                .prefix("docs-visual-")
                .tempdir_in(repository.join("target"))
                .unwrap(),
        ))
    }

    fn path(&self) -> &Path {
        self.0.as_ref().unwrap().path()
    }
}

impl Drop for VisualFixture {
    fn drop(&mut self) {
        if std::thread::panicking() {
            let path = self.0.take().unwrap().keep();
            eprintln!("Docs visual evidence retained at {}", path.display());
        }
    }
}

fn cli(root: &Path, args: &[&str]) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_wake"));
    command.args(["docs", "test"]).arg(root).args(args);
    if let Some(browser) =
        std::env::var_os("WAKE_SYSTEM_BROWSER_PATH").filter(|path| !path.is_empty())
    {
        command.arg("--browser-path").arg(browser);
    }
    command.output().unwrap()
}

#[test]
fn docs_test_is_discoverable_and_validates_matrix_before_building() {
    let help = Command::new(env!("CARGO_BIN_EXE_wake"))
        .args(["docs", "test", "--help"])
        .output()
        .unwrap();
    assert!(
        help.status.success(),
        "{}",
        String::from_utf8_lossy(&help.stderr)
    );
    assert!(String::from_utf8_lossy(&help.stdout).contains("--viewport"));
    let root = tempfile::tempdir().unwrap();
    let invalid = cli(root.path(), &["--viewport", "0x720"]);
    assert_eq!(invalid.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("viewport"));
    let invalid = cli(root.path(), &["--theme", "system"]);
    assert_eq!(invalid.status.code(), Some(2));
}

#[test]
#[ignore = "requires a current wake-test-host and installed system Chromium"]
fn docs_demo_matrix_uses_real_css_preview_and_reviewed_baselines() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let fixture = VisualFixture::new(&repository);
    let root = fixture.path();
    fs::create_dir(root.join("docs")).unwrap();
    fs::create_dir(root.join("public")).unwrap();
    fs::create_dir(root.join("reports")).unwrap();
    let run = |phase: &str, args: &[&str]| {
        let output = cli(root, args);
        fs::write(
            root.join(format!("reports/{phase}.stdout.json")),
            &output.stdout,
        )
        .unwrap();
        fs::write(
            root.join(format!("reports/{phase}.stderr.txt")),
            &output.stderr,
        )
        .unwrap();
        output
    };
    fs::write(root.join("public/pixel.svg"), r#"<svg xmlns="http://www.w3.org/2000/svg" width="12" height="12"><rect width="12" height="12" fill="green"/></svg>"#).unwrap();
    fs::write(
        root.join("docs/navigation.toml"),
        "[[group]]\nid = 'start'\ntitle = 'Start'\npages = ['index']\n",
    )
    .unwrap();
    fs::write(
        root.join("package.json"),
        r#"{"dependencies":{"react":"19.2.8","react-dom":"19.2.8"}}"#,
    )
    .unwrap();
    fs::write(
        root.join("wake.config.toml"),
        "[docs]\npreview = 'docs/preview.tsx'\nbase_path = '/visual/'\n[test]\ntimeout_ms = 15000\n",
    )
    .unwrap();
    fs::write(
        root.join("docs/index.mdx"),
        "# Demos\n<Demo src=\"./counter.demo.tsx\" />\n",
    )
    .unwrap();
    fs::write(root.join("docs/preview.tsx"), "import './demo.css'; export default function Preview({children}) {return <main data-preview>{children}</main>}").unwrap();
    let demo = root.join("docs/counter.demo.tsx");
    fs::write(&demo, "export const meta = {args: {label: 'Ready'}}; export default function Demo({label}) {return <button>{label}<img src='/visual/pixel.svg' /></button>}").unwrap();
    let css = root.join("docs/demo.css");
    fs::write(&css, "button {background: rgb(0, 100, 200); color: white; width: 100px} [data-theme=dark] button {color: yellow}").unwrap();
    let args = ["--reporter", "json"];
    let missing = run("missing-baselines", &args);
    assert_eq!(
        missing.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&missing.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&missing.stdout).unwrap();
    assert_eq!(result["counts"]["tests"]["failed"], 4);
    assert_eq!(result["snapshot"]["unmatched"], 4);
    let updated = run(
        "create-baselines",
        &["--reporter", "json", "--update-snapshots"],
    );
    assert!(
        updated.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&updated.stderr),
        String::from_utf8_lossy(&updated.stdout)
    );
    let result: serde_json::Value = serde_json::from_slice(&updated.stdout).unwrap();
    assert_eq!(result["snapshot"]["added"], 4);
    let matched = run("match-baselines", &args);
    assert!(
        matched.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&matched.stderr),
        String::from_utf8_lossy(&matched.stdout)
    );
    let result: serde_json::Value = serde_json::from_slice(&matched.stdout).unwrap();
    assert_eq!(result["snapshot"]["matched"], 4);
    assert!(
        result["suites"]
            .as_array()
            .unwrap()
            .iter()
            .all(|suite| suite["path"] == "docs/counter.demo.tsx")
    );
    fs::write(&css, "button {background: red; width: 200px}").unwrap();
    let changed = run("css-difference", &args);
    assert_eq!(changed.status.code(), Some(1));
    let result: serde_json::Value = serde_json::from_slice(&changed.stdout).unwrap();
    assert_eq!(result["snapshot"]["unmatched"], 4);
    assert!(
        result["artifacts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|artifact| artifact["kind"] == "screenshot-diff"
                && Path::new(artifact["path"].as_str().unwrap()).is_file())
    );
    fs::write(
        &demo,
        "export default function Demo() {throw new Error('demo exploded')}",
    )
    .unwrap();
    let broken = run(
        "render-error",
        &["--reporter", "json", "--update-snapshots"],
    );
    assert_eq!(broken.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&broken.stdout).contains("demo exploded"));
    let result: serde_json::Value = serde_json::from_slice(&broken.stdout).unwrap();
    assert_eq!(result["snapshot"]["updated"], 0);
    fs::write(&demo, "export default function Demo() {return <img src='https://example.invalid/not-found.png' />} ").unwrap();
    let blocked = run(
        "blocked-network",
        &[
            "--reporter",
            "json",
            "--update-snapshots",
            "--theme",
            "light",
            "--viewport",
            "390x844",
        ],
    );
    assert_eq!(blocked.status.code(), Some(1));
    let result: serde_json::Value = serde_json::from_slice(&blocked.stdout).unwrap();
    assert_eq!(result["snapshot"]["updated"], 0);
    assert_eq!(result["snapshot"]["added"], 0);
    assert!(String::from_utf8_lossy(&blocked.stdout).contains("example.invalid/not-found.png"));
}
