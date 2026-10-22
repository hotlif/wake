//! Visual suites for product-prepared, immutable Docs output.
use super::*;
use wake_test_contract::{DocsTestDemo, DocsTestPlan};

pub(super) fn run(
    options: &TestOptions,
    cancellation: &TestCancellationToken,
) -> Result<TestRunResult, TestError> {
    let started = Instant::now();
    let plan = options.docs_plan.as_ref().expect("Docs plan dispatched");
    validate_plan(plan)?;
    let root = options.root.as_deref().unwrap_or_else(|| Path::new("."));
    let config = wake_config::load(root)
        .map_err(|error| TestError::new("WAKE_TEST_CONFIG", error.to_string()))?;
    let update = match options.update_snapshots.as_deref() {
        Some("all") => SnapshotUpdate::All,
        None | Some("none") => SnapshotUpdate::None,
        _ => {
            return Err(TestError::new(
                "WAKE_TEST_CONFIG",
                "Docs snapshots require none or all",
            ));
        }
    };
    let resources = Arc::new(read_resources(plan)?);
    // This listener serves no product bytes. Only the authenticated CDP session can fulfill them.
    let origin = ResourceOrigin::start(BTreeMap::new()).map_err(browser_error)?;
    let mut result = TestRunResult::empty(
        format!("wake-docs-test-{}-{}", std::process::id(), random_seed()),
        "docs-visual".into(),
        environment_info(TestEnvironmentKind::Browser, None, None),
    );
    for theme in &plan.themes {
        for viewport in &plan.viewports {
            if cancellation.is_cancelled() {
                break;
            }
            let mut test_config = config.test.clone();
            test_config.browser.viewport.width = viewport.width;
            test_config.browser.viewport.height = viewport.height;
            test_config.browser.color_scheme = if theme == "dark" {
                wake_config::TestColorScheme::Dark
            } else {
                wake_config::TestColorScheme::Light
            };
            let browser = launch_browser(&test_config, options)?;
            result.environment =
                environment_info(TestEnvironmentKind::Browser, None, Some(&browser));
            for demo in &plan.demos {
                if cancellation.is_cancelled() {
                    break;
                }
                let name = format!(
                    "{} · {} · {}x{}",
                    demo.id, theme, viewport.width, viewport.height
                );
                let suite_label = format!("docs:{name}");
                let suite_id = stable_id("suite", &suite_label);
                let case_id = stable_id("test", &suite_label);
                let case_started = Instant::now();
                let mut operations = BrowserOperationOutput::default();
                let mut policy = BrowserScreenshotPolicy::new(
                    Path::new(&demo.path),
                    &suite_label,
                    &config.test.snapshot.screenshot_directory,
                    update,
                    &browser,
                );
                let outcome = capture_demo(
                    &browser,
                    &origin,
                    Arc::clone(&resources),
                    plan,
                    demo,
                    theme,
                    config.test.timeout_ms,
                    cancellation,
                )
                .and_then(|png| {
                    policy
                        .compare_png("initial", &name, &png, &mut operations)
                        .map_err(|error| TestError::new(error.code, error.message))
                });
                let failures = match outcome {
                    Ok(value) if value["pass"] == true => Vec::new(),
                    Ok(value) => vec![TestFailure {
                        code: Some("WAKE_TEST_SNAPSHOT".into()),
                        message: value["message"]
                            .as_str()
                            .unwrap_or("Screenshot mismatch")
                            .into(),
                        stack: None,
                        location: None,
                        diff: value
                            .get("diff")
                            .cloned()
                            .and_then(|value| serde_json::from_value(value).ok()),
                    }],
                    Err(error) => vec![TestFailure {
                        code: Some(error.code.into()),
                        message: error.message,
                        stack: None,
                        location: None,
                        diff: None,
                    }],
                };
                let passed = failures.is_empty();
                let duration_ms = case_started.elapsed().as_millis() as u64;
                for artifact in &mut operations.artifacts {
                    artifact.test_id = Some(case_id.clone());
                    artifact
                        .metadata
                        .insert("demo".into(), serde_json::json!(demo.id));
                    artifact
                        .metadata
                        .insert("theme".into(), serde_json::json!(theme));
                    artifact
                        .metadata
                        .insert("viewport".into(), serde_json::json!(viewport));
                }
                merge_snapshot_summary(&mut result.snapshot, &operations.snapshot);
                result.artifacts.extend(operations.artifacts);
                result.suites.push(TestSuiteResult {
                    id: suite_id,
                    path: demo.id.clone(),
                    name: Some(name.clone()),
                    project: None,
                    environment: Some(result.environment.clone()),
                    status: if passed {
                        TestSuiteStatus::Passed
                    } else {
                        TestSuiteStatus::Failed
                    },
                    duration_ms,
                    tests: vec![TestCaseResult {
                        id: case_id,
                        name: "initial render".into(),
                        full_name: name,
                        status: if passed {
                            TestStatus::Passed
                        } else {
                            TestStatus::Failed
                        },
                        duration_ms,
                        assertions: 1,
                        attempts: 1,
                        location: None,
                        failures,
                    }],
                    failures: Vec::new(),
                    snapshot: Some(operations.snapshot),
                });
                if passed {
                    result.counts.tests.passed += 1;
                    result.counts.suites.passed += 1;
                } else {
                    result.counts.tests.failed += 1;
                    result.counts.suites.failed += 1;
                }
            }
        }
    }
    result.counts.suites.total = result.suites.len();
    result.counts.tests.total = result.suites.len();
    result.success = !result.suites.is_empty()
        && result.counts.tests.failed == 0
        && !cancellation.is_cancelled();
    if cancellation.is_cancelled() {
        result.termination_reason = TestTerminationReason::Cancelled;
    }
    result.duration_ms = started.elapsed().as_millis() as u64;
    Ok(result)
}

