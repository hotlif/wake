//! Real-browser acceptance for a production Docs site hosted under a non-root path.
//!
//! Run explicitly with a system Chromium browser and the immutable Yarn cache:
//! `cargo test -p wake_test --test static_docs_browser_conformance -- --ignored`

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
use wake_app::{CancellationToken, DocsBuildOptions, DocsPresentation, ProjectOptions, build_docs};
use wake_test_browser::{BrowserDriver, BrowserLaunchOptions, BrowserPage};

const WAIT: Duration = Duration::from_secs(30);
const BASE: &str = "/handbook/";

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

fn copy_fixture(source: &Path, target: &Path) {
    fs::create_dir_all(target).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name();
        if matches!(name.to_str(), Some(".wake" | "docs-dist" | "dist")) {
            continue;
        }
        let source = entry.path();
        let target = target.join(name);
        if source.is_dir() {
            copy_fixture(&source, &target);
        } else {
            fs::copy(source, target).unwrap();
        }
    }
}

struct StaticServer {
    origin: String,
    stop: Arc<AtomicBool>,
    join: Option<thread::JoinHandle<()>>,
}

impl StaticServer {
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
                    Err(error) => panic!("Docs static host failed: {error}"),
                };
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                let mut request = Vec::new();
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
                let raw_path = request_line.next().unwrap_or("");
                let pathname = raw_path.split(['?', '#']).next().unwrap_or("");
                let relative = pathname.strip_prefix(BASE);
                let safe = relative.is_some_and(|path| {
                    !path.split('/').any(|segment| matches!(segment, "." | ".."))
                        && !path.contains('\\')
                });
                let candidate = relative
                    .filter(|_| safe)
                    .map(|path| root.join(path.replace('/', std::path::MAIN_SEPARATOR_STR)));
                let file = candidate.as_ref().map(|path| {
                    if path.is_dir() {
                        path.join("index.html")
                    } else {
                        path.clone()
                    }
                });
                let found = file.as_ref().and_then(|path| fs::read(path).ok());
                let (status, bytes, mime) = if let Some(bytes) = found {
                    let mime = match file
                        .as_ref()
                        .unwrap()
                        .extension()
                        .and_then(|value| value.to_str())
                    {
                        Some("html") => "text/html; charset=utf-8",
                        Some("js" | "mjs") => "text/javascript",
                        Some("css") => "text/css",
                        Some("json") => "application/json",
                        Some("svg") => "image/svg+xml",
                        Some("woff2") => "font/woff2",
                        _ => "application/octet-stream",
                    };
                    ("200 OK", bytes, mime)
                } else if safe
                    && candidate
                        .as_ref()
                        .is_some_and(|path| path.extension().is_none())
                {
                    (
                        "404 Not Found",
                        fs::read(root.join("404.html")).unwrap(),
                        "text/html; charset=utf-8",
                    )
                } else {
                    ("404 Not Found", b"not found".to_vec(), "text/plain")
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    bytes.len()
                );
                let _ = stream.write_all(response.as_bytes());
                if method != "HEAD" {
                    let _ = stream.write_all(&bytes);
                }
            }
        });
        Self {
            origin,
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
        r#"(() => ({
          href: location.href,
          title: document.title,
          heading: document.querySelector('main h1')?.textContent?.trim() ?? null,
          htmlLang: document.documentElement.lang,
          links: [...document.querySelectorAll('a[href]')].map(link => link.href),
          assets: [...document.querySelectorAll('script[src],link[rel="stylesheet"]')]
            .map(asset => asset.src || asset.href),
          demoFrames: [...document.querySelectorAll('iframe[src]')]
            .map(frame => ({url: frame.src, text: frame.contentDocument?.body?.innerText ?? ''})),
          body: document.body?.innerText.slice(0, 800) ?? ''
        }))()"#,
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
    panic!("production Docs page did not reach the expected state: {last}");
}

