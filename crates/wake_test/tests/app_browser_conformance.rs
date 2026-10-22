//! Real-browser evidence for the app development server's reload and mount contracts.
//!
//! Run explicitly with a system Chromium browser after the Yarn cache is installed:
//! `cargo test -p wake_test --test app_browser_conformance -- --ignored`

use std::fs;
use std::net::TcpListener;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use wake_app::{DevServer, DevServerEvent, DevServerOptions, ProjectOptions, start_dev_server};
use wake_common::zip::ZipArchive;
use wake_test_browser::{BrowserDriver, BrowserLaunchOptions, BrowserPage};

const WAIT: Duration = Duration::from_secs(25);

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("wake_test is below the workspace root")
        .to_path_buf()
}

fn write(root: &Path, path: &str, contents: &str) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

fn install_cached_package(root: &Path, name: &str, version: &str) {
    let prefix = format!("{name}-npm-{version}-");
    let mut matches = fs::read_dir(workspace_root().join(".yarn/cache"))
        .expect("Yarn cache is installed; run `corepack yarn install --immutable-cache`")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(&prefix) && name.ends_with(".zip"))
        })
        .collect::<Vec<_>>();
    matches.sort();
    assert_eq!(
        matches.len(),
        1,
        "expected one Yarn archive for {name}@{version}"
    );
    let archive = ZipArchive::parse(fs::read(&matches[0]).unwrap()).unwrap();
    extract_zip_tree(
        &archive,
        &format!("node_modules/{name}"),
        &root.join("node_modules").join(name),
    );
}

fn extract_zip_tree(archive: &ZipArchive, source: &str, target: &Path) {
    assert!(archive.is_dir(source), "ZIP directory is missing: {source}");
    fs::create_dir_all(target).unwrap();
    for child in archive.read_dir(source) {
        let name = child.trim_end_matches('/').rsplit('/').next().unwrap();
        let target = target.join(name);
        if archive.is_dir(&child) {
            extract_zip_tree(archive, &child, &target);
        } else {
            fs::write(target, archive.read(&child).unwrap().unwrap()).unwrap();
        }
    }
}

fn source(marker: &str, base_path: &str) -> String {
    format!(
        r#"import * as React from 'react';
import {{createRoot}} from 'react-dom/client';
import './style.css';

const marker = {marker:?};
const basePath = {base_path:?};
globalThis.__wakeBootId = String(Date.now()) + ':' + String(Math.random());
function Counter() {{
  const [count, setCount] = React.useState(0);
  const [lazy, setLazy] = React.useState('');
  React.useEffect(() => {{ import('./lazy.js').then(module => setLazy(module.value)); }}, []);
  return React.createElement('main', {{className: 'status'}},
    React.createElement('h1', {{id: 'marker'}}, marker),
    React.createElement('output', {{id: 'count'}}, String(count)),
    React.createElement('button', {{id: 'increment', onClick: () => setCount(value => value + 1)}}, 'increment'),
    React.createElement('img', {{id: 'badge', src: basePath + 'badge.svg'}}),
    React.createElement('output', {{id: 'lazy'}}, lazy),
    location.pathname.endsWith('/not-found')
      ? React.createElement('p', {{id: 'not-found'}}, 'App route not found')
      : null);
}}
const container = document.createElement('div');
container.id = 'app';
document.body.appendChild(container);
createRoot(container).render(React.createElement(Counter));
"#
    )
}

fn create_fixture(root: &Path, base_path: &str) {
    write(root, "package.json", r#"{"type":"module"}"#);
    write(
        root,
        "wake.config.toml",
        &format!("public_path = {base_path:?}\n"),
    );
    write(root, "src/main.jsx", &source("before", base_path));
    write(
        root,
        "src/lazy.js",
        "export const value = 'lazy asset loaded';\n",
    );
    write(
        root,
        "src/style.css",
        ".status { color: rgb(12, 34, 56); }\n",
    );
    write(
        root,
        "public/badge.svg",
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="4" height="4"><rect width="4" height="4" fill="red"/></svg>"#,
    );
    install_cached_package(root, "react", "19.3.0");
    install_cached_package(root, "react-dom", "19.3.0");
    install_cached_package(root, "scheduler", "0.28.0");
}

fn reserve_port() -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    listener.local_addr().unwrap().port()
}

struct RunningServer(DevServer);

