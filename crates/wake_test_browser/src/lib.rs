//! Internal system-Chromium driver for Wake Test.
//!
//! This crate owns browser discovery, launch, CDP transport, BrowserContext isolation, real input,
//! network interception, screenshots and precise V8 coverage. It intentionally knows nothing about
//! test discovery, assertions, snapshots or reporters.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tempfile::TempDir;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

const CDP_EVENT_CANCEL_POLL: Duration = Duration::from_millis(25);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserError {
    pub code: &'static str,
    pub message: String,
    pub path: Option<PathBuf>,
}

impl BrowserError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            code: "WAKE_TEST_BROWSER",
            message: message.into(),
            path: None,
        }
    }

    fn at(mut self, path: impl Into<PathBuf>) -> Self {
        self.path = Some(path.into());
        self
    }
}

impl fmt::Display for BrowserError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(path) = &self.path {
            write!(formatter, "{}: {}", path.display(), self.message)
        } else {
            formatter.write_str(&self.message)
        }
    }
}

impl std::error::Error for BrowserError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BrowserKind {
    Chrome,
    Edge,
    Chromium,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserInstallation {
    pub kind: BrowserKind,
    pub executable: PathBuf,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Viewport {
    pub width: u32,
    pub height: u32,
    pub device_scale_factor: f64,
}

impl Default for Viewport {
    fn default() -> Self {
        Self {
            width: 1280,
            height: 720,
            device_scale_factor: 1.0,
        }
    }
}

/// Viewport-relative CSS pixel clip for a typed PNG capture.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScreenshotClip {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub scale: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct BrowserLaunchOptions {
    pub executable: Option<PathBuf>,
    pub headless: bool,
    pub sandbox: bool,
    pub viewport: Viewport,
    pub locale: String,
    pub timezone: String,
    pub color_scheme: String,
    pub launch_timeout_ms: u64,
}

/// Wake's fixed browser rendering preference. Screenshot identity and result metadata consume this
/// same value, so host operating-system motion settings can never leak into a browser test run.
pub const REDUCED_MOTION: &str = "reduce";

impl Default for BrowserLaunchOptions {
    fn default() -> Self {
        Self {
            executable: None,
            headless: true,
            sandbox: true,
            viewport: Viewport::default(),
            locale: "en-US".to_string(),
            timezone: "UTC".to_string(),
            color_scheme: "light".to_string(),
            launch_timeout_ms: 15_000,
        }
    }
}

/// A one-shot, thread-safe cancellation signal for a bounded browser operation.
#[derive(Debug, Clone, Default)]
pub struct BrowserCancellationToken {
    cancelled: Arc<AtomicBool>,
}

impl BrowserCancellationToken {
    pub fn new() -> Self {
        Self::default()
    }

