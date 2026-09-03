use std::fs;
use std::io::{Read as _, Write as _};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};
use wake_app::{
    BuildOptions, CancellationToken, DevServer, DevServerEvent, DevServerOptions, ProjectOptions,
    build, start_dev_server,
};
use wake_common::zip::ZipArchive;
use wake_test_browser::{BrowserDriver, BrowserLaunchOptions};

fn write(root: &Path, path: &str, contents: &str) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().expect("fixture file has a parent")).unwrap();
    fs::write(path, contents).unwrap();
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("wake_test lives below the workspace root")
        .to_path_buf()
}

fn cached_package_archive(package: &str, version: &str) -> PathBuf {
    let prefix = format!("{package}-npm-{version}-");
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
        "expected exactly one Yarn cache archive matching {prefix}*.zip; run `corepack yarn install --immutable-cache`: {matches:?}"
    );
    matches.pop().unwrap()
}

fn extract_zip_tree(archive: &ZipArchive, source: &str, target: &Path) {
    assert!(archive.is_dir(source), "ZIP directory is missing: {source}");
    fs::create_dir_all(target).unwrap();
    for child in archive.read_dir(source) {
        let child_name = child
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .expect("ZIP child has a basename");
        let child_target = target.join(child_name);
        if archive.is_dir(&child) {
            extract_zip_tree(archive, &child, &child_target);
        } else {
            let bytes = archive
                .read(&child)
                .unwrap()
                .unwrap_or_else(|| panic!("ZIP file disappeared: {child}"));
            fs::write(child_target, bytes).unwrap();
        }
    }
}

fn install_cached_package(root: &Path, cache_name: &str, version: &str, package_name: &str) {
    let archive_path = cached_package_archive(cache_name, version);
    let archive = ZipArchive::parse(fs::read(&archive_path).unwrap())
        .unwrap_or_else(|error| panic!("cannot parse {}: {error}", archive_path.display()));
    extract_zip_tree(
        &archive,
        &format!("node_modules/{package_name}"),
        &root.join("node_modules").join(package_name),
    );
}

fn install_react_fixture(root: &Path, react_version: &str, scheduler_version: &str) {
    install_cached_package(root, "react", react_version, "react");
    install_cached_package(root, "react-dom", react_version, "react-dom");
    install_cached_package(root, "scheduler", scheduler_version, "scheduler");
    install_cached_package(root, "loose-envify", "1.4.0", "loose-envify");
    install_cached_package(root, "object-assign", "4.1.1", "object-assign");

    // React's runtime packages intentionally do not ship declarations. These fixture-only,
    // no-`any` declarations keep Wake's fail-closed federation type emitter enabled while the
    // browser executes the package's actual pinned JavaScript implementation.
    write(
        root,
        "node_modules/react/index.d.ts",
        r#"export interface ReactElement {
  readonly type: unknown;
  readonly props: Readonly<Record<string, unknown>>;
  readonly key: string | null;
}
export interface Context<T> {
  readonly Provider: unknown;
  readonly Consumer: unknown;
}
export const version: string;
export function createElement(type: unknown, props?: Readonly<Record<string, unknown>> | null, ...children: readonly unknown[]): ReactElement;
export function createContext<T>(defaultValue: T): Context<T>;
export function useContext<T>(context: Context<T>): T;
"#,
    );
    write(
        root,
        "node_modules/react/jsx-runtime.d.ts",
        r#"import type { ReactElement } from './index';
export const Fragment: unknown;
export function jsx(type: unknown, props: Readonly<Record<string, unknown>>, key?: string): ReactElement;
export function jsxs(type: unknown, props: Readonly<Record<string, unknown>>, key?: string): ReactElement;
"#,
    );
    write(
        root,
        "node_modules/react/jsx-dev-runtime.d.ts",
        r#"import type { ReactElement } from './index';
export const Fragment: unknown;
export function jsxDEV(type: unknown, props: Readonly<Record<string, unknown>>, key: string | undefined, isStaticChildren: boolean): ReactElement;
"#,
    );
    write(
        root,
        "node_modules/react-dom/index.d.ts",
        r#"import type { ReactElement } from 'react';
export const version: string;
export function render(element: ReactElement, container: Element): void;
export function unmountComponentAtNode(container: Element): boolean;
export function createPortal(children: ReactElement, container: Element): ReactElement;
"#,
    );
    write(
        root,
        "node_modules/react-dom/client.d.ts",
        r#"import type { ReactElement } from 'react';
export interface Root {
  render(element: ReactElement): void;
  unmount(): void;
}
export function createRoot(container: Element): Root;
"#,
    );
}

fn install_federation_react_helper(root: &Path) {
    let source = workspace_root().join("npm/wake");
    write(
        root,
        "node_modules/@crab-dev/wake/package.json",
        r#"{
  "name": "@crab-dev/wake",
  "version": "0.0.0-federation-browser-fixture",
  "type": "module",
  "exports": {
    "./federation": { "types": "./federation.d.mts", "import": "./federation.mjs" },
    "./federation/react": { "types": "./federation-react.d.ts", "import": "./federation-react.mjs" }
  }
}"#,
    );
    for file in [
        "federation.mjs",
        "federation.d.mts",
        "federation-react.mjs",
        "federation-react.d.ts",
    ] {
        let contents = fs::read(source.join(file)).unwrap();
        let target = root.join("node_modules/@crab-dev/wake").join(file);
        fs::write(target, contents).unwrap();
    }
}

