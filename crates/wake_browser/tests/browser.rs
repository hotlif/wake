use serde_json::json;
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread;
use std::time::Duration;
use wake_test_browser::{BrowserDriver, BrowserLaunchOptions};

fn browser_package(root: &Path) -> std::path::PathBuf {
    std::env::var_os("WAKE_BROWSER_PACKAGE_DIR")
        .map(Into::into)
        .unwrap_or_else(|| root.join("npm/wake"))
}

fn launch_browser() -> BrowserDriver {
    let driver = BrowserDriver::launch(BrowserLaunchOptions {
        executable: std::env::var_os("WAKE_SYSTEM_BROWSER_PATH")
            .filter(|path| !path.is_empty())
            .map(Into::into),
        sandbox: false,
        ..BrowserLaunchOptions::default()
    })
    .unwrap();
    eprintln!(
        "browser evidence: {}",
        serde_json::json!({
            "kind": driver.installation.kind, "executable": driver.installation.executable,
            "version": driver.installation.version, "headless": driver.is_headless(),
        })
    );
    driver
}

struct Server {
    address: String,
    stopped: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}
impl Server {
    fn new(files: BTreeMap<String, (Vec<u8>, &'static str)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        listener.set_nonblocking(true).unwrap();
        let stopped = Arc::new(AtomicBool::new(false));
        let flag = stopped.clone();
        let files = Arc::new(files);
        let thread = thread::spawn(move || {
            while !flag.load(Ordering::Acquire) {
                let Ok((stream, _)) = listener.accept() else {
                    thread::sleep(Duration::from_millis(5));
                    continue;
                };
                let files = files.clone();
                thread::spawn(move || {
                    if let Err(error) = serve_connection(stream, &files) {
                        eprintln!("browser fixture HTTP connection failed: {error}");
                    }
                });
            }
        });
        Self {
            address,
            stopped,
            thread: Some(thread),
        }
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        self.thread.take().unwrap().join().unwrap();
    }
}

fn serve_connection(
    mut stream: TcpStream,
    files: &BTreeMap<String, (Vec<u8>, &'static str)>,
) -> std::io::Result<()> {
    // Accepted sockets can inherit the listener's nonblocking mode on macOS.
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let mut request = Vec::new();
    let mut buffer = [0u8; 4096];
    while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") && request.len() < 16384 {
        let size = stream.read(&mut buffer)?;
        if size == 0 {
            return Ok(());
        }
        request.extend_from_slice(&buffer[..size]);
    }
    let request = String::from_utf8_lossy(&request);
    let path = request
        .split_whitespace()
        .nth(1)
        .unwrap_or("/")
        .split('?')
        .next()
        .unwrap();
    let (status, bytes, mime) = match files.get(path) {
        Some((bytes, mime)) => ("200 OK", bytes.as_slice(), *mime),
        None => ("404 Not Found", b"missing".as_slice(), "text/plain"),
    };
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nService-Worker-Allowed: /\r\nConnection: close\r\n\r\n",
        bytes.len()
    )?;
    if !request.starts_with("HEAD ") {
        stream.write_all(bytes)?;
    }
    Ok(())
}

#[test]
fn fixture_completes_large_responses_on_nonblocking_accepted_streams() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let (stream, _) = listener.accept().unwrap();
    // Model accepted-socket flag inheritance on every OS, then apply backpressure.
    stream.set_nonblocking(true).unwrap();
    let body = vec![0x5a; 32 * 1024 * 1024];
    let expected_length = body.len();
    let server = thread::spawn(move || {
        serve_connection(
            stream,
            &BTreeMap::from([("/large.wasm".into(), (body, "application/wasm"))]),
        )
    });
    // A newly accepted stream may need to wait for the request as well as for writes.
    thread::sleep(Duration::from_millis(100));
    let write = client.write_all(b"GET /large.wasm HTTP/1.1\r\nHost: localhost\r\n\r\n");
    thread::sleep(Duration::from_millis(100));
    let mut response = Vec::new();
    let read = client.read_to_end(&mut response);
    server.join().unwrap().unwrap();
    write.unwrap();
    read.unwrap();
    let body_start = response
        .windows(4)
        .position(|bytes| bytes == b"\r\n\r\n")
        .unwrap()
        + 4;
    assert_eq!(response.len() - body_start, expected_length);
    assert!(response[body_start..].iter().all(|&byte| byte == 0x5a));
}