    /// Request cancellation. Repeated calls are harmless.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

/// Outcome of waiting for an asynchronous CDP event.
#[derive(Debug, Clone, PartialEq)]
pub enum CdpEventWait<T> {
    Event(T),
    TimedOut,
    Cancelled,
}

/// Modifier bit field accepted by Chromium's Input domain.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InputModifiers(u8);

impl InputModifiers {
    pub const NONE: Self = Self(0);
    pub const ALT: Self = Self(1);
    pub const CONTROL: Self = Self(2);
    pub const META: Self = Self(4);
    pub const SHIFT: Self = Self(8);

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub const fn bits(self) -> u8 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerEventType {
    Down,
    Up,
    Move,
    Wheel,
}

impl PointerEventType {
    fn as_cdp(self) -> &'static str {
        match self {
            Self::Down => "mousePressed",
            Self::Up => "mouseReleased",
            Self::Move => "mouseMoved",
            Self::Wheel => "mouseWheel",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerButton {
    None,
    Left,
    Middle,
    Right,
    Back,
    Forward,
}

impl PointerButton {
    fn as_cdp(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Left => "left",
            Self::Middle => "middle",
            Self::Right => "right",
            Self::Back => "back",
            Self::Forward => "forward",
        }
    }

    const fn buttons(self) -> u8 {
        match self {
            Self::None => 0,
            Self::Left => 1,
            Self::Right => 2,
            Self::Middle => 4,
            Self::Back => 8,
            Self::Forward => 16,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerType {
    Mouse,
    Pen,
}

impl PointerType {
    fn as_cdp(self) -> &'static str {
        match self {
            Self::Mouse => "mouse",
            Self::Pen => "pen",
        }
    }
}

/// One protocol-level pointer event using viewport-relative CSS pixel coordinates.
#[derive(Debug, Clone, PartialEq)]
pub struct PointerInput {
    pub x: f64,
    pub y: f64,
    pub button: PointerButton,
    pub buttons: u8,
    pub click_count: u32,
    pub modifiers: InputModifiers,
    pub pointer_type: PointerType,
    pub force: f64,
    pub delta_x: f64,
    pub delta_y: f64,
}

impl PointerInput {
    pub fn at(x: f64, y: f64) -> Self {
        Self {
            x,
            y,
            button: PointerButton::None,
            buttons: 0,
            click_count: 0,
            modifiers: InputModifiers::NONE,
            pointer_type: PointerType::Mouse,
            force: 0.0,
            delta_x: 0.0,
            delta_y: 0.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyEventType {
    Down,
    Up,
    RawDown,
    Character,
}

impl KeyEventType {
    fn as_cdp(self) -> &'static str {
        match self {
            Self::Down => "keyDown",
            Self::Up => "keyUp",
            Self::RawDown => "rawKeyDown",
            Self::Character => "char",
        }
    }
}

/// One protocol-level keyboard event description.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyboardInput {
    pub key: String,
    pub code: String,
    pub text: Option<String>,
    pub unmodified_text: Option<String>,
    pub key_identifier: Option<String>,
    pub windows_virtual_key_code: u32,
    pub native_virtual_key_code: u32,
    pub modifiers: InputModifiers,
    pub auto_repeat: bool,
    pub is_keypad: bool,
    pub is_system_key: bool,
    pub location: u8,
    pub commands: Vec<String>,
}

impl KeyboardInput {
    pub fn new(key: impl Into<String>, code: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            code: code.into(),
            text: None,
            unmodified_text: None,
            key_identifier: None,
            windows_virtual_key_code: 0,
            native_virtual_key_code: 0,
            modifiers: InputModifiers::NONE,
            auto_repeat: false,
            is_keypad: false,
            is_system_key: false,
            location: 0,
            commands: Vec::new(),
        }
    }

    pub fn with_text(mut self, text: impl Into<String>) -> Self {
        let text = text.into();
        self.unmodified_text = Some(text.clone());
        self.text = Some(text);
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum FetchRequestStage {
    Request,
    Response,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FetchRequestPattern {
    pub url_pattern: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource_type: Option<String>,
    pub request_stage: FetchRequestStage,
}

impl Default for FetchRequestPattern {
    fn default() -> Self {
        Self {
            url_pattern: "*".to_string(),
            resource_type: None,
            request_stage: FetchRequestStage::Request,
        }
    }
}

/// Request data owned by a `Fetch.requestPaused` event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchRequest {
    pub url: String,
    pub method: String,
    pub headers: BTreeMap<String, String>,
    pub post_data: Option<String>,
}

/// An owned paused-request event without the process-local CDP session id.
#[derive(Debug, Clone, PartialEq)]
pub struct FetchRequestPaused {
    pub request_id: String,
    pub request: FetchRequest,
    pub frame_id: String,
    pub resource_type: String,
    pub response_error_reason: Option<String>,
    pub response_status_code: Option<u16>,
    pub response_status_text: Option<String>,
    pub network_id: Option<String>,
    pub redirected_request_id: Option<String>,
    raw_params: Value,
}

impl FetchRequestPaused {
    /// Preserve protocol fields outside Wake's minimal typed surface without exposing a session id.
    pub fn raw_params(&self) -> &Value {
        &self.raw_params
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FetchHeader {
    pub name: String,
    pub value: String,
}

/// Protocol response supplied to `Fetch.fulfillRequest`; matching policy remains above this crate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchFulfillResponse {
    pub status_code: u16,
    pub headers: Vec<FetchHeader>,
    pub body: Vec<u8>,
    pub response_phrase: Option<String>,
}

impl FetchFulfillResponse {
    pub fn new(status_code: u16, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status_code,
            headers: Vec::new(),
            body: body.into(),
            response_phrase: None,
        }
    }
}

pub fn detect_browser(explicit: Option<&Path>) -> Result<BrowserInstallation, BrowserError> {
    if let Some(path) = explicit {
        if !path.is_file() {
            return Err(BrowserError::new(
                "configured browser executable does not exist or is not a file",
            )
            .at(path));
        }
        return inspect_browser(path);
    }

    for candidate in browser_candidates() {
        if candidate.is_file()
            && let Ok(browser) = inspect_browser(&candidate)
        {
            return Ok(browser);
        }
    }
    Err(BrowserError::new(
        "no compatible system Chrome, Edge or Chromium executable was found; set --browser-path",
    ))
}

fn inspect_browser(path: &Path) -> Result<BrowserInstallation, BrowserError> {
    let inferred_kind = browser_kind_from_text(&path.to_string_lossy().to_ascii_lowercase());
    // Chrome and Edge on Windows do not implement a reliable `--version` process contract: some
    // builds attach to a running profile and never exit. CDP Browser.getVersion is authoritative
    // after launch, so avoid an unbounded probe here.
    let version = if cfg!(windows) {
        "unknown (verified after CDP launch)".to_string()
    } else {
        Command::new(path)
            .arg("--version")
            .stdin(Stdio::null())
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
            .filter(|version| !version.is_empty())
            .unwrap_or_else(|| "unknown (verified after CDP launch)".to_string())
    };
    let reported_kind = browser_kind_from_text(&version.to_ascii_lowercase());
    let kind = if reported_kind == BrowserKind::Unknown {
        inferred_kind
    } else {
        reported_kind
    };
    if kind == BrowserKind::Unknown {
        return Err(BrowserError::new(format!(
            "executable is not a recognized Chromium-family browser: {version}"
        ))
        .at(path));
    }
    Ok(BrowserInstallation {
        kind,
        executable: path.to_path_buf(),
        version,
    })
}

fn browser_kind_from_text(value: &str) -> BrowserKind {
    if value.contains("edge") || value.contains("msedge") {
        BrowserKind::Edge
    } else if value.contains("chromium") {
        BrowserKind::Chromium
    } else if value.contains("chrome") {
        BrowserKind::Chrome
    } else {
        BrowserKind::Unknown
    }
}

fn browser_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if cfg!(windows) {
        for root in ["LOCALAPPDATA", "PROGRAMFILES", "PROGRAMFILES(X86)"]
            .into_iter()
            .filter_map(std::env::var_os)
            .map(PathBuf::from)
        {
            candidates.push(root.join("Google/Chrome/Application/chrome.exe"));
            candidates.push(root.join("Microsoft/Edge/Application/msedge.exe"));
            candidates.push(root.join("Chromium/Application/chrome.exe"));
        }
    } else if cfg!(target_os = "macos") {
        candidates.extend([
            PathBuf::from("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"),
            PathBuf::from("/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge"),
            PathBuf::from("/Applications/Chromium.app/Contents/MacOS/Chromium"),
        ]);
    } else {
        candidates.extend([
            PathBuf::from("/usr/bin/google-chrome"),
            PathBuf::from("/usr/bin/google-chrome-stable"),
            PathBuf::from("/usr/bin/microsoft-edge"),
            PathBuf::from("/usr/bin/microsoft-edge-stable"),
            PathBuf::from("/usr/bin/chromium"),
            PathBuf::from("/usr/bin/chromium-browser"),
        ]);
    }

    let executable_names: &[&str] = if cfg!(windows) {
        &["chrome.exe", "msedge.exe", "chromium.exe"]
    } else {
        &[
            "google-chrome",
            "google-chrome-stable",
            "microsoft-edge",
            "chromium",
            "chromium-browser",
        ]
    };
    if let Some(path) = std::env::var_os("PATH") {
        for directory in std::env::split_paths(&path) {
            candidates.extend(executable_names.iter().map(|name| directory.join(name)));
        }
    }

    let mut seen = BTreeSet::new();
    candidates
        .into_iter()
        .filter(|candidate| seen.insert(candidate.clone()))
        .collect()
}

type BrowserSocket = WebSocket<MaybeTlsStream<TcpStream>>;

struct CdpConnection {
    next_id: AtomicU64,
    outbound: mpsc::Sender<CdpOutbound>,
    events: Arc<CdpEventQueue>,
    worker: Mutex<Option<JoinHandle<()>>>,
    command_timeout: Duration,
}

enum CdpOutbound {
    Command {
        id: u64,
        session_id: Option<String>,
        method: String,
        params: Value,
        reply: mpsc::Sender<Result<Value, BrowserError>>,
    },
    Forget {
        id: u64,
    },
    Shutdown,
}

struct PendingCommand {
    method: String,
    reply: mpsc::Sender<Result<Value, BrowserError>>,
}

#[derive(Default)]
struct CdpEventState {
    events: VecDeque<Value>,
    closed: Option<BrowserError>,
}

#[derive(Default)]
struct CdpEventQueue {
    state: Mutex<CdpEventState>,
    changed: Condvar,
}

impl CdpEventQueue {
    fn push(&self, event: Value) {
        let mut state = self.state.lock().expect("CDP event queue poisoned");
        state.events.push_back(event);
        self.changed.notify_all();
    }

    fn close(&self, error: BrowserError) {
        let mut state = self.state.lock().expect("CDP event queue poisoned");
        state.closed.get_or_insert(error);
        self.changed.notify_all();
    }

    fn closed_error(&self) -> Option<BrowserError> {
        self.state
            .lock()
            .expect("CDP event queue poisoned")
            .closed
            .clone()
    }

    fn take_event(&self, method: &str, session_id: Option<&str>) -> Option<Value> {
        let mut state = self.state.lock().expect("CDP event queue poisoned");
        let index = state
            .events
            .iter()
            .position(|event| event_matches(event, method, session_id))?;
        state.events.remove(index)
    }

    fn wait_for_event(
        &self,
        session_id: Option<&str>,
        method: &str,
        timeout: Duration,
        cancellation: &BrowserCancellationToken,
    ) -> Result<CdpEventWait<Value>, BrowserError> {
        if cancellation.is_cancelled() {
            return Ok(CdpEventWait::Cancelled);
        }

        let timeout = nonzero_timeout(timeout);
        let deadline = operation_deadline(timeout, method)?;
        let mut state = self.state.lock().expect("CDP event queue poisoned");
        loop {
            if let Some(index) = state
                .events
                .iter()
                .position(|event| event_matches(event, method, session_id))
            {
                return Ok(CdpEventWait::Event(
                    state
                        .events
                        .remove(index)
                        .expect("event index came from the same queue"),
                ));
            }
            if cancellation.is_cancelled() {
                return Ok(CdpEventWait::Cancelled);
            }
            if let Some(error) = &state.closed {
                return Err(error.clone());
            }
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return Ok(CdpEventWait::TimedOut);
            };
            let wait = remaining.min(CDP_EVENT_CANCEL_POLL);
            let (next_state, _) = self
                .changed
                .wait_timeout(state, nonzero_timeout(wait))
                .expect("CDP event queue poisoned while waiting");
            state = next_state;
        }
    }
}

impl CdpConnection {
    fn connect(url: &str, timeout: Duration) -> Result<Self, BrowserError> {
        let (mut socket, _) = tungstenite::connect(url).map_err(|error| {
            BrowserError::new(format!("CDP websocket handshake failed: {error}"))
        })?;
        if let MaybeTlsStream::Plain(stream) = socket.get_mut() {
            stream
                .set_read_timeout(Some(timeout))
                .and_then(|()| stream.set_write_timeout(Some(timeout)))
                .map_err(|error| {
                    BrowserError::new(format!("could not bound the CDP socket: {error}"))
                })?;
        }
        let command_timeout = nonzero_timeout(timeout);
        let (outbound, receiver) = mpsc::channel();
        let events = Arc::new(CdpEventQueue::default());
        let worker_events = Arc::clone(&events);
        let worker = std::thread::Builder::new()
            .name("wake-test-cdp".to_string())
            .spawn(move || cdp_worker(&mut socket, receiver, &worker_events))
            .map_err(|error| {
                BrowserError::new(format!("could not start the CDP dispatcher: {error}"))
            })?;
        Ok(Self {
            next_id: AtomicU64::new(1),
            outbound,
            events,
            worker: Mutex::new(Some(worker)),
            command_timeout,
        })
    }

    fn command(
        &self,
        session_id: Option<&str>,
        method: &str,
        params: Value,
    ) -> Result<Value, BrowserError> {
        self.command_with_timeout(session_id, method, params, self.command_timeout)
    }

    fn command_with_timeout(
        &self,
        session_id: Option<&str>,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, BrowserError> {
        let timeout = nonzero_timeout(timeout);
        let id = self
            .next_id
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current.checked_add(1)
            })
            .map_err(|_| BrowserError::new("CDP command identifier space was exhausted"))?;
        let (reply, response) = mpsc::channel();
        self.outbound
            .send(CdpOutbound::Command {
                id,
                session_id: session_id.map(str::to_string),
                method: method.to_string(),
                params,
                reply,
            })
            .map_err(|_| {
                self.events.closed_error().unwrap_or_else(|| {
                    BrowserError::new(format!(
                        "CDP dispatcher stopped before {method} could be sent"
                    ))
                })
            })?;

        match response.recv_timeout(timeout) {
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let _ = self.outbound.send(CdpOutbound::Forget { id });
                Err(command_timeout_error(method, timeout))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Err(self.events.closed_error().unwrap_or_else(|| {
                    BrowserError::new(format!("CDP dispatcher stopped while waiting for {method}"))
                }))
            }
        }
    }

    fn wait_for_event(
        &self,
        session_id: Option<&str>,
        method: &str,
        timeout: Duration,
        cancellation: &BrowserCancellationToken,
    ) -> Result<CdpEventWait<Value>, BrowserError> {
        self.events
            .wait_for_event(session_id, method, timeout, cancellation)
    }

    fn take_event(&self, method: &str, session_id: Option<&str>) -> Option<Value> {
        self.events.take_event(method, session_id)
    }
}

impl Drop for CdpConnection {
    fn drop(&mut self) {
        let _ = self.outbound.send(CdpOutbound::Shutdown);
        if let Ok(mut worker) = self.worker.lock()
            && let Some(worker) = worker.take()
        {
            let _ = worker.join();
        }
    }
}

fn cdp_worker(
    socket: &mut BrowserSocket,
    receiver: mpsc::Receiver<CdpOutbound>,
    events: &CdpEventQueue,
) {
    let mut pending = BTreeMap::<u64, PendingCommand>::new();
    loop {
        let mut shutdown = false;
        loop {
            match receiver.try_recv() {
                Ok(CdpOutbound::Command {
                    id,
                    session_id,
                    method,
                    params,
                    reply,
                }) => {
                    let mut request = json!({
                        "id": id,
                        "method": method,
                        "params": params,
                    });
                    if let Some(session_id) = session_id {
                        request["sessionId"] = Value::String(session_id);
                    }
                    if let Err(error) = socket.send(Message::Text(request.to_string().into())) {
                        let error = BrowserError::new(format!(
                            "could not send CDP command {method}: {error}"
                        ));
                        let _ = reply.send(Err(error.clone()));
                        close_cdp_worker(events, &mut pending, error);
                        return;
                    }
                    pending.insert(id, PendingCommand { method, reply });
                }
                Ok(CdpOutbound::Forget { id }) => {
                    pending.remove(&id);
                }
                Ok(CdpOutbound::Shutdown) | Err(mpsc::TryRecvError::Disconnected) => {
                    shutdown = true;
                    break;
                }
                Err(mpsc::TryRecvError::Empty) => break,
            }
        }
        if shutdown {
            let error = BrowserError::new("CDP connection was closed");
            close_cdp_worker(events, &mut pending, error);
            let _ = socket.close(None);
            return;
        }

        if let Err(error) = set_socket_timeout(socket, CDP_EVENT_CANCEL_POLL) {
            close_cdp_worker(events, &mut pending, error);
            return;
        }
        let message = match socket.read() {
            Ok(message) => message,
            Err(error) if is_socket_timeout(&error) => continue,
            Err(error) => {
                close_cdp_worker(
                    events,
                    &mut pending,
                    BrowserError::new(format!("could not read from CDP: {error}")),
                );
                return;
            }
        };
        match message {
            Message::Text(text) => {
                let value: Value = match serde_json::from_str(text.as_str()) {
                    Ok(value) => value,
                    Err(error) => {
                        close_cdp_worker(
                            events,
                            &mut pending,
                            BrowserError::new(format!("CDP returned invalid JSON: {error}")),
                        );
                        return;
                    }
                };
                if let Some(id) = value.get("id").and_then(Value::as_u64) {
                    if let Some(command) = pending.remove(&id) {
                        let result = if let Some(error) = value.get("error") {
                            Err(BrowserError::new(format!(
                                "{} failed: {error}",
                                command.method
                            )))
                        } else {
                            Ok(value.get("result").cloned().unwrap_or(Value::Null))
                        };
                        let _ = command.reply.send(result);
                    }
                } else if value.get("method").and_then(Value::as_str).is_some() {
                    events.push(value);
                }
            }
            Message::Ping(payload) => {
                if let Err(error) = socket.send(Message::Pong(payload)) {
                    close_cdp_worker(
                        events,
                        &mut pending,
                        BrowserError::new(format!("could not answer CDP ping: {error}")),
                    );
                    return;
                }
            }
            Message::Close(frame) => {
                close_cdp_worker(
                    events,
                    &mut pending,
                    BrowserError::new(format!("browser closed the CDP connection: {frame:?}")),
                );
                return;
            }
            _ => {}
        }
    }
}

fn close_cdp_worker(
    events: &CdpEventQueue,
    pending: &mut BTreeMap<u64, PendingCommand>,
    error: BrowserError,
) {
    events.close(error.clone());
    for (_, command) in std::mem::take(pending) {
        let _ = command.reply.send(Err(error.clone()));
    }
}

fn event_matches(event: &Value, method: &str, session_id: Option<&str>) -> bool {
    event.get("method").and_then(Value::as_str) == Some(method)
        && session_id
            .is_none_or(|expected| event.get("sessionId").and_then(Value::as_str) == Some(expected))
}

fn nonzero_timeout(timeout: Duration) -> Duration {
    timeout.max(Duration::from_millis(1))
}

fn operation_deadline(timeout: Duration, operation: &str) -> Result<Instant, BrowserError> {
    Instant::now().checked_add(timeout).ok_or_else(|| {
        BrowserError::new(format!(
            "CDP timeout for {operation} is too large to represent"
        ))
    })
}

fn command_timeout_error(method: &str, timeout: Duration) -> BrowserError {
    BrowserError::new(format!(
        "CDP command {method} timed out after {} ms",
        timeout.as_millis()
    ))
}

fn set_socket_timeout(socket: &mut BrowserSocket, timeout: Duration) -> Result<(), BrowserError> {
    if let MaybeTlsStream::Plain(stream) = socket.get_mut() {
        stream
            .set_read_timeout(Some(timeout))
            .and_then(|()| stream.set_write_timeout(Some(timeout)))
            .map_err(|error| {
                BrowserError::new(format!("could not bound the CDP socket: {error}"))
            })?;
    }
    Ok(())
}

fn is_socket_timeout(error: &tungstenite::Error) -> bool {
    matches!(
        error,
        tungstenite::Error::Io(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            )
    )
}

pub struct BrowserDriver {
    connection: Arc<CdpConnection>,
    child: Child,
    _profile: TempDir,
    pub installation: BrowserInstallation,
    options: BrowserLaunchOptions,
}

impl BrowserDriver {
    pub fn launch(options: BrowserLaunchOptions) -> Result<Self, BrowserError> {
        let mut installation = detect_browser(options.executable.as_deref())?;
        let profile = tempfile::Builder::new()
            .prefix("wake-test-browser-")
            .tempdir()
            .map_err(|error| {
                BrowserError::new(format!("could not create browser profile: {error}"))
            })?;
        let mut command = Command::new(&installation.executable);
        command
            .arg("--remote-debugging-address=127.0.0.1")
            .arg("--remote-debugging-port=0")
            .arg(format!("--user-data-dir={}", profile.path().display()))
            .arg("--no-first-run")
            .arg("--no-default-browser-check")
            .arg("--enable-automation")
            .arg("--disable-background-networking")
            .arg("--disable-component-update")
            .arg("--disable-default-apps")
            .arg("--disable-extensions")
            .arg("--disable-component-extensions-with-background-pages")
            .arg("--disable-sync")
            .arg("--metrics-recording-only")
            .arg("--disable-breakpad")
            .arg("--disable-crash-reporter")
            .arg("--disable-features=Translate,MediaRouter")
            .arg(format!(
                "--window-size={},{}",
                options.viewport.width, options.viewport.height
            ))
            .arg(format!("--lang={}", options.locale))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if options.headless {
            command.arg("--headless=new");
        }
        if !options.sandbox {
            command.arg("--no-sandbox");
        }
        command.arg("about:blank");
        let mut child = command.spawn().map_err(|error| {
            BrowserError::new(format!("could not launch system browser: {error}"))
                .at(&installation.executable)
        })?;
        let active_port = profile.path().join("DevToolsActivePort");
        let deadline = Instant::now() + Duration::from_millis(options.launch_timeout_ms);
        let contents = loop {
            match fs::read_to_string(&active_port) {
                Ok(contents) if contents.lines().count() >= 2 => break contents,
                _ => {}
            }
            if let Some(status) = child
                .try_wait()
                .map_err(|error| BrowserError::new(format!("could not inspect browser: {error}")))?
            {
                return Err(BrowserError::new(format!(
                    "browser exited before CDP became ready: {status}"
                ))
                .at(&installation.executable));
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err(BrowserError::new(format!(
                    "browser did not expose CDP within {} ms",
                    options.launch_timeout_ms
                ))
                .at(&installation.executable));
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        let mut lines = contents.lines();
        let port = lines
            .next()
            .and_then(|line| line.parse::<u16>().ok())
            .ok_or_else(|| BrowserError::new("DevToolsActivePort has an invalid port"))?;
        let socket_path = lines
            .next()
            .ok_or_else(|| BrowserError::new("DevToolsActivePort has no websocket path"))?;
        let socket_url = format!("ws://127.0.0.1:{port}{socket_path}");
        let connection = match CdpConnection::connect(
            &socket_url,
            Duration::from_millis(options.launch_timeout_ms.max(1)),
        ) {
            Ok(connection) => connection,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        };
        if let Ok(version) = connection.command(None, "Browser.getVersion", json!({}))
            && let Some(product) = version.get("product").and_then(Value::as_str)
        {
            installation.version = product.to_string();
            let reported_kind = browser_kind_from_text(&product.to_ascii_lowercase());
            if reported_kind != BrowserKind::Unknown {
                installation.kind = reported_kind;
            }
        }
        Ok(Self {
            connection: Arc::new(connection),
            child,
            _profile: profile,
            installation,
            options,
        })
    }

    pub fn create_context(&self) -> Result<BrowserContext, BrowserError> {
        let result = self.connection.command(
            None,
            "Target.createBrowserContext",
            json!({
                "disposeOnDetach": true,
            }),
        )?;
        let id = required_string(&result, "browserContextId", "Target.createBrowserContext")?;
        Ok(BrowserContext {
            connection: Arc::clone(&self.connection),
            id,
            options: self.options.clone(),
            disposed: false,
        })
    }

    /// Reports the launch mode used by this driver for stable test-result metadata.
    pub fn is_headless(&self) -> bool {
        self.options.headless
    }

    /// Exact rendering inputs used for every BrowserContext created by this driver.
    pub fn launch_options(&self) -> &BrowserLaunchOptions {
        &self.options
    }
}

impl Drop for BrowserDriver {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub struct BrowserContext {
    connection: Arc<CdpConnection>,
    id: String,
    options: BrowserLaunchOptions,
    disposed: bool,
}

impl BrowserContext {
    pub fn new_page(&self, url: &str) -> Result<BrowserPage, BrowserError> {
        let target = self.connection.command(
            None,
            "Target.createTarget",
            json!({
                "url": "about:blank",
                "browserContextId": self.id,
                // A freshly created off-the-record context has no BrowserList window yet. Chrome
                // rejects an explicit false here instead of creating the context's first page.
                "newWindow": true,
                "background": false,
            }),
        )?;
        let target_id = required_string(&target, "targetId", "Target.createTarget")?;
        let attached = self.connection.command(
            None,
            "Target.attachToTarget",
            json!({
                "targetId": target_id,
                "flatten": true,
            }),
        )?;
        let session_id = required_string(&attached, "sessionId", "Target.attachToTarget")?;
        self.connection
            .command(Some(&session_id), "Page.enable", json!({}))?;
        self.connection
            .command(Some(&session_id), "Runtime.enable", json!({}))?;
        self.connection.command(
            Some(&session_id),
            "Emulation.setDeviceMetricsOverride",
            json!({
                "width": self.options.viewport.width,
                "height": self.options.viewport.height,
                "deviceScaleFactor": self.options.viewport.device_scale_factor,
                "mobile": false,
            }),
        )?;
        self.connection.command(
            Some(&session_id),
            "Emulation.setLocaleOverride",
            json!({"locale": self.options.locale}),
        )?;
        self.connection.command(
            Some(&session_id),
            "Emulation.setTimezoneOverride",
            json!({"timezoneId": self.options.timezone}),
        )?;
        self.connection.command(
            Some(&session_id),
            "Emulation.setEmulatedMedia",
            json!({
                "features": [
                    {
                        "name": "prefers-color-scheme",
                        "value": self.options.color_scheme,
                    },
                    {
                        "name": "prefers-reduced-motion",
                        "value": REDUCED_MOTION,
                    },
                ],
            }),
        )?;
        let page = BrowserPage {
            connection: Arc::clone(&self.connection),
            target_id,
            session_id,
            closed: false,
        };
        if url != "about:blank" {
            page.navigate(url)?;
        }
        Ok(page)
    }

    pub fn dispose(mut self) -> Result<(), BrowserError> {
        self.dispose_inner()
    }

    fn dispose_inner(&mut self) -> Result<(), BrowserError> {
        if self.disposed {
            return Ok(());
        }
        self.connection.command(
            None,
            "Target.disposeBrowserContext",
            json!({"browserContextId": self.id}),
        )?;
        self.disposed = true;
        Ok(())
    }
}

impl Drop for BrowserContext {
    fn drop(&mut self) {
        let _ = self.dispose_inner();
    }
}

/// An owned Chromium target.
///
/// Operational methods are thread-safe, so callers may wrap a page in [`Arc`] to service Fetch
/// events while another thread awaits a command. Dropping the final owner closes the target.
pub struct BrowserPage {
    connection: Arc<CdpConnection>,
    target_id: String,
    session_id: String,
    closed: bool,
}

fn decode_evaluation_result(result: Value) -> Result<Value, BrowserError> {
    if let Some(exception) = result.get("exceptionDetails") {
        return Err(BrowserError::new(format!(
            "browser evaluation failed: {exception}"
        )));
    }
    Ok(result
        .get("result")
        .and_then(|result| result.get("value"))
        .cloned()
        .unwrap_or(Value::Null))
}

impl BrowserPage {
    pub fn navigate(&self, url: &str) -> Result<(), BrowserError> {
        let result = self.connection.command(
            Some(&self.session_id),
            "Page.navigate",
            json!({"url": url}),
        )?;
        if let Some(error) = result.get("errorText").and_then(Value::as_str) {
            return Err(BrowserError::new(format!("navigation failed: {error}")));
        }
        Ok(())
    }

    pub fn evaluate(&self, expression: &str) -> Result<Value, BrowserError> {
        self.evaluate_with_timeout(expression, None)
    }

    /// Interrupt JavaScript that is currently executing in this page's renderer.
    ///
    /// This is a typed CDP transport primitive. Timeout selection, failure attribution and
    /// post-termination recovery policy remain the caller's responsibility.
    pub fn terminate_execution(&self) -> Result<(), BrowserError> {
        self.command("Runtime.terminateExecution", json!({}))
            .map(|_| ())
    }

    /// Interrupt JavaScript with an explicit transport bound.
    ///
    /// The timeout only bounds the CDP command. It does not assign test timeout policy.
    pub fn terminate_execution_with_transport_timeout(
        &self,
        timeout_ms: u64,
    ) -> Result<(), BrowserError> {
        self.command_with_timeout(
            "Runtime.terminateExecution",
            json!({}),
            Duration::from_millis(timeout_ms.max(1)),
        )
        .map(|_| ())
    }

    pub fn evaluate_with_timeout(
        &self,
        expression: &str,
        timeout_ms: Option<u64>,
    ) -> Result<Value, BrowserError> {
        let mut parameters = json!({
            "expression": expression,
            "awaitPromise": true,
            "returnByValue": true,
            "userGesture": true,
        });
        if let Some(timeout_ms) = timeout_ms {
            parameters["timeout"] = json!(timeout_ms);
        }
        let result = if let Some(timeout_ms) = timeout_ms {
            self.command_with_timeout(
                "Runtime.evaluate",
                parameters,
                Duration::from_millis(timeout_ms).saturating_add(Duration::from_millis(250)),
            )?
        } else {
            self.command("Runtime.evaluate", parameters)?
        };
        decode_evaluation_result(result)
    }

    /// Evaluate JavaScript while bounding only the CDP transport.
    ///
    /// Unlike [`Self::evaluate_with_timeout`], this does not send Chromium's execution `timeout`
    /// parameter. Wake Test uses this primitive with its own deadline watchdog so a renderer or
    /// protocol failure can never be mistaken for a test-case timeout.
    pub fn evaluate_with_transport_timeout(
        &self,
        expression: &str,
        transport_timeout_ms: u64,
    ) -> Result<Value, BrowserError> {
        let result = self.command_with_timeout(
            "Runtime.evaluate",
            json!({
                "expression": expression,
                "awaitPromise": true,
                "returnByValue": true,
                "userGesture": true,
            }),
            Duration::from_millis(transport_timeout_ms.max(1)),
        )?;
        decode_evaluation_result(result)
    }

    pub fn command(&self, method: &str, params: Value) -> Result<Value, BrowserError> {
        self.connection
            .command(Some(&self.session_id), method, params)
    }

    /// Issue one raw CDP command with an absolute transport deadline.
    pub fn command_with_timeout(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, BrowserError> {
        self.connection
            .command_with_timeout(Some(&self.session_id), method, params, timeout)
    }

    /// Enable Fetch-domain interception without assigning any matching or response policy.
    pub fn enable_fetch_interception(
        &self,
        patterns: &[FetchRequestPattern],
        handle_auth_requests: bool,
    ) -> Result<(), BrowserError> {
        let mut parameters = json!({
            "handleAuthRequests": handle_auth_requests,
        });
        if !patterns.is_empty() {
            parameters["patterns"] = serde_json::to_value(patterns).map_err(|error| {
                BrowserError::new(format!("could not encode Fetch request patterns: {error}"))
            })?;
        }
        self.command("Fetch.enable", parameters).map(|_| ())
    }

    pub fn enable_network_interception(&self) -> Result<(), BrowserError> {
        self.enable_fetch_interception(&[FetchRequestPattern::default()], false)
    }

    pub fn disable_fetch_interception(&self) -> Result<(), BrowserError> {
        self.command("Fetch.disable", json!({})).map(|_| ())
    }

    /// Wait for one page-scoped CDP event without allowing timeout or cancellation starvation.
    pub fn wait_for_event(
        &self,
        method: &str,
        timeout: Duration,
        cancellation: &BrowserCancellationToken,
    ) -> Result<CdpEventWait<Value>, BrowserError> {
        self.connection
            .wait_for_event(Some(&self.session_id), method, timeout, cancellation)
    }

    pub fn wait_for_fetch_request(
        &self,
        timeout: Duration,
        cancellation: &BrowserCancellationToken,
    ) -> Result<CdpEventWait<FetchRequestPaused>, BrowserError> {
        match self.wait_for_event("Fetch.requestPaused", timeout, cancellation)? {
            CdpEventWait::Event(event) => {
                parse_fetch_request_paused(event).map(CdpEventWait::Event)
            }
            CdpEventWait::TimedOut => Ok(CdpEventWait::TimedOut),
            CdpEventWait::Cancelled => Ok(CdpEventWait::Cancelled),
        }
    }

    /// Continue one Fetch-domain request unchanged.
    pub fn continue_fetch_request(&self, request_id: &str) -> Result<(), BrowserError> {
        self.command("Fetch.continueRequest", json!({"requestId": request_id}))
            .map(|_| ())
    }

    /// Fulfill one Fetch-domain request with caller-owned protocol data.
    pub fn fulfill_fetch_request(
        &self,
        request_id: &str,
        response: &FetchFulfillResponse,
    ) -> Result<(), BrowserError> {
        if !(100..=599).contains(&response.status_code) {
            return Err(BrowserError::new(
                "Fetch.fulfillRequest response code must be between 100 and 599",
            ));
        }
        let mut parameters = json!({
            "requestId": request_id,
            "responseCode": response.status_code,
            "responseHeaders": response.headers,
            "body": base64::engine::general_purpose::STANDARD.encode(&response.body),
        });
        if let Some(phrase) = &response.response_phrase {
            parameters["responsePhrase"] = Value::String(phrase.clone());
        }
        self.command("Fetch.fulfillRequest", parameters).map(|_| ())
    }

    /// Dispatch one typed pointer event through Chromium's native input pipeline.
    pub fn dispatch_pointer_event(
        &self,
        event_type: PointerEventType,
        input: &PointerInput,
    ) -> Result<(), BrowserError> {
        validate_pointer_input(input)?;
        if matches!(event_type, PointerEventType::Down | PointerEventType::Up)
            && input.button == PointerButton::None
        {
            return Err(BrowserError::new(
                "pointer down/up requires a concrete button",
            ));
        }
        let mut parameters = json!({
            "type": event_type.as_cdp(),
            "x": input.x,
            "y": input.y,
            "modifiers": input.modifiers.bits(),
            "button": input.button.as_cdp(),
            "buttons": input.buttons,
            "clickCount": input.click_count,
            "pointerType": input.pointer_type.as_cdp(),
        });
        if input.force > 0.0 {
            parameters["force"] = json!(input.force);
        }
        if event_type == PointerEventType::Wheel {
            parameters["deltaX"] = json!(input.delta_x);
            parameters["deltaY"] = json!(input.delta_y);
        }
        self.command("Input.dispatchMouseEvent", parameters)
            .map(|_| ())
    }

    pub fn pointer_move(&self, x: f64, y: f64) -> Result<(), BrowserError> {
        self.dispatch_pointer_event(PointerEventType::Move, &PointerInput::at(x, y))
    }

    pub fn pointer_down(
        &self,
        x: f64,
        y: f64,
        button: PointerButton,
        modifiers: InputModifiers,
    ) -> Result<(), BrowserError> {
        let mut input = PointerInput::at(x, y);
        input.button = button;
        input.buttons = button.buttons();
        input.click_count = 1;
        input.modifiers = modifiers;
        self.dispatch_pointer_event(PointerEventType::Down, &input)
    }

    pub fn pointer_up(
        &self,
        x: f64,
        y: f64,
        button: PointerButton,
        modifiers: InputModifiers,
    ) -> Result<(), BrowserError> {
        let mut input = PointerInput::at(x, y);
        input.button = button;
        input.click_count = 1;
        input.modifiers = modifiers;
        self.dispatch_pointer_event(PointerEventType::Up, &input)
    }

    pub fn pointer_click(
        &self,
        x: f64,
        y: f64,
        button: PointerButton,
        modifiers: InputModifiers,
    ) -> Result<(), BrowserError> {
        self.pointer_down(x, y, button, modifiers)?;
        self.pointer_up(x, y, button, modifiers)
    }

    pub fn pointer_wheel(
        &self,
        x: f64,
        y: f64,
        delta_x: f64,
        delta_y: f64,
        modifiers: InputModifiers,
    ) -> Result<(), BrowserError> {
        let mut input = PointerInput::at(x, y);
        input.delta_x = delta_x;
        input.delta_y = delta_y;
        input.modifiers = modifiers;
        self.dispatch_pointer_event(PointerEventType::Wheel, &input)
    }

    /// Dispatch one typed keyboard event through Chromium's native input pipeline.
    pub fn dispatch_key_event(
        &self,
        event_type: KeyEventType,
        input: &KeyboardInput,
    ) -> Result<(), BrowserError> {
        validate_keyboard_input(input)?;
        let mut parameters = json!({
            "type": event_type.as_cdp(),
            "modifiers": input.modifiers.bits(),
            "code": input.code,
            "key": input.key,
            "windowsVirtualKeyCode": input.windows_virtual_key_code,
            "nativeVirtualKeyCode": input.native_virtual_key_code,
            "autoRepeat": input.auto_repeat,
            "isKeypad": input.is_keypad,
            "isSystemKey": input.is_system_key,
            "location": input.location,
            "commands": input.commands,
        });
        if let Some(text) = &input.text {
            parameters["text"] = Value::String(text.clone());
        }
        if let Some(text) = &input.unmodified_text {
            parameters["unmodifiedText"] = Value::String(text.clone());
        }
        if let Some(identifier) = &input.key_identifier {
            parameters["keyIdentifier"] = Value::String(identifier.clone());
        }
        self.command("Input.dispatchKeyEvent", parameters)
            .map(|_| ())
    }

    pub fn key_down(&self, input: &KeyboardInput) -> Result<(), BrowserError> {
        self.dispatch_key_event(KeyEventType::Down, input)
    }

    pub fn key_up(&self, input: &KeyboardInput) -> Result<(), BrowserError> {
        self.dispatch_key_event(KeyEventType::Up, input)
    }

    pub fn press_key(&self, input: &KeyboardInput) -> Result<(), BrowserError> {
        self.key_down(input)?;
        let mut release = input.clone();
        release.text = None;
        release.unmodified_text = None;
        release.commands.clear();
        self.key_up(&release)
    }

    /// Insert IME/emoji text which does not originate from a physical key press.
    pub fn insert_text(&self, text: &str) -> Result<(), BrowserError> {
        self.command("Input.insertText", json!({"text": text}))
            .map(|_| ())
    }

    /// Assign browser-readable files to one file input selected in the page.
    ///
    /// The caller owns file creation, selection policy and cleanup; this driver only translates an
    /// exact selector and owned paths into the corresponding DOM-domain commands.
    pub fn set_file_input_files(
        &self,
        selector: &str,
        files: &[PathBuf],
    ) -> Result<(), BrowserError> {
        if selector.is_empty() {
            return Err(BrowserError::new("file input selector must not be empty"));
        }
        for file in files {
            if !file.is_file() {
                return Err(
                    BrowserError::new("file input path does not exist or is not a file").at(file),
                );
            }
        }
        let document = self.command(
            "DOM.getDocument",
            json!({
                "depth": 0,
                "pierce": true,
            }),
        )?;
        let root_node_id = document
            .get("root")
            .and_then(|root| root.get("nodeId"))
            .and_then(Value::as_u64)
            .filter(|node_id| *node_id != 0)
            .ok_or_else(|| BrowserError::new("DOM.getDocument omitted root.nodeId"))?;
        let selected = self.command(
            "DOM.querySelector",
            json!({
                "nodeId": root_node_id,
                "selector": selector,
            }),
        )?;
        let node_id = selected
            .get("nodeId")
            .and_then(Value::as_u64)
            .filter(|node_id| *node_id != 0)
            .ok_or_else(|| {
                BrowserError::new(format!(
                    "file input selector {selector:?} did not match an element"
                ))
            })?;
        self.command(
            "DOM.setFileInputFiles",
            json!({
                "nodeId": node_id,
                "files": files
                    .iter()
                    .map(|file| file.to_string_lossy().into_owned())
                    .collect::<Vec<_>>(),
            }),
        )
        .map(|_| ())
    }

    pub fn fail_fetch_request(
        &self,
        request_id: &str,
        error_reason: &str,
    ) -> Result<(), BrowserError> {
        if error_reason.is_empty() {
            return Err(BrowserError::new(
                "Fetch.failRequest requires a Network.ErrorReason",
            ));
        }
        self.command(
            "Fetch.failRequest",
            json!({
                "requestId": request_id,
                "errorReason": error_reason,
            }),
        )
        .map(|_| ())
    }

    pub fn screenshot_png(&self) -> Result<Vec<u8>, BrowserError> {
        self.screenshot_png_with_clip(None)
    }

    pub fn screenshot_png_with_clip(
        &self,
        clip: Option<&ScreenshotClip>,
    ) -> Result<Vec<u8>, BrowserError> {
        let mut parameters = json!({
            "format": "png",
            "fromSurface": true,
            "captureBeyondViewport": false,
        });
        if let Some(clip) = clip {
            if ![clip.x, clip.y, clip.width, clip.height, clip.scale]
                .into_iter()
                .all(f64::is_finite)
                || clip.width <= 0.0
                || clip.height <= 0.0
                || clip.scale <= 0.0
            {
                return Err(BrowserError::new(
                    "screenshot clip requires finite coordinates and positive size/scale",
                ));
            }
            parameters["clip"] = serde_json::to_value(clip)
                .expect("ScreenshotClip has an infallible JSON representation");
        }
        let result = self.command("Page.captureScreenshot", parameters)?;
        let data = required_string(&result, "data", "Page.captureScreenshot")?;
        base64::engine::general_purpose::STANDARD
            .decode(data)
            .map_err(|error| BrowserError::new(format!("invalid screenshot payload: {error}")))
    }

    pub fn start_precise_coverage(&self) -> Result<(), BrowserError> {
        self.command("Profiler.enable", json!({}))?;
        self.command(
            "Profiler.startPreciseCoverage",
            json!({
                "callCount": true,
                "detailed": true,
                "allowTriggeredUpdates": false,
            }),
        )
        .map(|_| ())
    }

    pub fn take_precise_coverage(&self) -> Result<Value, BrowserError> {
        self.command("Profiler.takePreciseCoverage", json!({}))
    }

    pub fn take_event(&self, method: &str) -> Option<Value> {
        self.connection.take_event(method, Some(&self.session_id))
    }

    pub fn close(mut self) -> Result<(), BrowserError> {
        self.close_inner()
    }

    fn close_inner(&mut self) -> Result<(), BrowserError> {
        if self.closed {
            return Ok(());
        }
        self.connection.command(
            None,
            "Target.closeTarget",
            json!({"targetId": self.target_id}),
        )?;
        self.closed = true;
        Ok(())
    }
}

impl Drop for BrowserPage {
    fn drop(&mut self) {
        let _ = self.close_inner();
    }
}

fn validate_pointer_input(input: &PointerInput) -> Result<(), BrowserError> {
    if ![input.x, input.y, input.force, input.delta_x, input.delta_y]
        .into_iter()
        .all(f64::is_finite)
    {
        return Err(BrowserError::new(
            "pointer coordinates, pressure and wheel deltas must be finite",
        ));
    }
    if !(0.0..=1.0).contains(&input.force) {
        return Err(BrowserError::new(
            "pointer pressure must be between 0 and 1",
        ));
    }
    if input.buttons & !0b1_1111 != 0 {
        return Err(BrowserError::new(
            "pointer buttons contains an unsupported CDP bit",
        ));
    }
    Ok(())
}

fn validate_keyboard_input(input: &KeyboardInput) -> Result<(), BrowserError> {
    if input.location > 2 {
        return Err(BrowserError::new(
            "keyboard location must be 0 (standard), 1 (left) or 2 (right)",
        ));
    }
    Ok(())
}

fn parse_fetch_request_paused(event: Value) -> Result<FetchRequestPaused, BrowserError> {
    let params = event
        .get("params")
        .cloned()
        .ok_or_else(|| BrowserError::new("Fetch.requestPaused omitted params"))?;
    let request = params
        .get("request")
        .ok_or_else(|| BrowserError::new("Fetch.requestPaused omitted request"))?;
    let headers = request
        .get("headers")
        .and_then(Value::as_object)
        .ok_or_else(|| BrowserError::new("Fetch.requestPaused request omitted headers"))?
        .iter()
        .map(|(name, value)| {
            value
                .as_str()
                .map(|value| (name.clone(), value.to_string()))
                .ok_or_else(|| {
                    BrowserError::new(format!(
                        "Fetch.requestPaused header {name:?} is not a string"
                    ))
                })
        })
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    let response_status_code = match params.get("responseStatusCode") {
        Some(value) => Some(
            value
                .as_u64()
                .and_then(|value| u16::try_from(value).ok())
                .ok_or_else(|| {
                    BrowserError::new(
                        "Fetch.requestPaused responseStatusCode is not a valid HTTP status",
                    )
                })?,
        ),
        None => None,
    };
    Ok(FetchRequestPaused {
        request_id: required_string(&params, "requestId", "Fetch.requestPaused")?,
        request: FetchRequest {
            url: required_string(request, "url", "Fetch.requestPaused request")?,
            method: required_string(request, "method", "Fetch.requestPaused request")?,
            headers,
            post_data: optional_string(request, "postData", "Fetch.requestPaused request")?,
        },
        frame_id: required_string(&params, "frameId", "Fetch.requestPaused")?,
        resource_type: required_string(&params, "resourceType", "Fetch.requestPaused")?,
        response_error_reason: optional_string(
            &params,
            "responseErrorReason",
            "Fetch.requestPaused",
        )?,
        response_status_code,
        response_status_text: optional_string(
            &params,
            "responseStatusText",
            "Fetch.requestPaused",
        )?,
        network_id: optional_string(&params, "networkId", "Fetch.requestPaused")?,
        redirected_request_id: optional_string(
            &params,
            "redirectedRequestId",
            "Fetch.requestPaused",
        )?,
        raw_params: params,
    })
}

fn required_string(value: &Value, field: &str, operation: &str) -> Result<String, BrowserError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| BrowserError::new(format!("{operation} omitted {field}")))
}

fn optional_string(
    value: &Value,
    field: &str,
    operation: &str,
) -> Result<Option<String>, BrowserError> {
    match value.get(field) {
        Some(value) => {
            value.as_str().map(str::to_string).map(Some).ok_or_else(|| {
                BrowserError::new(format!("{operation} field {field} is not a string"))
            })
        }
        None => Ok(None),
    }
}

/// Authenticated by an unguessable path at the wake_test layer; this owner only serves immutable
/// in-memory resources over loopback.
pub struct ResourceOrigin {
    address: std::net::SocketAddr,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl ResourceOrigin {
    pub fn start(resources: BTreeMap<String, Vec<u8>>) -> Result<Self, BrowserError> {
        let listener = TcpListener::bind("127.0.0.1:0").map_err(|error| {
            BrowserError::new(format!("could not bind resource origin: {error}"))
        })?;
        listener.set_nonblocking(true).map_err(|error| {
            BrowserError::new(format!("could not configure resource origin: {error}"))
        })?;
        let address = listener.local_addr().map_err(|error| {
            BrowserError::new(format!("could not read resource origin: {error}"))
        })?;
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let worker = std::thread::Builder::new()
            .name("wake-test-origin".to_string())
            .spawn(move || {
                while !worker_stop.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((mut stream, _)) => serve_resource(&mut stream, &resources),
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(5));
                        }
                        Err(_) => break,
                    }
                }
            })
            .map_err(|error| {
                BrowserError::new(format!("could not start resource origin: {error}"))
            })?;
        Ok(Self {
            address,
            stop,
            worker: Some(worker),
        })
    }