fn react_shared_config(scope: &str, version: &str, group: &str, include_client: bool) -> String {
    let mut output = String::new();
    for share_key in [
        "react",
        "react/jsx-runtime",
        "react/jsx-dev-runtime",
        "react-dom",
    ] {
        output.push_str(&format!(
            r#"
[federation.shared.{share_key:?}]
scope = {scope:?}
required_version = {version:?}
singleton = true
strict = true
fallback = true
coherence_group = {group:?}
"#
        ));
    }
    if include_client {
        output.push_str(&format!(
            r#"
[federation.shared."react-dom/client"]
scope = {scope:?}
required_version = {version:?}
singleton = true
strict = true
fallback = true
coherence_group = {group:?}
"#
        ));
    }
    output
}

fn reserve_port() -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    listener.local_addr().unwrap().port()
}

struct StaticServer {
    origin: String,
    stop: Arc<AtomicBool>,
    join: Option<thread::JoinHandle<()>>,
}

impl StaticServer {
    fn start(root: PathBuf, port: u16) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", port)).unwrap();
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
                    Err(error) => panic!("static federation server failed: {error}"),
                };
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                let mut request = Vec::with_capacity(2048);
                let mut chunk = [0_u8; 2048];
                while request.len() < 64 * 1024
                    && !request.windows(4).any(|part| part == b"\r\n\r\n")
                {
                    match stream.read(&mut chunk) {
                        Ok(0) => break,
                        Ok(read) => request.extend_from_slice(&chunk[..read]),
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => continue,
                        Err(error) if error.kind() == std::io::ErrorKind::TimedOut => break,
                        Err(_) => break,
                    }
                }
                let head = String::from_utf8_lossy(&request);
                let mut request_line = head.lines().next().unwrap_or("").split_whitespace();
                let method = request_line.next().unwrap_or("");
                let raw_path = request_line.next().unwrap_or("/");
                let relative = raw_path
                    .split(['?', '#'])
                    .next()
                    .unwrap_or("")
                    .trim_start_matches('/');
                let safe = !relative.is_empty()
                    && !relative
                        .split('/')
                        .any(|segment| matches!(segment, "" | "." | ".."));
                let path = root.join(relative.replace('/', std::path::MAIN_SEPARATOR_STR));
                let body = safe.then(|| fs::read(&path).ok()).flatten();
                let (status, bytes, mime) = match body {
                    Some(bytes) => {
                        let mime = match path.extension().and_then(|extension| extension.to_str()) {
                            Some("js" | "mjs") => "text/javascript",
                            Some("json") => "application/json",
                            Some("map") => "application/source-map+json",
                            Some("css") => "text/css",
                            _ => "application/octet-stream",
                        };
                        ("200 OK", bytes, mime)
                    }
                    None => ("404 Not Found", b"not found".to_vec(), "text/plain"),
                };
                let has_source_map = status == "200 OK"
                    && matches!(
                        path.extension().and_then(|extension| extension.to_str()),
                        Some("js" | "mjs")
                    )
                    && path
                        .with_file_name(format!(
                            "{}.map",
                            path.file_name()
                                .and_then(|name| name.to_str())
                                .unwrap_or_default()
                        ))
                        .is_file();
                let source_map_header = if has_source_map {
                    let path = raw_path.split(['?', '#']).next().unwrap_or(raw_path);
                    format!("SourceMap: {path}.map\r\n")
                } else {
                    String::new()
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\nCross-Origin-Resource-Policy: cross-origin\r\n{source_map_header}Connection: close\r\n\r\n",
                    bytes.len()
                );
                let _ = stream.write_all(response.as_bytes());
                if method != "HEAD" {
                    let _ = stream.write_all(&bytes);
                }
            }
        });
        Self {
            origin: format!("http://127.0.0.1:{port}"),
            stop,
            join: Some(join),
        }
    }
}

impl Drop for StaticServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(join) = self.join.take() {
            join.join().unwrap();
        }
    }
}

fn write_shared_package(root: &Path, implementation: &str, execution_counter: &str) {
    write(
        root,
        "node_modules/singleton-lib/package.json",
        r#"{
          "name": "singleton-lib",
          "version": "1.0.0",
          "type": "module",
          "module": "./index.js",
          "main": "./index.js",
          "types": "./index.d.ts"
        }"#,
    );
    write(
        root,
        "node_modules/singleton-lib/index.js",
        &format!(
            "globalThis.{execution_counter}=(globalThis.{execution_counter}??0)+1;\nexport const value={implementation:?};\n"
        ),
    );
    write(
        root,
        "node_modules/singleton-lib/index.d.ts",
        "export declare const value: string;\n",
    );
}

fn start(root: &Path, port: u16) -> RunningServer {
    let server = start_dev_server(DevServerOptions {
        project: ProjectOptions {
            cwd: Some(root.to_path_buf()),
            config_path: None,
        },
        entry: Some(PathBuf::from("src/main.ts")),
        host: Some("127.0.0.1".to_owned()),
        port: Some(port),
        open: Some(false),
        federation: None,
    })
    .unwrap();
    RunningServer(server)
}

struct RunningServer(DevServer);

impl RunningServer {
    fn url(&self) -> &str {
        self.0.url()
    }

    fn assert_initial_build_succeeded(&self) {
        let events = self.0.drain_events();
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, DevServerEvent::Diagnostic { .. })),
            "development server reported initial diagnostics: {events:#?}"
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, DevServerEvent::Rebuilt { initial: true, .. })),
            "development server did not complete its initial build: {events:#?}"
        );
    }
}

impl Drop for RunningServer {
    fn drop(&mut self) {
        let _ = self.0.close();
    }
}

fn drain_script_events(page: &wake_test_browser::BrowserPage) -> Vec<Value> {
    drain_events(page, "Debugger.scriptParsed")
}