fn save_failure(page: &BrowserPage, fixture: &Path, case: &str) {
    let root = target_dir()
        .join("browser-artifacts/static-docs-conformance")
        .join(case);
    if let Err(error) = fs::create_dir_all(&root) {
        eprintln!("could not create static Docs browser evidence: {error}");
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
    let page_state = format!(
        "route: {:#?}\ninteractive Demo: {:#?}",
        state(page),
        interactive_demo_state(page)
    );
    let _ = fs::write(root.join("page-state.txt"), page_state);
    match page.screenshot_png() {
        Ok(png) => {
            let _ = fs::write(root.join("failure.png"), png);
        }
        Err(error) => {
            let _ = fs::write(root.join("screenshot-error.txt"), error.to_string());
        }
    }
    eprintln!("static Docs browser evidence: {}", root.display());
    eprintln!("static Docs fixture retained: {}", fixture.display());
}

#[test]
#[ignore = "requires an installed system Chromium browser and the immutable Yarn cache"]
fn chromium_serves_production_docs_routes_under_a_non_root_base() {
    let fixture = tempfile::Builder::new()
        .prefix("wake-static-docs-browser-")
        .tempdir_in(target_dir())
        .unwrap();
    copy_fixture(
        &workspace_root().join("fixtures/react-docs"),
        fixture.path(),
    );
    let outdir = fixture.path().join("dist");
    let built = build_docs(
        DocsBuildOptions {
            project: ProjectOptions {
                cwd: Some(fixture.path().to_path_buf()),
                config_path: None,
            },
            outdir: Some(outdir.clone()),
            base_path: Some(BASE.to_string()),
            presentation: Some(DocsPresentation::Standalone),
        },
        &CancellationToken::default(),
    );
    if let Err(error) = &built {
        eprintln!(
            "static Docs fixture retained after build failure: {}",
            fixture.path().display()
        );
        let _ = fixture.keep();
        panic!("production Docs build failed: {error}");
    }
    let built = built.unwrap();
    assert!(
        built
            .routes
            .iter()
            .any(|route| route.slug == "/components/button")
    );
    for file in [
        "index.html",
        "components/button/index.html",
        "404.html",
        "logo.svg",
    ] {
        assert!(
            outdir.join(file).is_file(),
            "production output is missing {file}"
        );
    }
    let server = StaticServer::start(outdir);
    let driver = browser();
    let context = driver.create_context().unwrap();
    let page = context.new_page("about:blank").unwrap();
    page.command("Network.enable", json!({})).unwrap();
    let home = format!("{}{BASE}", server.origin);
    let deep = format!("{}{BASE}components/button/", server.origin);

    let result = catch_unwind(AssertUnwindSafe(|| {
        page.navigate(&home).unwrap();
        let initial = wait_for_state(&page, |value| {
            value["href"] == home && value["heading"] == "简介"
        });
        assert_eq!(initial["htmlLang"], "zh-CN");
        assert!(initial["title"].as_str().unwrap().contains("简介"));
        assert!(initial["body"].as_str().unwrap().contains("Wake 组件库"));
        assert!(initial["links"].as_array().unwrap().iter().any(|url| {
            url.as_str().is_some_and(|url| {
                url.starts_with(&server.origin) && url.contains("/handbook/components/button")
            })
        }));
        assert!(
            initial["assets"].as_array().is_some_and(|assets| {
                !assets.is_empty()
                    && assets
                        .iter()
                        .all(|asset| asset.as_str().is_some_and(|url| url.starts_with(&home)))
            }),
            "production scripts and styles must stay under the mount: {initial}"
        );

        let clicked = page.evaluate(
            "(() => { const link = [...document.querySelectorAll('a[href]')].find(link => new URL(link.href).pathname.replace(/\\/$/, '') === '/handbook/components/button'); link?.click(); return Boolean(link); })()",
        ).unwrap();
        assert_eq!(clicked, true, "home navigation lacks the button page link");
        wait_for_state(&page, |value| {
            value["href"]
                .as_str()
                .is_some_and(|url| url.contains("/handbook/components/button"))
                && value["heading"] == "按钮"
        });

        page.navigate(&deep).unwrap();
        let direct = wait_for_state(&page, |value| {
            value["href"] == deep && value["heading"] == "按钮"
        });
        assert!(direct["title"].as_str().unwrap().contains("按钮"));
        assert!(
            direct["demoFrames"].as_array().is_some_and(|frames| {
                frames.iter().any(|frame| {
                    frame["url"]
                        .as_str()
                        .is_some_and(|url| url.starts_with(&home))
                })
            }),
            "production Demo iframe must use the Docs base path: {direct}"
        );
        page.command("Page.reload", json!({"ignoreCache": true}))
            .unwrap();
        wait_for_state(&page, |value| {
            value["href"] == deep && value["heading"] == "按钮"
        });

        let responses = page.evaluate(
            "Promise.all(['/handbook/logo.svg', '/handbook/missing.js', '/logo.svg'].map(async path => { const response = await fetch(path); return {status: response.status, type: response.headers.get('content-type')}; }))",
        ).unwrap();
        assert_eq!(responses[0]["status"], 200);
        assert_eq!(responses[0]["type"], "image/svg+xml");
        assert_eq!(responses[1]["status"], 404);
        assert_eq!(responses[2]["status"], 404);

        let missing = format!("{}{BASE}missing-page/", server.origin);
        page.navigate(&missing).unwrap();
        let not_found = wait_for_state(&page, |value| {
            value["href"] == missing && value["heading"] == "页面不存在"
        });
        assert!(not_found["title"].as_str().unwrap().starts_with("404"));
        let status = page
            .evaluate("fetch(location.href).then(response => response.status)")
            .unwrap();
        assert_eq!(
            status, 404,
            "unknown Docs route must use the generated 404 shell"
        );
    }));

    if let Err(panic) = result {
        save_failure(&page, fixture.path(), "routes");
        let _ = fixture.keep();
        resume_unwind(panic);
    }
}

fn interactive_demo_state(page: &BrowserPage) -> Result<Value, String> {
    page.evaluate(
        r#"(() => {
          const card = [...document.querySelectorAll('.demo-card')]
            .find(candidate => candidate.querySelector('.demo-titlebar strong')?.textContent === '交互验收');
          const frame = card?.querySelector('iframe[data-wake-demo]');
          const frameDocument = frame?.contentDocument;
          return {
            href: location.href,
            heading: document.querySelector('main h1')?.textContent?.trim() ?? null,
            cardFound: Boolean(card),
            frameUrl: frame?.src ?? null,
            counter: frameDocument?.getElementById('height-counter')?.textContent ?? null,
            frameHeight: frame ? Number.parseFloat(frame.style.height) : null,
            frameScrollHeight: frameDocument?.documentElement.scrollHeight ?? null,
            frameViewportHeight: frame?.contentWindow?.innerHeight ?? null,
            frameRootHeight: frameDocument?.querySelector('.demo-frame-root')?.getBoundingClientRect().height ?? null,
            previewHeight: frameDocument?.querySelector('.demo-frame-root > *')?.getBoundingClientRect().height ?? null,
            contentHeight: frameDocument?.getElementById('height-probe')?.getBoundingClientRect().height ?? null,
            contentBottom: frameDocument?.getElementById('height-probe')?.getBoundingClientRect().bottom ?? null,
            parentTheme: document.documentElement.dataset.theme ?? null,
            frameTheme: frameDocument?.documentElement.dataset.theme ?? null,
            previewTheme: frameDocument?.querySelector('.demo-frame-root')?.getAttribute('data-theme') ?? null,
            themeButton: document.querySelector('button[aria-label^="主题:"]')?.getAttribute('aria-label') ?? null,
            viewport: card?.querySelector('.demo-viewport')?.getAttribute('data-viewport') ?? null,
            sourceOpen: Boolean(card?.querySelector('.demo-code.demo-panel')),
            error: card?.querySelector('[role="alert"]')?.textContent ?? null
          };
        })()"#,
    )
    .map_err(|error| error.to_string())
}

