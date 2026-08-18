use std::cell::RefCell;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, Weak};
use std::thread::ThreadId;

use napi::bindgen_prelude::{AbortSignal, AsyncTask};
use napi::{Env, Task};
use napi_derive::napi;
use serde::Deserialize;
use serde_json::{Value, json};
use wake_app::{CancellationToken, WakeError};
use wake_common::Interner;
use wake_ecma_ast::SourceType;
use wake_ecma_parser::ParseOutput;

struct ContextResource {
    context: wake_app::BuildContext,
}

struct ServerResource {
    server: wake_app::DevServer,
}

#[derive(Default)]
struct EnvResources {
    contexts: Mutex<Vec<Weak<ContextResource>>>,
    servers: Mutex<Vec<Weak<ServerResource>>>,
}

impl EnvResources {
    #[cfg_attr(test, allow(dead_code))]
    fn close_all(&self) {
        let contexts = {
            let mut contexts = self
                .contexts
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            std::mem::take(&mut *contexts)
        };
        for context in contexts.into_iter().filter_map(|context| context.upgrade()) {
            context.context.close();
        }
        let servers = {
            let mut servers = self
                .servers
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            std::mem::take(&mut *servers)
        };
        for server in servers.into_iter().filter_map(|server| server.upgrade()) {
            let _ = server.server.close();
        }
    }
}

thread_local! {
    static ENV_RESOURCES: RefCell<Option<Arc<EnvResources>>> = const { RefCell::new(None) };
}

fn current_env_resources() -> Arc<EnvResources> {
    ENV_RESOURCES.with(|slot| {
        slot.borrow()
            .clone()
            .unwrap_or_else(|| Arc::new(EnvResources::default()))
    })
}

#[napi(module_exports)]
#[cfg_attr(test, allow(dead_code))]
fn initialize_module(env: Env) -> napi::Result<()> {
    let resources = Arc::new(EnvResources::default());
    env.add_env_cleanup_hook(Arc::clone(&resources), |resources| {
        let _ = catch_unwind(AssertUnwindSafe(|| resources.close_all()));
    })?;
    ENV_RESOURCES.with(|slot| *slot.borrow_mut() = Some(resources));
    Ok(())
}
#[napi]
pub fn version() -> &'static str {
    wake_app::VERSION
}
type JsonWork = Box<dyn FnOnce() -> Result<Value, WakeError> + Send + 'static>;

pub struct JsonTask {
    work: Option<JsonWork>,
}

impl JsonTask {
    fn new(work: impl FnOnce() -> Result<Value, WakeError> + Send + 'static) -> Self {
        Self {
            work: Some(Box::new(work)),
        }
    }
}

impl Task for JsonTask {
    type Output = String;
    type JsValue = String;

    fn compute(&mut self) -> napi::Result<Self::Output> {
        let work = self
            .work
            .take()
            .ok_or_else(|| napi::Error::from_reason("Wake task was already consumed"))?;
        let envelope = match catch_unwind(AssertUnwindSafe(work)) {
            Ok(Ok(value)) => json!({ "ok": true, "value": value }),
            Ok(Err(error)) => json!({ "ok": false, "error": error }),
            Err(_) => json!({
                "ok": false,
                "error": WakeError::new("WAKE_INTERNAL", "Wake native task panicked")
            }),
        };
        serde_json::to_string(&envelope)
            .map_err(|error| napi::Error::from_reason(error.to_string()))
    }

    fn resolve(&mut self, _env: napi::Env, output: Self::Output) -> napi::Result<Self::JsValue> {
        Ok(output)
    }
}