#[test]
#[ignore = "requires the built WASM artifact and a system Chromium browser"]
fn wasm_compiles_virtual_project_in_real_browser() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let wasm = std::env::var_os("WAKE_BROWSER_WASM_PATH")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| root.join("target/wasm32-unknown-unknown/debug/wake_browser.wasm"));
    let mut files = BTreeMap::new();
    files.insert(
        "/".into(),
        (
            b"<!doctype html><title>Wake WASM conformance</title>".to_vec(),
            "text/html",
        ),
    );
    files.insert(
        "/wake.wasm".into(),
        (
            std::fs::read(wasm).expect("build wake_browser for wasm32-unknown-unknown first"),
            "application/wasm",
        ),
    );
    files.insert(
        "/browser-wasm.mjs".into(),
        (
            std::fs::read(browser_package(root).join("browser-wasm.mjs")).unwrap(),
            "text/javascript",
        ),
    );
    let project = BTreeMap::from([
        ("src/page.ts".to_owned(), b"import styles from './page.module.css';import data from './data.json';import image from './image.svg';export const value={styles,data,image};export const lazy=()=>import('./lazy.ts');".to_vec()),
        ("src/page.module.css".to_owned(), b".root { color: red; background: url('./image.svg'); }".to_vec()),
        ("src/data.json".to_owned(), b"{\"value\":42}".to_vec()),
        ("src/image.svg".to_owned(), b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>".to_vec()),
        ("src/lazy.ts".to_owned(), b"export const value: number=7;".to_vec()),
    ]);
    let options = json!({"federation":{"enabled":true,"name":"preview","exposes":{"./page":{"entry":"src/page.ts"}}}});
    let expected =
        wake_browser::BrowserBuildContext::new(serde_json::from_value(options.clone()).unwrap())
            .unwrap()
            .build(project.clone())
            .unwrap();
    files.insert(
        "/expected.json".into(),
        (
            serde_json::to_vec(&json!({"project": project,"options":options,"output":expected}))
                .unwrap(),
            "application/json",
        ),
    );
    let server = Server::new(files);
    let driver = launch_browser();
    let context = driver.create_context().unwrap();
    let page = context.new_page("about:blank").unwrap();
    page.navigate(&format!("http://{}/", server.address))
        .unwrap();
    let result = page.evaluate_with_timeout(r#"(async()=>{
      try {
        const {createWasmBuildContext}=await import('/browser-wasm.mjs');
        const expected=await (await fetch('/expected.json')).json();
        const context=await createWasmBuildContext('/wake.wasm',expected.options);
        const actual=context.build(expected.project);
        if(actual.manifest.buildId!==expected.output.manifest.buildId)throw Error('native/WASM build identity mismatch');
        for(const file of expected.output.files){const match=actual.files.find(candidate=>candidate.path===file.path);if(!match||JSON.stringify(Array.from(match.bytes))!==JSON.stringify(file.bytes))throw Error('native/WASM byte mismatch: '+file.path)}
        if(actual.files.length!==expected.output.files.length)throw Error('native/WASM artifact set mismatch');
        const bytes=value=>Array.from(new TextEncoder().encode(value));
        const first=context.build({'src/page.ts':bytes('export const value: number = 42;')});
        let failed=false;try{context.build({'src/page.ts':bytes('export const = ;')})}catch(error){failed=error.code==='WAKE_BUILD'}
        const second=context.build({'src/page.ts':bytes('export const value: number = 43;')});
        context.dispose();
        return {ok:true,failed,changed:first.manifest.buildId!==second.manifest.buildId,assets:first.files.length};
      }catch(error){return {ok:false,error:String(error),stack:error.stack}}
    })()"#, Some(120_000)).unwrap();
    assert_eq!(result.get("ok"), Some(&json!(true)), "{result}");
    assert_eq!(result.get("failed"), Some(&json!(true)), "{result}");
    assert_eq!(result.get("changed"), Some(&json!(true)), "{result}");
}

