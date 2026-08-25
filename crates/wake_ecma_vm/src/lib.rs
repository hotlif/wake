//! Product-neutral ECMAScript execution façade.
//!
//! Wake embeds the V8 build pinned by Deno v2.9.5, but engine handles never leave this crate.
//! Runtime and product crates exchange owned source, values, diagnostics and a thread-safe
//! termination handle only. This crate deliberately owns no filesystem, resolver or test API.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::Duration;

use deno_core::{
    Extension, InspectorMsg, InspectorMsgKind, InspectorSessionKind, JsRuntime, JsRuntimeInspector,
    LocalInspectorSession, OpDecl, OpState, RuntimeOptions, op2, v8,
};
use deno_error::JsErrorBox;
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

/// Owned source identity used by VM diagnostics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScriptSource {
    pub path: String,
    pub code: String,
}

impl ScriptSource {
    pub fn new(path: impl Into<String>, code: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            code: code.into(),
        }
    }
}

/// Stable VM failure categories. No V8-owned exception value crosses the façade.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VmErrorKind {
    Exception,
    Timeout,
    Terminated,
    InvalidResult,
    Host,
}

/// A stable execution error that never exposes engine-owned values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VmError {
    pub kind: VmErrorKind,
    pub path: String,
    pub message: String,
}

impl VmError {
    fn new(kind: VmErrorKind, path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            kind,
            path: path.into(),
            message: message.into(),
        }
    }

    pub fn is_termination(&self) -> bool {
        matches!(self.kind, VmErrorKind::Timeout | VmErrorKind::Terminated)
    }
}

impl fmt::Display for VmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.path, self.message)
    }
}

impl std::error::Error for VmError {}

/// Isolate limits applied before V8 is created.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VmOptions {
    pub initial_heap_bytes: usize,
    pub max_heap_bytes: usize,
    pub execution_timeout: Option<Duration>,
    pub inspector: bool,
}

impl Default for VmOptions {
    fn default() -> Self {
        Self {
            initial_heap_bytes: 0,
            max_heap_bytes: 256 * 1024 * 1024,
            execution_timeout: None,
            inspector: false,
        }
    }
}

#[derive(Clone, Copy)]
struct JsonHostCallback(fn(&str) -> Result<String, String>);

#[derive(Default)]
struct TimerRegistry {
    pending: HashMap<u64, oneshot::Sender<()>>,
    cancelled_before_start: HashSet<u64>,
}

#[op2]
#[string]
fn op_wake_vm_host_call(
    state: &mut OpState,
    #[string] request: String,
) -> Result<String, JsErrorBox> {
    let callback = state
        .try_borrow::<JsonHostCallback>()
        .copied()
        .ok_or_else(|| JsErrorBox::generic("Wake VM host callback is not registered"))?;
    (callback.0)(&request).map_err(JsErrorBox::generic)
}

#[op2]
async fn op_wake_vm_sleep(
    state: Rc<RefCell<OpState>>,
    #[number] timer_id: u64,
    #[number] milliseconds: u64,
) {
    let (cancel, cancelled) = oneshot::channel();
    {
        let mut state = state.borrow_mut();
        let timers = state.borrow_mut::<TimerRegistry>();
        if timers.cancelled_before_start.remove(&timer_id) {
            return;
        }
        timers.pending.insert(timer_id, cancel);
    }
    tokio::select! {
        _ = tokio::time::sleep(Duration::from_millis(milliseconds)) => {}
        _ = cancelled => {}
    }
    state
        .borrow_mut()
        .borrow_mut::<TimerRegistry>()
        .pending
        .remove(&timer_id);
}

#[op2(fast)]
fn op_wake_vm_cancel_sleep(state: Rc<RefCell<OpState>>, #[number] timer_id: u64) {
    let mut state = state.borrow_mut();
    let timers = state.borrow_mut::<TimerRegistry>();
    if let Some(cancel) = timers.pending.remove(&timer_id) {
        let _ = cancel.send(());
    } else {
        timers.cancelled_before_start.insert(timer_id);
    }
}