    pub fn url(&self, path: &str) -> String {
        let path = path.trim_start_matches('/');
        format!("http://{}/{}", self.address, path)
    }
}

impl Drop for ResourceOrigin {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = TcpStream::connect(self.address);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn serve_resource(stream: &mut TcpStream, resources: &BTreeMap<String, Vec<u8>>) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
    let mut request = [0_u8; 8 * 1024];
    let Ok(read) = stream.read(&mut request) else {
        return;
    };
    let request = String::from_utf8_lossy(&request[..read]);
    let path = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/")
        .split('?')
        .next()
        .unwrap_or("/")
        .trim_start_matches('/');
    let (status, body) = resources
        .get(path)
        .map_or(("404 Not Found", b"not found".as_slice()), |body| {
            ("200 OK", body.as_slice())
        });
    let content_type = if path.ends_with(".js") || path.ends_with(".mjs") {
        "text/javascript; charset=utf-8"
    } else if path.ends_with(".html") {
        "text/html; charset=utf-8"
    } else if path.ends_with(".css") {
        "text/css; charset=utf-8"
    } else {
        "application/octet-stream"
    };
    let headers = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nContent-Type: {content_type}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(headers.as_bytes());
    let _ = stream.write_all(body);
    let _ = stream.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn start_cdp_server<R, F>(handler: F) -> (String, JoinHandle<R>)
    where
        R: Send + 'static,
        F: FnOnce(&mut WebSocket<TcpStream>) -> R + Send + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let worker = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut socket = tungstenite::accept(stream).unwrap();
            handler(&mut socket)
        });
        (format!("ws://{address}"), worker)
    }