fn drain_events(page: &wake_test_browser::BrowserPage, method: &str) -> Vec<Value> {
    let mut events = Vec::new();
    while let Some(event) = page.take_event(method) {
        events.push(event);
    }
    events
}

fn event_string<'a>(event: &'a Value, field: &str) -> Option<&'a str> {
    event
        .get("params")
        .and_then(|params| params.get(field))
        .and_then(Value::as_str)
}

fn browser_options() -> BrowserLaunchOptions {
    // This ignored integration test only executes locally generated loopback fixtures. Disabling
    // the OS sandbox also lets managed Windows Chromium builds create an off-the-record CDP page.
    BrowserLaunchOptions {
        sandbox: false,
        executable: std::env::var_os("WAKE_FEDERATION_E2E_BROWSER").map(PathBuf::from),
        ..BrowserLaunchOptions::default()
    }
}

#[test]
#[ignore = "requires an installed system Chromium browser"]
fn chromium_loads_a_wake_remote_reuses_the_host_singleton_and_discovers_remote_maps() {
    let fixture = tempfile::Builder::new()
        .prefix("wake-federation-chromium-")
        // Keep the fixture outside the repository's Yarn PnP boundary. It intentionally owns
        // independent host/remote node_modules trees so their PackageKey contexts can be tested.
        .tempdir()
        .unwrap();
    let remote_root = fixture.path().join("remote");
    let host_root = fixture.path().join("host");
    let remote_port = reserve_port();

    write(
        &remote_root,
        "wake.config.toml",
        r#"[federation]
enabled = true
name = "catalog"

[federation.exposes.Button]
entry = "src/button.ts"
mode = "generic"

[federation.shared.singleton-lib]
required_version = "1.0.0"
singleton = true
strict = true
fallback = true
"#,
    );
    write(
        &remote_root,
        "src/main.ts",
        "globalThis.__remoteStandaloneStarts=(globalThis.__remoteStandaloneStarts??0)+1;\n",
    );
    write(
        &remote_root,
        "src/button.ts",
        "import {value} from 'singleton-lib';\nexport const resolvedValue: string = value;\nexport async function loadLazyValue(): Promise<string> { const lazy = await import('./lazy'); return lazy.lazyValue; }\n",
    );
    write(
        &remote_root,
        "src/lazy.ts",
        "export const lazyValue: string = 'remote-lazy';\n",
    );
    write_shared_package(&remote_root, "remote-fallback", "__remoteSharedExecutions");
    let remote = start(&remote_root, remote_port);
    remote.assert_initial_build_succeeded();
    let remote_origin = remote.url().trim_end_matches('/').to_owned();
    let manifest_url = format!("{remote_origin}/wake-federation.json");

    write(
        &host_root,
        "wake.config.toml",
        &format!(
            r#"[federation]
enabled = true
name = "shell"

[federation.remotes.catalog]
manifest_url = {manifest_url:?}
allowed_origins = [{remote_origin:?}]
dev_follow = true

[federation.shared.singleton-lib]
required_version = "1.0.0"
singleton = true
strict = true
fallback = true
"#
        ),
    );
    write_shared_package(&host_root, "host-provider", "__hostSharedExecutions");
    write(
        &host_root,
        "src/main.ts",
        r#"import {value as hostValue} from 'singleton-lib';
globalThis.__hostApplicationStarted=true;
import('catalog/Button').then(async (remote) => {
  const lazyValue=await remote.loadLazyValue();
  const runtime=globalThis[Symbol.for('wake.federation.v1')];
  globalThis.__federationE2e={
    hostValue,
    remoteValue: remote.resolvedValue,
    lazyValue,
    decision: runtime.explain('catalog/Button'),
  };
}, (error) => {
  globalThis.__federationE2eError={
    code: error?.code ?? null,
    message: String(error?.message ?? error),
    details: error?.details ?? null,
    stack: error?.stack ?? null,
  };
});
"#,
    );
    let host = start(&host_root, reserve_port());
    host.assert_initial_build_succeeded();

    let driver = BrowserDriver::launch(browser_options()).unwrap();
    let context = driver.create_context().unwrap();
    let page = context.new_page("about:blank").unwrap();
    page.command("Debugger.enable", json!({})).unwrap();
    page.navigate(host.url()).unwrap();
    let observed = page
        .evaluate_with_timeout(
            r#"new Promise((resolve) => {
              const deadline=Date.now()+20000;
              const inspect=()=>{
                if(globalThis.__federationE2e||globalThis.__federationE2eError){
                  resolve({
                    result: globalThis.__federationE2e ?? null,
                    error: globalThis.__federationE2eError ?? null,
                    hostApplicationStarted: globalThis.__hostApplicationStarted ?? false,
                    hostSharedExecutions: globalThis.__hostSharedExecutions ?? 0,
                    remoteSharedExecutions: globalThis.__remoteSharedExecutions ?? 0,
                    remoteStandaloneStarts: globalThis.__remoteStandaloneStarts ?? 0,
                  });
                  return;
                }
                if(Date.now()>=deadline){
                  resolve({timeout:true});
                  return;
                }
                setTimeout(inspect,20);
              };
              inspect();
            })"#,
            Some(25_000),
        )
        .unwrap();

    if observed.get("timeout").is_some() {
        let page_state = page
            .evaluate(
                r#"({
                  location: location.href,
                  readyState: document.readyState,
                  scripts: [...document.scripts].map(script=>({src:script.src,type:script.type})),
                  resources: performance.getEntriesByType('resource').map(entry=>entry.name),
                  broker: typeof globalThis[Symbol.for('wake.federation.v1')],
                  containers: Object.keys(globalThis[Symbol.for('wake.federation.exposes.v1')] ?? {}),
                  localDecision: globalThis[Symbol.for('wake.federation.v1')]?.explain('shell/__local__') ?? null,
                  body: document.body?.innerText ?? '',
                })"#,
            )
            .unwrap();
        let exceptions = drain_events(&page, "Runtime.exceptionThrown");
        let exception_properties = exceptions
            .iter()
            .filter_map(|event| {
                event["params"]["exceptionDetails"]["exception"]["objectId"]
                    .as_str()
                    .map(|object_id| {
                        page.command(
                            "Runtime.getProperties",
                            json!({"objectId": object_id, "ownProperties": true}),
                        )
                    })
            })
            .collect::<Vec<_>>();
        let exception_details = exception_properties
            .iter()
            .filter_map(|properties| properties.as_ref().ok())
            .flat_map(|properties| {
                properties["result"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|property| property["name"] == "details")
                    .filter_map(|property| property["value"]["objectId"].as_str())
            })
            .map(|object_id| {
                page.command(
                    "Runtime.getProperties",
                    json!({"objectId": object_id, "ownProperties": true}),
                )
            })
            .collect::<Vec<_>>();
        let console = drain_events(&page, "Runtime.consoleAPICalled");
        panic!(
            "federation page timed out: state={page_state:#}, exceptions={exceptions:#?}, exception_properties={exception_properties:#?}, exception_details={exception_details:#?}, console={console:#?}"
        );
    }
    assert_eq!(observed["error"], Value::Null, "{observed:#}");
    assert_eq!(observed["hostApplicationStarted"], true, "{observed:#}");
    assert_eq!(observed["result"]["hostValue"], "host-provider");
    assert_eq!(observed["result"]["remoteValue"], "host-provider");
    assert_eq!(observed["result"]["lazyValue"], "remote-lazy");
    assert_eq!(observed["hostSharedExecutions"], 1, "{observed:#}");
    assert_eq!(observed["remoteSharedExecutions"], 0, "{observed:#}");
    assert_eq!(observed["remoteStandaloneStarts"], 0, "{observed:#}");

    let decision = &observed["result"]["decision"];
    assert_eq!(decision["status"], "loaded", "{decision:#}");
    assert_eq!(decision["container"]["name"], "catalog", "{decision:#}");
    let build_id = decision["container"]["buildId"]
        .as_str()
        .expect("runtime decision has a remote buildId");
    let shared = decision["shared"]
        .as_array()
        .expect("runtime decision has shared selections");
    assert!(
        shared.iter().any(|entry| {
            entry["shareKey"] == "singleton-lib"
                && entry["requested"] == "1.0.0"
                && entry["selected"] == "1.0.0"
                && entry["owner"] == "shell"
                && entry["source"] == "host"
        }),
        "{decision:#}"
    );

    let scripts = drain_script_events(&page);
    let namespace = format!("wake://catalog@{build_id}/");
    let remote_entry_source = format!("{namespace}remoteEntry.mjs");
    let mapped_remote_scripts = scripts
        .iter()
        .filter(|event| {
            event_string(event, "url")
                .is_some_and(|url| url.starts_with(&remote_origin) || url.starts_with(&namespace))
                || event_string(event, "embedderName")
                    .is_some_and(|url| url.starts_with(&remote_origin))
        })
        .filter(|event| event_string(event, "sourceMapURL").is_some_and(|url| !url.is_empty()))
        .collect::<Vec<_>>();
    assert!(
        mapped_remote_scripts
            .iter()
            .any(|event| { event_string(event, "url") == Some(remote_entry_source.as_str()) }),
        "remoteEntry was not reported with a source map: {scripts:#?}"
    );
    assert!(
        mapped_remote_scripts.iter().any(|event| {
            event_string(event, "url").is_some_and(|url| url.starts_with(&remote_origin))
        }),
        "the exposed module chunk was not reported with a source map: {scripts:#?}"
    );

    let mut lazy_chunk_map = None;
    for event in mapped_remote_scripts {
        let source_map_url = event_string(event, "sourceMapURL").unwrap();
        let script_url = event_string(event, "url").unwrap();
        let network_url = event_string(event, "embedderName")
            .filter(|url| url.starts_with("http://") || url.starts_with("https://"))
            .unwrap_or(script_url);
        let expression = format!(
            "fetch(new URL({},{}).href).then(response=>{{if(!response.ok)throw new Error('map '+response.status);return response.text()}})",
            serde_json::to_string(source_map_url).unwrap(),
            serde_json::to_string(network_url).unwrap(),
        );
        let source_map = page
            .evaluate_with_timeout(&expression, Some(5_000))
            .unwrap();
        let source_map = source_map.as_str().expect("source map response is text");
        assert!(
            source_map.contains(&namespace),
            "source map {source_map_url} did not use {namespace}: {source_map}"
        );
        if source_map.contains("lazy.ts") {
            lazy_chunk_map = Some((script_url.to_owned(), source_map_url.to_owned()));
        }
    }
    assert!(
        lazy_chunk_map.is_some(),
        "the executed remote dynamic-import chunk did not publish a {namespace} Source Map"
    );
}