#[op2(fast)]
fn op_wake_vm_detach_array_buffer<'s, 'i>(
    _scope: &mut v8::PinScope<'s, 'i>,
    buffer: v8::Local<'s, v8::ArrayBuffer>,
) -> Result<(), JsErrorBox> {
    if !buffer.is_detachable() || buffer.was_detached() {
        return Err(JsErrorBox::type_error(
            "ArrayBuffer is not detachable or is already detached",
        ));
    }
    buffer.detach(None);
    Ok(())
}

fn host_extension() -> Extension {
    const DECLS: [OpDecl; 4] = [
        op_wake_vm_host_call(),
        op_wake_vm_sleep(),
        op_wake_vm_cancel_sleep(),
        op_wake_vm_detach_array_buffer(),
    ];
    Extension {
        name: "wake_vm_host",
        ops: std::borrow::Cow::Borrowed(&DECLS),
        op_state_fn: Some(Box::new(|state| state.put(TimerRegistry::default()))),
        ..Default::default()
    }
}

/// Thread-safe, owned cancellation seam. It contains no realm or GC handle.
#[derive(Clone)]
pub struct VmHandle {
    isolate: v8::IsolateHandle,
}

impl VmHandle {
    /// Terminate the currently executing JavaScript instruction stream.
    pub fn terminate(&self) -> bool {
        self.isolate.terminate_execution()
    }
}

/// One isolated V8 realm.
///
/// A Vm is deliberately neither Send nor Sync: one suite owns it on one worker thread.
pub struct Vm {
    // Inspector sessions must be dropped before the JsRuntime tears down its contexts.
    coverage_inspector: Option<CoverageInspector>,
    runtime: JsRuntime,
    // Deno's V8 platform posts delayed foreground tasks through Tokio. The reactor must outlive the
    // isolate and be entered whenever V8 or its job queue can schedule work.
    reactor: tokio::runtime::Runtime,
    execution_timeout: Option<Duration>,
    inspector_enabled: bool,
}

struct CoverageInspector {
    session: LocalInspectorSession,
    receiver: mpsc::Receiver<InspectorMsg>,
    next_id: i32,
}

impl Default for Vm {
    fn default() -> Self {
        Self::new()
    }
}

impl Vm {
    pub fn new() -> Self {
        Self::with_options(VmOptions::default())
    }

    pub fn with_options(options: VmOptions) -> Self {
        let reactor = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("Wake must be able to create its V8 task reactor");
        let _reactor_guard = reactor.enter();
        let create_params = (options.max_heap_bytes > 0).then(|| {
            v8::Isolate::create_params()
                .heap_limits(options.initial_heap_bytes, options.max_heap_bytes)
        });
        let runtime = JsRuntime::new(RuntimeOptions {
            create_params,
            inspector: options.inspector,
            extensions: vec![host_extension()],
            ..Default::default()
        });
        Self {
            coverage_inspector: None,
            runtime,
            reactor,
            execution_timeout: options.execution_timeout,
            inspector_enabled: options.inspector,
        }
    }