fn wait_for_interactive_demo(page: &BrowserPage, ready: impl Fn(&Value) -> bool) -> Value {
    let deadline = Instant::now() + WAIT;
    let mut last = String::new();
    while Instant::now() < deadline {
        match interactive_demo_state(page) {
            Ok(value) if ready(&value) => return value,
            Ok(value) => last = value.to_string(),
            Err(error) => last = error,
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!("production Docs interactive Demo did not reach the expected state: {last}");
}

fn click_theme_until(page: &BrowserPage, target: &str) -> Value {
    for _ in 0..3 {
        let current = interactive_demo_state(page).unwrap();
        if current["themeButton"]
            .as_str()
            .is_some_and(|label| label.ends_with(target))
        {
            return current;
        }
        let old = current["themeButton"].clone();
        page.evaluate("document.querySelector('button[aria-label^=\"主题:\"]').click()")
            .unwrap();
        wait_for_interactive_demo(page, |value| value["themeButton"] != old);
    }
    panic!("could not select the Docs theme {target}");
}

#[test]
#[ignore = "requires an installed system Chromium browser and the immutable Yarn cache"]
fn chromium_docs_demo_interaction_resizes_and_tracks_the_parent_theme() {
    let fixture = tempfile::Builder::new()
        .prefix("wake-static-docs-browser-")
        .tempdir_in(target_dir())
        .unwrap();
    copy_fixture(
        &workspace_root().join("fixtures/react-docs"),
        fixture.path(),
    );
    fs::write(
        fixture
            .path()
            .join("docs/components/demos/interaction.demo.tsx"),
        r#"import React, {useState} from 'react';
export const meta = {title: '交互验收', order: 1, viewport: 'responsive', height: 'auto'};
export default function InteractiveDemo() {
  const [count, setCount] = useState(0);
  return <div>
    <button id="height-counter" onClick={() => setCount(value => value + 1)}>count:{count}</button>
    <div id="height-probe" style={{height: count ? 650 : 80}} />
  </div>;
}"#,
    )
    .unwrap();
    let page_source = fixture.path().join("docs/components/button.mdx");
    let source = fs::read_to_string(&page_source).unwrap();
    let original_demo = "<Demo src=\"./demos/basic.demo.tsx\" />";
    assert!(source.contains(original_demo));
    fs::write(
        page_source,
        source.replacen(
            original_demo,
            "<Demo src=\"./demos/interaction.demo.tsx\" />\n\n<Demo src=\"./demos/basic.demo.tsx\" />",
            1,
        ),
    )
    .unwrap();
    let outdir = fixture.path().join("dist");
    let built = build_docs(
        DocsBuildOptions {
            project: ProjectOptions {
                cwd: Some(fixture.path().to_path_buf()),
                config_path: None,
            },
            outdir: Some(outdir.clone()),
            base_path: Some(BASE.to_string()),
            presentation: Some(DocsPresentation::Standalone),
        },
        &CancellationToken::default(),
    );
    if let Err(error) = &built {
        eprintln!(
            "interactive Docs fixture retained after build failure: {}",
            fixture.path().display()
        );
        let _ = fixture.keep();
        panic!("interactive production Docs build failed: {error}");
    }
    let server = StaticServer::start(outdir);
    let driver = browser();
    let context = driver.create_context().unwrap();
    let page = context.new_page("about:blank").unwrap();
    page.command("Network.enable", json!({})).unwrap();
    let deep = format!("{}{BASE}components/button/", server.origin);

    let result = catch_unwind(AssertUnwindSafe(|| {
        page.navigate(&deep).unwrap();
        wait_for_state(&page, |value| {
            value["href"] == deep && value["heading"] == "按钮"
        });
        page.evaluate("document.querySelector('.demo-card').scrollIntoView({block:'center'})")
            .unwrap();
        let initial = wait_for_interactive_demo(&page, |value| {
            value["cardFound"] == true
                && value["counter"] == "count:0"
                && value["frameTheme"] == value["parentTheme"]
                && value["previewTheme"] == value["parentTheme"]
        });
        assert!(
            initial["frameUrl"].as_str().is_some_and(
                |url| url.starts_with(&format!("{}{BASE}?__wake_demo=", server.origin))
            ),
            "Demo iframe must stay under the configured base: {initial}"
        );
        assert!(
            initial["error"].is_null(),
            "Demo reported an error: {initial}"
        );
        let initial_height = initial["frameHeight"].as_f64().unwrap();

        page.evaluate(
            "[...document.querySelectorAll('.demo-card')].find(card => card.querySelector('.demo-titlebar strong')?.textContent === '交互验收').querySelector('iframe').contentDocument.getElementById('height-counter').click()",
        ).unwrap();
        let expanded = wait_for_interactive_demo(&page, |value| {
            value["counter"] == "count:1"
                && value["frameHeight"]
                    .as_f64()
                    .is_some_and(|height| height > initial_height + 300.0)
        });
        assert!(expanded["frameScrollHeight"].as_f64().unwrap() > initial_height + 300.0);
        assert_eq!(
            expanded["href"], deep,
            "Demo click must keep the document route"
        );
        thread::sleep(Duration::from_secs(2));
        let settled = interactive_demo_state(&page).unwrap();
        assert!(
            settled["frameHeight"]
                .as_f64()
                .is_some_and(|height| height < 1600.0),
            "iframe auto resize must settle near its content instead of growing with its own viewport: {settled}"
        );
        let settled_height = settled["frameHeight"].as_f64().unwrap();
        for count in 2..=4 {
            page.evaluate(
                "[...document.querySelectorAll('.demo-card')].find(card => card.querySelector('.demo-titlebar strong')?.textContent === '交互验收').querySelector('iframe').contentDocument.getElementById('height-counter').click()",
            ).unwrap();
            wait_for_interactive_demo(&page, |value| value["counter"] == format!("count:{count}"));
            thread::sleep(Duration::from_millis(150));
            let repeated = interactive_demo_state(&page).unwrap();
            assert!(
                repeated["frameHeight"]
                    .as_f64()
                    .is_some_and(|height| (height - settled_height).abs() < 32.0),
                "repeated clicks with unchanged content height must not drift the iframe height: {repeated}"
            );
        }

        page.evaluate(
            "[...document.querySelectorAll('.demo-card')].find(card => card.querySelector('.demo-titlebar strong')?.textContent === '交互验收').querySelector('.demo-device-tools button[aria-label^=\"手机\"]').click()",
        ).unwrap();
        wait_for_interactive_demo(&page, |value| {
            value["viewport"] == "mobile" && value["counter"] == "count:4" && value["href"] == deep
        });
        thread::sleep(Duration::from_millis(150));
        let mobile = interactive_demo_state(&page).unwrap();
        assert!(
            mobile["frameHeight"]
                .as_f64()
                .is_some_and(|height| (height - settled_height).abs() < 32.0),
            "mobile viewport must preserve the measured height instead of accumulating iframe chrome (baseline {settled_height}): {mobile}"
        );
        assert!(
            mobile["contentBottom"]
                .as_f64()
                .zip(mobile["frameViewportHeight"].as_f64())
                .is_some_and(|(bottom, viewport)| bottom <= viewport),
            "mobile viewport must keep the expanded Demo content visible: {mobile}"
        );
        page.evaluate(
            "[...document.querySelectorAll('.demo-card')].find(card => card.querySelector('.demo-titlebar strong')?.textContent === '交互验收').querySelector('button[aria-label=\"展开源码\"]').click()",
        ).unwrap();
        wait_for_interactive_demo(&page, |value| {
            value["sourceOpen"] == true && value["counter"] == "count:4" && value["href"] == deep
        });

        click_theme_until(&page, "深色");
        let dark = wait_for_interactive_demo(&page, |value| {
            value["parentTheme"] == "dark"
                && value["frameTheme"] == "dark"
                && value["previewTheme"] == "dark"
        });
        assert_eq!(
            dark["counter"], "count:4",
            "theme changes must keep Demo state"
        );
        click_theme_until(&page, "浅色");
        let light = wait_for_interactive_demo(&page, |value| {
            value["parentTheme"] == "light"
                && value["frameTheme"] == "light"
                && value["previewTheme"] == "light"
        });
        assert_eq!(light["counter"], "count:4");
        assert!(light["error"].is_null(), "Demo reported an error: {light}");
        assert!(
            light["frameHeight"]
                .as_f64()
                .is_some_and(|height| (height - settled_height).abs() < 32.0),
            "first theme round-trip must preserve iframe height (baseline {settled_height}): {light}"
        );
        for _ in 0..2 {
            click_theme_until(&page, "深色");
            click_theme_until(&page, "浅色");
            let repeated = interactive_demo_state(&page).unwrap();
            assert!(
                repeated["frameHeight"]
                    .as_f64()
                    .is_some_and(|height| (height - settled_height).abs() < 32.0),
                "repeated theme changes must not drift the iframe height: {repeated}"
            );
        }
    }));

    if let Err(panic) = result {
        save_failure(&page, fixture.path(), "demo-interaction");
        let _ = fixture.keep();
        resume_unwind(panic);
    }
}