    fn read_cdp_request(socket: &mut WebSocket<TcpStream>) -> Value {
        loop {
            if let Message::Text(text) = socket.read().unwrap() {
                return serde_json::from_str(text.as_str()).unwrap();
            }
        }
    }

    fn answer_cdp_request(socket: &mut WebSocket<TcpStream>, request: &Value) {
        socket
            .send(Message::Text(
                json!({
                    "id": request["id"],
                    "result": {},
                })
                .to_string()
                .into(),
            ))
            .unwrap();
    }

    fn test_page(connection: CdpConnection) -> BrowserPage {
        test_page_with_connection(Arc::new(connection))
    }

    fn test_page_with_connection(connection: Arc<CdpConnection>) -> BrowserPage {
        BrowserPage {
            connection,
            target_id: "target".to_string(),
            session_id: "page".to_string(),
            closed: true,
        }
    }

    #[test]
    fn explicit_missing_browser_is_a_locatable_error() {
        let missing = PathBuf::from("definitely-not-a-wake-browser");
        let error = detect_browser(Some(&missing)).unwrap_err();
        assert_eq!(error.code, "WAKE_TEST_BROWSER");
        assert_eq!(error.path.as_deref(), Some(missing.as_path()));
    }

    #[test]
    fn new_page_emulates_one_fixed_reduced_motion_profile() {
        let (url, worker) = start_cdp_server(|socket| {
            let mut requests = Vec::new();
            for _ in 0..8 {
                let request = read_cdp_request(socket);
                let result = match request["method"].as_str().unwrap() {
                    "Target.createTarget" => json!({"targetId": "target"}),
                    "Target.attachToTarget" => json!({"sessionId": "page"}),
                    _ => json!({}),
                };
                socket
                    .send(Message::Text(
                        json!({"id": request["id"], "result": result})
                            .to_string()
                            .into(),
                    ))
                    .unwrap();
                requests.push(request);
            }
            requests
        });
        let connection = Arc::new(CdpConnection::connect(&url, Duration::from_secs(1)).unwrap());
        let mut context = BrowserContext {
            connection,
            id: "context".to_string(),
            options: BrowserLaunchOptions {
                color_scheme: "dark".to_string(),
                ..BrowserLaunchOptions::default()
            },
            disposed: false,
        };
        let mut page = context.new_page("about:blank").unwrap();
        page.closed = true;
        context.disposed = true;
        drop(page);
        drop(context);
        let requests = worker.join().unwrap();

        let media = requests
            .iter()
            .find(|request| request["method"] == "Emulation.setEmulatedMedia")
            .expect("new pages must configure emulated media");
        assert_eq!(media["sessionId"], "page");
        assert_eq!(
            media["params"]["features"],
            json!([
                {"name": "prefers-color-scheme", "value": "dark"},
                {"name": "prefers-reduced-motion", "value": REDUCED_MOTION},
            ])
        );
        assert_eq!(REDUCED_MOTION, "reduce");
    }