#[test]
#[ignore = "requires an installed system Chromium browser"]
fn chromium_discovers_a_public_source_map_for_a_minified_production_lazy_chunk() {
    let fixture = tempfile::Builder::new()
        .prefix("wake-federation-minified-map-chromium-")
        .tempdir()
        .unwrap();
    let remote_root = fixture.path().join("remote");
    let remote_out = remote_root.join("dist");
    let host_root = fixture.path().join("host");

    write(
        &remote_root,
        "wake.config.toml",
        r#"[federation]
enabled = true
name = "catalog"

[federation.exposes.Widget]
entry = "src/widget.ts"
mode = "generic"
"#,
    );
    write(
        &remote_root,
        "src/main.ts",
        "globalThis.__productionRemoteApplicationEntryExecuted=true;\n",
    );
    write(
        &remote_root,
        "src/widget.ts",
        r#"export async function loadProductionLazyValue(): Promise<string> {
  const namespace = await import('./lazy');
  return namespace.productionLazyValue;
}
"#,
    );
    write(
        &remote_root,
        "src/lazy.ts",
        "export const productionLazyValue: string = 'minified-production-lazy';\n",
    );
    let built = build(
        BuildOptions {
            project: ProjectOptions {
                cwd: Some(remote_root.clone()),
                config_path: None,
            },
            entry: Some(PathBuf::from("src/main.ts")),
            outdir: Some(remote_out.clone()),
            source_map: true,
            write: true,
            ..BuildOptions::default()
        },
        &CancellationToken::default(),
    )
    .unwrap();
    assert!(
        built.success,
        "production build diagnostics: {:#?}",
        built.diagnostics
    );
    let remote = StaticServer::start(remote_out, reserve_port());
    let manifest_url = format!("{}/wake-federation.json", remote.origin);

    write(
        &host_root,
        "wake.config.toml",
        &format!(
            r#"[federation]
enabled = true
name = "shell"

[federation.remotes.catalog]
manifest_url = {manifest_url:?}
allowed_origins = [{origin:?}]
dev_follow = true
"#,
            origin = remote.origin,
        ),
    );
    write(
        &host_root,
        "src/main.ts",
        r#"import('catalog/Widget').then(async (remote) => {
  const value = await remote.loadProductionLazyValue();
  const runtime = globalThis[Symbol.for('wake.federation.v1')];
  globalThis.__minifiedFederationMapE2e = { value, decision: runtime.explain('catalog/Widget') };
}, (error) => {
  globalThis.__minifiedFederationMapE2eError = { code: error?.code ?? null, message: String(error?.message ?? error) };
});
"#,
    );
    let host = start(&host_root, reserve_port());
    host.assert_initial_build_succeeded();

    let driver = BrowserDriver::launch(browser_options()).unwrap();
    let context = driver.create_context().unwrap();
    let page = context.new_page("about:blank").unwrap();
    page.command("Debugger.enable", json!({})).unwrap();
    page.navigate(host.url()).unwrap();
    let observed = page
        .evaluate_with_timeout(
            r#"new Promise((resolve) => {
              const deadline=Date.now()+20000;
              const inspect=()=>{
                if(globalThis.__minifiedFederationMapE2e||globalThis.__minifiedFederationMapE2eError){
                  resolve({result:globalThis.__minifiedFederationMapE2e??null,error:globalThis.__minifiedFederationMapE2eError??null});
                  return;
                }
                if(Date.now()>=deadline){resolve({timeout:true});return;}
                setTimeout(inspect,20);
              };
              inspect();
            })"#,
            Some(25_000),
        )
        .unwrap();
    assert!(observed.get("timeout").is_none(), "{observed:#}");
    assert_eq!(observed["error"], Value::Null, "{observed:#}");
    assert_eq!(
        observed["result"]["value"], "minified-production-lazy",
        "{observed:#}"
    );
    let build_id = observed["result"]["decision"]["container"]["buildId"]
        .as_str()
        .expect("production runtime decision buildId");
    let namespace = format!("wake://catalog@{build_id}/");

    let scripts = drain_script_events(&page);
    let mut lazy_evidence = None;
    let mut mapped_sources = Vec::new();
    for event in scripts.iter().filter(|event| {
        event_string(event, "sourceMapURL").is_some_and(|url| !url.is_empty())
            && (event_string(event, "url")
                .is_some_and(|url| url.starts_with(&remote.origin) || url.starts_with(&namespace))
                || event_string(event, "embedderName")
                    .is_some_and(|url| url.starts_with(&remote.origin)))
    }) {
        let source_map_url = event_string(event, "sourceMapURL").unwrap();
        let script_url = event_string(event, "url").unwrap();
        let network_url = event_string(event, "embedderName")
            .filter(|url| url.starts_with("http://") || url.starts_with("https://"))
            .unwrap_or(script_url);
        let map_expression = format!(
            "fetch(new URL({},{}).href).then(response=>response.text())",
            serde_json::to_string(source_map_url).unwrap(),
            serde_json::to_string(network_url).unwrap(),
        );
        let source_map = page
            .evaluate_with_timeout(&map_expression, Some(5_000))
            .unwrap();
        let source_map = source_map.as_str().expect("public map response text");
        assert!(
            source_map.contains(&namespace),
            "production Source Map did not use {namespace}: {source_map}"
        );
        mapped_sources.push((
            script_url.to_owned(),
            source_map_url.to_owned(),
            serde_json::from_str::<Value>(source_map)
                .ok()
                .and_then(|map| map.get("sources").cloned()),
        ));
        if source_map.contains("lazy.ts") {
            let script_expression = format!(
                "fetch({}).then(response=>response.text())",
                serde_json::to_string(network_url).unwrap(),
            );
            let script = page
                .evaluate_with_timeout(&script_expression, Some(5_000))
                .unwrap();
            let script = script.as_str().expect("production chunk response text");
            lazy_evidence = Some((
                script_url.to_owned(),
                source_map_url.to_owned(),
                script.to_owned(),
            ));
        }
    }
    let (script_url, source_map_url, script) = lazy_evidence.unwrap_or_else(|| {
        panic!(
            "executed production lazy chunk has a public Source Map: scripts={scripts:#?}, mapped_sources={mapped_sources:#?}"
        )
    });
    assert!(
        script.contains("value:!0")
            && script.contains("const a=\"minified-production-lazy\"")
            && !script.contains("const productionLazyValue"),
        "production lazy module body was not minified/mangled: script={script_url}, map={source_map_url}, body={script}"
    );
}