fn async_json(task: JsonTask, signal: Option<AbortSignal>) -> AsyncTask<JsonTask> {
    AsyncTask::with_optional_signal(task, signal)
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct RawBuildOptions {
    cwd: Option<String>,
    config_path: Option<String>,
    entry: Option<String>,
    outdir: Option<String>,
    cache: bool,
    source_map: bool,
}

impl RawBuildOptions {
    fn parse(value: Option<String>) -> Result<Self, WakeError> {
        value
            .map(|value| {
                serde_json::from_str(&value)
                    .map_err(|error| WakeError::new("WAKE_CONFIG", error.to_string()))
            })
            .unwrap_or_else(|| Ok(Self::default()))
    }

    fn into_app(self, write: bool) -> wake_app::BuildOptions {
        wake_app::BuildOptions {
            project: wake_app::ProjectOptions {
                cwd: self.cwd.map(PathBuf::from),
                config_path: self.config_path.map(PathBuf::from),
            },
            entry: self.entry.map(PathBuf::from),
            outdir: self.outdir.map(PathBuf::from),
            cache: self.cache,
            source_map: self.source_map,
            write,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct RawBundleOptions {
    cwd: Option<String>,
    config_path: Option<String>,
    entry: Option<String>,
    outfile: Option<String>,
    platform: Option<String>,
    format: Option<String>,
    target: Option<String>,
    external: Vec<String>,
    minify: bool,
    source_map: bool,
    cache: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct RawGenerateCssTokenOptions {
    cwd: Option<String>,
    config_path: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct RawGenerateDocgenOptions {
    cwd: Option<String>,
    entry: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct RawLibraryBuildOptions {
    cwd: Option<String>,
    entry: Option<String>,
}

impl RawGenerateCssTokenOptions {
    fn parse(value: Option<String>) -> Result<Self, WakeError> {
        value
            .map(|value| {
                serde_json::from_str(&value)
                    .map_err(|error| WakeError::new("WAKE_CONFIG", error.to_string()))
            })
            .unwrap_or_else(|| Ok(Self::default()))
    }

    fn into_app(self) -> wake_app::GenerateCssTokenOptions {
        wake_app::GenerateCssTokenOptions {
            project: wake_app::ProjectOptions {
                cwd: self.cwd.map(PathBuf::from),
                config_path: None,
            },
            config_path: self.config_path.map(PathBuf::from),
        }
    }
}

impl RawGenerateDocgenOptions {
    fn parse(value: Option<String>) -> Result<Self, WakeError> {
        value
            .map(|value| {
                serde_json::from_str(&value)
                    .map_err(|error| WakeError::new("WAKE_CONFIG", error.to_string()))
            })
            .unwrap_or_else(|| Ok(Self::default()))
    }

    fn into_app(self) -> wake_app::GenerateDocgenOptions {
        wake_app::GenerateDocgenOptions {
            project: wake_app::ProjectOptions {
                cwd: self.cwd.map(PathBuf::from),
                config_path: None,
            },
            entry: self.entry.map(PathBuf::from),
        }
    }
}

impl RawLibraryBuildOptions {
    fn parse(value: Option<String>) -> Result<Self, WakeError> {
        value
            .map(|value| {
                serde_json::from_str(&value)
                    .map_err(|error| WakeError::new("WAKE_CONFIG", error.to_string()))
            })
            .unwrap_or_else(|| Ok(Self::default()))
    }

    fn into_app(self) -> wake_app::LibraryBuildOptions {
        wake_app::LibraryBuildOptions {
            project: wake_app::ProjectOptions {
                cwd: self.cwd.map(PathBuf::from),
                config_path: None,
            },
            entry: self.entry.map(PathBuf::from),
        }
    }
}

impl RawBundleOptions {
    fn parse(value: Option<String>) -> Result<Self, WakeError> {
        value
            .map(|value| {
                serde_json::from_str(&value)
                    .map_err(|error| WakeError::new("WAKE_CONFIG", error.to_string()))
            })
            .unwrap_or_else(|| Ok(Self::default()))
    }

    fn into_app(self) -> Result<wake_app::BundleOptions, WakeError> {
        let platform = match self.platform.as_deref() {
            None => None,
            Some("browser") => Some(wake_app::BuildPlatform::Browser),
            Some("node") => Some(wake_app::BuildPlatform::Node),
            Some(value) => {
                return Err(WakeError::new(
                    "WAKE_CONFIG",
                    format!("unsupported bundle platform: {value}"),
                ));
            }
        };
        let format = match self.format.as_deref() {
            None => None,
            Some("iife") => Some(wake_app::ModuleFormat::Iife),
            Some("cjs") => Some(wake_app::ModuleFormat::CommonJs),
            Some(value) => {
                return Err(WakeError::new(
                    "WAKE_CONFIG",
                    format!("unsupported bundle format: {value}"),
                ));
            }
        };
        Ok(wake_app::BundleOptions {
            project: wake_app::ProjectOptions {
                cwd: self.cwd.map(PathBuf::from),
                config_path: self.config_path.map(PathBuf::from),
            },
            entry: self.entry.map(PathBuf::from),
            outfile: self.outfile.map(PathBuf::from),
            platform,
            format,
            target: self.target,
            external: self.external,
            minify: self.minify,
            source_map: self.source_map,
            cache: self.cache,
        })
    }
}

#[napi(js_name = "build")]
pub fn native_build(
    options_json: Option<String>,
    signal: Option<AbortSignal>,
) -> AsyncTask<JsonTask> {
    let cancellation = CancellationToken::default();
    if let Some(signal) = &signal {
        let cancellation = cancellation.clone();
        signal.on_abort(move || cancellation.cancel());
    }
    async_json(
        JsonTask::new(move || {
            let options = RawBuildOptions::parse(options_json)?.into_app(true);
            let result = wake_app::build(options, &cancellation)?;
            serde_json::to_value(result)
                .map_err(|error| WakeError::new("WAKE_INTERNAL", error.to_string()))
        }),
        signal,
    )
}

#[napi(js_name = "bundle")]
pub fn native_bundle(
    options_json: Option<String>,
    signal: Option<AbortSignal>,
) -> AsyncTask<JsonTask> {
    let cancellation = CancellationToken::default();
    if let Some(signal) = &signal {
        let cancellation = cancellation.clone();
        signal.on_abort(move || cancellation.cancel());
    }
    async_json(
        JsonTask::new(move || {
            let options = RawBundleOptions::parse(options_json)?.into_app()?;
            let result = wake_app::bundle(options, &cancellation)?;
            serde_json::to_value(result)
                .map_err(|error| WakeError::new("WAKE_INTERNAL", error.to_string()))
        }),
        signal,
    )
}

#[napi(js_name = "generateCssToken")]
pub fn native_generate_css_token(
    options_json: Option<String>,
    signal: Option<AbortSignal>,
) -> AsyncTask<JsonTask> {
    let cancellation = CancellationToken::default();
    if let Some(signal) = &signal {
        let cancellation = cancellation.clone();
        signal.on_abort(move || cancellation.cancel());
    }
    async_json(
        JsonTask::new(move || {
            let options = RawGenerateCssTokenOptions::parse(options_json)?.into_app();
            let result = wake_app::generate_css_token(options, &cancellation)?;
            serde_json::to_value(result)
                .map_err(|error| WakeError::new("WAKE_INTERNAL", error.to_string()))
        }),
        signal,
    )
}

#[napi(js_name = "generateDocgen")]
pub fn native_generate_docgen(
    options_json: Option<String>,
    signal: Option<AbortSignal>,
) -> AsyncTask<JsonTask> {
    let cancellation = CancellationToken::default();
    if let Some(signal) = &signal {
        let cancellation = cancellation.clone();
        signal.on_abort(move || cancellation.cancel());
    }
    async_json(
        JsonTask::new(move || {
            let options = RawGenerateDocgenOptions::parse(options_json)?.into_app();
            let result = wake_app::generate_docgen(options, &cancellation)?;
            serde_json::to_value(result)
                .map_err(|error| WakeError::new("WAKE_INTERNAL", error.to_string()))
        }),
        signal,
    )
}

#[napi(js_name = "buildLibrary")]
pub fn native_build_library(
    options_json: Option<String>,
    signal: Option<AbortSignal>,
) -> AsyncTask<JsonTask> {
    let cancellation = CancellationToken::default();
    if let Some(signal) = &signal {
        let cancellation = cancellation.clone();
        signal.on_abort(move || cancellation.cancel());
    }
    async_json(
        JsonTask::new(move || {
            let options = RawLibraryBuildOptions::parse(options_json)?.into_app();
            let result = wake_app::build_library(options, &cancellation)?;
            serde_json::to_value(result)
                .map_err(|error| WakeError::new("WAKE_INTERNAL", error.to_string()))
        }),
        signal,
    )
}

#[napi]
pub struct NativeBuildContext {
    resource: Arc<ContextResource>,
}

#[napi]
impl NativeBuildContext {
    #[napi]
    pub fn rebuild(
        &self,
        changed_paths: Option<Vec<String>>,
        signal: Option<AbortSignal>,
    ) -> AsyncTask<JsonTask> {
        let context = self.resource.context.clone();
        let cancellation = CancellationToken::default();
        if let Some(signal) = &signal {
            let cancellation = cancellation.clone();
            signal.on_abort(move || cancellation.cancel());
        }
        async_json(
            JsonTask::new(move || {
                let paths = changed_paths
                    .unwrap_or_default()
                    .into_iter()
                    .map(PathBuf::from)
                    .collect();
                let result = context.rebuild(paths, cancellation)?;
                serde_json::to_value(result)
                    .map_err(|error| WakeError::new("WAKE_INTERNAL", error.to_string()))
            }),
            signal,
        )
    }

    #[napi]
    pub fn close(&self) -> AsyncTask<JsonTask> {
        let context = self.resource.context.clone();
        async_json(
            JsonTask::new(move || {
                context.close();
                Ok(Value::Null)
            }),
            None,
        )
    }

    #[napi(getter)]
    pub fn closed(&self) -> bool {
        self.resource.context.is_closed()
    }
}

#[napi(js_name = "createBuildContext")]
pub fn create_build_context(options_json: Option<String>) -> napi::Result<NativeBuildContext> {
    let context = catch_unwind(AssertUnwindSafe(|| {
        RawBuildOptions::parse(options_json)
            .and_then(|options| wake_app::BuildContext::create(options.into_app(true)))
    }))
    .map_err(|_| {
        napi_wake_error(WakeError::new(
            "WAKE_INTERNAL",
            "Wake context creation panicked",
        ))
    })?
    .map_err(napi_wake_error)?;
    let resources = current_env_resources();
    let resource = Arc::new(ContextResource { context });
    resources
        .contexts
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .push(Arc::downgrade(&resource));
    Ok(NativeBuildContext { resource })
}

impl Drop for NativeBuildContext {
    fn drop(&mut self) {
        self.resource.context.request_close();
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct RawDevServerOptions {
    cwd: Option<String>,
    config_path: Option<String>,
    entry: Option<String>,
    host: Option<String>,
    port: Option<u16>,
    open: Option<bool>,
    mode: Option<wake_app::DocsMode>,
}

impl RawDevServerOptions {
    fn parse(value: Option<String>) -> Result<Self, WakeError> {
        value
            .map(|value| {
                serde_json::from_str(&value)
                    .map_err(|error| WakeError::new("WAKE_CONFIG", error.to_string()))
            })
            .unwrap_or_else(|| Ok(Self::default()))
    }

    fn into_app(self) -> (wake_app::DevServerOptions, wake_app::DocsMode) {
        let mode = self.mode.unwrap_or_default();
        let options = wake_app::DevServerOptions {
            project: wake_app::ProjectOptions {
                cwd: self.cwd.map(PathBuf::from),
                config_path: self.config_path.map(PathBuf::from),
            },
            entry: self.entry.map(PathBuf::from),
            host: self.host,
            port: self.port,
            open: self.open,
        };
        (options, mode)
    }
}

enum ServerKind {
    Application,
    Documentation,
}

pub struct StartServerTask {
    options: Option<String>,
    kind: ServerKind,
    resources: Arc<EnvResources>,
}

impl Task for StartServerTask {
    type Output = Result<wake_app::DevServer, WakeError>;
    type JsValue = NativeDevServer;

    fn compute(&mut self) -> napi::Result<Self::Output> {
        let options = self.options.take();
        let kind = &self.kind;
        Ok(
            match catch_unwind(AssertUnwindSafe(|| {
                let (options, docs_mode) = RawDevServerOptions::parse(options)?.into_app();
                match kind {
                    ServerKind::Application => wake_app::start_dev_server(options),
                    ServerKind::Documentation => {
                        wake_app::start_docs_dev_server_with_mode(options, docs_mode)
                    }
                }
            })) {
                Ok(result) => result,
                Err(_) => Err(WakeError::new(
                    "WAKE_INTERNAL",
                    "Wake dev server startup panicked",
                )),
            },
        )
    }

    fn resolve(&mut self, _env: napi::Env, output: Self::Output) -> napi::Result<Self::JsValue> {
        output
            .map(|server| {
                let resource = Arc::new(ServerResource { server });
                self.resources
                    .servers
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .push(Arc::downgrade(&resource));
                NativeDevServer { resource }
            })
            .map_err(napi_wake_error)
    }
}

#[napi]
pub struct NativeDevServer {
    resource: Arc<ServerResource>,
}

#[napi]
impl NativeDevServer {
    #[napi(getter)]
    pub fn url(&self) -> String {
        self.resource.server.url().to_string()
    }

    #[napi(js_name = "eventsJson")]
    pub fn events_json(&self) -> napi::Result<String> {
        serde_json::to_value(self.resource.server.drain_events())
            .map_err(|error| napi::Error::from_reason(error.to_string()))
            .and_then(value_string)
    }

    #[napi]
    pub fn close(&self) -> AsyncTask<JsonTask> {
        let server = self.resource.server.clone();
        async_json(
            JsonTask::new(move || {
                server.close()?;
                Ok(Value::Null)
            }),
            None,
        )
    }

    #[napi(js_name = "waitUntilClosed")]
    pub fn wait_until_closed(&self) -> AsyncTask<JsonTask> {
        let server = self.resource.server.clone();
        async_json(
            JsonTask::new(move || {
                server.wait_until_closed()?;
                Ok(Value::Null)
            }),
            None,
        )
    }
}

impl Drop for NativeDevServer {
    fn drop(&mut self) {
        self.resource.server.request_close();
    }
}

#[napi(js_name = "startDevServer")]
pub fn start_dev_server(
    options_json: Option<String>,
    signal: Option<AbortSignal>,
) -> AsyncTask<StartServerTask> {
    AsyncTask::with_optional_signal(
        StartServerTask {
            options: options_json,
            kind: ServerKind::Application,
            resources: current_env_resources(),
        },
        signal,
    )
}

#[napi(js_name = "startDocsDevServer")]
pub fn start_docs_dev_server(
    options_json: Option<String>,
    signal: Option<AbortSignal>,
) -> AsyncTask<StartServerTask> {
    AsyncTask::with_optional_signal(
        StartServerTask {
            options: options_json,
            kind: ServerKind::Documentation,
            resources: current_env_resources(),
        },
        signal,
    )
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct RawDocsOptions {
    cwd: Option<String>,
    config_path: Option<String>,
    outdir: Option<String>,
    base_path: Option<String>,
    mode: Option<wake_app::DocsMode>,
}

#[napi(js_name = "buildDocs")]
pub fn build_docs(
    options_json: Option<String>,
    signal: Option<AbortSignal>,
) -> AsyncTask<JsonTask> {
    let cancellation = CancellationToken::default();
    if let Some(signal) = &signal {
        let cancellation = cancellation.clone();
        signal.on_abort(move || cancellation.cancel());
    }
    async_json(
        JsonTask::new(move || {
            let options: RawDocsOptions = options_json
                .map(|value| {
                    serde_json::from_str(&value)
                        .map_err(|error| WakeError::new("WAKE_CONFIG", error.to_string()))
                })
                .unwrap_or_else(|| Ok(RawDocsOptions::default()))?;
            let docs_mode = options.mode.unwrap_or_default();
            let result = wake_app::build_docs_with_mode(
                wake_app::DocsBuildOptions {
                    project: wake_app::ProjectOptions {
                        cwd: options.cwd.map(PathBuf::from),
                        config_path: options.config_path.map(PathBuf::from),
                    },
                    outdir: options.outdir.map(PathBuf::from),
                    base_path: options.base_path,
                },
                docs_mode,
                &cancellation,
            )?;
            serde_json::to_value(result)
                .map_err(|error| WakeError::new("WAKE_INTERNAL", error.to_string()))
        }),
        signal,
    )
}

struct ParsedOwned {
    source: String,
    interner: Interner,
    output: ParseOutput,
}

#[napi]
pub struct NativeParsedModule {
    inner: Arc<Mutex<Option<ParsedOwned>>>,
    owner: ThreadId,
}

#[napi]
impl NativeParsedModule {
    #[napi]
    pub fn dispose(&self) {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *inner = None;
    }

    #[napi(getter)]
    pub fn disposed(&self) -> bool {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_none()
    }

    #[napi(js_name = "summaryJson")]
    pub fn summary_json(&self) -> napi::Result<String> {
        self.with_parsed(|parsed| {
            let diagnostics = parsed
                .output
                .diagnostics
                .iter()
                .map(wake_app::DiagnosticInfo::from)
                .collect::<Vec<_>>();
            Ok(json!({
                "sourceBytes": parsed.source.len(),
                "statementCount": parsed.output.module.statement_count(),
                "dependencies": parsed.output.dependencies.len(),
                "hasTopLevelAwait": parsed.output.has_top_level_await,
                "diagnostics": diagnostics,
            }))
        })
        .and_then(value_string)
    }

    #[napi(js_name = "transformJson")]
    pub fn transform_json(&self) -> napi::Result<String> {
        self.with_parsed(|parsed| {
            let code = parsed
                .output
                .module
                .with_ast(|program| wake_ecma_codegen::codegen(program, &parsed.interner));
            Ok(json!({
                "code": code,
                "diagnostics": parsed.output.diagnostics.iter().map(wake_app::DiagnosticInfo::from).collect::<Vec<_>>()
            }))
        })
        .and_then(value_string)
    }

    #[napi(js_name = "analyzeJson")]
    pub fn analyze_json(&self) -> napi::Result<String> {
        self.with_parsed(|parsed| {
            let semantic = parsed
                .output
                .module
                .with_ast(wake_ecma_semantic::analyze);
            let scopes = semantic
                .scopes
                .iter()
                .enumerate()
                .map(|(id, scope)| {
                    let mut bindings = scope
                        .bindings
                        .iter()
                        .map(|(name, symbol)| (parsed.interner.resolve(*name), *symbol))
                        .collect::<Vec<_>>();
                    bindings.sort_by(|left, right| left.0.cmp(&right.0));
                    json!({
                        "id": id,
                        "kind": format!("{:?}", scope.kind).to_lowercase(),
                        "parent": scope.parent,
                        "bindings": bindings.into_iter().map(|(name, symbol)| json!({"name": name, "symbol": symbol})).collect::<Vec<_>>()
                    })
                })
                .collect::<Vec<_>>();
            let symbols = semantic
                .symbols
                .iter()
                .enumerate()
                .map(|(id, symbol)| {
                    json!({
                        "id": id,
                        "name": parsed.interner.resolve(symbol.name),
                        "declarationKind": format!("{:?}", symbol.decl_kind).to_lowercase(),
                        "scope": symbol.scope,
                        "start": symbol.span.lo,
                        "end": symbol.span.hi
                    })
                })
                .collect::<Vec<_>>();
            let references = semantic
                .references
                .iter()
                .enumerate()
                .map(|(id, reference)| {
                    json!({
                        "id": id,
                        "name": parsed.interner.resolve(reference.name),
                        "scope": reference.scope,
                        "resolved": reference.resolved,
                        "start": reference.span.lo,
                        "end": reference.span.hi
                    })
                })
                .collect::<Vec<_>>();
            Ok(json!({
                "schemaVersion": "wake.semantic.v1",
                "scopes": scopes,
                "symbols": symbols,
                "references": references,
            }))
        })
        .and_then(value_string)
    }
}

impl NativeParsedModule {
    fn with_parsed(
        &self,
        operation: impl FnOnce(&ParsedOwned) -> Result<Value, WakeError>,
    ) -> napi::Result<Value> {
        if std::thread::current().id() != self.owner {
            return Err(napi_wake_error(WakeError::new(
                "WAKE_INTERNAL",
                "ParsedModule cannot be used from a different Worker",
            )));
        }
        let inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let parsed = inner
            .as_ref()
            .ok_or_else(|| napi_wake_error(WakeError::closed("ParsedModule")))?;
        match catch_unwind(AssertUnwindSafe(|| operation(parsed))) {
            Ok(result) => result.map_err(napi_wake_error),
            Err(_) => Err(napi_wake_error(WakeError::new(
                "WAKE_INTERNAL",
                "Wake experimental operation panicked",
            ))),
        }
    }
}

#[napi(js_name = "tokenize")]
pub fn tokenize(source: String) -> napi::Result<String> {
    let result = catch_unwind(AssertUnwindSafe(|| {
        let (tokens, diagnostics) = wake_ecma_lexer::tokenize(&source);
        json!({
            "tokens": tokens.into_iter().map(|token| {
                json!({
                    "kind": format!("{:?}", token.kind),
                    "start": token.span.lo,
                    "end": token.span.hi,
                    "newlineBefore": token.newline_before,
                    "text": source.get(token.span.lo as usize..token.span.hi as usize).unwrap_or_default()
                })
            }).collect::<Vec<_>>(),
            "diagnostics": diagnostics.iter().map(wake_app::DiagnosticInfo::from).collect::<Vec<_>>()
        })
    }))
    .map_err(|_| napi_wake_error(WakeError::new("WAKE_INTERNAL", "Wake tokenizer panicked")))?;
    value_string(result)
}

#[napi(js_name = "parse")]
pub fn parse(source: String, source_type: Option<String>) -> napi::Result<NativeParsedModule> {
    catch_unwind(AssertUnwindSafe(|| {
        let source_type = match source_type.as_deref() {
            Some("script") | Some("commonjs") => SourceType::Script,
            _ => SourceType::Module,
        };
        let interner = Interner::new();
        let output = wake_ecma_parser::parse(&source, &interner, source_type);
        NativeParsedModule {
            inner: Arc::new(Mutex::new(Some(ParsedOwned {
                source,
                interner,
                output,
            }))),
            owner: std::thread::current().id(),
        }
    }))
    .map_err(|_| napi_wake_error(WakeError::new("WAKE_INTERNAL", "Wake parser panicked")))
}

fn value_string(value: Value) -> napi::Result<String> {
    serde_json::to_string(&value).map_err(|error| napi::Error::from_reason(error.to_string()))
}

fn napi_wake_error(error: WakeError) -> napi::Error {
    let payload = serde_json::to_string(&error).unwrap_or_else(|_| {
        "{\"code\":\"WAKE_INTERNAL\",\"message\":\"failed to serialize Wake error\"}".to_string()
    });
    napi::Error::from_reason(format!("WAKE_ERROR_JSON:{payload}"))
}