#[test]
#[ignore = "requires a system Chromium browser"]
fn preview_runtime_refresh_release_and_failure_preserve_the_host() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let mut files = BTreeMap::new();
    files.insert(
        "/".into(),
        (
            b"<!doctype html><title>Wake preview runtime</title>".to_vec(),
            "text/html",
        ),
    );
    files.insert(
        "/federation.mjs".into(),
        (
            std::fs::read(browser_package(root).join("federation.mjs")).unwrap(),
            "text/javascript",
        ),
    );
    let server = Server::new(files);
    let driver = launch_browser();
    let context = driver.create_context().unwrap();
    let page = context.new_page("about:blank").unwrap();
    page.navigate(&format!("http://{}/", server.address))
        .unwrap();
    let options = serde_json::from_value(json!({"federation":{"enabled":true,"name":"preview","exposes":{"./page":{"entry":"page.ts"}}}})).unwrap();
    let mut builder = wake_browser::BrowserBuildContext::new(options).unwrap();
    let mut manifests = Vec::new();
    for index in 0..12 {
        let files = BTreeMap::from([(
            "page.ts".to_owned(),
            format!("export const value = {index};").into_bytes(),
        )]);
        manifests.push(builder.build(files).unwrap().manifest);
    }
    let expression = format!(
        r#"(async()=>{{try{{
      const {{createFederationRuntime}}=await import('/federation.mjs');
      const manifests={};let selected=manifests[0],reloads=0;
      const runtime=createFederationRuntime({{mode:'preview',global:{{location:{{href:location.href,reload(){{reloads++}}}}}},transport:{{
        fetchManifest(){{return selected}},
        loadScript(asset,context){{const id=context.manifest.buildId;return {{init(){{}},get(){{return ()=>({{id}})}}}}}},
      }}}});
      runtime.registerRemote({{name:'preview',manifestUrl:new URL('/wake-federation.json',location.href).href}});
      let old=await runtime.loadRemote('preview/page');
      for(const manifest of manifests.slice(1)){{selected=manifest;await runtime.refreshPreviewRemote('preview');const fresh=await runtime.loadRemote('preview/page');if(fresh.id===old.id)throw Error('stale module');runtime.releasePreviewBuild('preview',old.id);old=fresh}}
      selected={{...selected,schemaVersion:'invalid'}};let rejected=false;
      try{{await runtime.refreshPreviewRemote('preview')}}catch{{rejected=true}}
      const restored=await runtime.loadRemote('preview/page');
      const retained=runtime.remotes.get('preview').acceptedManifests.size;
      runtime.releasePreviewBuild('preview',old.id);
      const released=runtime.remotes.get('preview').acceptedManifests.size;
      return {{ok:true,rejected,restored:restored.id===old.id,retained,released,reloads}};
    }}catch(error){{return {{ok:false,error:String(error),stack:error.stack}}}}}})()"#,
        serde_json::to_string(&manifests).unwrap()
    );
    let result = page
        .evaluate_with_timeout(&expression, Some(30_000))
        .unwrap();
    assert_eq!(
        result,
        json!({"ok":true,"rejected":true,"restored":true,"retained":1,"released":0,"reloads":0}),
        "{result}"
    );
}