fn validate_plan(plan: &DocsTestPlan) -> Result<(), TestError> {
    if plan.schema_version != "wake.docs-test.v1"
        || !plan.base_path.starts_with('/')
        || !plan.base_path.ends_with('/')
        || plan.base_path.contains(['?', '#', '\\'])
        || plan.demos.is_empty()
        || plan.themes.is_empty()
        || plan.themes.len() > 2
        || plan
            .themes
            .iter()
            .any(|v| !matches!(v.as_str(), "light" | "dark"))
        || plan.viewports.is_empty()
        || plan.viewports.len() > 8
        || plan
            .viewports
            .iter()
            .any(|v| !(1..=8192).contains(&v.width) || !(1..=8192).contains(&v.height))
    {
        return Err(TestError::new(
            "WAKE_TEST_CONFIG",
            "Invalid Docs visual test plan",
        ));
    }
    Ok(())
}

fn browser_error(error: BrowserError) -> TestError {
    TestError::new("WAKE_TEST_BROWSER", error.to_string())
}

fn encode_path(path: &str, keep_slashes: bool) -> String {
    let mut encoded = String::new();
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) || (keep_slashes && byte == b'/')
        {
            encoded.push(char::from(byte));
        } else {
            use std::fmt::Write;
            write!(encoded, "%{byte:02X}").expect("writing into a String");
        }
    }
    encoded
}

