//! Real-browser evidence for the app development server's reload and mount contracts.
//!
//! Run explicitly with a system Chromium browser after the Yarn cache is installed:
//! `cargo test -p wake_test --test app_browser_conformance -- --ignored`

use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use wake_app::{
    BuildOptions, CancellationToken, DevServer, DevServerEvent, DevServerOptions, ProjectOptions,
    build, start_dev_server,
};
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
            define: Default::default(),
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

// These pages have exclusive JS dependencies and styles. No framework or package installation is
// needed: the entry represents a menu/router which only reads metadata and captures the loaders.
fn scan_page(name: &str, route: &str) -> String {
    format!(
        r#"/*
path = {route:?}
layout = "emptyLayout"
*/
import {{ dependency }} from '../{name}-dependency.js';
import '../{name}.css';
(globalThis.__scanExecutions ||= []).push('scan-{name}-executed');
export default {{ name: {name:?}, dependency }};
"#
    )
}

fn create_scan_fixture(root: &Path, generate_source: bool) {
    write(root, "package.json", r#"{"type":"module"}"#);
    write(
        root,
        "wake.config.toml",
        &format!(
            r#"[html]
entry = "src/main.jsx"

[[component_scan]]
namespace = "pages"
cwd = "src/pages"
include = '\.page\.ts$'
generate_source = {generate_source}
"#
        ),
    );
    write(
        root,
        "src/main.jsx",
        r#"import pages from '@@@/pages';
globalThis.__scanBoot = String(Math.random());
globalThis.__scanMenu = pages.map(page => page.frontmatter?.path);
globalThis.__scanMetadata = pages.map(({ name, path, frontmatter, source, component }) => ({
  name, path, frontmatter, source, loaderType: typeof component
}));
// Capturing/destructuring a loader must be as inert as reading the menu.
globalThis.__scanLoaders = pages.map(({ component }) => component);
"#,
    );
    for (name, route, color) in [
        ("Login", "/user/login", "red"),
        ("Editor", "/editor", "blue"),
    ] {
        write(
            root,
            &format!("src/pages/{name}.page.ts"),
            &scan_page(name, route),
        );
        write(
            root,
            &format!("src/{name}-dependency.js"),
            &format!(
                "(globalThis.__scanExecutions ||= []).push('scan-{name}-dependency'); export const dependency = '{name}-dependency';\n"
            ),
        );
        write(
            root,
            &format!("src/{name}.css"),
            &format!(":root {{ --scan-{name}: {color}; }}\n"),
        );
    }
    // The include filter still excludes unrelated files from the metadata list.
    write(
        root,
        "src/pages/ignored.ts",
        "throw new Error('excluded scan file');",
    );
}

fn scan_state(page: &BrowserPage) -> Result<Value, String> {
    page.evaluate(r#"({
      boot: globalThis.__scanBoot,
      menu: globalThis.__scanMenu,
      metadata: globalThis.__scanMetadata,
      executions: globalThis.__scanExecutions || [],
      loginStyle: getComputedStyle(document.documentElement).getPropertyValue('--scan-Login').trim(),
      editorStyle: getComputedStyle(document.documentElement).getPropertyValue('--scan-Editor').trim(),
      styles: Array.from(document.styleSheets, sheet => ({href: sheet.href, rules: Array.from(sheet.cssRules, rule => rule.cssText)}))
    })"#).map_err(|error| error.to_string())
}

