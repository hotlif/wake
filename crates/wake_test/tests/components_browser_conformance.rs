//! Browser evidence for the standalone Components workbench's Controls and URL state.
//!
//! Run explicitly with a system Chromium browser and the installed Yarn cache:
//! `cargo test -p wake_test --test components_browser_conformance -- --ignored`

use std::fs;
use std::net::TcpListener;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use wake_app::{
    DevServer, DevServerEvent, DevServerOptions, DocsMode, ProjectOptions,
    start_docs_dev_server_with_mode,
};
use wake_test_browser::{BrowserDriver, BrowserLaunchOptions, BrowserPage};

const WAIT: Duration = Duration::from_secs(30);

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .unwrap()
        .to_path_buf()
}

fn target_dir() -> PathBuf {
    std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace_root().join("target"))
}

fn write(root: &Path, path: &str, contents: &str) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

fn create_fixture(root: &Path) {
    write(
        root,
        "package.json",
        r#"{"dependencies":{"react":"19.2.8","react-dom":"19.2.8"}}"#,
    );
    write(
        root,
        "wake.config.toml",
        "[docs]\nbase_path = '/workbench/'\nlocale = 'en-US'\n",
    );
    write(
        root,
        "src/button.tsx",
        r#"export interface ButtonProps {
  /** Label shown inside the preview. */
  label?: string;
  enabled?: boolean;
  count?: number;
  variant?: 'primary' | 'secondary';
}"#,
    );
    write(
        root,
        "docs/demos/controls.demo.tsx",
        r#"import type { ButtonProps } from '../../src/button';
export const meta = {
  title: 'Editable controls',
  group: 'Actions',
  component: 'Button',
  args: {label: 'Ready', enabled: true, count: 2, variant: 'primary'},
};
export default function Demo(props: ButtonProps) {
  if (props.label === 'Crash') throw new Error('planned Components preview error');
  return <output id="preview"
    data-label={Object.prototype.hasOwnProperty.call(props, 'label') ? props.label : '<unset>'}
    data-enabled={String(props.enabled)}
    data-count={String(props.count)}
    data-variant={String(props.variant)}>{props.label ?? '<unset>'}</output>;
}"#,
    );
}

fn reserve_port() -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    listener.local_addr().unwrap().port()
}

struct RunningServer(DevServer);

impl RunningServer {
    fn start(root: &Path, port: u16) -> Self {
        let server = start_docs_dev_server_with_mode(
            DevServerOptions {
                project: ProjectOptions {
                    cwd: Some(root.to_path_buf()),
                    config_path: None,
                },
                entry: None,
                host: Some("127.0.0.1".to_owned()),
                port: Some(port),
                open: Some(false),
                federation: None,
            },
            DocsMode::Components,
        )
        .unwrap();
        let events = server.drain_events();
        assert!(
            events
                .iter()
                .any(|event| matches!(event, DevServerEvent::Rebuilt { initial: true, .. })),
            "Components workbench did not build: {events:#?}"
        );
        assert!(
            !events.iter().any(|event| matches!(
                event,
                DevServerEvent::Diagnostic { diagnostic } if diagnostic.severity == "error"
            )),
            "Components workbench reported an error: {events:#?}"
        );
        Self(server)
    }
}

impl Drop for RunningServer {
    fn drop(&mut self) {
        let _ = self.0.close();
    }
}

fn browser() -> BrowserDriver {
    BrowserDriver::launch(BrowserLaunchOptions {
        sandbox: false,
        executable: std::env::var_os("WAKE_SYSTEM_BROWSER_PATH")
            .filter(|path| !path.is_empty())
            .map(PathBuf::from),
        ..BrowserLaunchOptions::default()
    })
    .unwrap()
}