#[test]
#[ignore = "requires an installed system Chromium browser and the immutable Yarn cache"]
fn chromium_runs_real_react_context_and_parallel_react_17_18_isolated_roots() {
    let fixture = tempfile::Builder::new()
        .prefix("wake-federation-react-chromium-")
        .tempdir()
        .unwrap();
    let catalog_root = fixture.path().join("catalog");
    let legacy_root = fixture.path().join("legacy");
    let modern_root = fixture.path().join("modern");
    let host_root = fixture.path().join("host");
    for root in [&catalog_root, &host_root, &modern_root] {
        install_react_fixture(root, "18.3.1", "0.23.2");
    }
    install_react_fixture(&legacy_root, "17.0.2", "0.20.2");
    install_federation_react_helper(&host_root);

    write(
        &catalog_root,
        "wake.config.toml",
        &format!(
            r#"[federation]
enabled = true
name = "catalog"

[federation.exposes.Reader]
entry = "src/reader.ts"
mode = "host-rendered"
scope = "host-react18"
shadow = "none"
{}
"#,
            react_shared_config("host-react18", "18.3.1", "host-react18", true)
        ),
    );
    write(
        &catalog_root,
        "src/main.ts",
        "globalThis.__catalogApplicationEntryExecuted=true;\n",
    );
    write(
        &catalog_root,
        "src/reader.ts",
        r#"import * as React from 'react';

export interface ReaderProps {
  readonly context: React.Context<string>;
}

export const createElementIdentity: typeof React.createElement = React.createElement;
export const useContextIdentity: typeof React.useContext = React.useContext;

export function Reader(props: ReaderProps): React.ReactElement {
  const value = React.useContext(props.context);
  return React.createElement('p', { id: 'context-value' }, value);
}
"#,
    );

    write(
        &legacy_root,
        "wake.config.toml",
        &format!(
            r#"[federation]
enabled = true
name = "legacy"

[federation.exposes.Card]
entry = "src/card.ts"
mode = "isolated"
scope = "react17"
shadow = "open"
{}
"#,
            react_shared_config("react17", "17.0.2", "react17", false)
        ),
    );
    write(
        &legacy_root,
        "src/main.ts",
        "globalThis.__legacyApplicationEntryExecuted=true;\n",
    );
    write(
        &legacy_root,
        "src/card.css",
        ".card { color: rgb(17, 34, 51); }\n",
    );
    write(
        &legacy_root,
        "src/card.ts",
        r#"declare function require(specifier: string): unknown;
require('./card.css');
import * as React from 'react';
import * as ReactDOM from 'react-dom';

export interface CardProps { readonly label?: string; }
export interface CardContext {
  readonly mountRoot: Element;
  readonly portalRoot: Element;
  readonly props: CardProps;
  readonly slots: Readonly<Record<string, Node>>;
  emit(type: string, detail: Readonly<{ version: string }>): boolean;
}
export interface CardInstance { readonly version: string; }

function view(context: CardContext): React.ReactElement {
  return React.createElement(
    'section',
    { className: 'card', 'data-react-version': React.version },
    React.createElement(
      'button',
      { id: 'select', onClick: () => context.emit('federated-select', { version: React.version }) },
      context.props.label ?? 'legacy',
    ),
    ReactDOM.createPortal(
      React.createElement('span', { id: 'portal' }, `portal-${React.version}`),
      context.portalRoot,
    ),
  );
}

export function mount(context: CardContext): CardInstance {
  const badge = context.slots.badge;
  if (badge !== undefined) context.portalRoot.append(badge);
  ReactDOM.render(view(context), context.mountRoot);
  return { version: React.version };
}
export function update(_instance: CardInstance, context: CardContext): void {
  ReactDOM.render(view(context), context.mountRoot);
}
export function unmount(_instance: CardInstance, context: CardContext): void {
  ReactDOM.unmountComponentAtNode(context.mountRoot);
}
"#,
    );

    write(
        &modern_root,
        "wake.config.toml",
        &format!(
            r#"[federation]
enabled = true
name = "modern"

[federation.exposes.Card]
entry = "src/card.ts"
mode = "isolated"
scope = "react18"
shadow = "open"
{}
"#,
            react_shared_config("react18", "18.3.1", "react18", true)
        ),
    );
    write(
        &modern_root,
        "src/main.ts",
        "globalThis.__modernApplicationEntryExecuted=true;\n",
    );
    write(
        &modern_root,
        "src/card.css",
        ".card { color: rgb(68, 85, 102); }\n",
    );
    write(
        &modern_root,
        "src/card.ts",
        r#"declare function require(specifier: string): unknown;
require('./card.css');
import * as React from 'react';
import * as ReactDOM from 'react-dom';
import { createRoot } from 'react-dom/client';
import type { Root } from 'react-dom/client';

export interface CardProps { readonly label?: string; }
export interface CardContext {
  readonly mountRoot: Element;
  readonly portalRoot: Element;
  readonly props: CardProps;
  readonly slots: Readonly<Record<string, Node>>;
  emit(type: string, detail: Readonly<{ version: string }>): boolean;
}
export interface CardInstance { readonly version: string; readonly root: Root; }

function view(context: CardContext): React.ReactElement {
  return React.createElement(
    'section',
    { className: 'card', 'data-react-version': React.version },
    React.createElement(
      'button',
      { id: 'select', onClick: () => context.emit('federated-select', { version: React.version }) },
      context.props.label ?? 'modern',
    ),
    ReactDOM.createPortal(
      React.createElement('span', { id: 'portal' }, `portal-${React.version}`),
      context.portalRoot,
    ),
  );
}

export function mount(context: CardContext): CardInstance {
  const badge = context.slots.badge;
  if (badge !== undefined) context.portalRoot.append(badge);
  const root = createRoot(context.mountRoot);
  root.render(view(context));
  return { version: React.version, root };
}
export function update(instance: CardInstance, context: CardContext): void {
  instance.root.render(view(context));
}
export function unmount(instance: CardInstance, _context: CardContext): void {
  instance.root.unmount();
}
"#,
    );

    let catalog_port = reserve_port();
    let legacy_port = reserve_port();
    let modern_port = reserve_port();
    let catalog = start(&catalog_root, catalog_port);
    catalog.assert_initial_build_succeeded();
    let legacy = start(&legacy_root, legacy_port);
    legacy.assert_initial_build_succeeded();
    let modern = start(&modern_root, modern_port);
    modern.assert_initial_build_succeeded();
    let catalog_origin = catalog.url().trim_end_matches('/').to_owned();
    let legacy_origin = legacy.url().trim_end_matches('/').to_owned();
    let modern_origin = modern.url().trim_end_matches('/').to_owned();

    write(
        &host_root,
        "wake.config.toml",
        &format!(
            r#"[federation]
enabled = true
name = "shell"

[federation.remotes.catalog]
manifest_url = {catalog_manifest:?}
allowed_origins = [{catalog_origin:?}]
dev_follow = true

[federation.remotes.legacy]
manifest_url = {legacy_manifest:?}
allowed_origins = [{legacy_origin:?}]
dev_follow = true

[federation.remotes.modern]
manifest_url = {modern_manifest:?}
allowed_origins = [{modern_origin:?}]
dev_follow = true
{}
"#,
            react_shared_config("host-react18", "18.3.1", "host-react18", true),
            catalog_manifest = format!("{catalog_origin}/wake-federation.json"),
            legacy_manifest = format!("{legacy_origin}/wake-federation.json"),
            modern_manifest = format!("{modern_origin}/wake-federation.json"),
        ),
    );
    write(
        &host_root,
        "src/main.ts",
        r#"import * as React from 'react';
import { createRoot } from 'react-dom/client';
import { createFederatedIsolatedBridge } from '@crab-dev/wake/federation/react';
import { getFederationRuntime } from '@crab-dev/wake/federation';

const runtime = getFederationRuntime();

async function waitFor(read: () => Element | null): Promise<Element> {
  const deadline = Date.now() + 10000;
  while (Date.now() < deadline) {
    const value = read();
    if (value !== null) return value;
    await new Promise<void>((resolve) => setTimeout(resolve, 10));
  }
  throw new Error('timed out waiting for a React commit');
}

async function run(): Promise<void> {
  const catalog = await import('catalog/Reader');
  const context = React.createContext('context-fallback');
  const hostRenderedRoot = document.createElement('div');
  hostRenderedRoot.id = 'host-rendered-root';
  document.body.append(hostRenderedRoot);
  const hostRoot = createRoot(hostRenderedRoot);
  hostRoot.render(
    React.createElement(
      context.Provider,
      { value: 'context-from-host' },
      React.createElement(catalog.Reader, { context }),
    ),
  );
  const contextValue = await waitFor(() => hostRenderedRoot.querySelector('#context-value'));

  const legacyHost = document.createElement('div');
  const modernHost = document.createElement('div');
  legacyHost.id = 'legacy-host';
  modernHost.id = 'modern-host';
  document.body.append(legacyHost, modernHost);
  const events: Array<Readonly<{ version: string; composed: boolean }>> = [];
  const recordEvent = (event: Event): void => {
    const detail = (event as CustomEvent<Readonly<{ version: string }>>).detail;
    events.push({ version: detail.version, composed: event.composed });
  };
  legacyHost.addEventListener('federated-select', recordEvent);
  modernHost.addEventListener('federated-select', recordEvent);

  const legacyBadge = document.createElement('strong');
  legacyBadge.id = 'legacy-slot';
  legacyBadge.textContent = 'legacy-slot';
  const modernBadge = document.createElement('strong');
  modernBadge.id = 'modern-slot';
  modernBadge.textContent = 'modern-slot';
  const [legacyBridge, modernBridge] = await Promise.all([
    createFederatedIsolatedBridge(runtime, 'legacy/Card'),
    createFederatedIsolatedBridge(runtime, 'modern/Card'),
  ]);
  const [legacyInstance, modernInstance] = await Promise.all([
    legacyBridge.mount(legacyHost, { label: 'legacy-card' }, { slots: { badge: legacyBadge } }),
    modernBridge.mount(modernHost, { label: 'modern-card' }, { slots: { badge: modernBadge } }),
  ]);
  const legacyCard = await waitFor(() => legacyBridge.shadowRoot?.querySelector('.card') ?? null);
  const modernCard = await waitFor(() => modernBridge.shadowRoot?.querySelector('.card') ?? null);
  const legacyButton = legacyBridge.shadowRoot?.querySelector<HTMLButtonElement>('#select');
  const modernButton = modernBridge.shadowRoot?.querySelector<HTMLButtonElement>('#select');
  if (legacyButton === undefined || legacyButton === null || modernButton === undefined || modernButton === null) {
    throw new Error('isolated React buttons were not rendered');
  }
  legacyButton.click();
  modernButton.click();

  const result = {
    hostReactVersion: React.version,
    sameCreateElement: catalog.createElementIdentity === React.createElement,
    sameUseContext: catalog.useContextIdentity === React.useContext,
    contextValue: contextValue.textContent,
    legacyVersion: (legacyInstance as Readonly<{ version: string }>).version,
    modernVersion: (modernInstance as Readonly<{ version: string }>).version,
    legacyShadowMode: legacyBridge.shadowRoot?.mode,
    modernShadowMode: modernBridge.shadowRoot?.mode,
    legacyColor: getComputedStyle(legacyCard).color,
    modernColor: getComputedStyle(modernCard).color,
    legacyPortal: legacyBridge.portalRoot?.querySelector('#portal')?.textContent,
    modernPortal: modernBridge.portalRoot?.querySelector('#portal')?.textContent,
    legacySlot: legacyBridge.portalRoot?.querySelector('#legacy-slot')?.textContent,
    modernSlot: modernBridge.portalRoot?.querySelector('#modern-slot')?.textContent,
    events,
    catalogApplicationEntryExecuted: globalThis.__catalogApplicationEntryExecuted ?? false,
    legacyApplicationEntryExecuted: globalThis.__legacyApplicationEntryExecuted ?? false,
    modernApplicationEntryExecuted: globalThis.__modernApplicationEntryExecuted ?? false,
  };
  await Promise.all([legacyBridge.unmount(), modernBridge.unmount()]);
  globalThis.__reactFederationE2e = {
    ...result,
    legacyUnmounted: legacyBridge.shadowRoot?.childNodes.length === 0,
    modernUnmounted: modernBridge.shadowRoot?.childNodes.length === 0,
  };
  hostRoot.unmount();
}

run().catch((error: unknown) => {
  globalThis.__reactFederationE2eError = {
    code: typeof error === 'object' && error !== null && 'code' in error ? error.code : null,
    message: error instanceof Error ? error.message : String(error),
    stack: error instanceof Error ? error.stack : null,
  };
});
"#,
    );
    let host = start(&host_root, reserve_port());
    host.assert_initial_build_succeeded();

    let driver = BrowserDriver::launch(browser_options()).unwrap();
    let context = driver.create_context().unwrap();
    let page = context.new_page("about:blank").unwrap();
    page.command("Runtime.enable", json!({})).unwrap();
    page.navigate(host.url()).unwrap();
    let observed = page
        .evaluate_with_timeout(
            r#"new Promise((resolve) => {
              const deadline=Date.now()+30000;
              const inspect=()=>{
                if(globalThis.__reactFederationE2e||globalThis.__reactFederationE2eError){
                  resolve({
                    result: globalThis.__reactFederationE2e ?? null,
                    error: globalThis.__reactFederationE2eError ?? null,
                  });
                  return;
                }
                if(Date.now()>=deadline){
                  resolve({timeout:true,body:document.body?.innerText??'',resources:performance.getEntriesByType('resource').map((entry)=>entry.name)});
                  return;
                }
                setTimeout(inspect,20);
              };
              inspect();
            })"#,
            Some(35_000),
        )
        .unwrap();

    if observed.get("timeout").is_some() {
        let exceptions = drain_events(&page, "Runtime.exceptionThrown");
        let console = drain_events(&page, "Runtime.consoleAPICalled");
        let state = page
            .evaluate(
                r#"({
                  location: location.href,
                  readyState: document.readyState,
                  broker: typeof globalThis[Symbol.for('wake.federation.v1')],
                  containers: Object.keys(globalThis[Symbol.for('wake.federation.exposes.v1')] ?? {}),
                })"#,
            )
            .unwrap();
        panic!(
            "React federation page timed out: observed={observed:#}, state={state:#}, exceptions={exceptions:#?}, console={console:#?}"
        );
    }
    assert_eq!(observed["error"], Value::Null, "{observed:#}");
    let result = &observed["result"];
    assert_eq!(result["hostReactVersion"], "18.3.1", "{result:#}");
    assert_eq!(result["sameCreateElement"], true, "{result:#}");
    assert_eq!(result["sameUseContext"], true, "{result:#}");
    assert_eq!(result["contextValue"], "context-from-host", "{result:#}");
    assert_eq!(result["legacyVersion"], "17.0.2", "{result:#}");
    assert_eq!(result["modernVersion"], "18.3.1", "{result:#}");
    assert_eq!(result["legacyShadowMode"], "open", "{result:#}");
    assert_eq!(result["modernShadowMode"], "open", "{result:#}");
    assert_eq!(result["legacyColor"], "rgb(17, 34, 51)", "{result:#}");
    assert_eq!(result["modernColor"], "rgb(68, 85, 102)", "{result:#}");
    assert_eq!(result["legacyPortal"], "portal-17.0.2", "{result:#}");
    assert_eq!(result["modernPortal"], "portal-18.3.1", "{result:#}");
    assert_eq!(result["legacySlot"], "legacy-slot", "{result:#}");
    assert_eq!(result["modernSlot"], "modern-slot", "{result:#}");
    assert_eq!(result["legacyUnmounted"], true, "{result:#}");
    assert_eq!(result["modernUnmounted"], true, "{result:#}");
    assert_eq!(
        result["catalogApplicationEntryExecuted"], false,
        "{result:#}"
    );
    assert_eq!(
        result["legacyApplicationEntryExecuted"], false,
        "{result:#}"
    );
    assert_eq!(
        result["modernApplicationEntryExecuted"], false,
        "{result:#}"
    );
    let events = result["events"].as_array().expect("isolated events");
    assert_eq!(events.len(), 2, "{result:#}");
    assert!(
        events
            .iter()
            .all(|event| event["composed"] == true && event["version"].is_string()),
        "{result:#}"
    );
    assert!(
        events.iter().any(|event| event["version"] == "17.0.2")
            && events.iter().any(|event| event["version"] == "18.3.1"),
        "{result:#}"
    );
}