fn wait_for_scan(page: &BrowserPage, ready: impl Fn(&Value) -> bool) -> Value {
    let deadline = Instant::now() + WAIT;
    let mut last = String::new();
    while Instant::now() < deadline {
        match scan_state(page) {
            Ok(state) if ready(&state) => return state,
            Ok(state) => last = state.to_string(),
            Err(error) => last = error,
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!("scan page did not become ready: {last}");
}

fn wait_for_scan_update(page: &BrowserPage, previous_boot: &Value, menu: Value) -> Value {
    let deadline = Instant::now() + WAIT;
    let mut boot = Value::Null;
    let mut stable_since = Instant::now();
    let mut last = String::new();
    // File creation/deletion can deliver separate watcher batches. Exercise the final document,
    // after the reload burst settles, rather than racing a loader call against another navigation.
    while Instant::now() < deadline {
        match scan_state(page) {
            Ok(state) if state["boot"] != *previous_boot && state["menu"] == menu => {
                if state["boot"] != boot {
                    boot = state["boot"].clone();
                    stable_since = Instant::now();
                } else if stable_since.elapsed() >= Duration::from_millis(750) {
                    return state;
                }
                last = state.to_string();
            }
            other => {
                boot = Value::Null;
                stable_since = Instant::now();
                last = format!("{other:?}");
            }
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!("scan update did not settle: {last}");
}

fn scan_requests(evidence: &mut Evidence, page: &BrowserPage) -> Vec<String> {
    evidence.capture(page);
    evidence
        .events
        .iter()
        .filter(|item| {
            item["method"] == "Network.requestWillBeSent"
                && matches!(
                    item["event"]["params"]["type"].as_str(),
                    Some("Script" | "Stylesheet")
                )
        })
        .map(|item| {
            item["event"]["params"]["request"]["url"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect()
}

fn assert_scan_is_idle(page: &BrowserPage, generate_source: bool) -> Value {
    wait_for_scan(page, |state| {
        state["metadata"]
            .as_array()
            .is_some_and(|items| items.len() == 2)
    });
    // Give an accidentally eager import time to complete even though the entry has finished.
    page.evaluate("new Promise(resolve => setTimeout(resolve, 250))")
        .unwrap();
    let state = scan_state(page).unwrap();
    assert_eq!(
        state["executions"],
        json!([]),
        "metadata import executed pages: {state}"
    );
    assert_eq!(state["loginStyle"], "");
    assert_eq!(state["editorStyle"], "");
    assert_eq!(state["menu"], json!(["/editor", "/user/login"]));
    for (index, name, route) in [(0, "Editor", "/editor"), (1, "Login", "/user/login")] {
        let metadata = &state["metadata"][index];
        assert_eq!(metadata["loaderType"], "function");
        assert_eq!(metadata["name"], format!("___src_pages_{name}_page_ts"));
        assert_eq!(metadata["path"], format!("/src/pages/{name}.page.ts"));
        assert_eq!(
            metadata["frontmatter"],
            json!({"path": route, "layout": "emptyLayout"})
        );
        assert_eq!(
            metadata["source"],
            if generate_source {
                json!(scan_page(name, route))
            } else {
                Value::Null
            }
        );
    }
    state
}

fn assert_scan_loads_on_call(
    page: &BrowserPage,
    evidence: &mut Evidence,
    generate_source: bool,
    production: bool,
) {
    assert_scan_is_idle(page, generate_source);
    let initial = scan_requests(evidence, page);
    assert_eq!(
        initial.len(),
        2,
        "metadata-only entry requested page assets: {initial:?}"
    );
    let initial_files = initial
        .iter()
        .map(|url| url.rsplit('/').next().unwrap())
        .collect::<Vec<_>>();
    if production {
        assert!(
            initial_files
                .iter()
                .any(|file| file.starts_with("entry.") && file.ends_with(".js"))
        );
        assert!(
            initial_files
                .iter()
                .any(|file| file.starts_with("main.") && file.ends_with(".js"))
        );
    } else {
        assert!(initial.iter().any(|url| url.ends_with("/__wake/client.js")));
        assert!(initial_files.contains(&"bundle.js"));
    }

    let loaded = page
        .evaluate("__scanLoaders[1]().then(module => module.default)")
        .unwrap();
    assert_eq!(
        loaded,
        json!({"name": "Login", "dependency": "Login-dependency"})
    );
    let login = scan_state(page).unwrap();
    assert_eq!(
        login["executions"],
        json!(["scan-Login-dependency", "scan-Login-executed"])
    );
    assert_eq!(login["loginStyle"], "red", "login style state: {login}");
    assert_eq!(login["editorStyle"], "");
    let after_login = scan_requests(evidence, page);
    assert!(after_login.len() > initial.len());
    assert_eq!(
        after_login
            .iter()
            .filter(|url| url.ends_with(".css"))
            .count(),
        usize::from(production)
    );

    // The other page's exclusive code must not even have been downloaded. Source inlining
    // intentionally puts page text into the metadata entry, so only that case excludes the entry.
    for event in &evidence.events {
        if event["method"] == "Network.responseReceived"
            && matches!(
                event["event"]["params"]["type"].as_str(),
                Some("Script" | "Stylesheet")
            )
            && (!generate_source
                || !initial
                    .iter()
                    .any(|url| event["event"]["params"]["response"]["url"] == *url))
        {
            let body = page
                .command(
                    "Network.getResponseBody",
                    json!({"requestId": event["event"]["params"]["requestId"]}),
                )
                .unwrap();
            let text = body["body"].as_str().unwrap();
            assert!(
                !text.contains("scan-Editor"),
                "downloaded unvisited Editor code/style: {text}"
            );
        }
    }

    page.evaluate("Promise.all([__scanLoaders[1](), __scanLoaders[1]()])")
        .unwrap();
    assert_eq!(scan_state(page).unwrap()["executions"], login["executions"]);
    assert_eq!(
        scan_requests(evidence, page),
        after_login,
        "repeat loading requested duplicate assets"
    );

    let loaded = page
        .evaluate("__scanLoaders[0]().then(module => module.default)")
        .unwrap();
    assert_eq!(
        loaded,
        json!({"name": "Editor", "dependency": "Editor-dependency"})
    );
    let editor = scan_state(page).unwrap();
    assert_eq!(
        editor["executions"],
        json!([
            "scan-Login-dependency",
            "scan-Login-executed",
            "scan-Editor-dependency",
            "scan-Editor-executed"
        ])
    );
    assert_eq!(editor["editorStyle"], "blue");
    let after_editor = scan_requests(evidence, page);
    assert!(after_editor.len() > after_login.len());
    assert_eq!(
        after_editor
            .iter()
            .filter(|url| url.ends_with(".css"))
            .count(),
        if production { 2 } else { 0 }
    );
    eprintln!(
        "component scan production={production} source={generate_source}: asset requests {}/{}/{}, executions {}",
        initial.len(),
        after_login.len(),
        after_editor.len(),
        editor["executions"]
    );
}

// A small static host serves the actual production output; it never compiles or injects dev code.
struct ScanStaticServer {
    origin: String,
    stop: Arc<AtomicBool>,
    join: Option<thread::JoinHandle<()>>,
}

impl ScanStaticServer {
    fn start(root: PathBuf) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let join = thread::spawn(move || {
            while !worker_stop.load(Ordering::Acquire) {
                let (mut stream, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(error) => panic!("scan static host failed: {error}"),
                };
                // Windows can inherit the listener's nonblocking mode. Partial writes would
                // corrupt the JS/CSS evidence, so serve complete requests and responses.
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut request = Vec::new();
                let mut chunk = [0_u8; 2048];
                while request.len() < 64 * 1024
                    && !request.windows(4).any(|part| part == b"\r\n\r\n")
                {
                    match stream.read(&mut chunk) {
                        Ok(0) | Err(_) => break,
                        Ok(read) => request.extend_from_slice(&chunk[..read]),
                    }
                }
                if !request.windows(4).any(|part| part == b"\r\n\r\n") {
                    continue;
                }
                let head = String::from_utf8_lossy(&request);
                let path = head
                    .lines()
                    .next()
                    .unwrap_or("")
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or("/");
                let relative = path.trim_start_matches('/');
                let safe = !relative.contains(['\\', ':'])
                    && !relative.split('/').any(|part| part == ".." || part == ".");
                let file = root.join(if relative.is_empty() {
                    "index.html"
                } else {
                    relative
                });
                let found = safe.then(|| fs::read(&file).ok()).flatten();
                let (status, bytes) = match found {
                    Some(bytes) => ("200 OK", bytes),
                    None => ("404 Not Found", b"not found".to_vec()),
                };
                let mime = match file.extension().and_then(|ext| ext.to_str()) {
                    Some("html") => "text/html",
                    Some("js") => "text/javascript",
                    Some("css") => "text/css",
                    _ => "application/octet-stream",
                };
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    bytes.len()
                );
                let _ = stream.write_all(&bytes);
            }
        });
        Self {
            origin,
            stop,
            join: Some(join),
        }
    }
}

impl Drop for ScanStaticServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.join.take().unwrap().join().unwrap();
    }
}

#[test]
#[ignore = "requires an installed system Chromium browser"]
fn chromium_component_scan_production_loads_only_called_pages_and_css() {
    let driver = browser();
    eprintln!("component scan browser: {:?}", driver.installation);
    for generate_source in [false, true] {
        let fixture = tempfile::Builder::new()
            .prefix("wake-scan-production-")
            .tempdir()
            .unwrap();
        create_scan_fixture(fixture.path(), generate_source);
        let result = build(
            BuildOptions {
                project: ProjectOptions {
                    cwd: Some(fixture.path().to_path_buf()),
                    config_path: None,
                },
                outdir: Some(PathBuf::from("dist")),
                ..BuildOptions::default()
            },
            &CancellationToken::default(),
        )
        .unwrap();
        assert!(result.success, "{:?}", result.diagnostics);
        let server = ScanStaticServer::start(fixture.path().join("dist"));
        let context = driver.create_context().unwrap();
        let page = context.new_page("about:blank").unwrap();
        page.command("Network.enable", json!({})).unwrap();
        page.command("Network.setCacheDisabled", json!({"cacheDisabled": true}))
            .unwrap();
        with_evidence("component-scan-production", &page, |evidence| {
            page.navigate(&format!("{}/", server.origin)).unwrap();
            assert_scan_loads_on_call(&page, evidence, generate_source, true);
        });
    }
}

#[test]
#[ignore = "requires an installed system Chromium browser"]
fn chromium_component_scan_development_loads_on_call_and_refreshes_metadata() {
    let driver = browser();
    eprintln!("component scan browser: {:?}", driver.installation);
    for generate_source in [false, true] {
        let fixture = tempfile::Builder::new()
            .prefix("wake-scan-development-")
            .tempdir()
            .unwrap();
        create_scan_fixture(fixture.path(), generate_source);
        let port = reserve_port();
        let server = RunningServer::start(fixture.path(), port);
        let context = driver.create_context().unwrap();
        let page = context.new_page("about:blank").unwrap();
        page.command("Network.enable", json!({})).unwrap();
        page.command("Network.setCacheDisabled", json!({"cacheDisabled": true}))
            .unwrap();
        with_evidence("component-scan-development", &page, |evidence| {
            page.navigate(&format!("http://127.0.0.1:{port}/")).unwrap();
            assert_scan_loads_on_call(&page, evidence, generate_source, false);
            let deadline = Instant::now() + WAIT;
            while !evidence.has_live_reload_socket() && Instant::now() < deadline {
                evidence.capture(&page);
                thread::sleep(Duration::from_millis(20));
            }
            assert!(evidence.has_live_reload_socket());

            let initial = scan_state(&page).unwrap();
            write(
                fixture.path(),
                "src/pages/Login.page.ts",
                &scan_page("Login", "/sign-in"),
            );
            server.await_rebuild();
            let edited =
                wait_for_scan_update(&page, &initial["boot"], json!(["/editor", "/sign-in"]));
            assert_eq!(edited["executions"], json!([]));
            assert_eq!(
                edited["metadata"][1]["source"],
                if generate_source {
                    json!(scan_page("Login", "/sign-in"))
                } else {
                    Value::Null
                }
            );

            write(
                fixture.path(),
                "src/pages/New.page.ts",
                "/* path = '/new' */\n(globalThis.__scanExecutions ||= []).push('new'); export default 'new';\n",
            );
            server.await_rebuild();
            let added = wait_for_scan_update(
                &page,
                &edited["boot"],
                json!(["/editor", "/sign-in", "/new"]),
            );
            assert_eq!(added["executions"], json!([]));
            assert_eq!(
                page.evaluate("__scanLoaders[2]().then(module => module.default)")
                    .unwrap(),
                "new"
            );
            assert_eq!(scan_state(&page).unwrap()["executions"], json!(["new"]));

            fs::remove_file(fixture.path().join("src/pages/Editor.page.ts")).unwrap();
            server.await_rebuild();
            let removed = wait_for_scan_update(&page, &added["boot"], json!(["/sign-in", "/new"]));
            assert_eq!(removed["executions"], json!([]));
            assert_eq!(removed["loginStyle"], "");
            assert_eq!(removed["editorStyle"], "");
            assert_eq!(removed["metadata"][0]["path"], "/src/pages/Login.page.ts");
            assert_eq!(
                page.evaluate("__scanLoaders[0]().then(module => module.default.name)")
                    .unwrap(),
                "Login"
            );
            eprintln!(
                "component scan source={generate_source}: frontmatter edit, addition, deletion reloaded successfully"
            );
        });
    }
}