    #[test]
    fn loopback_origin_serves_only_registered_resources() {
        let origin = ResourceOrigin::start(BTreeMap::from([(
            "suite.js".to_string(),
            b"export const answer = 42;".to_vec(),
        )]))
        .unwrap();
        let address = origin.address;
        let mut stream = TcpStream::connect(address).unwrap();
        stream
            .write_all(b"GET /suite.js HTTP/1.1\r\nHost: wake\r\nConnection: close\r\n\r\n")
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        assert!(response.starts_with("HTTP/1.1 200 OK"));
        assert!(response.ends_with("export const answer = 42;"));
    }

    #[test]
    fn cdp_command_deadline_cannot_be_starved_by_unrelated_events() {
        let (url, worker) = start_cdp_server(|socket| {
            let _request = read_cdp_request(socket);
            for sequence in 0..30 {
                let event = json!({
                    "method": "Runtime.consoleAPICalled",
                    "params": {"sequence": sequence},
                });
                if socket
                    .send(Message::Text(event.to_string().into()))
                    .is_err()
                {
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        });
        let connection = CdpConnection::connect(&url, Duration::from_secs(1)).unwrap();
        let started = Instant::now();
        let error = connection
            .command_with_timeout(
                None,
                "Runtime.evaluate",
                json!({}),
                Duration::from_millis(40),
            )
            .unwrap_err();
        let elapsed = started.elapsed();
        drop(connection);
        worker.join().unwrap();
        assert!(error.message.contains("Runtime.evaluate timed out"));
        assert!(
            elapsed < Duration::from_millis(300),
            "absolute command deadline was starved for {elapsed:?}"
        );
    }

    #[test]
    fn paused_fetch_can_be_resumed_while_evaluation_command_is_pending() {
        let (url, worker) = start_cdp_server(|socket| {
            let evaluation = read_cdp_request(socket);
            assert_eq!(evaluation["method"], "Runtime.evaluate");
            socket
                .send(Message::Text(
                    json!({
                        "method": "Fetch.requestPaused",
                        "sessionId": "page",
                        "params": {
                            "requestId": "blocked-fetch",
                            "request": {
                                "url": "https://wake.test/data",
                                "method": "GET",
                                "headers": {},
                            },
                            "frameId": "frame-1",
                            "resourceType": "Fetch",
                        },
                    })
                    .to_string()
                    .into(),
                ))
                .unwrap();

            // The evaluation response is deliberately withheld until a second caller resumes the
            // intercepted request on the same CDP socket.
            let continued = read_cdp_request(socket);
            assert_eq!(continued["method"], "Fetch.continueRequest");
            answer_cdp_request(socket, &continued);
            socket
                .send(Message::Text(
                    json!({
                        "id": evaluation["id"],
                        "result": {"result": {"value": "evaluation-finished"}},
                    })
                    .to_string()
                    .into(),
                ))
                .unwrap();
            continued
        });
        let connection = Arc::new(
            CdpConnection::connect(&url, Duration::from_secs(1)).expect("connect test CDP"),
        );
        let page = Arc::new(test_page_with_connection(Arc::clone(&connection)));
        let evaluation_page = Arc::clone(&page);
        let evaluator = std::thread::spawn(move || {
            evaluation_page.evaluate_with_timeout("fetch('/data')", Some(750))
        });
        let paused = page
            .wait_for_fetch_request(Duration::from_secs(1), &BrowserCancellationToken::new())
            .unwrap();
        let CdpEventWait::Event(paused) = paused else {
            panic!("expected a paused Fetch request");
        };
        page.continue_fetch_request(&paused.request_id).unwrap();
        assert_eq!(
            evaluator.join().unwrap().unwrap(),
            Value::String("evaluation-finished".to_string())
        );
        drop(page);
        drop(connection);
        let continued = worker.join().unwrap();
        assert_eq!(continued["sessionId"], "page");
        assert_eq!(continued["params"]["requestId"], "blocked-fetch");
    }

    #[test]
    fn cdp_event_wait_is_promptly_cancellable_without_incoming_messages() {
        let (url, worker) = start_cdp_server(|socket| {
            std::thread::sleep(Duration::from_millis(80));
            let request = read_cdp_request(socket);
            answer_cdp_request(socket, &request);
            request
        });
        let connection = CdpConnection::connect(&url, Duration::from_secs(1)).unwrap();
        let cancellation = BrowserCancellationToken::new();
        let canceller = cancellation.clone();
        let worker_cancel = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(35));
            canceller.cancel();
        });
        let started = Instant::now();
        let outcome = connection
            .wait_for_event(
                None,
                "Fetch.requestPaused",
                Duration::from_secs(2),
                &cancellation,
            )
            .unwrap();
        let elapsed = started.elapsed();
        worker_cancel.join().unwrap();
        connection
            .command(None, "Browser.getVersion", json!({}))
            .unwrap();
        drop(connection);
        let request = worker.join().unwrap();
        assert_eq!(outcome, CdpEventWait::Cancelled);
        assert_eq!(request["method"], "Browser.getVersion");
        assert!(
            elapsed < Duration::from_millis(250),
            "event cancellation took {elapsed:?}"
        );
    }

    #[test]
    fn fetch_event_wait_filters_sessions_preserves_events_and_continues_request() {
        let (url, worker) = start_cdp_server(|socket| {
            let enabled = read_cdp_request(socket);
            answer_cdp_request(socket, &enabled);
            socket
                .send(Message::Text(
                    json!({
                        "method": "Runtime.consoleAPICalled",
                        "sessionId": "page",
                        "params": {"type": "log"},
                    })
                    .to_string()
                    .into(),
                ))
                .unwrap();
            socket
                .send(Message::Text(
                    json!({
                        "method": "Fetch.requestPaused",
                        "sessionId": "other-page",
                        "params": {
                            "requestId": "wrong-session",
                            "request": {"url": "https://wrong.test", "method": "GET", "headers": {}},
                            "frameId": "other-frame",
                            "resourceType": "Document",
                        },
                    })
                    .to_string()
                    .into(),
                ))
                .unwrap();
            socket
                .send(Message::Text(
                    json!({
                        "method": "Fetch.requestPaused",
                        "sessionId": "page",
                        "params": {
                            "requestId": "request-7",
                            "request": {
                                "url": "https://wake.test/api",
                                "method": "POST",
                                "headers": {"accept": "application/json"},
                                "postData": "{}",
                            },
                            "frameId": "frame-1",
                            "resourceType": "Fetch",
                            "networkId": "network-7",
                        },
                    })
                    .to_string()
                    .into(),
                ))
                .unwrap();
            let continued = read_cdp_request(socket);
            answer_cdp_request(socket, &continued);
            let fulfilled = read_cdp_request(socket);
            answer_cdp_request(socket, &fulfilled);
            (enabled, continued, fulfilled)
        });
        let connection = CdpConnection::connect(&url, Duration::from_secs(1)).unwrap();
        let page = test_page(connection);
        page.enable_fetch_interception(
            &[FetchRequestPattern {
                url_pattern: "https://wake.test/*".to_string(),
                resource_type: Some("Fetch".to_string()),
                request_stage: FetchRequestStage::Request,
            }],
            false,
        )
        .unwrap();
        let outcome = page
            .wait_for_fetch_request(Duration::from_secs(1), &BrowserCancellationToken::new())
            .unwrap();
        let CdpEventWait::Event(paused) = outcome else {
            panic!("expected a paused request");
        };
        assert_eq!(paused.request_id, "request-7");
        assert_eq!(paused.request.url, "https://wake.test/api");
        assert_eq!(paused.request.method, "POST");
        assert_eq!(
            paused.request.headers.get("accept").map(String::as_str),
            Some("application/json")
        );
        assert_eq!(paused.request.post_data.as_deref(), Some("{}"));
        assert_eq!(paused.network_id.as_deref(), Some("network-7"));
        assert!(paused.raw_params().get("sessionId").is_none());
        assert!(page.take_event("Runtime.consoleAPICalled").is_some());
        page.continue_fetch_request(&paused.request_id).unwrap();
        let mut response = FetchFulfillResponse::new(201, b"created".to_vec());
        response.headers.push(FetchHeader {
            name: "content-type".to_string(),
            value: "text/plain".to_string(),
        });
        page.fulfill_fetch_request("request-8", &response).unwrap();
        drop(page);
        let (enabled, continued, fulfilled) = worker.join().unwrap();
        assert_eq!(enabled["method"], "Fetch.enable");
        assert_eq!(
            enabled["params"]["patterns"][0],
            json!({
                "urlPattern": "https://wake.test/*",
                "resourceType": "Fetch",
                "requestStage": "Request",
            })
        );
        assert_eq!(continued["method"], "Fetch.continueRequest");
        assert_eq!(continued["sessionId"], "page");
        assert_eq!(continued["params"]["requestId"], "request-7");
        assert_eq!(fulfilled["method"], "Fetch.fulfillRequest");
        assert_eq!(fulfilled["params"]["requestId"], "request-8");
        assert_eq!(fulfilled["params"]["responseCode"], 201);
        assert_eq!(fulfilled["params"]["body"], "Y3JlYXRlZA==");
    }

    #[test]
    fn typed_pointer_keyboard_and_text_input_emit_protocol_values() {
        let (url, worker) = start_cdp_server(|socket| {
            let mut requests = Vec::new();
            for _ in 0..7 {
                let request = read_cdp_request(socket);
                answer_cdp_request(socket, &request);
                requests.push(request);
            }
            requests
        });
        let connection = CdpConnection::connect(&url, Duration::from_secs(1)).unwrap();
        let page = test_page(connection);
        page.pointer_move(12.5, 24.0).unwrap();
        let modifiers = InputModifiers::CONTROL.union(InputModifiers::SHIFT);
        page.pointer_click(12.5, 24.0, PointerButton::Left, modifiers)
            .unwrap();
        page.pointer_wheel(12.5, 24.0, -3.0, 9.0, InputModifiers::NONE)
            .unwrap();
        page.press_key(&KeyboardInput::new("a", "KeyA").with_text("a"))
            .unwrap();
        page.insert_text("🦀").unwrap();
        drop(page);
        let requests = worker.join().unwrap();

        assert_eq!(requests[0]["method"], "Input.dispatchMouseEvent");
        assert_eq!(requests[0]["params"]["type"], "mouseMoved");
        assert_eq!(requests[1]["params"]["type"], "mousePressed");
        assert_eq!(requests[1]["params"]["button"], "left");
        assert_eq!(requests[1]["params"]["buttons"], 1);
        assert_eq!(requests[1]["params"]["modifiers"], 10);
        assert_eq!(requests[2]["params"]["type"], "mouseReleased");
        assert_eq!(requests[2]["params"]["buttons"], 0);
        assert_eq!(requests[3]["params"]["type"], "mouseWheel");
        assert_eq!(requests[3]["params"]["deltaX"], -3.0);
        assert_eq!(requests[3]["params"]["deltaY"], 9.0);
        assert_eq!(requests[4]["method"], "Input.dispatchKeyEvent");
        assert_eq!(requests[4]["params"]["type"], "keyDown");
        assert_eq!(requests[4]["params"]["key"], "a");
        assert_eq!(requests[4]["params"]["code"], "KeyA");
        assert_eq!(requests[4]["params"]["text"], "a");
        assert_eq!(requests[5]["params"]["type"], "keyUp");
        assert!(requests[5]["params"].get("text").is_none());
        assert_eq!(requests[6]["method"], "Input.insertText");
        assert_eq!(requests[6]["params"]["text"], "🦀");
        assert!(
            requests
                .iter()
                .all(|request| request["sessionId"] == "page")
        );
    }

    #[test]
    fn typed_file_input_resolves_selector_and_sends_owned_paths() {
        let (url, worker) = start_cdp_server(|socket| {
            let document = read_cdp_request(socket);
            socket
                .send(Message::Text(
                    json!({
                        "id": document["id"],
                        "result": {"root": {"nodeId": 7}},
                    })
                    .to_string()
                    .into(),
                ))
                .unwrap();
            let query = read_cdp_request(socket);
            socket
                .send(Message::Text(
                    json!({
                        "id": query["id"],
                        "result": {"nodeId": 9},
                    })
                    .to_string()
                    .into(),
                ))
                .unwrap();
            let assign = read_cdp_request(socket);
            answer_cdp_request(socket, &assign);
            (document, query, assign)
        });
        let fixture = tempfile::tempdir().unwrap();
        let upload = fixture.path().join("wake.txt");
        fs::write(&upload, b"Wake").unwrap();
        let connection = CdpConnection::connect(&url, Duration::from_secs(1)).unwrap();
        let page = test_page(connection);
        page.set_file_input_files("[data-wake-input=\"7\"]", std::slice::from_ref(&upload))
            .unwrap();
        drop(page);
        let (document, query, assign) = worker.join().unwrap();

        assert_eq!(document["method"], "DOM.getDocument");
        assert_eq!(query["method"], "DOM.querySelector");
        assert_eq!(query["params"]["nodeId"], 7);
        assert_eq!(query["params"]["selector"], "[data-wake-input=\"7\"]");
        assert_eq!(assign["method"], "DOM.setFileInputFiles");
        assert_eq!(assign["params"]["nodeId"], 9);
        assert_eq!(assign["params"]["files"], json!([upload]));
        assert!(
            [&document, &query, &assign]
                .iter()
                .all(|request| request["sessionId"] == "page")
        );
    }

    #[test]
    fn typed_screenshot_clip_emits_exact_protocol_values_and_owned_png() {
        let (url, worker) = start_cdp_server(|socket| {
            let request = read_cdp_request(socket);
            socket
                .send(Message::Text(
                    json!({
                        "id": request["id"],
                        "result": {"data": "AQIDBA=="},
                    })
                    .to_string()
                    .into(),
                ))
                .unwrap();
            request
        });
        let connection = CdpConnection::connect(&url, Duration::from_secs(1)).unwrap();
        let page = test_page(connection);
        let png = page
            .screenshot_png_with_clip(Some(&ScreenshotClip {
                x: 10.5,
                y: 20.25,
                width: 320.0,
                height: 180.0,
                scale: 1.0,
            }))
            .unwrap();
        drop(page);
        let request = worker.join().unwrap();

        assert_eq!(png, [1, 2, 3, 4]);
        assert_eq!(request["method"], "Page.captureScreenshot");
        assert_eq!(request["sessionId"], "page");
        assert_eq!(request["params"]["format"], "png");
        assert_eq!(request["params"]["fromSurface"], true);
        assert_eq!(request["params"]["captureBeyondViewport"], false);
        assert_eq!(
            request["params"]["clip"],
            json!({
                "x": 10.5,
                "y": 20.25,
                "width": 320.0,
                "height": 180.0,
                "scale": 1.0,
            })
        );
    }

    #[test]
    fn typed_termination_emits_exact_runtime_command() {
        let (url, worker) = start_cdp_server(|socket| {
            let request = read_cdp_request(socket);
            answer_cdp_request(socket, &request);
            request
        });
        let connection = CdpConnection::connect(&url, Duration::from_secs(1)).unwrap();
        let page = test_page(connection);
        page.terminate_execution().unwrap();
        drop(page);
        let request = worker.join().unwrap();

        assert_eq!(request["method"], "Runtime.terminateExecution");
        assert_eq!(request["sessionId"], "page");
        assert_eq!(request["params"], json!({}));
    }

    #[test]
    fn transport_bounded_evaluation_does_not_assign_execution_timeout_policy() {
        let (url, worker) = start_cdp_server(|socket| {
            let request = read_cdp_request(socket);
            socket
                .send(Message::Text(
                    json!({
                        "id": request["id"],
                        "result": {"result": {"value": "step-ack"}},
                    })
                    .to_string()
                    .into(),
                ))
                .unwrap();
            request
        });
        let connection = CdpConnection::connect(&url, Duration::from_secs(1)).unwrap();
        let page = test_page(connection);
        let value = page
            .evaluate_with_transport_timeout("globalThis.__wakeSchedulerRunStep('step-1')", 250)
            .unwrap();
        drop(page);
        let request = worker.join().unwrap();

        assert_eq!(value, "step-ack");
        assert_eq!(request["method"], "Runtime.evaluate");
        assert_eq!(request["sessionId"], "page");
        assert_eq!(
            request["params"],
            json!({
                "expression": "globalThis.__wakeSchedulerRunStep('step-1')",
                "awaitPromise": true,
                "returnByValue": true,
                "userGesture": true,
            })
        );
    }

    #[test]
    #[ignore = "requires an installed system Chromium browser"]
    fn termination_allows_reuse_but_preserves_queued_jobs() {
        let driver = BrowserDriver::launch(BrowserLaunchOptions::default()).unwrap();
        let context = driver.create_context().unwrap();
        let page = Arc::new(context.new_page("about:blank").unwrap());
        page.evaluate("globalThis.__wakeTerminatedJobLeaked = false")
            .unwrap();

        let evaluation_page = Arc::clone(&page);
        let evaluator = std::thread::spawn(move || {
            evaluation_page.evaluate(
                "console.log('wake-termination-started'); Promise.resolve().then(() => { globalThis.__wakeTerminatedJobLeaked = true }); for (;;) {}",
            )
        });
        let started = page
            .wait_for_event(
                "Runtime.consoleAPICalled",
                Duration::from_secs(2),
                &BrowserCancellationToken::new(),
            )
            .unwrap();
        assert!(matches!(started, CdpEventWait::Event(_)));

        page.terminate_execution().unwrap();
        let error = evaluator.join().unwrap().unwrap_err();
        assert_eq!(error.code, "WAKE_TEST_BROWSER");
        assert!(
            error.message.contains("terminated")
                || error.message.contains("Execution was terminated")
                || error.message.contains("Internal error"),
            "unexpected termination error: {error:?}"
        );

        let recovered = page
            .evaluate("({ answer: 6 * 7, leaked: globalThis.__wakeTerminatedJobLeaked })")
            .unwrap();
        assert_eq!(recovered["answer"], 42);
        // Chromium keeps Promise jobs queued before Runtime.terminateExecution. A caller may use
        // the page for diagnostics, but a test runner must destroy its BrowserContext instead of
        // executing another case in this realm.
        assert_eq!(recovered["leaked"], true);
    }

    #[test]
    #[ignore = "requires an installed system Chromium browser"]
    fn system_browser_cdp_vertical_spike() {
        let driver = BrowserDriver::launch(BrowserLaunchOptions::default()).unwrap();
        let context = driver.create_context().unwrap();
        let page = context.new_page("about:blank").unwrap();
        let identity = page
            .evaluate(
                "({ answer: 6 * 7, chromium: navigator.userAgent.includes('Chrome'), reducedMotion: matchMedia('(prefers-reduced-motion: reduce)').matches })",
            )
            .unwrap();
        assert_eq!(identity["answer"], 42);
        assert_eq!(identity["chromium"], true);
        assert_eq!(identity["reducedMotion"], true);

        page.evaluate(
            "document.body.innerHTML = '<input id=field style=\"position:fixed;left:0;top:0;width:200px;height:40px\">'; globalThis.inputEvents = []; const field = document.querySelector('#field'); for (const name of ['pointerdown', 'click', 'keydown', 'input']) field.addEventListener(name, event => inputEvents.push(event.type));",
        )
        .unwrap();
        page.pointer_click(10.0, 10.0, PointerButton::Left, InputModifiers::NONE)
            .unwrap();
        page.press_key(&KeyboardInput::new("a", "KeyA").with_text("a"))
            .unwrap();
        page.insert_text("🦀").unwrap();
        let input = page
            .evaluate("({value: document.querySelector('#field').value, events: inputEvents})")
            .unwrap();
        assert_eq!(input["value"], "a🦀");
        assert!(
            input["events"]
                .as_array()
                .unwrap()
                .iter()
                .any(|event| event == "pointerdown")
        );
        assert!(
            input["events"]
                .as_array()
                .unwrap()
                .iter()
                .any(|event| event == "keydown")
        );
        assert!(
            input["events"]
                .as_array()
                .unwrap()
                .iter()
                .any(|event| event == "input")
        );
    }
}