    pub fn engine_version() -> &'static str {
        v8::V8::get_version()
    }

    pub fn handle(&mut self) -> VmHandle {
        VmHandle {
            isolate: self.runtime.v8_isolate().thread_safe_handle(),
        }
    }

    pub fn set_execution_timeout(&mut self, timeout: Option<Duration>) {
        self.execution_timeout = timeout;
    }

    /// Begin V8 precise range coverage for this realm.
    ///
    /// The returned data from [`Self::take_precise_coverage`] is owned JSON. Inspector sessions,
    /// protocol callbacks and V8 handles remain entirely inside this façade.
    pub fn start_precise_coverage(&mut self) -> Result<(), VmError> {
        self.ensure_coverage_inspector()?;
        self.inspector_request("Profiler.enable", serde_json::json!({}))?;
        self.inspector_request(
            "Profiler.startPreciseCoverage",
            serde_json::json!({
                "callCount": true,
                "detailed": true,
                "allowTriggeredUpdates": false,
            }),
        )?;
        Ok(())
    }

    /// Take and stop V8 precise range coverage for this realm.
    pub fn take_precise_coverage(&mut self) -> Result<serde_json::Value, VmError> {
        let result =
            self.inspector_request("Profiler.takePreciseCoverage", serde_json::json!({}))?;
        self.inspector_request("Profiler.stopPreciseCoverage", serde_json::json!({}))?;
        self.inspector_request("Profiler.disable", serde_json::json!({}))?;
        Ok(result)
    }

    fn ensure_coverage_inspector(&mut self) -> Result<(), VmError> {
        if self.coverage_inspector.is_some() {
            return Ok(());
        }
        if !self.inspector_enabled {
            return Err(VmError::new(
                VmErrorKind::Host,
                "<coverage>",
                "V8 inspector coverage was not enabled when the realm was created",
            ));
        }
        let _reactor_guard = self.reactor.enter();
        let (sender, receiver) = mpsc::channel();
        let callback = Box::new(move |message| {
            let _ = sender.send(message);
        });
        let session = JsRuntimeInspector::create_local_session(
            self.runtime.inspector(),
            callback,
            InspectorSessionKind::NonBlocking {
                wait_for_disconnect: false,
            },
        );
        self.coverage_inspector = Some(CoverageInspector {
            session,
            receiver,
            next_id: 1,
        });
        Ok(())
    }

    fn inspector_request(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, VmError> {
        self.ensure_coverage_inspector()?;
        let inspector = self
            .coverage_inspector
            .as_mut()
            .expect("coverage inspector was initialized");
        let request_id = inspector.next_id;
        inspector.next_id += 1;
        let _reactor_guard = self.reactor.enter();
        inspector
            .session
            .post_message(request_id, method, Some(params));
        loop {
            let message = inspector
                .receiver
                .recv_timeout(Duration::from_secs(2))
                .map_err(|error| {
                    VmError::new(
                        VmErrorKind::Host,
                        "<coverage>",
                        format!("V8 inspector did not answer {method}: {error}"),
                    )
                })?;
            if !matches!(message.kind, InspectorMsgKind::Message(id) if id == request_id) {
                continue;
            }
            let envelope =
                serde_json::from_str::<serde_json::Value>(&message.content).map_err(|error| {
                    VmError::new(
                        VmErrorKind::InvalidResult,
                        "<coverage>",
                        format!("V8 inspector returned invalid JSON for {method}: {error}"),
                    )
                })?;
            if let Some(error) = envelope.get("error") {
                return Err(VmError::new(
                    VmErrorKind::Host,
                    "<coverage>",
                    format!("V8 inspector {method} failed: {error}"),
                ));
            }
            return Ok(envelope
                .get("result")
                .cloned()
                .unwrap_or(serde_json::Value::Null));
        }
    }

    /// Register a product-neutral JSON request/response host seam.
    ///
    /// The callback receives owned UTF-8 JSON and returns owned UTF-8 JSON. Engine values and
    /// handles never cross this boundary. The internal Deno bootstrap global is removed before
    /// project code is evaluated.
    pub fn register_json_host_function(
        &mut self,
        name: &str,
        callback: fn(&str) -> Result<String, String>,
    ) -> Result<(), VmError> {
        if !is_safe_identifier(name) {
            return Err(VmError::new(
                VmErrorKind::Host,
                "<host>",
                format!("invalid host function name {name:?}"),
            ));
        }
        self.runtime
            .op_state()
            .borrow_mut()
            .put(JsonHostCallback(callback));
        let name = serde_json::to_string(name).expect("host function name is a string");
        let source = ScriptSource::new(
            "<wake-vm-bootstrap>",
            format!(
                r#"(() => {{
  const operation = globalThis.Deno?.core?.ops?.op_wake_vm_host_call;
  const sleep = globalThis.Deno?.core?.ops?.op_wake_vm_sleep;
  const cancelSleep = globalThis.Deno?.core?.ops?.op_wake_vm_cancel_sleep;
  if (typeof operation !== "function" || typeof sleep !== "function" || typeof cancelSleep !== "function") throw new Error("Wake VM host ops are unavailable");
  Object.defineProperty(globalThis, {name}, {{
    value(request) {{ return operation(String(request)); }},
    configurable: false,
    enumerable: false,
    writable: false,
  }});
  Object.defineProperty(globalThis, "__wakeVmSleep", {{
    value(timerId, milliseconds) {{ return sleep(Number(timerId), Math.max(0, Number(milliseconds) || 0)); }},
    configurable: false,
    enumerable: false,
    writable: false,
  }});
  Object.defineProperty(globalThis, "__wakeVmCancelSleep", {{
    value(timerId) {{ cancelSleep(Number(timerId)); }},
    configurable: false,
    enumerable: false,
    writable: false,
  }});
  Reflect.deleteProperty(globalThis, "Deno");
}})()"#
            ),
        );
        self.eval(&source).map(|_| ())
    }

    /// Install a named, product-neutral ArrayBuffer detach primitive for conformance hosts.
    ///
    /// The V8 handle remains inside this facade. The function is opt-in and the internal Deno
    /// bootstrap global is removed before caller code is evaluated.
    pub fn register_array_buffer_detach_function(&mut self, name: &str) -> Result<(), VmError> {
        if !is_safe_identifier(name) {
            return Err(VmError::new(
                VmErrorKind::Host,
                "<host>",
                format!("invalid host function name {name:?}"),
            ));
        }
        let name = serde_json::to_string(name).expect("host function name is a string");
        let source = ScriptSource::new(
            "<wake-vm-array-buffer-host>",
            format!(
                r#"(() => {{
  const detach = globalThis.Deno?.core?.ops?.op_wake_vm_detach_array_buffer;
  if (typeof detach !== "function") throw new Error("Wake VM ArrayBuffer detach op is unavailable");
  Object.defineProperty(globalThis, {name}, {{
    value(buffer) {{ return detach(buffer); }},
    configurable: false,
    enumerable: false,
    writable: false,
  }});
  Reflect.deleteProperty(globalThis, "Deno");
}})()"#
            ),
        );
        self.eval(&source).map(|_| ())
    }

    /// Execute a script, drain its Promise jobs and return the JavaScript string value.
    pub fn execute(&mut self, source: &ScriptSource) -> Result<String, VmError> {
        let value = self.eval(source)?;
        self.run_jobs(source)?;
        self.value_to_string(value, source)
    }

    /// Execute a script, drain jobs, then evaluate a stable result expression.
    pub fn execute_and_read(
        &mut self,
        source: &ScriptSource,
        result_expression: &str,
    ) -> Result<String, VmError> {
        self.eval(source)?;
        self.run_jobs(source)?;
        let result = ScriptSource::new(
            format!("{}#result", source.path),
            result_expression.to_string(),
        );
        let value = self.eval(&result)?;
        self.run_jobs(&result)?;
        self.value_to_string(value, &result)
    }

    /// Execute a script expected to produce JSON and deserialize the owned result.
    pub fn execute_json<T>(&mut self, source: &ScriptSource) -> Result<T, VmError>
    where
        T: for<'de> Deserialize<'de>,
    {
        let json = self.execute(source)?;
        serde_json::from_str(&json).map_err(|error| {
            VmError::new(
                VmErrorKind::InvalidResult,
                source.path.clone(),
                format!("VM result is not valid JSON: {error}"),
            )
        })
    }

    fn eval(&mut self, source: &ScriptSource) -> Result<v8::Global<v8::Value>, VmError> {
        let _reactor_guard = self.reactor.enter();
        let deadline = Deadline::start(&mut self.runtime, self.execution_timeout);
        let result = self
            .runtime
            .execute_script(source_name(&source.path), source.code.clone());
        let timed_out = deadline.finish();
        match result {
            Ok(value) => Ok(value),
            Err(error) => {
                let message = error.to_string();
                let terminated = message.contains("execution terminated");
                if terminated {
                    self.runtime.v8_isolate().cancel_terminate_execution();
                }
                let kind = if timed_out {
                    VmErrorKind::Timeout
                } else if terminated {
                    VmErrorKind::Terminated
                } else {
                    VmErrorKind::Exception
                };
                Err(VmError::new(
                    kind,
                    source.path.clone(),
                    source_error_message(source, message),
                ))
            }
        }
    }

    fn run_jobs(&mut self, source: &ScriptSource) -> Result<(), VmError> {
        let deadline = Deadline::start(&mut self.runtime, self.execution_timeout);
        let result = self
            .reactor
            .block_on(self.runtime.run_event_loop(Default::default()));
        let timed_out = deadline.finish();
        result.map_err(|error| {
            let message = error.to_string();
            let terminated = message.contains("execution terminated");
            if terminated {
                self.runtime.v8_isolate().cancel_terminate_execution();
            }
            VmError::new(
                if timed_out {
                    VmErrorKind::Timeout
                } else if terminated {
                    VmErrorKind::Terminated
                } else {
                    VmErrorKind::Exception
                },
                source.path.clone(),
                message,
            )
        })
    }

    fn value_to_string(
        &mut self,
        value: v8::Global<v8::Value>,
        source: &ScriptSource,
    ) -> Result<String, VmError> {
        let _reactor_guard = self.reactor.enter();
        deno_core::scope!(scope, self.runtime);
        let value = v8::Local::new(scope, value);
        let value = value.to_string(scope).ok_or_else(|| {
            VmError::new(
                VmErrorKind::InvalidResult,
                source.path.clone(),
                "JavaScript result could not be converted to a string",
            )
        })?;
        Ok(value.to_rust_string_lossy(scope))
    }
}