fn read_resources(plan: &DocsTestPlan) -> Result<BTreeMap<String, Vec<u8>>, TestError> {
    fn visit(
        root: &Path,
        dir: &Path,
        base: &str,
        resources: &mut BTreeMap<String, Vec<u8>>,
    ) -> Result<(), TestError> {
        let entries = fs::read_dir(dir)
            .map_err(|error| TestError::new("WAKE_TEST_CONFIG", error.to_string()).at(dir))?;
        for entry in entries {
            let entry =
                entry.map_err(|error| TestError::new("WAKE_TEST_CONFIG", error.to_string()))?;
            let path = entry.path();
            let kind = entry
                .file_type()
                .map_err(|error| TestError::new("WAKE_TEST_CONFIG", error.to_string()))?;
            if kind.is_symlink() {
                return Err(TestError::new(
                    "WAKE_TEST_CONFIG",
                    "Docs test output cannot contain symlinks",
                )
                .at(&path));
            }
            if kind.is_dir() {
                visit(root, &path, base, resources)?;
            } else if kind.is_file() && entry.file_name() != ".wake-output.json" {
                let relative = path
                    .strip_prefix(root)
                    .expect("visited child")
                    .to_string_lossy()
                    .replace('\\', "/");
                let key = format!("{base}{}", encode_path(&relative, true));
                let bytes = fs::read(&path).map_err(|error| {
                    TestError::new("WAKE_TEST_CONFIG", error.to_string()).at(&path)
                })?;
                resources.insert(key, bytes);
            }
        }
        Ok(())
    }
    let mut resources = BTreeMap::new();
    visit(
        &plan.output_dir,
        &plan.output_dir,
        &plan.base_path,
        &mut resources,
    )?;
    if !resources.contains_key(&format!("{}index.html", plan.base_path)) {
        return Err(TestError::new(
            "WAKE_TEST_CONFIG",
            "Docs output has no index.html",
        ));
    }
    Ok(resources)
}

struct NetworkGuard {
    stop: BrowserCancellationToken,
    worker: Option<std::thread::JoinHandle<()>>,
    errors: Arc<Mutex<Vec<String>>>,
}

impl NetworkGuard {
    fn start(
        page: Arc<BrowserPage>,
        origin: String,
        resources: Arc<BTreeMap<String, Vec<u8>>>,
    ) -> Result<Self, TestError> {
        page.enable_network_interception().map_err(browser_error)?;
        let stop = BrowserCancellationToken::new();
        let worker_stop = stop.clone();
        let errors = Arc::new(Mutex::new(Vec::new()));
        let worker_errors = Arc::clone(&errors);
        let worker = std::thread::Builder::new()
            .name("wake-docs-resources".into())
            .spawn(move || {
                let serve = || -> Result<(), BrowserError> {
                    loop {
                        let paused = match page
                            .wait_for_fetch_request(Duration::from_millis(50), &worker_stop)?
                        {
                            CdpEventWait::Event(value) => value,
                            CdpEventWait::TimedOut => continue,
                            CdpEventWait::Cancelled => return Ok(()),
                        };
                        let path = paused
                            .request
                            .url
                            .strip_prefix(&origin)
                            .map(|path| path.split('?').next().unwrap_or(path));
                        let resource = path.and_then(|path| resources.get(path));
                        if let Some(bytes) = resource.filter(|_| paused.request.method == "GET") {
                            let mut response = FetchFulfillResponse::new(200, bytes.clone());
                            response.headers = vec![FetchHeader {
                                name: "Content-Type".into(),
                                value: content_type(path.unwrap()).into(),
                            }];
                            page.fulfill_fetch_request(&paused.request_id, &response)?;
                        } else if path == Some("/favicon.ico") {
                            page.fulfill_fetch_request(
                                &paused.request_id,
                                &FetchFulfillResponse::new(204, Vec::new()),
                            )?;
                        } else {
                            worker_errors.lock().expect("resource errors").push(format!(
                                "Demo resource was not in the Docs build: {}",
                                paused.request.url
                            ));
                            page.fail_fetch_request(&paused.request_id, "BlockedByClient")?;
                        }
                    }
                };
                if let Err(error) = serve() {
                    worker_errors
                        .lock()
                        .expect("resource errors")
                        .push(error.to_string());
                }
            })
            .map_err(|error| TestError::new("WAKE_TEST_BROWSER", error.to_string()))?;
        Ok(Self {
            stop,
            worker: Some(worker),
            errors,
        })
    }

    fn check(&self) -> Result<(), TestError> {
        let errors = self.errors.lock().expect("resource errors");
        if errors.is_empty() {
            Ok(())
        } else {
            Err(TestError::new("WAKE_TEST_BROWSER", errors.join("\n")))
        }
    }
}