fn state(page: &BrowserPage) -> Result<Value, String> {
    page.evaluate(
        r#"(() => {
          const params = new URLSearchParams(location.hash.split('?')[1] || '');
          const frame = document.querySelector('iframe');
          const preview = frame?.contentDocument?.getElementById('preview');
          return {
            href: location.href,
            hash: location.hash,
            label: document.querySelector('input[aria-label="label"]')?.value ?? null,
            controls: document.querySelectorAll('.workbench-controls .workbench-control').length,
            previewLabel: preview?.dataset.label ?? null,
            previewEnabled: preview?.dataset.enabled ?? null,
            args: JSON.parse(params.get('args') || '{}'),
            unset: JSON.parse(params.get('unset') || '[]'),
            viewport: params.get('viewport') || 'responsive',
            mobile: Boolean(document.querySelector('.workbench-viewport-mobile')),
            error: document.querySelector('.workbench-diagnostics-alert')?.textContent ?? null,
          };
        })()"#,
    )
    .map_err(|error| error.to_string())
}

fn wait_for_state(page: &BrowserPage, ready: impl Fn(&Value) -> bool) -> Value {
    let deadline = Instant::now() + WAIT;
    let mut last = String::new();
    while Instant::now() < deadline {
        match state(page) {
            Ok(value) if ready(&value) => return value,
            Ok(value) => last = value.to_string(),
            Err(error) => last = error,
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!("Components workbench did not reach the expected state: {last}");
}

fn save_failure(page: &BrowserPage, fixture: &Path) {
    let root = target_dir().join("browser-artifacts/components-conformance/controls-url");
    if let Err(error) = fs::create_dir_all(&root) {
        eprintln!("could not create Components browser evidence: {error}");
        return;
    }
    let mut events = Vec::new();
    for method in [
        "Network.requestWillBeSent",
        "Network.responseReceived",
        "Network.loadingFailed",
        "Runtime.consoleAPICalled",
        "Runtime.exceptionThrown",
    ] {
        while let Some(event) = page.take_event(method) {
            events.push(json!({"method": method, "event": event}));
        }
    }
    let _ = fs::write(
        root.join("console-network.json"),
        serde_json::to_vec_pretty(&events).unwrap(),
    );
    let _ = fs::write(root.join("page-state.txt"), format!("{:#?}", state(page)));
    match page.screenshot_png() {
        Ok(png) => {
            let _ = fs::write(root.join("failure.png"), png);
        }
        Err(error) => {
            let _ = fs::write(root.join("screenshot-error.txt"), error.to_string());
        }
    }
    eprintln!("Components browser evidence: {}", root.display());
    eprintln!("Components fixture retained: {}", fixture.display());
}

#[test]
#[ignore = "requires an installed system Chromium browser and the immutable Yarn cache"]
fn chromium_components_controls_round_trip_through_hash_and_refresh() {
    let fixture = tempfile::Builder::new()
        .prefix("wake-components-browser-")
        .tempdir_in(target_dir())
        .unwrap();
    create_fixture(fixture.path());
    let port = reserve_port();
    let _server = RunningServer::start(fixture.path(), port);
    let driver = browser();
    let context = driver.create_context().unwrap();
    let page = context.new_page("about:blank").unwrap();
    page.command("Network.enable", json!({})).unwrap();
    let url = format!("http://127.0.0.1:{port}/workbench/");

    let result = catch_unwind(AssertUnwindSafe(|| {
        page.navigate(&url).unwrap();
        let initial = wait_for_state(&page, |value| {
            value["label"] == "Ready" && value["previewLabel"] == "Ready"
        });
        assert_eq!(initial["controls"], 4, "typed demo exposes four Controls");
        assert!(
            initial["error"].is_null(),
            "workbench reported a runtime error"
        );

        page.evaluate(
            "(() => { const input = document.querySelector('input[aria-label=\"label\"]'); input.focus(); input.select(); })()",
        )
        .unwrap();
        page.command("Input.insertText", json!({"text": "Edited"}))
            .unwrap();
        let edited = wait_for_state(&page, |value| {
            value["label"] == "Edited"
                && value["previewLabel"] == "Edited"
                && value["args"]["label"] == "Edited"
        });
        let edited_url = edited["href"].as_str().unwrap().to_owned();
        assert!(edited["hash"].as_str().unwrap().contains("args="));

        page.command("Page.reload", json!({"ignoreCache": true}))
            .unwrap();
        wait_for_state(&page, |value| {
            value["href"] == edited_url
                && value["label"] == "Edited"
                && value["previewLabel"] == "Edited"
        });

        page.evaluate(
            "document.querySelector('button[aria-label=\"Unset label and leave it unspecified\"]').click()",
        )
        .unwrap();
        let unset = wait_for_state(&page, |value| {
            value["unset"]
                .as_array()
                .is_some_and(|names| names.contains(&json!("label")))
                && value["previewLabel"] == "<unset>"
        });
        assert!(unset["args"]["label"].is_null());
        let unset_url = unset["href"].as_str().unwrap().to_owned();
        page.command("Page.reload", json!({"ignoreCache": true}))
            .unwrap();
        wait_for_state(&page, |value| {
            value["href"] == unset_url
                && value["unset"]
                    .as_array()
                    .is_some_and(|names| names.contains(&json!("label")))
                && value["previewLabel"] == "<unset>"
        });

        page.evaluate(
            "document.querySelector('button[aria-label=\"Restore all controls to their defaults\"]').click()",
        )
        .unwrap();
        wait_for_state(&page, |value| {
            value["label"] == "Ready"
                && value["previewLabel"] == "Ready"
                && value["args"]
                    .as_object()
                    .is_some_and(|args| args.is_empty())
                && value["unset"]
                    .as_array()
                    .is_some_and(|names| names.is_empty())
        });

        page.navigate(&edited_url).unwrap();
        wait_for_state(&page, |value| {
            value["href"] == edited_url
                && value["label"] == "Edited"
                && value["previewLabel"] == "Edited"
        });
        page.evaluate(
            "document.querySelector('[aria-label=\"Preview viewport\"] input[value=\"mobile\"]').click()",
        )
        .unwrap();
        let mobile = wait_for_state(&page, |value| {
            value["viewport"] == "mobile" && value["mobile"] == true
        });
        let mobile_url = mobile["href"].as_str().unwrap().to_owned();
        page.command("Page.reload", json!({"ignoreCache": true}))
            .unwrap();
        wait_for_state(&page, |value| {
            value["href"] == mobile_url
                && value["label"] == "Edited"
                && value["previewLabel"] == "Edited"
                && value["viewport"] == "mobile"
                && value["mobile"] == true
        });

        page.evaluate(
            "(() => { const input = document.querySelector('input[aria-label=\"label\"]'); input.focus(); input.select(); })()",
        )
        .unwrap();
        page.command("Input.insertText", json!({"text": "Crash"}))
            .unwrap();
        let crashed = wait_for_state(&page, |value| {
            value["label"] == "Crash"
                && value["error"]
                    .as_str()
                    .is_some_and(|error| error.contains("planned Components preview error"))
        });
        assert!(crashed["previewLabel"].is_null());
        assert_eq!(crashed["args"]["label"], "Crash");

        page.evaluate(
            "(() => { const input = document.querySelector('input[aria-label=\"label\"]'); input.focus(); input.select(); })()",
        )
        .unwrap();
        page.command("Input.insertText", json!({"text": "Recovered"}))
            .unwrap();
        let recovered = wait_for_state(&page, |value| {
            value["label"] == "Recovered"
                && value["previewLabel"] == "Recovered"
                && value["error"].is_null()
        });
        assert_eq!(recovered["args"]["label"], "Recovered");
        assert_eq!(recovered["viewport"], "mobile");
    }));

    if let Err(panic) = result {
        save_failure(&page, fixture.path());
        let retained = fixture.keep();
        eprintln!("Components fixture: {}", retained.display());
        resume_unwind(panic);
    }
}
