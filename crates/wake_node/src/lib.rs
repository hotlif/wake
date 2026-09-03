use std::cell::RefCell;
use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::{
    Arc, Mutex, Weak,
    atomic::{AtomicBool, Ordering},
};
use std::thread::ThreadId;

use napi::bindgen_prelude::{AbortSignal, AsyncTask};
use napi::{Env, Task};
use napi_derive::napi;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use wake_app::{CancellationToken, WakeError};
use wake_common::{Interner, SourceFile};
use wake_ecma_ast::SourceType;
use wake_ecma_parser::ParseOutput;

struct ContextResource {
    context: wake_app::BuildContext,
}

struct ServerResource {
    server: wake_app::DevServer,
}

struct TestContextResource {
    options: wake_app::TestOptions,
    host_path: Option<PathBuf>,
    session: Mutex<Option<wake_app::TestSession>>,
    events: Mutex<Vec<wake_app::TestSessionEvent>>,
    event_error: Mutex<Option<WakeError>>,
    active_cancellation: Mutex<Option<CancellationToken>>,
    running: AtomicBool,
    watching: AtomicBool,
    closed: AtomicBool,
}

fn test_context_closed_error() -> WakeError {
    WakeError::new("WAKE_TEST_CONTEXT", "TestContext has already been closed")
}

impl TestContextResource {
    fn ensure_not_running(&self, operation: &str) -> Result<(), WakeError> {
        if self.running.load(Ordering::Acquire) {
            Err(WakeError::new(
                "WAKE_TEST_BUSY",
                format!("cannot {operation} while TestContext.run() is active"),
            ))
        } else {
            Ok(())
        }
    }

    fn with_session<T>(
        &self,
        startup_cancellation: &CancellationToken,
        operation: impl FnOnce(&mut wake_app::TestSession) -> Result<T, WakeError>,
    ) -> Result<T, WakeError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(test_context_closed_error());
        }
        let mut slot = self
            .session
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if self.closed.load(Ordering::Acquire) {
            return Err(test_context_closed_error());
        }
        if slot.is_none() {
            *slot = Some(wake_app::TestSession::start_with_host(
                self.host_path.as_deref(),
                startup_cancellation,
            )?);
        }
        let session = slot
            .as_mut()
            .expect("the persistent test session was initialized");
        let result = operation(session);
        let events = session.drain_events();
        self.events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .extend(events);
        drop(slot);
        result
    }

    fn run(&self, cancellation: &CancellationToken) -> Result<wake_app::TestRunResult, WakeError> {
        if self.running.swap(true, Ordering::AcqRel) {
            return Err(WakeError::new(
                "WAKE_TEST_BUSY",
                "TestContext already has an active run",
            ));
        }
        *self
            .active_cancellation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(cancellation.clone());
        let result = self.with_session(cancellation, |session| {
            session.run(self.options.clone(), cancellation)
        });
        self.active_cancellation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        self.running.store(false, Ordering::Release);
        result
    }

    fn start_watch(&self) -> Result<(), WakeError> {
        self.ensure_not_running("start watch")?;
        let options = self.options.clone();
        self.with_session(&CancellationToken::default(), |session| {
            session.start_watch(options)
        })?;
        self.watching.store(true, Ordering::Release);
        Ok(())
    }

    fn stop_watch(&self) -> Result<(), WakeError> {
        self.ensure_not_running("stop watch")?;
        self.with_session(
            &CancellationToken::default(),
            wake_app::TestSession::stop_watch,
        )?;
        self.watching.store(false, Ordering::Release);
        Ok(())
    }

    fn watch_control(&self, control: wake_app::TestWatchControl) -> Result<(), WakeError> {
        self.ensure_not_running("control watch")?;
        self.with_session(&CancellationToken::default(), |session| {
            session.watch_control(control)
        })
    }

    fn watching(&self) -> bool {
        self.watching.load(Ordering::Acquire)
    }

    fn drain_events(&self) -> Result<Vec<wake_app::TestSessionEvent>, WakeError> {
        let take_buffered = || {
            std::mem::take(
                &mut *self
                    .events
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()),
            )
        };
        let buffered = take_buffered();
        if !buffered.is_empty() {
            return Ok(buffered);
        }
        if let Some(error) = self
            .event_error
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        {
            return Err(error);
        }
        if !self.closed.load(Ordering::Acquire) {
            match self.session.try_lock() {
                Ok(mut slot) => {
                    if let Some(session) = slot.as_mut() {
                        if let Err(error) = session.poll_events() {
                            self.watching.store(false, Ordering::Release);
                            self.events
                                .lock()
                                .unwrap_or_else(|poisoned| poisoned.into_inner())
                                .extend(session.drain_events());
                            *self
                                .event_error
                                .lock()
                                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(error);
                        }
                        self.events
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .extend(session.drain_events());
                    }
                }
                Err(std::sync::TryLockError::WouldBlock) => {}
                Err(std::sync::TryLockError::Poisoned(poisoned)) => {
                    let mut slot = poisoned.into_inner();
                    if let Some(session) = slot.as_mut() {
                        if let Err(error) = session.poll_events() {
                            self.watching.store(false, Ordering::Release);
                            self.events
                                .lock()
                                .unwrap_or_else(|events| events.into_inner())
                                .extend(session.drain_events());
                            *self
                                .event_error
                                .lock()
                                .unwrap_or_else(|error| error.into_inner()) = Some(error);
                        }
                        self.events
                            .lock()
                            .unwrap_or_else(|events| events.into_inner())
                            .extend(session.drain_events());
                    }
                }
            }
        }
        let buffered = take_buffered();
        if !buffered.is_empty() {
            Ok(buffered)
        } else if let Some(error) = self
            .event_error
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        {
            Err(error)
        } else {
            Ok(Vec::new())
        }
    }

    fn close(&self) -> Result<(), WakeError> {
        if self.closed.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        if let Some(cancellation) = self
            .active_cancellation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
        {
            cancellation.cancel();
        }
        self.watching.store(false, Ordering::Release);
        let mut session = self
            .session
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        let result = session
            .as_mut()
            .map_or(Ok(()), wake_app::TestSession::close);
        let mut events = Vec::new();
        if let Some(session) = &mut session {
            events.extend(session.drain_events());
        }
        if !events
            .iter()
            .any(|event| matches!(event, wake_app::TestSessionEvent::Closed))
        {
            events.push(wake_app::TestSessionEvent::Closed);
        }
        self.events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .extend(events);
        result
    }
}