#[test]
#[ignore = "requires built WASM and a system browser with Service Workers"]
fn browser_worker_preview_end_to_end() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let mut files = BTreeMap::new();
    files.insert(
        "/".into(),
        (
            b"<!doctype html><title>Wake editor preview</title>".to_vec(),
            "text/html",
        ),
    );
    for name in [
        "browser.mjs",
        "browser-worker.mjs",
        "browser-wasm.mjs",
        "browser-preview.mjs",
        "browser-service-worker.mjs",
        "browser-service-worker-handler.mjs",
        "federation.mjs",
    ] {
        files.insert(
            format!("/{name}"),
            (
                std::fs::read(browser_package(root).join(name)).unwrap(),
                "text/javascript",
            ),
        );
    }
    files.insert(
        "/preview-fixture.mjs".into(),
        (
            std::fs::read(root.join("crates/wake_browser/tests/preview-fixture.mjs")).unwrap(),
            "text/javascript",
        ),
    );
    let wasm = std::env::var_os("WAKE_BROWSER_WASM_PATH")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| root.join("target/wasm32-unknown-unknown/debug/wake_browser.wasm"));
    files.insert(
        "/wake.wasm".into(),
        (std::fs::read(wasm).unwrap(), "application/wasm"),
    );
    let server = Server::new(files);
    let driver = launch_browser();
    let context = driver.create_context().unwrap();
    let page = context.new_page("about:blank").unwrap();
    page.navigate(&format!("http://{}/", server.address))
        .unwrap();
    let result = page.evaluate_with_timeout(r#"(async()=>{try{return await (await import('/preview-fixture.mjs')).run()}catch(error){return {ok:false,error:String(error),stack:error.stack,diagnostics:error.diagnostics}}})()"#, Some(120_000)).unwrap();
    assert_eq!(result.get("ok"), Some(&json!(true)), "{result}");
}

fn cached_package(root: &Path, name: &str, version: &str, files: &mut BTreeMap<String, Vec<u8>>) {
    let prefix = format!("{name}-npm-{version}-");
    let paths = std::fs::read_dir(root.join(".yarn/cache"))
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(&prefix)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        paths.len(),
        1,
        "expected the locked {name}@{version} archive"
    );
    let archive = wake_common::zip::ZipArchive::parse(std::fs::read(&paths[0]).unwrap()).unwrap();
    fn collect(
        archive: &wake_common::zip::ZipArchive,
        directory: &str,
        files: &mut BTreeMap<String, Vec<u8>>,
    ) {
        for child in archive.read_dir(directory) {
            if child.ends_with('/') {
                collect(archive, &child, files);
            } else {
                files.insert(child.clone(), archive.read(&child).unwrap().unwrap());
            }
        }
    }
    collect(&archive, &format!("node_modules/{name}"), files);
}

#[test]
#[ignore = "requires built WASM, locked React archives and a system browser"]
fn browser_react_host_sharing_css_and_tsx() {
    check_react_host(true);
}

#[test]
#[ignore = "requires built WASM, locked React archives and a system browser"]
fn browser_react_production_host_sharing_css_and_tsx() {
    check_react_host(false);
}