impl Drop for NetworkGuard {
    fn drop(&mut self) {
        self.stop.cancel();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn content_type(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        _ => "application/octet-stream",
    }
}

#[allow(clippy::too_many_arguments)]
fn capture_demo(
    browser: &BrowserDriver,
    origin: &ResourceOrigin,
    resources: Arc<BTreeMap<String, Vec<u8>>>,
    plan: &DocsTestPlan,
    demo: &DocsTestDemo,
    theme: &str,
    timeout_ms: u64,
    cancellation: &TestCancellationToken,
) -> Result<Vec<u8>, TestError> {
    let context = browser.create_context().map_err(browser_error)?;
    let page = Arc::new(context.new_page("about:blank").map_err(browser_error)?);
    let _cancel = BrowserExecutionCancellationGuard::arm(Arc::clone(&page), cancellation);
    let network = NetworkGuard::start(
        Arc::clone(&page),
        origin.url("").trim_end_matches('/').to_string(),
        resources,
    )?;
    let id = serde_json::to_string(&demo.id).expect("Demo ID");
    let theme = serde_json::to_string(theme).expect("theme");
    let script = format!(
        r#"
        localStorage.setItem('wake-docs-theme', {theme});
        globalThis.__wakeDocsVisual = {{ready: false, errors: []}};
        addEventListener('message', event => {{
            if (event.source !== window || !event.data) return;
            if (event.data.type === 'wake:ready' && event.data.id === {id}) __wakeDocsVisual.ready = true;
            if (event.data.type === 'wake:error') __wakeDocsVisual.errors.push(String(event.data.error));
        }});
        addEventListener('error', event => __wakeDocsVisual.errors.push(String(event.error?.stack || event.message || 'Resource failed: ' + (event.target?.src || event.target?.href || 'unknown'))), true);
        addEventListener('unhandledrejection', event => __wakeDocsVisual.errors.push(String(event.reason?.stack || event.reason)));
    "#
    );
    page.command(
        "Page.addScriptToEvaluateOnNewDocument",
        serde_json::json!({"source": script}),
    )
    .map_err(browser_error)?;
    let url = origin.url(&format!(
        "{}index.html?__wake_demo={}",
        plan.base_path,
        encode_path(&demo.id, false)
    ));
    page.navigate(&url).map_err(browser_error)?;
    let started = Instant::now();
    loop {
        if cancellation.is_cancelled() {
            return Err(TestError::new("WAKE_CANCELLED", "Docs test cancelled"));
        }
        network.check()?;
        if started.elapsed() >= Duration::from_millis(timeout_ms) {
            return Err(TestError::new(
                "WAKE_TEST_TIMEOUT",
                format!(
                    "Demo {} did not become ready within {timeout_ms}ms",
                    demo.id
                ),
            ));
        }
        let state = page
            .evaluate_with_timeout("globalThis.__wakeDocsVisual || null", Some(500))
            .map_err(browser_error)?;
        if let Some(errors) = state["errors"]
            .as_array()
            .filter(|errors| !errors.is_empty())
        {
            return Err(TestError::new(
                "WAKE_TEST_BROWSER",
                errors
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .collect::<Vec<_>>()
                    .join("\n"),
            ));
        }
        if state["ready"] == true {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let remaining = timeout_ms
        .saturating_sub(started.elapsed().as_millis() as u64)
        .max(1);
    page.evaluate_with_timeout(r#"(async () => {
        await document.fonts.ready;
        await Promise.all(Array.from(document.images, image => image.complete ? Promise.resolve() : new Promise((resolve, reject) => {
            image.addEventListener('load', resolve, {once: true}); image.addEventListener('error', () => reject(new Error('Image failed: ' + image.src)), {once: true});
        })));
        for (const image of document.images) if (!image.naturalWidth) throw new Error('Image failed: ' + image.src);
        await new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)));
        if (!document.querySelector('.demo-frame-root') || document.querySelector('.frame-error')) throw new Error('Demo did not render');
        if (__wakeDocsVisual.errors.length) throw new Error(__wakeDocsVisual.errors.join('\n'));
        return true;
    })()"#, Some(remaining)).map_err(browser_error)?;
    let png = page.screenshot_png().map_err(browser_error)?;
    network.check()?;
    Ok(png)
}