impl Drop for TestContextResource {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

#[derive(Default)]
struct EnvResources {
    contexts: Mutex<Vec<Weak<ContextResource>>>,
    servers: Mutex<Vec<Weak<ServerResource>>>,
    test_contexts: Mutex<Vec<Weak<TestContextResource>>>,
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
        let test_contexts = {
            let mut test_contexts = self
                .test_contexts
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            std::mem::take(&mut *test_contexts)
        };
        for context in test_contexts
            .into_iter()
            .filter_map(|context| context.upgrade())
        {
            let _ = context.close();
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

const fn node_output_file_kind(kind: wake_app::OutputFileKind) -> &'static str {
    match kind {
        wake_app::OutputFileKind::Asset => "asset",
        wake_app::OutputFileKind::Chunk => "chunk",
        wake_app::OutputFileKind::Css => "css",
        wake_app::OutputFileKind::Declaration => "declaration",
        wake_app::OutputFileKind::Entry => "entry",
        wake_app::OutputFileKind::FederationBootstrap => "federation-bootstrap",
        wake_app::OutputFileKind::FederationChunk => "federation-chunk",
        wake_app::OutputFileKind::FederationEntry => "federation-entry",
        wake_app::OutputFileKind::FederationManifest => "federation-manifest",
        wake_app::OutputFileKind::FederationShared => "federation-shared",
        wake_app::OutputFileKind::FederationTypes => "types",
        wake_app::OutputFileKind::Html => "html",
        wake_app::OutputFileKind::Manifest => "manifest",
        wake_app::OutputFileKind::SourceMap => "map",
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct NodeOutputFile<'a> {
    path: &'a str,
    kind: &'static str,
    bytes: usize,
}

fn node_output_files(files: &[wake_app::OutputFile]) -> Vec<NodeOutputFile<'_>> {
    files
        .iter()
        .map(|file| NodeOutputFile {
            path: file.path.as_str(),
            kind: node_output_file_kind(file.kind),
            bytes: file.bytes,
        })
        .collect()
}

fn node_result_value<T: Serialize>(
    result: &T,
    files: &[wake_app::OutputFile],
) -> Result<Value, WakeError> {
    let mut value = serde_json::to_value(result)
        .map_err(|error| WakeError::new("WAKE_INTERNAL", error.to_string()))?;
    let object = value.as_object_mut().ok_or_else(|| {
        WakeError::new(
            "WAKE_INTERNAL",
            "Wake result did not serialize as an object",
        )
    })?;
    if !object.contains_key("files") {
        return Err(WakeError::new(
            "WAKE_INTERNAL",
            "Wake result did not expose its output file inventory",
        ));
    }
    object.insert(
        "files".to_owned(),
        serde_json::to_value(node_output_files(files))
            .map_err(|error| WakeError::new("WAKE_INTERNAL", error.to_string()))?,
    );
    Ok(value)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct NodeDocsHeading<'a> {
    depth: u8,
    title: &'a str,
    id: &'a str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct NodeDocsRoute<'a> {
    id: &'a str,
    file: &'a str,
    title: &'a str,
    description: &'a str,
    kind: &'a str,
    group: &'a str,
    group_id: &'a str,
    section: &'a str,
    section_id: &'a str,
    slug: &'a str,
    status: &'a str,
    draft: bool,
    hidden: bool,
    headings: Vec<NodeDocsHeading<'a>>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct NodeDocsDemo<'a> {
    id: &'a str,
    title: &'a str,
    group: &'a str,
    component: &'a str,
    order: i32,
    control_count: usize,
    warnings: &'a [String],
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct NodeDocsWorkspaceBuildInfo<'a> {
    name: &'a str,
    root: &'a str,
    base_path: &'a str,
    mode: &'static str,
    presentation: &'a str,
    demos: usize,
}

impl<'a> From<&'a wake_app::DocsWorkspaceBuildInfo> for NodeDocsWorkspaceBuildInfo<'a> {
    fn from(value: &'a wake_app::DocsWorkspaceBuildInfo) -> Self {
        Self {
            name: &value.name,
            root: &value.root,
            base_path: &value.base_path,
            mode: value.mode.as_str(),
            presentation: &value.presentation,
            demos: value.demos,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct NodeDocsBuildResult<'a> {
    success: bool,
    module_count: usize,
    updated_module_count: usize,
    cached_module_count: usize,
    duration_ms: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    output_dir: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    code: Option<&'a str>,
    files: Vec<NodeOutputFile<'a>>,
    diagnostics: &'a [wake_app::DiagnosticInfo],
    routes: Vec<NodeDocsRoute<'a>>,
    mode: &'static str,
    demos: Vec<NodeDocsDemo<'a>>,
    workspaces: Vec<NodeDocsWorkspaceBuildInfo<'a>>,
}

impl<'a> From<&'a wake_app::DocsBuildResult> for NodeDocsBuildResult<'a> {
    fn from(value: &'a wake_app::DocsBuildResult) -> Self {
        Self {
            success: value.build.success,
            module_count: value.build.module_count,
            updated_module_count: value.build.updated_module_count,
            cached_module_count: value.build.cached_module_count,
            duration_ms: value.build.duration_ms,
            output_dir: value.build.output_dir.as_deref(),
            code: value.build.code.as_deref(),
            files: node_output_files(&value.build.files),
            diagnostics: &value.build.diagnostics,
            routes: value
                .routes
                .iter()
                .map(|route| NodeDocsRoute {
                    id: &route.id,
                    file: &route.file,
                    title: &route.title,
                    description: &route.description,
                    kind: &route.kind,
                    group: &route.group,
                    group_id: &route.group_id,
                    section: &route.section,
                    section_id: &route.section_id,
                    slug: &route.slug,
                    status: &route.status,
                    draft: route.draft,
                    hidden: route.hidden,
                    headings: route
                        .headings
                        .iter()
                        .map(|heading| NodeDocsHeading {
                            depth: heading.depth,
                            title: &heading.title,
                            id: &heading.id,
                        })
                        .collect(),
                })
                .collect(),
            mode: value.mode.as_str(),
            demos: value
                .demos
                .iter()
                .map(|demo| NodeDocsDemo {
                    id: &demo.id,
                    title: &demo.title,
                    group: &demo.group,
                    component: &demo.component,
                    order: demo.order,
                    control_count: demo.control_count,
                    warnings: &demo.warnings,
                })
                .collect(),
            workspaces: value
                .workspaces
                .iter()
                .map(NodeDocsWorkspaceBuildInfo::from)
                .collect(),
        }
    }
}

fn node_docs_result_value(result: &wake_app::DocsBuildResult) -> Result<Value, WakeError> {
    serde_json::to_value(NodeDocsBuildResult::from(result))
        .map_err(|error| WakeError::new("WAKE_INTERNAL", error.to_string()))
}

fn push_json_pointer_segment(pointer: &mut String, segment: &str) {
    pointer.push('/');
    for character in segment.chars() {
        match character {
            '~' => pointer.push_str("~0"),
            '/' => pointer.push_str("~1"),
            character => pointer.push(character),
        }
    }
}

fn explicit_null_pointer(value: &Value, pointer: &mut String) -> Option<String> {
    match value {
        Value::Null => Some(if pointer.is_empty() {
            "<root>".to_string()
        } else {
            pointer.clone()
        }),
        Value::Array(values) => {
            for (index, value) in values.iter().enumerate() {
                let mark = pointer.len();
                push_json_pointer_segment(pointer, &index.to_string());
                if let Some(pointer) = explicit_null_pointer(value, pointer) {
                    return Some(pointer);
                }
                pointer.truncate(mark);
            }
            None
        }
        Value::Object(fields) => {
            for (name, value) in fields {
                let mark = pointer.len();
                push_json_pointer_segment(pointer, name);
                if let Some(pointer) = explicit_null_pointer(value, pointer) {
                    return Some(pointer);
                }
                pointer.truncate(mark);
            }
            None
        }
        Value::Bool(_) | Value::Number(_) | Value::String(_) => None,
    }
}

fn deserialize_node_request<T>(json: &str) -> Result<T, String>
where
    T: serde::de::DeserializeOwned,
{
    let value = serde_json::from_str::<Value>(json).map_err(|error| error.to_string())?;
    if let Some(pointer) = explicit_null_pointer(&value, &mut String::new()) {
        return Err(format!(
            "explicit null is not allowed in a Node request at {pointer}; omit the field to use its default"
        ));
    }
    serde_json::from_value(value).map_err(|error| error.to_string())
}

fn parse_optional_node_request<T>(
    json: Option<String>,
    error_code: &'static str,
) -> Result<T, WakeError>
where
    T: serde::de::DeserializeOwned + Default,
{
    match json {
        Some(json) => {
            deserialize_node_request(&json).map_err(|error| WakeError::new(error_code, error))
        }
        None => Ok(T::default()),
    }
}

#[derive(Debug)]
struct FederationEnabledLiteral;

impl<'de> Deserialize<'de> for FederationEnabledLiteral {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        if bool::deserialize(deserializer)? {
            Ok(Self)
        } else {
            Err(serde::de::Error::custom("expected enabled to be true"))
        }
    }
}

#[derive(Debug, Default)]
enum FederationDisabledField {
    #[default]
    Missing,
    False,
}

impl<'de> Deserialize<'de> for FederationDisabledField {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        if bool::deserialize(deserializer)? {
            Err(serde::de::Error::custom("expected enabled to be false"))
        } else {
            Ok(Self::False)
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawNodeFederationOptions {
    Enabled(RawNodeFederationEnabledOptions),
    Disabled(RawNodeFederationDisabledOptions),
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RawNodeFederationEnabledOptions {
    enabled: FederationEnabledLiteral,
    name: String,
    #[serde(default)]
    remotes: BTreeMap<String, RawNodeFederationRemoteOptions>,
    #[serde(default)]
    exposes: BTreeMap<String, RawNodeFederationExposeOptions>,
    #[serde(default)]
    shared: BTreeMap<String, RawNodeFederationSharedOptions>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RawNodeFederationDisabledOptions {
    #[serde(default)]
    enabled: FederationDisabledField,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RawNodeFederationRemoteOptions {
    manifest_url: String,
    #[serde(default)]
    allowed_origins: Vec<String>,
    #[serde(default = "node_default_true")]
    dev_follow: bool,
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum RawNodeFederationExposeMode {
    #[default]
    Generic,
    HostRendered,
    Isolated,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum RawNodeFederationShadowMode {
    None,
    Open,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RawNodeFederationExposeOptions {
    entry: String,
    #[serde(default)]
    mode: RawNodeFederationExposeMode,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    shadow: Option<RawNodeFederationShadowMode>,
    #[serde(default)]
    allow_global_css: bool,
}

fn node_default_scope() -> String {
    "default".to_string()
}

const fn node_default_true() -> bool {
    true
}

#[derive(Debug, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
struct RawNodeFederationSharedOptions {
    scope: String,
    required_version: Option<String>,
    singleton: bool,
    strict: bool,
    fallback: bool,
    coherence_group: Option<String>,
    owner: Option<String>,
}

impl Default for RawNodeFederationSharedOptions {
    fn default() -> Self {
        Self {
            scope: node_default_scope(),
            required_version: None,
            singleton: false,
            strict: false,
            fallback: true,
            coherence_group: None,
            owner: None,
        }
    }
}

impl RawNodeFederationOptions {
    fn into_contract(self) -> wake_app::FederationOptions {
        match self {
            Self::Disabled(RawNodeFederationDisabledOptions { enabled }) => {
                let _ = enabled;
                wake_app::FederationOptions::default()
            }
            Self::Enabled(RawNodeFederationEnabledOptions {
                enabled,
                name,
                remotes,
                exposes,
                shared,
            }) => {
                let _ = enabled;
                wake_app::FederationOptions {
                    enabled: true,
                    name: wake_app::ContainerName::new(name),
                    remotes: remotes
                        .into_iter()
                        .map(|(name, remote)| {
                            (wake_app::ContainerName::new(name), remote.into_contract())
                        })
                        .collect(),
                    exposes: exposes
                        .into_iter()
                        .map(|(key, expose)| {
                            (wake_app::ExposeKey::new(key), expose.into_contract())
                        })
                        .collect(),
                    shared: shared
                        .into_iter()
                        .map(|(name, shared)| (name, shared.into_contract()))
                        .collect(),
                }
            }
        }
    }
}

impl RawNodeFederationRemoteOptions {
    fn into_contract(self) -> wake_app::RemoteConfig {
        wake_app::RemoteConfig {
            manifest_url: self.manifest_url,
            allowed_origins: self.allowed_origins,
            dev_follow: self.dev_follow,
        }
    }
}

impl From<RawNodeFederationExposeMode> for wake_app::ExposeMode {
    fn from(value: RawNodeFederationExposeMode) -> Self {
        match value {
            RawNodeFederationExposeMode::Generic => Self::Generic,
            RawNodeFederationExposeMode::HostRendered => Self::HostRendered,
            RawNodeFederationExposeMode::Isolated => Self::Isolated,
        }
    }
}

impl From<RawNodeFederationShadowMode> for wake_app::ShadowMode {
    fn from(value: RawNodeFederationShadowMode) -> Self {
        match value {
            RawNodeFederationShadowMode::None => Self::None,
            RawNodeFederationShadowMode::Open => Self::Open,
        }
    }
}

impl RawNodeFederationExposeOptions {
    fn into_contract(self) -> wake_app::ExposeConfig {
        let mode = self.mode.into();
        let scope = self.scope.unwrap_or_else(|| match self.mode {
            RawNodeFederationExposeMode::Generic => node_default_scope(),
            RawNodeFederationExposeMode::HostRendered | RawNodeFederationExposeMode::Isolated => {
                String::new()
            }
        });
        let shadow = self.shadow.map(Into::into).unwrap_or(match self.mode {
            RawNodeFederationExposeMode::Isolated => wake_app::ShadowMode::Open,
            RawNodeFederationExposeMode::Generic | RawNodeFederationExposeMode::HostRendered => {
                wake_app::ShadowMode::None
            }
        });
        wake_app::ExposeConfig {
            entry: self.entry,
            mode,
            scope,
            shadow,
            allow_global_css: self.allow_global_css,
        }
    }
}

impl RawNodeFederationSharedOptions {
    fn into_contract(self) -> wake_app::SharedConfig {
        wake_app::SharedConfig {
            scope: self.scope,
            required_version: self.required_version,
            singleton: self.singleton,
            strict: self.strict,
            fallback: self.fallback,
            coherence_group: self.coherence_group,
            owner: self.owner.map(wake_app::ContainerName::new),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
struct RawBuildOptions {
    cwd: Option<String>,
    config_path: Option<String>,
    entry: Option<String>,
    outdir: Option<String>,
    cache: bool,
    source_map: bool,
    federation: Option<RawNodeFederationOptions>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
struct RawFederationProjectOptions {
    cwd: Option<String>,
}

impl RawFederationProjectOptions {
    fn parse(value: Option<String>) -> Result<Self, WakeError> {
        parse_optional_node_request(value, "WAKE_CONFIG")
    }

    fn start(self) -> PathBuf {
        self.cwd.map(PathBuf::from).unwrap_or_else(|| ".".into())
    }
}

impl RawBuildOptions {
    fn parse(value: Option<String>) -> Result<Self, WakeError> {
        parse_optional_node_request(value, "WAKE_CONFIG")
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
            federation: self.federation.map(RawNodeFederationOptions::into_contract),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
struct RawTestOptions {
    root: Option<PathBuf>,
    patterns: Vec<String>,
    name_pattern: Option<String>,
    projects: Vec<String>,
    environment: Option<RawNodeTestEnvironment>,
    changed: bool,
    related: Vec<PathBuf>,
    coverage: bool,
    update_snapshots: Option<RawNodeSnapshotUpdateMode>,
    serial: bool,
    workers: Option<RawNodeTestWorkers>,
    bail: Option<u32>,
    shard: Option<String>,
    seed: Option<String>,
    shuffle: bool,
    allow_no_tests: bool,
    browser_path: Option<PathBuf>,
    headful: bool,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum RawNodeTestEnvironment {
    Auto,
    Dom,
    Browser,
}

impl RawNodeTestEnvironment {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Dom => "dom",
            Self::Browser => "browser",
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum RawNodeSnapshotUpdateMode {
    None,
    New,
    All,
}

impl RawNodeSnapshotUpdateMode {
    const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::New => "new",
            Self::All => "all",
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawNodeTestWorkers {
    Count(usize),
    Text(String),
}

impl RawNodeTestWorkers {
    fn into_app(self) -> Result<wake_app::WorkerOverride, WakeError> {
        match self {
            Self::Count(0) => Err(WakeError::new(
                "WAKE_TEST_CONFIG",
                "workers must be a positive integer, 'auto', or a percentage from '1%' to '100%'",
            )),
            Self::Count(count) => Ok(wake_app::WorkerOverride::Count(count)),
            Self::Text(value) if value == "auto" => Ok(wake_app::WorkerOverride::Text(value)),
            Self::Text(value) => {
                let percentage = value
                    .strip_suffix('%')
                    .and_then(|digits| digits.parse::<usize>().ok())
                    .filter(|percentage| (1..=100).contains(percentage))
                    .filter(|percentage| value == format!("{percentage}%"));
                if percentage.is_some() {
                    Ok(wake_app::WorkerOverride::Text(value))
                } else {
                    Err(WakeError::new(
                        "WAKE_TEST_CONFIG",
                        "workers must be a positive integer, 'auto', or a percentage from '1%' to '100%'",
                    ))
                }
            }
        }
    }
}

impl RawTestOptions {
    fn into_app(self) -> Result<wake_app::TestOptions, WakeError> {
        Ok(wake_app::TestOptions {
            root: self.root,
            patterns: self.patterns,
            name_pattern: self.name_pattern,
            projects: self.projects,
            environment: self
                .environment
                .map(|environment| environment.as_str().to_string()),
            // Watching is an operation on Node's persistent TestContext, never a request flag.
            watch: false,
            changed: self.changed,
            related: self.related,
            coverage: self.coverage,
            update_snapshots: self.update_snapshots.map(|mode| mode.as_str().to_string()),
            serial: self.serial,
            workers: self.workers.map(RawNodeTestWorkers::into_app).transpose()?,
            bail: self.bail,
            shard: self.shard,
            seed: self.seed,
            shuffle: self.shuffle,
            // Reporter selection and output presentation belong to the CLI, not the Node request.
            reporter: None,
            output: None,
            allow_no_tests: self.allow_no_tests,
            browser_path: self.browser_path,
            headful: self.headful,
        })
    }
}

fn parse_test_options(options_json: Option<String>) -> Result<wake_app::TestOptions, WakeError> {
    parse_optional_node_request::<RawTestOptions>(options_json, "WAKE_TEST_CONFIG")?.into_app()
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
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
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
struct RawGenerateCssTokenOptions {
    cwd: Option<String>,
    config_path: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
struct RawGenerateDocgenOptions {
    cwd: Option<String>,
    entry: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
struct RawLibraryBuildOptions {
    cwd: Option<String>,
    entry: Option<String>,
}

impl RawGenerateCssTokenOptions {
    fn parse(value: Option<String>) -> Result<Self, WakeError> {
        parse_optional_node_request(value, "WAKE_CONFIG")
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
        parse_optional_node_request(value, "WAKE_CONFIG")
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
        parse_optional_node_request(value, "WAKE_CONFIG")
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
        parse_optional_node_request(value, "WAKE_CONFIG")
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
            node_result_value(&result, &result.files)
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
            node_result_value(&result, &result.files)
        }),
        signal,
    )
}

#[napi(js_name = "initializeFederation")]
pub fn native_initialize_federation(
    options_json: Option<String>,
    signal: Option<AbortSignal>,
) -> AsyncTask<JsonTask> {
    async_json(
        JsonTask::new(move || {
            let start = RawFederationProjectOptions::parse(options_json)?.start();
            let result = wake_app::initialize_federation_types(&start)?;
            serde_json::to_value(result)
                .map_err(|error| WakeError::new("WAKE_INTERNAL", error.to_string()))
        }),
        signal,
    )
}

#[napi(js_name = "generateFederationLock")]
pub fn native_generate_federation_lock(
    options_json: Option<String>,
    signal: Option<AbortSignal>,
) -> AsyncTask<JsonTask> {
    async_json(
        JsonTask::new(move || {
            let start = RawFederationProjectOptions::parse(options_json)?.start();
            let (project_root, lock) = wake_app::generate_project_federation_lock(&start)?;
            let lock_path = project_root.join("wake-federation.lock");
            Ok(json!({
                "projectRoot": project_root,
                "lockPath": lock_path,
                "remotes": lock.remotes.len(),
                "lock": lock,
            }))
        }),
        signal,
    )
}

#[napi(js_name = "runTests")]
pub fn native_run_tests(
    options_json: Option<String>,
    signal: Option<AbortSignal>,
    host_path: Option<String>,
) -> AsyncTask<JsonTask> {
    let cancellation = CancellationToken::default();
    if let Some(signal) = &signal {
        let cancellation = cancellation.clone();
        signal.on_abort(move || cancellation.cancel());
    }
    async_json(
        JsonTask::new(move || {
            let options = parse_test_options(options_json)?;
            let host_path = host_path.map(PathBuf::from);
            let result =
                wake_app::run_tests_with_host(options, host_path.as_deref(), &cancellation)?;
            serde_json::to_value(result)
                .map_err(|error| WakeError::new("WAKE_INTERNAL", error.to_string()))
        }),
        signal,
    )
}

#[derive(Debug)]
enum RawNodeWatchControl {
    All,
    Failed,
    Path { pattern: String },
    Name { pattern: String },
    UpdateSnapshots,
    Rerun,
}

#[derive(Debug, Default)]
enum RawNodeWatchPattern {
    #[default]
    Missing,
    Present(String),
}

impl<'de> Deserialize<'de> for RawNodeWatchPattern {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        String::deserialize(deserializer).map(Self::Present)
    }
}

impl<'de> Deserialize<'de> for RawNodeWatchControl {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct RawNodeWatchControlFields {
            #[serde(rename = "type")]
            kind: String,
            #[serde(default)]
            pattern: RawNodeWatchPattern,
        }

        let raw = RawNodeWatchControlFields::deserialize(deserializer)?;
        match (raw.kind.as_str(), raw.pattern) {
            ("all", RawNodeWatchPattern::Missing) => Ok(Self::All),
            ("failed", RawNodeWatchPattern::Missing) => Ok(Self::Failed),
            ("path", RawNodeWatchPattern::Present(pattern)) => Ok(Self::Path { pattern }),
            ("name", RawNodeWatchPattern::Present(pattern)) => Ok(Self::Name { pattern }),
            ("updateSnapshots", RawNodeWatchPattern::Missing) => Ok(Self::UpdateSnapshots),
            ("rerun", RawNodeWatchPattern::Missing) => Ok(Self::Rerun),
            ("path" | "name", RawNodeWatchPattern::Missing) => {
                Err(serde::de::Error::missing_field("pattern"))
            }
            ("all" | "failed" | "updateSnapshots" | "rerun", RawNodeWatchPattern::Present(_)) => {
                Err(serde::de::Error::unknown_field("pattern", &["type"]))
            }
            (kind, _) => Err(serde::de::Error::unknown_variant(
                kind,
                &["all", "failed", "path", "name", "updateSnapshots", "rerun"],
            )),
        }
    }
}

impl From<RawNodeWatchControl> for wake_app::TestWatchControl {
    fn from(value: RawNodeWatchControl) -> Self {
        match value {
            RawNodeWatchControl::All => Self::All,
            RawNodeWatchControl::Failed => Self::Failed,
            RawNodeWatchControl::Path { pattern } => Self::Path { pattern },
            RawNodeWatchControl::Name { pattern } => Self::Name { pattern },
            RawNodeWatchControl::UpdateSnapshots => Self::UpdateSnapshots,
            RawNodeWatchControl::Rerun => Self::Rerun,
        }
    }
}

#[napi]
pub struct NativeTestContext {
    resource: Arc<TestContextResource>,
}

#[napi]
impl NativeTestContext {
    #[napi]
    pub fn run(&self, signal: Option<AbortSignal>) -> AsyncTask<JsonTask> {
        let resource = Arc::clone(&self.resource);
        let cancellation = CancellationToken::default();
        if let Some(signal) = &signal {
            let cancellation = cancellation.clone();
            signal.on_abort(move || cancellation.cancel());
        }
        async_json(
            JsonTask::new(move || {
                if resource.closed.load(Ordering::Acquire) {
                    return Err(test_context_closed_error());
                }
                let result = resource.run(&cancellation)?;
                serde_json::to_value(result)
                    .map_err(|error| WakeError::new("WAKE_INTERNAL", error.to_string()))
            }),
            signal,
        )
    }

    #[napi(js_name = "startWatch")]
    pub fn start_watch(&self) -> napi::Result<()> {
        self.resource.start_watch().map_err(napi_wake_error)
    }

    #[napi(js_name = "stopWatch")]
    pub fn stop_watch(&self) -> napi::Result<()> {
        self.resource.stop_watch().map_err(napi_wake_error)
    }

    #[napi(js_name = "watchControl")]
    pub fn watch_control(&self, control_json: String) -> napi::Result<()> {
        let control = deserialize_node_request::<RawNodeWatchControl>(&control_json)
            .map_err(|error| napi::Error::from_reason(format!("invalid watch control: {error}")))?;
        self.resource
            .watch_control(control.into())
            .map_err(napi_wake_error)
    }

    #[napi(js_name = "eventsJson")]
    pub fn events_json(&self) -> napi::Result<String> {
        serde_json::to_value(self.resource.drain_events().map_err(napi_wake_error)?)
            .map_err(|error| napi::Error::from_reason(error.to_string()))
            .and_then(value_string)
    }

    #[napi]
    pub fn close(&self) -> AsyncTask<JsonTask> {
        let resource = Arc::clone(&self.resource);
        async_json(
            JsonTask::new(move || {
                resource.close()?;
                Ok(Value::Null)
            }),
            None,
        )
    }

    #[napi(getter)]
    pub fn closed(&self) -> bool {
        self.resource.closed.load(Ordering::Acquire)
    }

    #[napi(getter)]
    pub fn watching(&self) -> bool {
        self.resource.watching()
    }
}

#[napi(js_name = "createTestContext")]
pub fn create_test_context(
    options_json: Option<String>,
    host_path: Option<String>,
) -> napi::Result<NativeTestContext> {
    let options = parse_test_options(options_json).map_err(napi_wake_error)?;
    let resources = current_env_resources();
    let resource = Arc::new(TestContextResource {
        options,
        host_path: host_path.map(PathBuf::from),
        session: Mutex::new(None),
        events: Mutex::new(Vec::new()),
        event_error: Mutex::new(None),
        active_cancellation: Mutex::new(None),
        running: AtomicBool::new(false),
        watching: AtomicBool::new(false),
        closed: AtomicBool::new(false),
    });
    resources
        .test_contexts
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .push(Arc::downgrade(&resource));
    Ok(NativeTestContext { resource })
}

impl Drop for NativeTestContext {
    fn drop(&mut self) {
        let _ = self.resource.close();
    }
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
            node_result_value(&result, &result.files)
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
            node_result_value(&result, &result.files)
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
            node_result_value(&result, &result.files)
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
                node_result_value(&result, &result.files)
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
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
struct RawApplicationDevServerOptions {
    cwd: Option<String>,
    config_path: Option<String>,
    entry: Option<String>,
    host: Option<String>,
    port: Option<u16>,
    open: Option<bool>,
    federation: Option<RawNodeFederationOptions>,
}

impl RawApplicationDevServerOptions {
    fn parse(value: Option<String>) -> Result<Self, WakeError> {
        parse_optional_node_request(value, "WAKE_CONFIG")
    }

    fn into_app(self) -> wake_app::DevServerOptions {
        wake_app::DevServerOptions {
            project: wake_app::ProjectOptions {
                cwd: self.cwd.map(PathBuf::from),
                config_path: self.config_path.map(PathBuf::from),
            },
            entry: self.entry.map(PathBuf::from),
            host: self.host,
            port: self.port,
            open: self.open,
            federation: self.federation.map(RawNodeFederationOptions::into_contract),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
struct RawDocsDevServerOptions {
    cwd: Option<String>,
    config_path: Option<String>,
    host: Option<String>,
    port: Option<u16>,
    open: Option<bool>,
    mode: Option<wake_app::DocsMode>,
}

impl RawDocsDevServerOptions {
    fn parse(value: Option<String>) -> Result<Self, WakeError> {
        parse_optional_node_request(value, "WAKE_CONFIG")
    }

    fn into_app(self) -> (wake_app::DevServerOptions, wake_app::DocsMode) {
        let mode = self.mode.unwrap_or_default();
        (
            wake_app::DevServerOptions {
                project: wake_app::ProjectOptions {
                    cwd: self.cwd.map(PathBuf::from),
                    config_path: self.config_path.map(PathBuf::from),
                },
                entry: None,
                host: self.host,
                port: self.port,
                open: self.open,
                federation: None,
            },
            mode,
        )
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
            match catch_unwind(AssertUnwindSafe(|| match kind {
                ServerKind::Application => wake_app::start_dev_server(
                    RawApplicationDevServerOptions::parse(options)?.into_app(),
                ),
                ServerKind::Documentation => {
                    let (options, docs_mode) = RawDocsDevServerOptions::parse(options)?.into_app();
                    wake_app::start_docs_dev_server_with_mode(options, docs_mode)
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
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
struct RawDocsOptions {
    cwd: Option<String>,
    config_path: Option<String>,
    outdir: Option<String>,
    base_path: Option<String>,
    mode: Option<wake_app::DocsMode>,
}

impl RawDocsOptions {
    fn parse(value: Option<String>) -> Result<Self, WakeError> {
        parse_optional_node_request(value, "WAKE_CONFIG")
    }
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
            let options = RawDocsOptions::parse(options_json)?;
            let docs_mode = options.mode.unwrap_or_default();
            let result = wake_app::build_docs_with_mode(
                wake_app::DocsBuildOptions {
                    project: wake_app::ProjectOptions {
                        cwd: options.cwd.map(PathBuf::from),
                        config_path: options.config_path.map(PathBuf::from),
                    },
                    outdir: options.outdir.map(PathBuf::from),
                    base_path: options.base_path,
                    presentation: None,
                },
                docs_mode,
                &cancellation,
            )?;
            node_docs_result_value(&result)
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
            let source = SourceFile::new("<input>", parsed.source.clone());
            let diagnostics = parsed
                .output
                .diagnostics
                .iter()
                .map(|diagnostic| {
                    wake_app::DiagnosticInfo::from_diagnostic(diagnostic, Some(&source))
                })
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
            let source = SourceFile::new("<input>", parsed.source.clone());
            let code = parsed
                .output
                .module
                .with_ast(|program| wake_ecma_codegen::codegen(program, &parsed.interner));
            Ok(json!({
                "code": code,
                "diagnostics": parsed.output.diagnostics.iter().map(|diagnostic| wake_app::DiagnosticInfo::from_diagnostic(diagnostic, Some(&source))).collect::<Vec<_>>()
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
        let source_file = SourceFile::new("<input>", source.clone());
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
            "diagnostics": diagnostics.iter().map(|diagnostic| wake_app::DiagnosticInfo::from_diagnostic(diagnostic, Some(&source_file))).collect::<Vec<_>>()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_file_kinds_have_explicit_node_wire_mappings() {
        let cases = [
            (wake_app::OutputFileKind::Asset, "asset"),
            (wake_app::OutputFileKind::Chunk, "chunk"),
            (wake_app::OutputFileKind::Css, "css"),
            (wake_app::OutputFileKind::Declaration, "declaration"),
            (wake_app::OutputFileKind::Entry, "entry"),
            (
                wake_app::OutputFileKind::FederationBootstrap,
                "federation-bootstrap",
            ),
            (
                wake_app::OutputFileKind::FederationChunk,
                "federation-chunk",
            ),
            (
                wake_app::OutputFileKind::FederationEntry,
                "federation-entry",
            ),
            (
                wake_app::OutputFileKind::FederationManifest,
                "federation-manifest",
            ),
            (
                wake_app::OutputFileKind::FederationShared,
                "federation-shared",
            ),
            (wake_app::OutputFileKind::FederationTypes, "types"),
            (wake_app::OutputFileKind::Html, "html"),
            (wake_app::OutputFileKind::Manifest, "manifest"),
            (wake_app::OutputFileKind::SourceMap, "map"),
        ];

        for (kind, expected) in cases {
            assert_eq!(node_output_file_kind(kind), expected);
            assert_eq!(kind.as_str(), expected);
        }
    }

    #[test]
    fn node_bundle_request_reaches_the_output_collision_error() {
        struct Fixture(PathBuf);

        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }

        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let fixture = Fixture(std::env::temp_dir().join(format!(
            "wake-node-output-collision-{}-{unique}",
            std::process::id()
        )));
        std::fs::create_dir_all(fixture.0.join("src")).unwrap();
        std::fs::write(
            fixture.0.join("wake.config.toml"),
            "[html]\nentry = \"src/index.js\"\n",
        )
        .unwrap();
        let entry = fixture.0.join("src/index.js");
        let original = b"export const value = 42;\n";
        std::fs::write(&entry, original).unwrap();
        let options = RawBundleOptions::parse(Some(
            json!({
                "cwd": fixture.0.as_path(),
                "entry": "src/index.js",
                "outfile": "src/index.js",
            })
            .to_string(),
        ))
        .unwrap()
        .into_app()
        .unwrap();

        let error = wake_app::bundle(options, &CancellationToken::default()).unwrap_err();

        assert_eq!(error.code, "WAKE_OUTPUT_COLLISION");
        assert_eq!(std::fs::read(entry).unwrap(), original);
    }

    #[test]
    fn programmatic_federation_options_reach_the_application_boundary() {
        let build = RawBuildOptions::parse(Some(
            r#"{"federation":{"enabled":true,"name":"shell","remotes":{"catalog":{"manifestUrl":"https://catalog.test/wake-federation.json"}}}}"#.to_owned(),
        ))
        .unwrap()
        .into_app(true);
        let federation = build.federation.expect("build federation override");
        assert_eq!(federation.name.as_str(), "shell");
        assert!(federation.remotes.contains_key(&"catalog".into()));

        let dev = RawApplicationDevServerOptions::parse(Some(
            r#"{"federation":{"enabled":true,"name":"shell"}}"#.to_owned(),
        ))
        .unwrap()
        .into_app();
        assert_eq!(
            dev.federation
                .expect("dev federation override")
                .name
                .as_str(),
            "shell"
        );
    }

    #[test]
    fn node_numeric_options_enforce_rust_integer_ranges_at_deserialization() {
        let application =
            RawApplicationDevServerOptions::parse(Some(r#"{"port":65535}"#.to_owned())).unwrap();
        assert_eq!(application.port, Some(u16::MAX));
        let docs = RawDocsDevServerOptions::parse(Some(r#"{"port":65535}"#.to_owned())).unwrap();
        assert_eq!(docs.port, Some(u16::MAX));
        let tests = parse_test_options(Some(r#"{"bail":4294967295}"#.to_owned())).unwrap();
        assert_eq!(tests.bail, Some(u32::MAX));

        for rejected in [r#"{"port":65536}"#, r#"{"port":-1}"#, r#"{"port":1.5}"#] {
            let error =
                RawApplicationDevServerOptions::parse(Some(rejected.to_owned())).unwrap_err();
            assert_eq!(error.code, "WAKE_CONFIG", "{rejected}");
            let error = RawDocsDevServerOptions::parse(Some(rejected.to_owned())).unwrap_err();
            assert_eq!(error.code, "WAKE_CONFIG", "{rejected}");
        }
        for rejected in [
            r#"{"bail":4294967296}"#,
            r#"{"bail":-1}"#,
            r#"{"bail":1.5}"#,
        ] {
            let error = parse_test_options(Some(rejected.to_owned())).unwrap_err();
            assert_eq!(error.code, "WAKE_TEST_CONFIG", "{rejected}");
        }
    }

    #[test]
    fn node_federation_input_is_an_exact_camel_case_closed_union() {
        let federation = serde_json::from_str::<RawNodeFederationOptions>(
            r#"{
                "enabled": true,
                "name": "shell",
                "remotes": {
                    "catalog": {
                        "manifestUrl": "https://catalog.test/wake-federation.json",
                        "allowedOrigins": ["https://catalog.test"],
                        "devFollow": false
                    }
                },
                "exposes": {
                    "./Button": {
                        "entry": "src/button.tsx",
                        "mode": "host-rendered",
                        "scope": "react",
                        "shadow": "none",
                        "allowGlobalCss": true
                    }
                },
                "shared": {
                    "react": {
                        "scope": "react",
                        "requiredVersion": "^19.2.8",
                        "singleton": true,
                        "strict": true,
                        "fallback": false,
                        "coherenceGroup": "react",
                        "owner": "shell"
                    }
                }
            }"#,
        )
        .unwrap()
        .into_contract();
        assert!(federation.enabled);
        assert_eq!(federation.name.as_str(), "shell");
        assert!(!federation.remotes[&"catalog".into()].dev_follow);
        assert!(federation.exposes[&"./Button".into()].allow_global_css);
        assert_eq!(
            federation.shared["react"].required_version.as_deref(),
            Some("^19.2.8")
        );

        for disabled in [r#"{}"#, r#"{"enabled":false}"#] {
            let federation = serde_json::from_str::<RawNodeFederationOptions>(disabled)
                .unwrap()
                .into_contract();
            assert_eq!(federation, wake_app::FederationOptions::default());
        }

        for rejected in [
            r#"{"enabled":true}"#,
            r#"{"enabled":null}"#,
            r#"{"enabled":false,"name":"shell"}"#,
            r#"{"name":"shell"}"#,
            r#"{"enabled":true,"name":"shell","typo":true}"#,
            r#"{"enabled":true,"name":"shell","remotes":{"catalog":{"manifest_url":"https://catalog.test/wake-federation.json"}}}"#,
            r#"{"enabled":true,"name":"shell","remotes":{"catalog":{"manifestUrl":"https://catalog.test/wake-federation.json","allowed_origins":[]}}}"#,
            r#"{"enabled":true,"name":"shell","remotes":{"catalog":{"manifestUrl":"https://catalog.test/wake-federation.json","dev_follow":true}}}"#,
            r#"{"enabled":true,"name":"shell","remotes":{"catalog":{"manifestUrl":"https://catalog.test/wake-federation.json","typo":true}}}"#,
            r#"{"enabled":true,"name":"shell","exposes":{"Button":{"entry":"src/button.tsx","allow_global_css":true}}}"#,
            r#"{"enabled":true,"name":"shell","exposes":{"Button":{"entry":"src/button.tsx","typo":true}}}"#,
            r#"{"enabled":true,"name":"shell","shared":{"react":{"required_version":"^19"}}}"#,
            r#"{"enabled":true,"name":"shell","shared":{"react":{"coherence_group":"react"}}}"#,
            r#"{"enabled":true,"name":"shell","shared":{"react":{"typo":true}}}"#,
        ] {
            assert!(
                serde_json::from_str::<RawNodeFederationOptions>(rejected).is_err(),
                "Node federation unexpectedly accepted {rejected}"
            );
        }
    }

    #[test]
    fn node_request_boundary_rejects_explicit_null_and_preserves_missing() {
        fn assert_rejected<T>(result: Result<T, WakeError>, expected_code: &str) {
            let error = match result {
                Ok(_) => panic!("Node request unexpectedly accepted explicit null"),
                Err(error) => error,
            };
            assert_eq!(error.code, expected_code);
            assert!(error.message.contains("explicit null"), "{error}");
        }

        assert!(RawBuildOptions::parse(None).unwrap().federation.is_none());
        assert!(
            RawBuildOptions::parse(Some("{}".to_owned()))
                .unwrap()
                .federation
                .is_none()
        );
        let tests = parse_test_options(None).unwrap();
        assert!(tests.environment.is_none());
        assert!(tests.update_snapshots.is_none());
        assert!(tests.workers.is_none());

        let pointer_error =
            deserialize_node_request::<Value>(r#"{"nested/key":{"~items":[0,null]}}"#).unwrap_err();
        assert!(
            pointer_error.contains("/nested~1key/~0items/1"),
            "{pointer_error}"
        );

        for rejected in [
            r#"null"#,
            r#"{"cwd":null}"#,
            r#"{"federation":null}"#,
            r#"{"federation":{"enabled":true,"name":"shell","remotes":{"catalog":{"manifestUrl":null}}}}"#,
            r#"{"federation":{"enabled":true,"name":"shell","remotes":{"catalog":{"manifestUrl":"https://catalog.test/wake-federation.json","allowedOrigins":[null]}}}}"#,
            r#"{"federation":{"enabled":true,"name":"shell","exposes":{"Button":{"entry":"src/button.tsx","scope":null}}}}"#,
            r#"{"federation":{"enabled":true,"name":"shell","exposes":{"Button":{"entry":"src/button.tsx","shadow":null}}}}"#,
            r#"{"federation":{"enabled":true,"name":"shell","shared":{"react":{"requiredVersion":null}}}}"#,
            r#"{"federation":{"enabled":true,"name":"shell","shared":{"react":{"coherenceGroup":null}}}}"#,
            r#"{"federation":{"enabled":true,"name":"shell","shared":{"react":{"owner":null}}}}"#,
        ] {
            assert_rejected(
                RawBuildOptions::parse(Some(rejected.to_owned())),
                "WAKE_CONFIG",
            );
        }

        for rejected in [
            r#"{"environment":null}"#,
            r#"{"updateSnapshots":null}"#,
            r#"{"workers":null}"#,
            r#"{"related":[null]}"#,
        ] {
            assert_rejected(
                parse_test_options(Some(rejected.to_owned())),
                "WAKE_TEST_CONFIG",
            );
        }

        assert_rejected(
            RawFederationProjectOptions::parse(Some(r#"{"cwd":null}"#.to_owned())),
            "WAKE_CONFIG",
        );
        assert_rejected(
            RawBundleOptions::parse(Some(r#"{"cwd":null}"#.to_owned())),
            "WAKE_CONFIG",
        );
        assert_rejected(
            RawGenerateCssTokenOptions::parse(Some(r#"{"cwd":null}"#.to_owned())),
            "WAKE_CONFIG",
        );
        assert_rejected(
            RawGenerateDocgenOptions::parse(Some(r#"{"cwd":null}"#.to_owned())),
            "WAKE_CONFIG",
        );
        assert_rejected(
            RawLibraryBuildOptions::parse(Some(r#"{"cwd":null}"#.to_owned())),
            "WAKE_CONFIG",
        );
        assert_rejected(
            RawApplicationDevServerOptions::parse(Some(r#"{"cwd":null}"#.to_owned())),
            "WAKE_CONFIG",
        );
        assert_rejected(
            RawDocsDevServerOptions::parse(Some(r#"{"cwd":null}"#.to_owned())),
            "WAKE_CONFIG",
        );
        assert_rejected(
            RawDocsOptions::parse(Some(r#"{"cwd":null}"#.to_owned())),
            "WAKE_CONFIG",
        );
        assert!(
            deserialize_node_request::<RawNodeWatchControl>(r#"{"type":"all","pattern":null}"#,)
                .is_err()
        );
    }

    #[test]
    fn federation_control_options_are_a_closed_project_boundary() {
        assert_eq!(
            RawFederationProjectOptions::parse(None).unwrap().start(),
            PathBuf::from(".")
        );
        assert_eq!(
            RawFederationProjectOptions::parse(Some(r#"{"cwd":"packages/shell"}"#.to_owned()))
                .unwrap()
                .start(),
            PathBuf::from("packages/shell")
        );
        let error = RawFederationProjectOptions::parse(Some(
            r#"{"cwd":".","configPath":"wake.config.toml"}"#.to_owned(),
        ))
        .unwrap_err();
        assert_eq!(error.code, "WAKE_CONFIG");
    }

    #[test]
    fn dev_server_rejects_fake_hmr_options() {
        for options in [
            r#"{"hmr":true}"#,
            r#"{"hot":true}"#,
            r#"{"liveReload":false}"#,
        ] {
            let error =
                RawApplicationDevServerOptions::parse(Some(options.to_owned())).unwrap_err();
            assert_eq!(error.code, "WAKE_CONFIG");
            assert!(
                error.message.contains("unknown field"),
                "{options}: {error}"
            );
        }
    }

    #[test]
    fn test_options_reject_unknown_fields_at_the_native_boundary() {
        let error = parse_test_options(Some(r#"{"runInBand":true}"#.to_string())).unwrap_err();
        assert_eq!(error.code, "WAKE_TEST_CONFIG");
    }

    #[test]
    fn node_watch_controls_are_closed_tagged_dtos() {
        for control in [
            r#"{"type":"all"}"#,
            r#"{"type":"failed"}"#,
            r#"{"type":"path","pattern":"src/button"}"#,
            r#"{"type":"name","pattern":"renders"}"#,
            r#"{"type":"updateSnapshots"}"#,
            r#"{"type":"rerun"}"#,
        ] {
            serde_json::from_str::<RawNodeWatchControl>(control).unwrap();
        }
        for rejected in [
            r#"{"type":"all","pattern":"unexpected"}"#,
            r#"{"type":"all","pattern":null}"#,
            r#"{"type":"path"}"#,
            r#"{"type":"path","pattern":"src/button","typo":true}"#,
            r#"{"type":"rerun","typo":true}"#,
            r#"{"type":"watchAll"}"#,
        ] {
            assert!(
                serde_json::from_str::<RawNodeWatchControl>(rejected).is_err(),
                "Node watch control unexpectedly accepted {rejected}"
            );
        }
    }

    fn sorted_object_keys(value: &Value) -> Vec<String> {
        let mut keys = value
            .as_object()
            .expect("serialized DTO must be an object")
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        keys.sort();
        keys
    }

    #[test]
    fn docs_result_uses_the_exact_node_owned_camel_case_shape() {
        let value = serde_json::to_value(NodeDocsBuildResult {
            success: true,
            module_count: 1,
            updated_module_count: 1,
            cached_module_count: 0,
            duration_ms: 1.25,
            output_dir: Some("dist"),
            code: Some("export {}"),
            files: vec![NodeOutputFile {
                path: "index.html",
                kind: "html",
                bytes: 42,
            }],
            diagnostics: &[],
            routes: vec![NodeDocsRoute {
                id: "guide",
                file: "guide.mdx",
                title: "Guide",
                description: "Description",
                kind: "guide",
                group: "Learn",
                group_id: "learn",
                section: "Start",
                section_id: "start",
                slug: "/guide/",
                status: "stable",
                draft: false,
                hidden: false,
                headings: vec![NodeDocsHeading {
                    depth: 2,
                    title: "Install",
                    id: "install",
                }],
            }],
            mode: "site",
            demos: vec![NodeDocsDemo {
                id: "button",
                title: "Button",
                group: "Inputs",
                component: "Button",
                order: 1,
                control_count: 2,
                warnings: &[],
            }],
            workspaces: vec![NodeDocsWorkspaceBuildInfo {
                name: "components",
                root: "packages/components",
                base_path: "/components/",
                mode: "components",
                presentation: "embedded",
                demos: 1,
            }],
        })
        .unwrap();

        assert_eq!(
            sorted_object_keys(&value),
            [
                "cachedModuleCount",
                "code",
                "demos",
                "diagnostics",
                "durationMs",
                "files",
                "mode",
                "moduleCount",
                "outputDir",
                "routes",
                "success",
                "updatedModuleCount",
                "workspaces",
            ]
        );
        let route = &value["routes"][0];
        assert_eq!(
            sorted_object_keys(route),
            [
                "description",
                "draft",
                "file",
                "group",
                "groupId",
                "headings",
                "hidden",
                "id",
                "kind",
                "section",
                "sectionId",
                "slug",
                "status",
                "title",
            ]
        );
        assert_eq!(
            sorted_object_keys(&route["headings"][0]),
            ["depth", "id", "title"]
        );
        assert_eq!(
            sorted_object_keys(&value["demos"][0]),
            [
                "component",
                "controlCount",
                "group",
                "id",
                "order",
                "title",
                "warnings",
            ]
        );
        assert_eq!(
            sorted_object_keys(&value["workspaces"][0]),
            ["basePath", "demos", "mode", "name", "presentation", "root"]
        );
        assert!(route.get("group_id").is_none());
        assert!(route.get("section_id").is_none());
        assert!(value["demos"][0].get("control_count").is_none());
    }

    fn assert_closed_options<T>(valid: &str, invalid: &[&str])
    where
        T: serde::de::DeserializeOwned,
    {
        serde_json::from_str::<T>(valid).expect("the complete legal option set must deserialize");
        for value in invalid {
            assert!(
                serde_json::from_str::<T>(value).is_err(),
                "closed options unexpectedly accepted {value}"
            );
        }
    }

    #[test]
    fn one_shot_option_dtos_are_closed_and_type_checked() {
        assert_closed_options::<RawBuildOptions>(
            r#"{"cwd":".","configPath":"wake.config.toml","entry":"src/index.ts","outdir":"dist","cache":true,"sourceMap":true,"federation":{"enabled":false}}"#,
            &[
                r#"{"sourceMaps":true}"#,
                r#"{"source_map":true}"#,
                r#"{"cache":"yes"}"#,
            ],
        );
        assert_closed_options::<RawBundleOptions>(
            r#"{"cwd":".","configPath":"wake.config.toml","entry":"src/index.ts","outfile":"dist/index.js","platform":"node","format":"cjs","target":"node22","external":["node:fs"],"minify":true,"sourceMap":true,"cache":true}"#,
            &[
                r#"{"outFile":"dist/index.js"}"#,
                r#"{"source_map":true}"#,
                r#"{"external":"node:fs"}"#,
            ],
        );
        assert_closed_options::<RawGenerateCssTokenOptions>(
            r#"{"cwd":".","configPath":"token.toml"}"#,
            &[
                r#"{"config":"token.toml"}"#,
                r#"{"config_path":"token.toml"}"#,
                r#"{"cwd":42}"#,
            ],
        );
        assert_closed_options::<RawGenerateDocgenOptions>(
            r#"{"cwd":".","entry":"src/button.tsx"}"#,
            &[
                r#"{"input":"src/button.tsx"}"#,
                r#"{"entry_path":"src/button.tsx"}"#,
                r#"{"entry":false}"#,
            ],
        );
        assert_closed_options::<RawLibraryBuildOptions>(
            r#"{"cwd":".","entry":"src/index.ts"}"#,
            &[
                r#"{"input":"src/index.ts"}"#,
                r#"{"entry_path":"src/index.ts"}"#,
                r#"{"cwd":[]}"#,
            ],
        );
        assert_closed_options::<RawDocsOptions>(
            r#"{"cwd":".","configPath":"wake.config.toml","outdir":"dist","basePath":"/docs/","mode":"components"}"#,
            &[
                r#"{"base":"/docs/"}"#,
                r#"{"base_path":"/docs/"}"#,
                r#"{"mode":true}"#,
            ],
        );
    }

    #[test]
    fn application_and_docs_dev_server_options_are_disjoint_closed_dtos() {
        assert_closed_options::<RawApplicationDevServerOptions>(
            r#"{"cwd":".","configPath":"wake.config.toml","entry":"src/index.ts","host":"127.0.0.1","port":5173,"open":false,"federation":{"enabled":false}}"#,
            &[
                r#"{"mode":"site"}"#,
                r#"{"config_path":"wake.config.toml"}"#,
                r#"{"port":"5173"}"#,
            ],
        );
        assert_closed_options::<RawDocsDevServerOptions>(
            r#"{"cwd":".","configPath":"wake.config.toml","host":"127.0.0.1","port":5173,"open":false,"mode":"components"}"#,
            &[
                r#"{"entry":"src/index.ts"}"#,
                r#"{"federation":{"enabled":false}}"#,
                r#"{"config_path":"wake.config.toml"}"#,
                r#"{"open":"yes"}"#,
            ],
        );

        assert_eq!(
            RawApplicationDevServerOptions::parse(Some(r#"{"mode":"site"}"#.to_owned()))
                .unwrap_err()
                .code,
            "WAKE_CONFIG"
        );
        for value in [
            r#"{"entry":"src/index.ts"}"#,
            r#"{"federation":{"enabled":false}}"#,
        ] {
            assert_eq!(
                RawDocsDevServerOptions::parse(Some(value.to_owned()))
                    .unwrap_err()
                    .code,
                "WAKE_CONFIG"
            );
        }
    }

    #[test]
    fn node_test_request_maps_only_implemented_fields() {
        let options = parse_test_options(Some(
            r#"{"root":".","patterns":["src/**/*.test.ts"],"namePattern":"button","projects":["ui"],"environment":"dom","changed":true,"related":["src/button.ts"],"coverage":true,"updateSnapshots":"all","serial":true,"workers":2,"bail":1,"shard":"1/2","seed":"seed","shuffle":true,"allowNoTests":true,"browserPath":"browser","headful":true}"#.to_owned(),
        ))
        .unwrap();
        assert_eq!(options.root.as_deref(), Some(std::path::Path::new(".")));
        assert_eq!(options.patterns, ["src/**/*.test.ts"]);
        assert_eq!(options.name_pattern.as_deref(), Some("button"));
        assert_eq!(options.projects, ["ui"]);
        assert_eq!(options.environment.as_deref(), Some("dom"));
        assert!(options.changed);
        assert_eq!(options.related, [PathBuf::from("src/button.ts")]);
        assert!(options.coverage);
        assert_eq!(options.update_snapshots.as_deref(), Some("all"));
        assert!(options.serial);
        assert!(matches!(
            options.workers,
            Some(wake_app::WorkerOverride::Count(2))
        ));
        assert_eq!(options.bail, Some(1));
        assert_eq!(options.shard.as_deref(), Some("1/2"));
        assert_eq!(options.seed.as_deref(), Some("seed"));
        assert!(options.shuffle);
        assert!(options.allow_no_tests);
        assert_eq!(
            options.browser_path.as_deref(),
            Some(std::path::Path::new("browser"))
        );
        assert!(options.headful);
        assert!(!options.watch);
        assert!(options.reporter.is_none());
        assert!(options.output.is_none());

        for rejected in [
            r#"{"watch":true}"#,
            r#"{"reporter":"json"}"#,
            r#"{"output":"artifacts/tests"}"#,
            r#"{"name_pattern":"button"}"#,
            r#"{"coverage":"yes"}"#,
        ] {
            let error = parse_test_options(Some(rejected.to_owned())).unwrap_err();
            assert_eq!(error.code, "WAKE_TEST_CONFIG", "{rejected}");
        }
    }

    #[test]
    fn node_test_request_enums_and_worker_ranges_are_closed() {
        for environment in ["auto", "dom", "browser"] {
            let options =
                parse_test_options(Some(json!({"environment": environment}).to_string())).unwrap();
            assert_eq!(options.environment.as_deref(), Some(environment));
        }
        for mode in ["none", "new", "all"] {
            let options =
                parse_test_options(Some(json!({"updateSnapshots": mode}).to_string())).unwrap();
            assert_eq!(options.update_snapshots.as_deref(), Some(mode));
        }
        for workers in [
            json!(1),
            json!(64),
            json!("auto"),
            json!("1%"),
            json!("100%"),
        ] {
            parse_test_options(Some(json!({"workers": workers}).to_string())).unwrap();
        }

        for rejected in [
            json!({"environment": "jsdom"}),
            json!({"environment": "DOM"}),
            json!({"updateSnapshots": "overwrite"}),
            json!({"updateSnapshots": true}),
            json!({"workers": 0}),
            json!({"workers": -1}),
            json!({"workers": 1.5}),
            json!({"workers": false}),
            json!({"workers": "0%"}),
            json!({"workers": "101%"}),
            json!({"workers": "01%"}),
            json!({"workers": "1.5%"}),
            json!({"workers": "AUTO"}),
            json!({"workers": "2"}),
        ] {
            let rejected = rejected.to_string();
            let error = parse_test_options(Some(rejected.clone())).unwrap_err();
            assert_eq!(error.code, "WAKE_TEST_CONFIG", "{rejected}");
        }
    }

    #[test]
    fn closed_test_context_uses_the_public_context_error_code() {
        let resource = TestContextResource {
            options: wake_app::TestOptions::default(),
            host_path: None,
            session: Mutex::new(None),
            events: Mutex::new(Vec::new()),
            event_error: Mutex::new(None),
            active_cancellation: Mutex::new(None),
            running: AtomicBool::new(false),
            watching: AtomicBool::new(false),
            closed: AtomicBool::new(true),
        };
        let error = resource
            .with_session(&CancellationToken::default(), |_| Ok(()))
            .unwrap_err();
        assert_eq!(error.code, "WAKE_TEST_CONTEXT");
    }

    #[test]
    fn concurrent_test_context_run_uses_the_public_busy_error_code() {
        let resource = TestContextResource {
            options: wake_app::TestOptions::default(),
            host_path: None,
            session: Mutex::new(None),
            events: Mutex::new(Vec::new()),
            event_error: Mutex::new(None),
            active_cancellation: Mutex::new(None),
            running: AtomicBool::new(true),
            watching: AtomicBool::new(false),
            closed: AtomicBool::new(false),
        };
        let error = resource.run(&CancellationToken::default()).unwrap_err();
        assert_eq!(error.code, "WAKE_TEST_BUSY");
        assert_eq!(resource.start_watch().unwrap_err().code, "WAKE_TEST_BUSY");
        assert_eq!(resource.stop_watch().unwrap_err().code, "WAKE_TEST_BUSY");
    }
}