fn check_react_host(development: bool) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let mut dependencies = BTreeMap::new();
    cached_package(root, "react", "19.2.8", &mut dependencies);
    cached_package(root, "react-dom", "19.2.8", &mut dependencies);
    cached_package(root, "scheduler", "0.27.0", &mut dependencies);
    for name in ["package.json", "index.mjs", "index.cjs"] {
        dependencies.insert(
            format!("node_modules/@crab-dev/css/{name}"),
            std::fs::read(root.join("npm/css").join(name)).unwrap(),
        );
    }
    let host_fs = wake_common::MemoryFileSystem::from_files(
        dependencies
            .iter()
            .map(|(path, bytes)| (Path::new("/__host__").join(path), bytes.clone())),
    );
    host_fs.insert("/__host__/host.ts", b"import * as React from 'react';import * as ReactDOM from 'react-dom';import * as Client from 'react-dom/client';import * as JSX from 'react/jsx-runtime';import * as JSXDev from 'react/jsx-dev-runtime';globalThis.__wakeBrowserTestShared={react:React,'react-dom':ReactDOM,'react-dom/client':Client,'react/jsx-runtime':JSX,'react/jsx-dev-runtime':JSXDev};".to_vec());
    let mut options = wake_bundler::BuildOptions::default();
    options
        .define
        .retain(|(key, _)| key != "process.env.NODE_ENV");
    options.define.push((
        "process.env.NODE_ENV".into(),
        if development {
            "\"development\""
        } else {
            "\"production\""
        }
        .into(),
    ));
    let host_fs = Arc::new(host_fs);
    let resolver = wake_resolver::Resolver::with_options(
        host_fs.clone(),
        wake_resolver::ResolveOptions {
            conditions: ["browser", "development", "import", "module", "default"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            ..wake_resolver::ResolveOptions::default()
        },
    );
    let host = wake_bundler::BuildSession::new_one_shot(host_fs, options)
        .build_once(wake_bundler::BuildRequest::new("/__host__/host.ts"));
    assert!(!host.has_errors(), "{:?}", host.diagnostics);
    let host_variant = format!(
        "fixture-host-{}",
        wake_federation_build::short_hash(host.bundle.as_bytes())
    );
    let host_shared: BTreeMap<_, _> = ["react", "react-dom", "react-dom/client", "react/jsx-runtime", "react/jsx-dev-runtime"].into_iter().map(|key| {
        let resolved = resolver.resolve_module(key, Path::new("/__host__")).unwrap();
        let wake_resolver::ModuleIdentity::Package { package, .. } = resolved.identity else { panic!("expected package") };
        (key, json!({"name": package.name, "version":package.version, "packageContext":package.context.unwrap_or_else(|| "default".into()), "buildVariant":host_variant}))
    }).collect();
    let mut files = BTreeMap::new();
    files.insert(
        "/application-service-worker.mjs".into(),
        (
            std::fs::read(root.join("crates/wake_browser/tests/application-service-worker.mjs"))
                .unwrap(),
            "text/javascript",
        ),
    );
    files.insert(
        "/".into(),
        (
            b"<!doctype html><title>Wake browser React</title><script src='/host.js'></script>"
                .to_vec(),
            "text/html",
        ),
    );
    files.insert(
        "/host.js".into(),
        (host.bundle.into_bytes(), "text/javascript"),
    );
    files.insert(
        "/host-shared.json".into(),
        (
            serde_json::to_vec(&host_shared).unwrap(),
            "application/json",
        ),
    );
    files.insert(
        "/dependencies.json".into(),
        (
            serde_json::to_vec(&dependencies).unwrap(),
            "application/json",
        ),
    );
    for name in [
        "browser.mjs",
        "browser-worker.mjs",
        "browser-wasm.mjs",
        "browser-preview.mjs",
        "browser-service-worker.mjs",
        "browser-service-worker-handler.mjs",
        "federation.mjs",
    ] {
        files.insert(
            format!("/{name}"),
            (
                std::fs::read(browser_package(root).join(name)).unwrap(),
                "text/javascript",
            ),
        );
    }
    files.insert(
        "/react-fixture.mjs".into(),
        (
            std::fs::read(root.join("crates/wake_browser/tests/react-fixture.mjs")).unwrap(),
            "text/javascript",
        ),
    );
    let wasm = std::env::var_os("WAKE_BROWSER_WASM_PATH")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| root.join("target/wasm32-unknown-unknown/debug/wake_browser.wasm"));
    files.insert(
        "/wake.wasm".into(),
        (std::fs::read(wasm).unwrap(), "application/wasm"),
    );
    let server = Server::new(files);
    let driver = launch_browser();
    let context = driver.create_context().unwrap();
    let page = context.new_page("about:blank").unwrap();
    page.navigate(&format!("http://{}/", server.address))
        .unwrap();
    let result = page.evaluate_with_timeout(r#"(async()=>{try{return await (await import('/react-fixture.mjs')).run()}catch(error){return {ok:false,error:String(error),stack:error.stack,diagnostics:error.diagnostics}}})()"#, Some(120_000)).unwrap();
    assert_eq!(result.get("ok"), Some(&json!(true)), "{result}");
}