impl RunningServer {
    fn start(root: &Path, port: u16) -> Self {
        let server = start_dev_server(DevServerOptions {
            project: ProjectOptions {
                cwd: Some(root.to_path_buf()),
                config_path: None,
            },
            entry: Some(PathBuf::from("src/main.jsx")),
            host: Some("127.0.0.1".to_owned()),
            port: Some(port),
            open: Some(false),
            federation: None,
        })
        .unwrap();
        let events = server.drain_events();
        assert!(
            events
                .iter()
                .any(|event| matches!(event, DevServerEvent::Rebuilt { initial: true, .. })),
            "initial app build did not succeed: {events:#?}"
        );
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, DevServerEvent::Diagnostic { .. })),
            "initial app build reported diagnostics: {events:#?}"
        );
        Self(server)
    }

    fn await_rebuild(&self) {
        let deadline = Instant::now() + WAIT;
        let mut events = Vec::new();
        while Instant::now() < deadline {
            events.extend(self.0.drain_events());
            assert!(
                !events
                    .iter()
                    .any(|event| matches!(event, DevServerEvent::Diagnostic { .. })),
                "app rebuild reported diagnostics: {events:#?}"
            );
            if events
                .iter()
                .any(|event| matches!(event, DevServerEvent::Rebuilt { initial: false, .. }))
            {
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }
        panic!("app rebuild timed out: {events:#?}");
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

#[derive(Default)]
struct Evidence {
    events: Vec<Value>,
}

impl Evidence {
    fn capture(&mut self, page: &BrowserPage) {
        for method in [
            "Network.requestWillBeSent",
            "Network.responseReceived",
            "Network.loadingFailed",
            "Network.webSocketHandshakeResponseReceived",
            "Runtime.consoleAPICalled",
            "Runtime.exceptionThrown",
        ] {
            while let Some(event) = page.take_event(method) {
                self.events.push(json!({"method": method, "event": event}));
            }
        }
    }

    fn document_requests(&self, url: &str) -> usize {
        self.events
            .iter()
            .filter(|item| {
                item["method"] == "Network.requestWillBeSent"
                    && item["event"]["params"]["type"] == "Document"
                    && item["event"]["params"]["request"]["url"] == url
            })
            .count()
    }

    fn has_live_reload_socket(&self) -> bool {
        self.events.iter().any(|item| {
            item["method"] == "Network.webSocketHandshakeResponseReceived"
                && item["event"]["params"]["response"]["status"] == 101
        })
    }

    fn save_failure(&mut self, page: &BrowserPage, name: &str) {
        self.capture(page);
        let root = std::env::var_os("CARGO_TARGET_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| workspace_root().join("target"))
            .join("browser-artifacts/app-conformance")
            .join(name);
        if let Err(error) = fs::create_dir_all(&root) {
            eprintln!("could not create browser evidence directory: {error}");
            return;
        }
        let events = serde_json::to_vec_pretty(&self.events).unwrap();
        let _ = fs::write(root.join("console-network.json"), events);
        let state = page.evaluate(
            "({href: location.href, readyState: document.readyState, body: document.body?.innerText})",
        );
        let _ = fs::write(root.join("page-state.txt"), format!("{state:#?}"));
        match page.screenshot_png() {
            Ok(png) => {
                let _ = fs::write(root.join("failure.png"), png);
            }
            Err(error) => {
                let _ = fs::write(root.join("screenshot-error.txt"), error.to_string());
            }
        }
        eprintln!("browser failure evidence: {}", root.display());
    }
}

fn with_evidence(name: &str, page: &BrowserPage, test: impl FnOnce(&mut Evidence)) {
    let mut evidence = Evidence::default();
    let result = catch_unwind(AssertUnwindSafe(|| test(&mut evidence)));
    if let Err(panic) = result {
        evidence.save_failure(page, name);
        resume_unwind(panic);
    }
}

fn page_state(page: &BrowserPage) -> Result<Value, String> {
    page.evaluate(
        r#"({
            href: location.href,
            marker: document.getElementById('marker')?.textContent ?? null,
            count: document.getElementById('count')?.textContent ?? null,
            boot: globalThis.__wakeBootId ?? null,
            lazy: document.getElementById('lazy')?.textContent ?? null,
            styled: document.querySelector('.status') ? getComputedStyle(document.querySelector('.status')).color : null,
            imageLoaded: (document.getElementById('badge')?.naturalWidth ?? 0) > 0,
            notFound: document.getElementById('not-found')?.textContent ?? null
        })"#,
    )
    .map_err(|error| error.to_string())
}

fn wait_for_page(page: &BrowserPage, expected_url: &str, ready: impl Fn(&Value) -> bool) -> Value {
    let deadline = Instant::now() + WAIT;
    let mut last = String::new();
    while Instant::now() < deadline {
        match page_state(page) {
            Ok(state) if state["href"] == expected_url && ready(&state) => return state,
            Ok(state) => last = state.to_string(),
            Err(error) => last = error,
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!("browser page did not reach {expected_url}: {last}");
}

fn navigate_ready(page: &BrowserPage, url: &str) -> Value {
    page.navigate(url).unwrap();
    wait_for_page(page, url, |state| {
        state["marker"] == "before"
            && state["lazy"] == "lazy asset loaded"
            && state["styled"] == "rgb(12, 34, 56)"
            && state["imageLoaded"] == true
    })
}

#[test]
#[ignore = "requires an installed system Chromium browser and the immutable Yarn cache"]
fn chromium_reloads_a_react_app_once_and_resets_state() {
    let fixture = tempfile::Builder::new()
        .prefix("wake-app-browser-reload-")
        .tempdir()
        .unwrap();
    create_fixture(fixture.path(), "/");
    let port = reserve_port();
    let server = RunningServer::start(fixture.path(), port);
    let driver = browser();
    let context = driver.create_context().unwrap();
    let page = context.new_page("about:blank").unwrap();
    page.command("Network.enable", json!({})).unwrap();
    let url = format!("http://127.0.0.1:{port}/");

    with_evidence("reload", &page, |evidence| {
        let initial = navigate_ready(&page, &url);
        page.evaluate("document.getElementById('increment').click()")
            .unwrap();
        let changed = wait_for_page(&page, &url, |state| state["count"] == "1");
        assert_eq!(changed["boot"], initial["boot"]);

        let socket_deadline = Instant::now() + WAIT;
        while Instant::now() < socket_deadline {
            evidence.capture(&page);
            if evidence.has_live_reload_socket() {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        assert!(
            evidence.has_live_reload_socket(),
            "live reload WebSocket did not connect"
        );
        let previous_requests = evidence.document_requests(&url);
        assert_eq!(
            previous_requests, 1,
            "initial navigation requested more than one document"
        );

        write(fixture.path(), "src/main.jsx", &source("after", "/"));
        server.await_rebuild();
        let reloaded = wait_for_page(&page, &url, |state| {
            state["marker"] == "after" && state["count"] == "0"
        });
        assert_ne!(
            reloaded["boot"], initial["boot"],
            "the React page did not reload"
        );
        thread::sleep(Duration::from_millis(750));
        evidence.capture(&page);
        assert_eq!(
            evidence.document_requests(&url) - previous_requests,
            1,
            "one source edit must request exactly one new document"
        );
    });
}

#[test]
#[ignore = "requires an installed system Chromium browser and the immutable Yarn cache"]
fn chromium_loads_root_and_nested_app_routes_with_resources_and_404s() {
    for (label, base_path) in [("root", "/"), ("portal", "/portal/")] {
        let fixture = tempfile::Builder::new()
            .prefix("wake-app-browser-routes-")
            .tempdir()
            .unwrap();
        create_fixture(fixture.path(), base_path);
        let port = reserve_port();
        let _server = RunningServer::start(fixture.path(), port);
        let driver = browser();
        let context = driver.create_context().unwrap();
        let page = context.new_page("about:blank").unwrap();
        page.command("Network.enable", json!({})).unwrap();
        let origin = format!("http://127.0.0.1:{port}");

        with_evidence(label, &page, |evidence| {
            let home = format!("{origin}{base_path}");
            navigate_ready(&page, &home);
            let route = format!("{origin}{base_path}nested/item");
            let first_route = navigate_ready(&page, &route);
            page.command("Page.reload", json!({"ignoreCache": true}))
                .unwrap();
            let refreshed = wait_for_page(&page, &route, |state| {
                state["marker"] == "before"
                    && state["lazy"] == "lazy asset loaded"
                    && state["imageLoaded"] == true
                    && state["boot"] != first_route["boot"]
            });
            assert_eq!(refreshed["styled"], "rgb(12, 34, 56)");

            let not_found_route = format!("{origin}{base_path}not-found");
            navigate_ready(&page, &not_found_route);
            let shell = page_state(&page).unwrap();
            assert_eq!(shell["notFound"], "App route not found");
            let missing_asset = format!("{base_path}missing.js");
            let outside_mount = if base_path == "/portal/" {
                "/"
            } else {
                "/outside.png"
            };
            let checks = page
                .evaluate(&format!(
                    "Promise.all([{missing_asset:?}, {outside_mount:?}].map(async path => {{ const response = await fetch(path); return {{status: response.status, contentType: response.headers.get('content-type')}}; }}))"
                ))
                .unwrap();
            assert_eq!(
                checks[0]["status"], 404,
                "missing JS asset must remain a 404"
            );
            assert_eq!(
                checks[1]["status"], 404,
                "request outside the mount must be a 404"
            );
            assert_ne!(checks[0]["contentType"], "text/html; charset=utf-8");
            evidence.capture(&page);
            assert!(
                evidence.document_requests(&route) >= 2,
                "direct navigation and refresh must each request the nested route"
            );
        });
    }
}