struct Deadline {
    cancel: Option<mpsc::Sender<()>>,
    worker: Option<JoinHandle<()>>,
    fired: Arc<AtomicBool>,
}

impl Deadline {
    fn start(runtime: &mut JsRuntime, timeout: Option<Duration>) -> Self {
        let fired = Arc::new(AtomicBool::new(false));
        let Some(timeout) = timeout.filter(|timeout| !timeout.is_zero()) else {
            return Self {
                cancel: None,
                worker: None,
                fired,
            };
        };
        let isolate = runtime.v8_isolate().thread_safe_handle();
        let (cancel, receiver) = mpsc::channel();
        let worker_fired = Arc::clone(&fired);
        let worker = std::thread::Builder::new()
            .name("wake-v8-deadline".to_string())
            .spawn(move || {
                if receiver.recv_timeout(timeout).is_err() {
                    worker_fired.store(true, Ordering::Release);
                    isolate.terminate_execution();
                }
            })
            .expect("Wake must be able to create a V8 deadline thread");
        Self {
            cancel: Some(cancel),
            worker: Some(worker),
            fired,
        }
    }

    fn finish(mut self) -> bool {
        if let Some(cancel) = self.cancel.take() {
            let _ = cancel.send(());
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        self.fired.load(Ordering::Acquire)
    }
}

fn is_safe_identifier(name: &str) -> bool {
    let mut characters = name.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    (first == '_' || first == '$' || first.is_ascii_alphabetic())
        && characters.all(|character| {
            character == '_' || character == '$' || character.is_ascii_alphanumeric()
        })
}

fn source_name(path: &str) -> String {
    if path.is_ascii() {
        path.to_string()
    } else {
        "<wake-script>".to_string()
    }
}

fn source_error_message(source: &ScriptSource, message: String) -> String {
    let Some(position) = message
        .rsplit_once(" at line ")
        .map(|(_, position)| position)
    else {
        return message;
    };
    let Some(line) = position
        .split_once(',')
        .map(|(line, _)| line)
        .and_then(|line| line.parse::<usize>().ok())
    else {
        return message;
    };
    let Some(source_line) = source.code.lines().nth(line.saturating_sub(1)) else {
        return message;
    };
    format!("{message}\n  {line} | {}", source_line.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn executes_es2024_script_in_an_isolated_v8_context() {
        let mut vm = Vm::new();
        let result = vm
            .execute(&ScriptSource::new(
                "fixture.js",
                "JSON.stringify({ value: [1, 2, 3].map(x => x * 2), total: 6 })",
            ))
            .unwrap();
        assert_eq!(result, r#"{"value":[2,4,6],"total":6}"#);
        assert!(!Vm::engine_version().is_empty());
    }

    #[test]
    fn opt_in_array_buffer_detach_host_keeps_v8_handles_inside_the_facade() {
        let mut vm = Vm::new();
        vm.register_array_buffer_detach_function("detachForHost")
            .unwrap();
        let result = vm
            .execute(&ScriptSource::new(
                "detach.js",
                "const buffer = new ArrayBuffer(8); detachForHost(buffer); String(buffer.byteLength)",
            ))
            .unwrap();
        assert_eq!(result, "0");
        assert_eq!(
            vm.execute(&ScriptSource::new("deno-hidden.js", "typeof Deno"))
                .unwrap(),
            "undefined"
        );
    }

    #[test]
    fn contexts_do_not_share_globals() {
        let mut first = Vm::new();
        first
            .execute(&ScriptSource::new("first.js", "globalThis.leaked = 1"))
            .unwrap();
        let mut second = Vm::new();
        let value = second
            .execute(&ScriptSource::new("second.js", "typeof globalThis.leaked"))
            .unwrap();
        assert_eq!(value, "undefined");
    }

    #[test]
    fn drains_promise_jobs_before_reading_a_result() {
        let mut vm = Vm::new();
        let value = vm
            .execute_and_read(
                &ScriptSource::new(
                    "promise.js",
                    "globalThis.answer = 0; Promise.resolve().then(() => answer = 42)",
                ),
                "String(globalThis.answer)",
            )
            .unwrap();
        assert_eq!(value, "42");
    }

    #[test]
    fn exposes_only_the_registered_owned_host_seam() {
        fn echo(request: &str) -> Result<String, String> {
            Ok(request.to_uppercase())
        }
        let mut vm = Vm::new();
        vm.register_json_host_function("__wakeHostCall", echo)
            .unwrap();
        let value = vm
            .execute(&ScriptSource::new(
                "host.js",
                "JSON.stringify([__wakeHostCall('wake'), typeof globalThis.Deno])",
            ))
            .unwrap();
        assert_eq!(value, r#"["WAKE","undefined"]"#);
    }

    #[test]
    fn wall_clock_timeout_terminates_an_infinite_loop() {
        let mut vm = Vm::with_options(VmOptions {
            execution_timeout: Some(Duration::from_millis(50)),
            ..VmOptions::default()
        });
        let error = vm
            .execute(&ScriptSource::new(
                "loop.js",
                "globalThis.timeoutEvents = []; Promise.resolve().then(() => timeoutEvents.push('stale')); for (;;) {}",
            ))
            .unwrap_err();
        assert_eq!(error.kind, VmErrorKind::Timeout);
        vm.set_execution_timeout(Some(Duration::from_secs(1)));
        let recovered = vm
            .execute(&ScriptSource::new(
                "after-timeout.js",
                "timeoutEvents.push('next'); JSON.stringify(timeoutEvents)",
            ))
            .unwrap();
        assert_eq!(recovered, r#"["next"]"#);
    }

    #[test]
    fn precise_coverage_returns_owned_script_ranges() {
        let mut vm = Vm::with_options(VmOptions {
            inspector: true,
            ..VmOptions::default()
        });
        vm.start_precise_coverage().unwrap();
        vm.execute(&ScriptSource::new(
            "coverage-fixture.js",
            "function covered(value) { return value * 2 }\ncovered(21)",
        ))
        .unwrap();
        let coverage = vm.take_precise_coverage().unwrap();
        let scripts = coverage["result"].as_array().unwrap();
        let script = scripts
            .iter()
            .find(|script| script["url"] == "coverage-fixture.js")
            .expect("the executed source is reported by V8");
        assert!(
            script["functions"]
                .as_array()
                .is_some_and(|value| !value.is_empty())
        );
    }
}
