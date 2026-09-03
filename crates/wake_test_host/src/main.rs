//! Crash-isolated process boundary for Wake's native test runtime.

use std::collections::{BTreeSet, HashSet};
use std::io::{BufWriter, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use notify::{EventKind, RecursiveMode, Watcher};
use wake_test::{
    TestCancellationToken, TestWatchPath, TestWatchPathRoles, TestWatchRequest, TestWatchSelection,
    TestWatchSuite, TestWorkspaceSession,
};
use wake_test_contract::protocol::{
    FrameDecoder, HOST_BUILD_ID, HostAck, HostCommand, HostError, HostEvent, HostHello,
    HostRequest, HostResponse, HostResponseBody, PROTOCOL_VERSION, WatchControl, write_frame,
};
use wake_test_contract::{TestOptions, TestRunResult, TestSuiteStatus, TestTerminationReason};

const SESSION_POLL_INTERVAL: Duration = Duration::from_millis(10);
const READER_POLL_INTERVAL: Duration = Duration::from_millis(50);
const WATCH_DEBOUNCE: Duration = Duration::from_millis(60);

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("wake-test-host: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let token = parse_token()?;
    let listener = TcpListener::bind("127.0.0.1:0")
        .map_err(|error| format!("could not bind loopback socket: {error}"))?;
    let hello = HostHello {
        protocol_version: PROTOCOL_VERSION,
        build_id: HOST_BUILD_ID.to_string(),
        address: listener
            .local_addr()
            .map_err(|error| format!("could not read loopback address: {error}"))?
            .to_string(),
        process_id: std::process::id(),
    };
    let stdout = std::io::stdout();
    let mut output = BufWriter::new(stdout.lock());
    serde_json::to_writer(&mut output, &hello)
        .map_err(|error| format!("could not serialize handshake: {error}"))?;
    output
        .write_all(b"\n")
        .and_then(|()| output.flush())
        .map_err(|error| format!("could not write handshake: {error}"))?;

    let (stream, _) = listener
        .accept()
        .map_err(|error| format!("could not accept session: {error}"))?;
    configure_stream(&stream)?;
    let _ = handle_session(stream, &token)?;
    Ok(())
}

fn parse_token() -> Result<String, String> {
    let mut arguments = std::env::args().skip(1);
    match (
        arguments.next().as_deref(),
        arguments.next(),
        arguments.next(),
    ) {
        (Some("--token"), Some(token), None) if token.len() >= 32 => Ok(token),
        _ => Err("expected --token followed by a random token".to_string()),
    }
}

fn configure_stream(stream: &TcpStream) -> Result<(), String> {
    let timeout = Some(Duration::from_secs(60 * 60));
    stream
        .set_read_timeout(timeout)
        .and_then(|()| stream.set_write_timeout(timeout))
        .map_err(|error| format!("could not configure session socket: {error}"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionAction {
    Continue,
    Shutdown,
}

type TestRunner = Arc<
    dyn Fn(TestOptions, Option<TestWatchRequest>, CancellationSignal) -> WorkerExecution
        + Send
        + Sync
        + 'static,
>;

fn handle_session(stream: TcpStream, token: &str) -> Result<SessionAction, String> {
    let workspace = Arc::new(std::sync::Mutex::new(None::<TestWorkspaceSession>));
    let runner: TestRunner = Arc::new(move |options, watch_request, cancellation| {
        let Ok(mut workspace) = workspace.lock() else {
            return WorkerExecution::from_error(WorkerError::Host(
                "test workspace lock is poisoned".to_string(),
            ));
        };
        let session = workspace.get_or_insert_with(|| TestWorkspaceSession::new(options.clone()));
        let outcome = match watch_request {
            Some(request) => session.run_watch(options, request, cancellation.0),
            None => session.run(options, cancellation.0),
        }
        .map_err(WorkerError::Test);
        WorkerExecution {
            outcome,
            watch_paths: session.watch_paths(),
        }
    });
    handle_session_with_runner(stream, token, runner)
}

fn handle_session_with_runner(
    mut stream: TcpStream,
    token: &str,
    runner: TestRunner,
) -> Result<SessionAction, String> {
    let reader_stream = stream
        .try_clone()
        .map_err(|error| format!("could not clone session socket: {error}"))?;
    let (incoming_sender, incoming_receiver) = mpsc::channel();
    let reader_stop = Arc::new(AtomicBool::new(false));
    let reader_stop_signal = reader_stop.clone();
    let reader = std::thread::Builder::new()
        .name("wake-test-host-reader".to_string())
        .spawn(move || {
            read_session_frames(reader_stream, &incoming_sender, &reader_stop_signal);
        })
        .map_err(|error| format!("could not create session reader: {error}"))?;
    let (completion_sender, completion_receiver) = mpsc::channel();
    let (watch_sender, watch_receiver) = mpsc::channel();

    let result = run_session_loop(
        &mut stream,
        token,
        runner,
        &incoming_receiver,
        &completion_sender,
        &completion_receiver,
        &watch_sender,
        &watch_receiver,
    );

    // A short reader timeout observes this signal even when socket shutdown does not wake a
    // cloned blocking socket on the current platform.
    reader_stop.store(true, Ordering::Release);
    let _ = stream.shutdown(Shutdown::Both);
    if reader.join().is_err() && result.is_ok() {
        return Err("test-host session reader panicked".to_string());
    }
    result
}

enum IncomingFrame {
    Request(Box<HostRequest>),
    Closed,
    DecodeError(String),
}

fn read_session_frames(
    mut stream: TcpStream,
    sender: &mpsc::Sender<IncomingFrame>,
    stop: &AtomicBool,
) {
    if let Err(error) = stream.set_read_timeout(Some(READER_POLL_INTERVAL)) {
        let _ = sender.send(IncomingFrame::DecodeError(format!(
            "could not configure session reader: {error}"
        )));
        return;
    }
    let mut decoder = FrameDecoder::new();
    let mut chunk = [0_u8; 16 * 1024];
    loop {
        if stop.load(Ordering::Acquire) {
            return;
        }
        match stream.read(&mut chunk) {
            Ok(0) => {
                let terminal = if decoder.is_empty() {
                    IncomingFrame::Closed
                } else {
                    IncomingFrame::DecodeError("session ended in the middle of a frame".to_string())
                };
                let _ = sender.send(terminal);
                return;
            }
            Ok(length) => {
                decoder.push(&chunk[..length]);
                loop {
                    match decoder.decode_next::<HostRequest>() {
                        Ok(Some(request)) => {
                            if sender
                                .send(IncomingFrame::Request(Box::new(request)))
                                .is_err()
                            {
                                return;
                            }
                        }
                        Ok(None) => break,
                        Err(error) => {
                            let _ = sender.send(IncomingFrame::DecodeError(error.to_string()));
                            return;
                        }
                    }
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => {
                let _ = sender.send(IncomingFrame::DecodeError(error.to_string()));
                return;
            }
        }
    }
}

fn run_session_loop(
    stream: &mut TcpStream,
    token: &str,
    runner: TestRunner,
    incoming_receiver: &mpsc::Receiver<IncomingFrame>,
    completion_sender: &mpsc::Sender<WorkerCompletion>,
    completion_receiver: &mpsc::Receiver<WorkerCompletion>,
    watch_sender: &mpsc::Sender<WatchNotification>,
    watch_receiver: &mpsc::Receiver<WatchNotification>,
) -> Result<SessionAction, String> {
    let mut state = SessionState::default();
    let mut seen_request_ids = HashSet::new();
    let mut sequence = 0_u64;

    let result = run_session_loop_inner(
        stream,
        token,
        runner,
        incoming_receiver,
        completion_sender,
        completion_receiver,
        watch_sender,
        watch_receiver,
        &mut state,
        &mut seen_request_ids,
        &mut sequence,
    );
    if result.is_err() {
        cleanup_active_run(&mut state, completion_receiver);
    }
    result
}

fn run_session_loop_inner(
    stream: &mut TcpStream,
    token: &str,
    runner: TestRunner,
    incoming_receiver: &mpsc::Receiver<IncomingFrame>,
    completion_sender: &mpsc::Sender<WorkerCompletion>,
    completion_receiver: &mpsc::Receiver<WorkerCompletion>,
    watch_sender: &mpsc::Sender<WatchNotification>,
    watch_receiver: &mpsc::Receiver<WatchNotification>,
    state: &mut SessionState,
    seen_request_ids: &mut HashSet<u64>,
    sequence: &mut u64,
) -> Result<SessionAction, String> {
    loop {
        let first_incoming = match incoming_receiver.recv_timeout(SESSION_POLL_INTERVAL) {
            Ok(incoming) => Some(incoming),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            Err(mpsc::RecvTimeoutError::Disconnected) => Some(IncomingFrame::Closed),
        };

        if let Some(incoming) = first_incoming {
            let mut queued = Some(incoming);
            while let Some(incoming) = queued {
                match incoming {
                    IncomingFrame::Request(request) => {
                        match handle_request(
                            stream,
                            token,
                            runner.clone(),
                            *request,
                            state,
                            seen_request_ids,
                            completion_sender,
                            watch_sender,
                            sequence,
                        )? {
                            RequestAction::Continue => {}
                            RequestAction::Close => {
                                cleanup_active_run(state, completion_receiver);
                                return Ok(SessionAction::Continue);
                            }
                            RequestAction::Shutdown => return Ok(SessionAction::Shutdown),
                        }
                    }
                    IncomingFrame::Closed => {
                        cleanup_active_run(state, completion_receiver);
                        return Ok(SessionAction::Continue);
                    }
                    IncomingFrame::DecodeError(error) => {
                        cleanup_active_run(state, completion_receiver);
                        return Err(format!("could not decode session frame: {error}"));
                    }
                }
                queued = incoming_receiver.try_recv().ok();
            }
        }

        while let Ok(notification) = watch_receiver.try_recv() {
            if let WatchNotificationAction::Fatal(fatal) =
                state.enqueue_watch_notification(notification)
            {
                write_response(
                    stream,
                    fatal.request_id,
                    HostResponseBody::WatchRunError {
                        watch_id: fatal.watch_id,
                        run_id: fatal.run_id,
                        started: fatal.started,
                        error: fatal.error,
                    },
                    sequence,
                )?;
                cleanup_active_run(state, completion_receiver);
                return Err("filesystem watcher became unhealthy".to_string());
            }
        }
        // Filesystem invalidation wins a same-tick race with worker completion. This marks the
        // just-finished run as obsolete instead of briefly publishing stale output as authoritative.
        while let Ok(completion) = completion_receiver.try_recv() {
            finish_worker(stream, state, completion, sequence)?;
        }
        maybe_start_watch_run(stream, state, runner.clone(), completion_sender, sequence)?;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RequestAction {
    Continue,
    Close,
    Shutdown,
}

fn handle_request(
    stream: &mut TcpStream,
    token: &str,
    runner: TestRunner,
    request: HostRequest,
    state: &mut SessionState,
    seen_request_ids: &mut HashSet<u64>,
    completion_sender: &mpsc::Sender<WorkerCompletion>,
    watch_sender: &mpsc::Sender<WatchNotification>,
    sequence: &mut u64,
) -> Result<RequestAction, String> {
    if !constant_time_equal(request.token.as_bytes(), token.as_bytes()) {
        write_error(
            stream,
            request.request_id,
            None,
            host_error("WAKE_TEST_HOST", "test-host authentication failed", None),
            sequence,
        )?;
        return Ok(RequestAction::Close);
    }
    if request.protocol_version != PROTOCOL_VERSION {
        write_error(
            stream,
            request.request_id,
            None,
            host_error(
                "WAKE_TEST_HOST",
                format!(
                    "protocol version mismatch: client {}, host {}",
                    request.protocol_version, PROTOCOL_VERSION
                ),
                None,
            ),
            sequence,
        )?;
        return Ok(RequestAction::Close);
    }
    if request.build_id != HOST_BUILD_ID {
        write_error(
            stream,
            request.request_id,
            None,
            host_error(
                "WAKE_TEST_HOST",
                format!(
                    "test-host build mismatch: client {}, host {}",
                    request.build_id, HOST_BUILD_ID
                ),
                None,
            ),
            sequence,
        )?;
        return Ok(RequestAction::Close);
    }
    if !seen_request_ids.insert(request.request_id) {
        write_error(
            stream,
            request.request_id,
            None,
            host_error(
                "WAKE_TEST_HOST",
                format!("duplicate request id {}", request.request_id),
                None,
            ),
            sequence,
        )?;
        return Ok(RequestAction::Continue);
    }

    match request.command {
        HostCommand::Run { run_id, options } => {
            let cancellation = match state.begin_run(run_id.clone(), request.request_id) {
                Ok(cancellation) => cancellation,
                Err(error) => {
                    write_error(stream, request.request_id, Some(run_id), error, sequence)?;
                    return Ok(RequestAction::Continue);
                }
            };
            match spawn_test_worker(
                run_id.clone(),
                *options,
                None,
                cancellation,
                runner,
                completion_sender.clone(),
            ) {
                Ok(worker) => state.attach_worker(&run_id, worker),
                Err(error) => {
                    state.discard_run(&run_id);
                    write_error(
                        stream,
                        request.request_id,
                        Some(run_id),
                        host_error("WAKE_TEST_HOST", error, None),
                        sequence,
                    )?;
                    return Ok(RequestAction::Continue);
                }
            }
            write_response(
                stream,
                request.request_id,
                HostResponseBody::Ack {
                    command: HostAck::Run {
                        run_id: run_id.clone(),
                    },
                },
                sequence,
            )?;
            write_response(
                stream,
                request.request_id,
                HostResponseBody::Event {
                    event: Box::new(HostEvent::RunStart {
                        run_id,
                        watching: state.watch.is_some(),
                    }),
                },
                sequence,
            )?;
        }
        HostCommand::Cancel { run_id } => match state.cancel_run(&run_id) {
            Ok(()) => write_response(
                stream,
                request.request_id,
                HostResponseBody::Ack {
                    command: HostAck::Cancel { run_id },
                },
                sequence,
            )?,
            Err(error) => write_error(stream, request.request_id, Some(run_id), error, sequence)?,
        },
        HostCommand::StartWatch { watch_id, options } => match state.start_watch(
            &watch_id,
            request.request_id,
            *options,
            watch_sender.clone(),
        ) {
            Ok(()) => write_response(
                stream,
                request.request_id,
                HostResponseBody::Ack {
                    command: HostAck::StartWatch {
                        watch_id: watch_id.clone(),
                    },
                },
                sequence,
            )
            .and_then(|()| {
                let root = state
                    .watch
                    .as_ref()
                    .map(|watch| watch.root.to_string_lossy().replace('\\', "/"))
                    .unwrap_or_default();
                write_response(
                    stream,
                    request.request_id,
                    HostResponseBody::Event {
                        event: Box::new(HostEvent::WatchReady {
                            watch_id: watch_id.clone(),
                            root,
                        }),
                    },
                    sequence,
                )
            })?,
            Err(error) => {
                write_error(stream, request.request_id, None, error, sequence)?;
            }
        },
        HostCommand::StopWatch { watch_id } => match state.stop_watch(&watch_id) {
            Ok(()) => write_response(
                stream,
                request.request_id,
                HostResponseBody::Ack {
                    command: HostAck::StopWatch { watch_id },
                },
                sequence,
            )?,
            Err(error) => {
                write_error(stream, request.request_id, None, error, sequence)?;
            }
        },
        HostCommand::WatchControl { watch_id, control } => {
            match state.watch_control(&watch_id, control) {
                Ok(()) => write_response(
                    stream,
                    request.request_id,
                    HostResponseBody::Ack {
                        command: HostAck::WatchControl { watch_id },
                    },
                    sequence,
                )?,
                Err(error) => {
                    write_error(stream, request.request_id, None, error, sequence)?;
                }
            }
        }
        HostCommand::Shutdown => {
            if let Err(error) = state.can_shutdown() {
                write_error(stream, request.request_id, None, error, sequence)?;
                return Ok(RequestAction::Continue);
            }
            write_response(
                stream,
                request.request_id,
                HostResponseBody::Ack {
                    command: HostAck::Shutdown,
                },
                sequence,
            )?;
            return Ok(RequestAction::Shutdown);
        }
    }
    Ok(RequestAction::Continue)
}

#[derive(Debug, Clone)]
struct CancellationSignal(TestCancellationToken);

impl CancellationSignal {
    fn new() -> Self {
        Self(TestCancellationToken::default())
    }

    fn cancel(&self) {
        self.0.cancel();
    }

    fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }
}

#[derive(Default)]
struct SessionState {
    active_run: Option<ActiveRun>,
    watch: Option<ActiveWatch>,
    next_watch_run: u64,
    next_watch_generation: u64,
}

struct ActiveRun {
    run_id: String,
    request_id: u64,
    cancellation: CancellationSignal,
    cancellation_requested: bool,
    watch_id: Option<String>,
    watch_context_id: Option<String>,
    failed_cache_scope: FailedCacheScope,
    watch_restart: bool,
    updates_snapshots: bool,
    worker: Option<JoinHandle<()>>,
}

struct ActiveWatch {
    id: String,
    request_id: u64,
    root: std::path::PathBuf,
    options: TestOptions,
    watcher: notify::RecommendedWatcher,
    sender: mpsc::Sender<WatchNotification>,
    registrations: Vec<TestWatchPath>,
    registration_view: Arc<std::sync::RwLock<Vec<TestWatchPath>>>,
    generation: u64,
    owned_outputs: BTreeSet<std::path::PathBuf>,
    pending_paths: BTreeSet<std::path::PathBuf>,
    changed_at: Option<Instant>,
    rerun_requested: bool,
    rescan_required: bool,
    mode: WatchMode,
    update_snapshots_once: bool,
    last_failed: std::collections::BTreeMap<HostSuiteIdentity, TestWatchSuite>,
    has_completed_run: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FailedCacheScope {
    Full,
    Partial,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct HostSuiteIdentity {
    path: std::path::PathBuf,
    project: Option<String>,
}

#[derive(Debug, Clone)]
enum WatchMode {
    Configured,
    All,
    Failed,
    Path(String),
    Name(String),
}

enum WatchNotification {
    Paths {
        watch_id: String,
        generation: u64,
        paths: Vec<std::path::PathBuf>,
        rediscover: bool,
    },
    Rescan {
        watch_id: String,
        generation: u64,
    },
    Fatal {
        watch_id: String,
        generation: u64,
        message: String,
        paths: Vec<std::path::PathBuf>,
    },
}

enum WatchNotificationAction {
    Continue,
    Fatal(FatalWatchNotification),
}

struct FatalWatchNotification {
    request_id: u64,
    watch_id: String,
    run_id: Option<String>,
    started: bool,
    error: HostError,
}

impl SessionState {
    fn begin_run(
        &mut self,
        run_id: String,
        request_id: u64,
    ) -> Result<CancellationSignal, HostError> {
        validate_id("run", &run_id)?;
        if let Some(active) = &self.active_run {
            return Err(busy_error(format!(
                "run `{}` is still active",
                active.run_id
            )));
        }
        let cancellation = CancellationSignal::new();
        let watch_context_id = self.watch.as_ref().map(|watch| watch.id.clone());
        self.active_run = Some(ActiveRun {
            run_id,
            request_id,
            cancellation: cancellation.clone(),
            cancellation_requested: false,
            watch_id: None,
            watch_context_id,
            failed_cache_scope: FailedCacheScope::Full,
            watch_restart: false,
            updates_snapshots: false,
            worker: None,
        });
        Ok(cancellation)
    }

    fn attach_worker(&mut self, run_id: &str, worker: JoinHandle<()>) {
        let active = self
            .active_run
            .as_mut()
            .expect("a worker is only attached to an accepted run");
        assert_eq!(active.run_id, run_id, "worker must match the active run");
        active.worker = Some(worker);
    }

    fn discard_run(&mut self, run_id: &str) {
        if self
            .active_run
            .as_ref()
            .is_some_and(|active| active.run_id == run_id)
        {
            self.active_run = None;
        }
    }

    fn finish_run(&mut self, run_id: &str) -> Option<ActiveRun> {
        if self
            .active_run
            .as_ref()
            .is_some_and(|active| active.run_id == run_id)
        {
            self.active_run.take()
        } else {
            None
        }
    }

    fn cancel_run(&mut self, run_id: &str) -> Result<(), HostError> {
        let Some(active) = self.active_run.as_mut() else {
            return Err(unknown_run_error(run_id));
        };
        if active.run_id != run_id {
            return Err(unknown_run_error(run_id));
        }
        if active.cancellation_requested {
            return Err(busy_error(format!(
                "cancellation for run `{run_id}` was already requested"
            )));
        }
        active.cancellation_requested = true;
        active.cancellation.cancel();
        Ok(())
    }

    fn start_watch(
        &mut self,
        watch_id: &str,
        request_id: u64,
        options: TestOptions,
        sender: mpsc::Sender<WatchNotification>,
    ) -> Result<(), HostError> {
        validate_id("watch", watch_id)?;
        if let Some(active) = &self.active_run {
            return Err(busy_error(format!(
                "cannot start watch while run `{}` is active",
                active.run_id
            )));
        }
        if let Some(active_watch) = &self.watch {
            return Err(busy_error(format!(
                "watch `{}` is already active",
                active_watch.id
            )));
        }
        let root = options
            .root
            .clone()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .canonicalize()
            .map_err(|error| {
                host_error(
                    "WAKE_TEST_DISCOVERY",
                    format!("could not resolve watch root: {error}"),
                    options
                        .root
                        .as_ref()
                        .map(|path| path.to_string_lossy().into_owned()),
                )
            })?;
        let process_root = std::env::current_dir()
            .map_err(|error| host_error("WAKE_TEST_HOST", error.to_string(), None))?;
        let process_root = absolute_watch_path(&process_root, &process_root);
        let owned_outputs = options
            .output
            .as_ref()
            .map(|path| absolute_watch_path(&process_root, path))
            .into_iter()
            .collect::<BTreeSet<_>>();
        let registrations = vec![TestWatchPath {
            path: root.clone(),
            recursive: true,
            roles: TestWatchPathRoles::PROJECT_TREE,
        }];
        let registration_view = Arc::new(std::sync::RwLock::new(registrations.clone()));
        self.next_watch_generation = self.next_watch_generation.saturating_add(1);
        let generation = self.next_watch_generation;
        let watcher = create_watcher(
            watch_id,
            generation,
            &root,
            &owned_outputs,
            Arc::clone(&registration_view),
            sender.clone(),
        )?;
        self.watch = Some(ActiveWatch {
            id: watch_id.to_string(),
            request_id,
            root,
            options,
            watcher,
            sender,
            registrations,
            registration_view,
            generation,
            owned_outputs,
            pending_paths: BTreeSet::new(),
            changed_at: Some(Instant::now()),
            rerun_requested: true,
            rescan_required: false,
            mode: WatchMode::Configured,
            update_snapshots_once: false,
            last_failed: std::collections::BTreeMap::new(),
            has_completed_run: false,
        });
        Ok(())
    }

    fn stop_watch(&mut self, watch_id: &str) -> Result<(), HostError> {
        if let Some(active) = self.active_run.as_ref()
            && active.watch_id.as_deref() != Some(watch_id)
        {
            return Err(busy_error(format!(
                "cannot stop watch while run `{}` is active",
                active.run_id
            )));
        }
        match self.watch.as_ref() {
            Some(active_watch) if active_watch.id == watch_id => {
                if let Some(active) = self.active_run.as_mut()
                    && active.watch_id.as_deref() == Some(watch_id)
                {
                    active.cancellation_requested = true;
                    active.cancellation.cancel();
                }
                self.watch = None;
                Ok(())
            }
            _ => Err(unknown_watch_error(watch_id)),
        }
    }

    fn watch_control(&mut self, watch_id: &str, control: WatchControl) -> Result<(), HostError> {
        let Some(watch) = self.watch.as_mut().filter(|watch| watch.id == watch_id) else {
            return Err(unknown_watch_error(watch_id));
        };
        match control {
            WatchControl::All => watch.mode = WatchMode::All,
            WatchControl::Failed => watch.mode = WatchMode::Failed,
            WatchControl::Path { pattern } => watch.mode = WatchMode::Path(pattern),
            WatchControl::Name { pattern } => watch.mode = WatchMode::Name(pattern),
            WatchControl::UpdateSnapshots => watch.update_snapshots_once = true,
            WatchControl::Rerun => {}
        }
        watch.rerun_requested = true;
        watch.changed_at = Some(Instant::now() - WATCH_DEBOUNCE);
        if let Some(active) = self.active_run.as_mut()
            && active.watch_id.as_deref() == Some(watch_id)
        {
            active.watch_restart = true;
            active.cancellation.cancel();
        }
        Ok(())
    }

    fn enqueue_watch_notification(
        &mut self,
        notification: WatchNotification,
    ) -> WatchNotificationAction {
        let Some(watch) = self.watch.as_mut() else {
            return WatchNotificationAction::Continue;
        };
        let (watch_id, generation) = match &notification {
            WatchNotification::Paths {
                watch_id,
                generation,
                ..
            }
            | WatchNotification::Rescan {
                watch_id,
                generation,
            }
            | WatchNotification::Fatal {
                watch_id,
                generation,
                ..
            } => (watch_id, *generation),
        };
        if watch.id != *watch_id || watch.generation != generation {
            return WatchNotificationAction::Continue;
        }
        let interrupt_active_run = match notification {
            WatchNotification::Paths {
                paths, rediscover, ..
            } => {
                if rediscover {
                    watch.rescan_required = true;
                }
                let interrupt = paths
                    .iter()
                    .any(|path| !is_baseline_watch_input(&watch.registrations, path));
                watch.pending_paths.extend(paths);
                interrupt
            }
            WatchNotification::Rescan { .. } => {
                watch.pending_paths.clear();
                watch.rescan_required = true;
                true
            }
            WatchNotification::Fatal { message, paths, .. } => {
                let run_id = self
                    .active_run
                    .as_ref()
                    .filter(|run| run.watch_id.as_deref() == Some(watch.id.as_str()))
                    .map(|run| run.run_id.clone());
                if let Some(active) = self.active_run.as_mut() {
                    active.cancellation.cancel();
                }
                return WatchNotificationAction::Fatal(FatalWatchNotification {
                    request_id: watch.request_id,
                    watch_id: watch.id.clone(),
                    started: run_id.is_some(),
                    run_id,
                    error: host_error(
                        "WAKE_TEST_HOST",
                        format!("filesystem watcher error: {message}"),
                        paths
                            .first()
                            .map(|path| path.to_string_lossy().into_owned()),
                    ),
                });
            }
        };
        watch.changed_at = Some(Instant::now());
        if interrupt_active_run
            && let Some(active) = self.active_run.as_mut()
            && active.watch_id.as_deref() == Some(watch.id.as_str())
        {
            active.watch_restart = true;
            active.cancellation.cancel();
        }
        WatchNotificationAction::Continue
    }

    fn can_shutdown(&self) -> Result<(), HostError> {
        if let Some(active) = &self.active_run {
            return Err(busy_error(format!(
                "cannot shut down while run `{}` is active",
                active.run_id
            )));
        }
        if let Some(watch) = &self.watch {
            return Err(busy_error(format!(
                "cannot shut down while watch `{}` is active",
                watch.id
            )));
        }
        Ok(())
    }

    fn refresh_watch_registrations(
        &mut self,
        paths: Vec<TestWatchPath>,
    ) -> Result<bool, HostError> {
        let next_generation = self.next_watch_generation.saturating_add(1);
        let Some(watch) = self.watch.as_mut() else {
            return Ok(false);
        };
        let mut registrations = normalize_watch_registrations(&watch.root, paths);
        registrations.push(TestWatchPath {
            path: watch.root.clone(),
            recursive: true,
            roles: TestWatchPathRoles::PROJECT_TREE,
        });
        registrations = normalize_watch_registrations(&watch.root, registrations);
        if registrations == watch.registrations {
            return Ok(false);
        }
        let old_anchors = watch_anchors(&watch.root, &watch.registrations)?;
        let new_anchors = watch_anchors(&watch.root, &registrations)?;
        if old_anchors == new_anchors {
            *watch.registration_view.write().map_err(|_| {
                host_error(
                    "WAKE_TEST_HOST",
                    "watch registration lock is poisoned",
                    None,
                )
            })? = registrations.clone();
            watch.registrations = registrations;
            return Ok(false);
        }
        let generation = next_generation;
        let registration_view = Arc::new(std::sync::RwLock::new(registrations.clone()));
        let watcher = create_watcher(
            &watch.id,
            generation,
            &watch.root,
            &watch.owned_outputs,
            Arc::clone(&registration_view),
            watch.sender.clone(),
        )?;
        // The replacement is fully subscribed before the previous generation is dropped. Queued
        // callbacks retain their old generation and are ignored by the session state machine.
        watch.watcher = watcher;
        watch.registrations = registrations;
        watch.registration_view = registration_view;
        watch.generation = generation;
        self.next_watch_generation = generation;
        watch.rescan_required = true;
        watch.changed_at = Some(Instant::now() - WATCH_DEBOUNCE);
        Ok(true)
    }
}

fn maybe_start_watch_run(
    stream: &mut TcpStream,
    state: &mut SessionState,
    runner: TestRunner,
    completion_sender: &mpsc::Sender<WorkerCompletion>,
    sequence: &mut u64,
) -> Result<(), String> {
    if state.active_run.is_some() {
        return Ok(());
    }
    let Some(watch) = state.watch.as_mut() else {
        return Ok(());
    };
    let Some(changed_at) = watch.changed_at else {
        return Ok(());
    };
    if changed_at.elapsed() < WATCH_DEBOUNCE {
        return Ok(());
    }

    let watch_id = watch.id.clone();
    let request_id = watch.request_id;
    let mode = watch.mode.clone();
    let changed_paths = std::mem::take(&mut watch.pending_paths)
        .into_iter()
        .collect::<Vec<_>>();
    let rerun_requested = std::mem::take(&mut watch.rerun_requested);
    let rescan_required = std::mem::take(&mut watch.rescan_required);
    let updates_snapshots = watch.update_snapshots_once;
    watch.changed_at = None;
    prune_missing_failed_suites(watch);
    let mut options = watch.options.clone();
    options.watch = true;
    if updates_snapshots {
        options.update_snapshots = Some("all".to_string());
    }
    let (selection, failed_cache_scope) = match mode {
        WatchMode::Configured if rescan_required && changed_paths.is_empty() => {
            options.changed = false;
            options.related.clear();
            (TestWatchSelection::All, FailedCacheScope::Full)
        }
        WatchMode::Configured if !changed_paths.is_empty() => {
            (TestWatchSelection::Affected, FailedCacheScope::Partial)
        }
        WatchMode::Configured => {
            let scope = if options.changed || !options.related.is_empty() {
                FailedCacheScope::Partial
            } else {
                FailedCacheScope::Full
            };
            (TestWatchSelection::Configured, scope)
        }
        WatchMode::All => {
            options.changed = false;
            options.related.clear();
            options.patterns.clear();
            options.name_pattern = None;
            (TestWatchSelection::All, FailedCacheScope::Full)
        }
        WatchMode::Failed if !watch.has_completed_run => {
            (TestWatchSelection::Configured, FailedCacheScope::Partial)
        }
        WatchMode::Failed => {
            options.allow_no_tests = true;
            (
                TestWatchSelection::Suites(watch.last_failed.values().cloned().collect()),
                FailedCacheScope::Partial,
            )
        }
        WatchMode::Path(pattern) => {
            options.changed = false;
            options.related.clear();
            options.patterns = vec![pattern];
            options.name_pattern = None;
            (TestWatchSelection::All, FailedCacheScope::Partial)
        }
        WatchMode::Name(pattern) => {
            options.changed = false;
            options.related.clear();
            options.patterns.clear();
            options.name_pattern = Some(pattern);
            if changed_paths.is_empty() {
                (TestWatchSelection::All, FailedCacheScope::Partial)
            } else {
                (TestWatchSelection::Affected, FailedCacheScope::Partial)
            }
        }
    };
    if !rerun_requested && !rescan_required && changed_paths.is_empty() {
        return Ok(());
    }
    let request = TestWatchRequest {
        invalidated_paths: changed_paths,
        selection,
        rediscover: rescan_required,
    };

    state.next_watch_run = state.next_watch_run.saturating_add(1);
    let run_id = format!("{watch_id}-run-{}", state.next_watch_run);
    let cancellation = match state.begin_run(run_id.clone(), request_id) {
        Ok(cancellation) => cancellation,
        Err(error) => {
            return write_error(stream, request_id, Some(run_id), error, sequence);
        }
    };
    if let Some(active) = state.active_run.as_mut() {
        active.watch_id = Some(watch_id.clone());
        active.updates_snapshots = updates_snapshots;
        active.failed_cache_scope = failed_cache_scope;
    }
    match spawn_test_worker(
        run_id.clone(),
        options,
        Some(request),
        cancellation,
        runner,
        completion_sender.clone(),
    ) {
        Ok(worker) => state.attach_worker(&run_id, worker),
        Err(error) => {
            state.discard_run(&run_id);
            return write_response(
                stream,
                request_id,
                HostResponseBody::WatchRunError {
                    watch_id,
                    run_id: None,
                    started: false,
                    error: host_error("WAKE_TEST_HOST", error, None),
                },
                sequence,
            );
        }
    }
    write_response(
        stream,
        request_id,
        HostResponseBody::Event {
            event: Box::new(HostEvent::RunStart {
                run_id,
                watching: true,
            }),
        },
        sequence,
    )
}

fn normalize_watch_registrations(
    root: &std::path::Path,
    paths: Vec<TestWatchPath>,
) -> Vec<TestWatchPath> {
    let mut normalized: std::collections::BTreeMap<_, (bool, TestWatchPathRoles)> =
        std::collections::BTreeMap::new();
    for registration in paths {
        let path = absolute_watch_path(root, &registration.path);
        normalized
            .entry(path)
            .and_modify(|metadata| {
                metadata.0 |= registration.recursive;
                metadata.1 = metadata.1.union(registration.roles);
            })
            .or_insert((registration.recursive, registration.roles));
    }
    normalized
        .into_iter()
        .map(|(path, (recursive, roles))| TestWatchPath {
            path,
            recursive,
            roles,
        })
        .collect()
}

fn create_watcher(
    watch_id: &str,
    generation: u64,
    root: &std::path::Path,
    owned_outputs: &BTreeSet<std::path::PathBuf>,
    registrations: Arc<std::sync::RwLock<Vec<TestWatchPath>>>,
    sender: mpsc::Sender<WatchNotification>,
) -> Result<notify::RecommendedWatcher, HostError> {
    let callback_watch_id = watch_id.to_string();
    let callback_root = root.to_path_buf();
    let callback_outputs = owned_outputs.clone();
    let initial_registrations = registrations
        .read()
        .map_err(|_| {
            host_error(
                "WAKE_TEST_HOST",
                "watch registration lock is poisoned",
                None,
            )
        })?
        .clone();
    let anchors = watch_anchors(root, &initial_registrations)?;
    let callback_anchors = anchors.clone();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let notification = match event {
            Ok(event) if event.need_rescan() => WatchNotification::Rescan {
                watch_id: callback_watch_id.clone(),
                generation,
            },
            Ok(event) if !matches!(event.kind, EventKind::Access(_)) => {
                let callback_registrations = match registrations.read() {
                    Ok(registrations) => registrations,
                    Err(_) => {
                        let _ = sender.send(WatchNotification::Fatal {
                            watch_id: callback_watch_id.clone(),
                            generation,
                            message: "watch registration lock is poisoned".to_string(),
                            paths: Vec::new(),
                        });
                        return;
                    }
                };
                let rediscover = matches!(
                    event.kind,
                    EventKind::Any
                        | EventKind::Other
                        | EventKind::Create(_)
                        | EventKind::Remove(_)
                        | EventKind::Modify(notify::event::ModifyKind::Name(_))
                ) || event.paths.iter().any(|path| path.is_dir());
                let paths = event
                    .paths
                    .into_iter()
                    .filter_map(|path| {
                        let path = absolute_watch_path(&callback_root, &path);
                        let relevant =
                            watch_event_matches(
                                &callback_root,
                                &callback_registrations,
                                &callback_anchors,
                                &path,
                            ) && should_watch_path(&callback_root, &callback_outputs, &path)
                                && !is_owned_test_artifact(&callback_registrations, &path);
                        relevant.then_some(path)
                    })
                    .collect::<Vec<_>>();
                if paths.is_empty() {
                    return;
                }
                WatchNotification::Paths {
                    watch_id: callback_watch_id.clone(),
                    generation,
                    paths,
                    rediscover,
                }
            }
            Ok(_) => return,
            Err(error) => WatchNotification::Fatal {
                watch_id: callback_watch_id.clone(),
                generation,
                message: error.to_string(),
                paths: error.paths,
            },
        };
        let _ = sender.send(notification);
    })
    .map_err(|error| host_error("WAKE_TEST_HOST", error.to_string(), None))?;
    for (anchor, recursive) in anchors {
        watcher
            .watch(
                &anchor,
                if recursive {
                    RecursiveMode::Recursive
                } else {
                    RecursiveMode::NonRecursive
                },
            )
            .map_err(|error| {
                host_error(
                    "WAKE_TEST_HOST",
                    format!("could not subscribe {}: {error}", anchor.display()),
                    Some(anchor.to_string_lossy().into_owned()),
                )
            })?;
    }
    Ok(watcher)
}

fn watch_anchors(
    root: &std::path::Path,
    registrations: &[TestWatchPath],
) -> Result<std::collections::BTreeMap<std::path::PathBuf, bool>, HostError> {
    let mut anchors = std::collections::BTreeMap::new();
    for registration in registrations {
        let input = absolute_watch_path(root, &registration.path);
        let anchor = if input.is_dir() {
            input.clone()
        } else {
            nearest_existing_directory(&input).ok_or_else(|| {
                host_error(
                    "WAKE_TEST_HOST",
                    format!("no existing parent can be watched for {}", input.display()),
                    Some(input.to_string_lossy().into_owned()),
                )
            })?
        };
        if anchor.parent().is_none() && anchor != root {
            return Err(host_error(
                "WAKE_TEST_HOST",
                format!(
                    "refusing to subscribe a filesystem root for external input {}",
                    input.display()
                ),
                Some(input.to_string_lossy().into_owned()),
            ));
        }
        let recursive = registration.recursive && anchor == input;
        anchors
            .entry(anchor)
            .and_modify(|existing| *existing |= recursive)
            .or_insert(recursive);
    }
    let recursive_anchors = anchors
        .iter()
        .filter_map(|(anchor, recursive)| recursive.then_some(anchor.clone()))
        .collect::<Vec<_>>();
    anchors.retain(|anchor, _| {
        !recursive_anchors
            .iter()
            .any(|ancestor| ancestor != anchor && anchor.starts_with(ancestor))
    });
    Ok(anchors)
}

fn nearest_existing_directory(path: &std::path::Path) -> Option<std::path::PathBuf> {
    let mut candidate = if path.is_file() { path.parent()? } else { path };
    loop {
        if candidate.is_dir() {
            return Some(
                candidate
                    .canonicalize()
                    .unwrap_or_else(|_| candidate.to_path_buf()),
            );
        }
        candidate = candidate.parent()?;
    }
}

fn watch_event_matches(
    root: &std::path::Path,
    registrations: &[TestWatchPath],
    anchors: &std::collections::BTreeMap<std::path::PathBuf, bool>,
    path: &std::path::Path,
) -> bool {
    let path = absolute_watch_path(root, path);
    path.starts_with(root)
        || anchors.iter().any(|(anchor, recursive)| {
            path == *anchor
                || if *recursive {
                    path.starts_with(anchor)
                } else {
                    path.parent() == Some(anchor.as_path())
                }
        })
        || registrations.iter().any(|registration| {
            if registration.recursive {
                path.starts_with(&registration.path)
            } else {
                path == registration.path
            }
        })
}

fn should_watch_path(
    root: &std::path::Path,
    owned_outputs: &BTreeSet<std::path::PathBuf>,
    path: &std::path::Path,
) -> bool {
    // Windows temp roots can pass through a junction (for example the per-user Temp alias).
    // Compare every path in the same canonical-or-lexical identity space, including missing
    // generated files, otherwise an output beneath `coverage/` can look unrelated to its root.
    let normalized_root = absolute_watch_path(root, root);
    let absolute = absolute_watch_path(&normalized_root, path);
    let generated_root = [
        normalized_root.join(".git"),
        normalized_root.join("target"),
        normalized_root.join("coverage"),
    ]
    .into_iter()
    .any(|directory| absolute.starts_with(directory));
    let owned_output = owned_outputs
        .iter()
        .any(|output| absolute_watch_path(&normalized_root, output) == absolute);
    !generated_root && !owned_output
}

fn is_owned_test_artifact(registrations: &[TestWatchPath], path: &std::path::Path) -> bool {
    let file_name = path.file_name().and_then(|name| name.to_str());
    registrations.iter().any(|registration| {
        if !registration
            .roles
            .contains(TestWatchPathRoles::BASELINE_INPUT)
        {
            return false;
        }
        let baseline_root = if registration.recursive {
            registration.path.as_path()
        } else {
            registration.path.parent().unwrap_or(&registration.path)
        };
        (path.starts_with(baseline_root)
            && path
                .components()
                .any(|component| matches!(component.as_os_str().to_str(), Some("__diffs__"))))
            || (path.starts_with(baseline_root)
                && file_name.is_some_and(|name| {
                    name.starts_with(".wake-snapshot-") || name.starts_with(".wake-screenshot-")
                }))
    })
}

fn absolute_watch_path(root: &std::path::Path, path: &std::path::Path) -> std::path::PathBuf {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    let path = lexical_normalize_watch_path(&path);
    if let Ok(canonical) = path.canonicalize() {
        return canonical;
    }
    let mut ancestor = path.as_path();
    let mut missing = Vec::new();
    while !ancestor.exists() {
        let Some(name) = ancestor.file_name() else {
            return path;
        };
        missing.push(name.to_os_string());
        let Some(parent) = ancestor.parent() else {
            return path;
        };
        ancestor = parent;
    }
    let mut resolved = ancestor
        .canonicalize()
        .unwrap_or_else(|_| ancestor.to_path_buf());
    for component in missing.into_iter().rev() {
        resolved.push(component);
    }
    resolved
}

fn lexical_normalize_watch_path(path: &std::path::Path) -> std::path::PathBuf {
    let mut normalized = std::path::PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if matches!(
                    normalized.components().next_back(),
                    Some(std::path::Component::Normal(_))
                ) {
                    normalized.pop();
                }
            }
            std::path::Component::Prefix(_)
            | std::path::Component::RootDir
            | std::path::Component::Normal(_) => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

fn is_baseline_watch_input(registrations: &[TestWatchPath], path: &std::path::Path) -> bool {
    registrations.iter().any(|registration| {
        registration
            .roles
            .contains(TestWatchPathRoles::BASELINE_INPUT)
            && if registration.recursive {
                path.starts_with(&registration.path)
            } else {
                path == registration.path
            }
    })
}

struct WorkerCompletion {
    run_id: String,
    outcome: Result<TestRunResult, WorkerError>,
    watch_paths: Vec<TestWatchPath>,
}

struct WorkerExecution {
    outcome: Result<TestRunResult, WorkerError>,
    watch_paths: Vec<TestWatchPath>,
}

impl WorkerExecution {
    #[cfg(test)]
    fn from_result(result: TestRunResult) -> Self {
        Self {
            outcome: Ok(result),
            watch_paths: Vec::new(),
        }
    }

    fn from_error(error: WorkerError) -> Self {
        Self {
            outcome: Err(error),
            watch_paths: Vec::new(),
        }
    }
}

fn spawn_test_worker(
    run_id: String,
    options: TestOptions,
    watch_request: Option<TestWatchRequest>,
    cancellation: CancellationSignal,
    runner: TestRunner,
    completion_sender: mpsc::Sender<WorkerCompletion>,
) -> Result<JoinHandle<()>, String> {
    std::thread::Builder::new()
        .name("wake-test-worker".to_string())
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                runner(options, watch_request, cancellation)
            }))
            .unwrap_or_else(|payload| {
                let message = payload
                    .downcast_ref::<&str>()
                    .map(|value| (*value).to_string())
                    .or_else(|| payload.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "unknown panic payload".to_string());
                WorkerExecution::from_error(WorkerError::Host(format!(
                    "test worker panicked: {message}"
                )))
            });
            let _ = completion_sender.send(WorkerCompletion {
                run_id,
                outcome: outcome.outcome,
                watch_paths: outcome.watch_paths,
            });
        })
        .map_err(|error| format!("could not create test worker: {error}"))
}

fn finish_worker(
    stream: &mut TcpStream,
    state: &mut SessionState,
    completion: WorkerCompletion,
    sequence: &mut u64,
) -> Result<(), String> {
    let WorkerCompletion {
        run_id,
        mut outcome,
        watch_paths,
    } = completion;
    let Some(mut active) = state.finish_run(&run_id) else {
        return Err(format!("received completion for unknown run `{}`", run_id));
    };
    if let Some(worker) = active.worker.take() {
        worker.join().map_err(|_| {
            format!(
                "worker for run `{}` panicked after completion",
                active.run_id
            )
        })?;
    }

    if !watch_paths.is_empty()
        && let Err(error) = state.refresh_watch_registrations(watch_paths)
    {
        outcome = Err(WorkerError::HostError(error));
    }

    match outcome {
        Ok(mut result) => {
            result.run_id.clone_from(&active.run_id);
            if active.watch_restart {
                result.success = false;
                result.termination_reason = TestTerminationReason::WatchRestart;
            } else if active.cancellation.is_cancelled() {
                result.success = false;
                result.termination_reason = TestTerminationReason::Cancelled;
            }
            for suite in &result.suites {
                for test in &suite.tests {
                    write_response(
                        stream,
                        active.request_id,
                        HostResponseBody::Event {
                            event: Box::new(HostEvent::TestCaseResult {
                                run_id: active.run_id.clone(),
                                suite_id: suite.id.clone(),
                                result: Box::new(test.clone()),
                            }),
                        },
                        sequence,
                    )?;
                }
                write_response(
                    stream,
                    active.request_id,
                    HostResponseBody::Event {
                        event: Box::new(HostEvent::SuiteResult {
                            run_id: active.run_id.clone(),
                            result: Box::new(suite.clone()),
                        }),
                    },
                    sequence,
                )?;
            }
            for diagnostic in &result.diagnostics {
                write_response(
                    stream,
                    active.request_id,
                    HostResponseBody::Event {
                        event: Box::new(HostEvent::Diagnostic {
                            run_id: Some(active.run_id.clone()),
                            diagnostic: Box::new(diagnostic.clone()),
                        }),
                    },
                    sequence,
                )?;
            }
            if matches!(
                result.termination_reason,
                TestTerminationReason::Completed
                    | TestTerminationReason::Bail
                    | TestTerminationReason::Timeout
            ) && let Some(watch_context_id) = active.watch_context_id.as_deref()
                && let Some(watch) = state
                    .watch
                    .as_mut()
                    .filter(|watch| watch.id == watch_context_id)
            {
                let scope = if result.termination_reason == TestTerminationReason::Completed {
                    active.failed_cache_scope
                } else {
                    FailedCacheScope::Partial
                };
                update_failed_suites(watch, &result, scope);
            }
            if let Some(watch_id) = active.watch_id {
                if let Some(watch) = state.watch.as_mut().filter(|watch| watch.id == watch_id)
                    && active.updates_snapshots
                    && !active.watch_restart
                {
                    watch.update_snapshots_once = false;
                }
                write_response(
                    stream,
                    active.request_id,
                    HostResponseBody::Event {
                        event: Box::new(HostEvent::RunComplete {
                            watch_id,
                            run_id: active.run_id,
                            result: Box::new(result),
                        }),
                    },
                    sequence,
                )
            } else {
                write_response(
                    stream,
                    active.request_id,
                    HostResponseBody::Result {
                        run_id: active.run_id,
                        result: Box::new(result),
                    },
                    sequence,
                )
            }
        }
        Err(error) => {
            let path = error.path().map(|path| path.to_string_lossy().into_owned());
            let code = error.code().to_string();
            let message = error.to_string();
            let error = host_error(code, message, path);
            if let Some(watch_id) = active.watch_id {
                write_response(
                    stream,
                    active.request_id,
                    HostResponseBody::WatchRunError {
                        watch_id,
                        run_id: Some(active.run_id),
                        started: true,
                        error,
                    },
                    sequence,
                )?;
                Err("watch run failed without a partial result".to_string())
            } else {
                write_error(
                    stream,
                    active.request_id,
                    Some(active.run_id),
                    error,
                    sequence,
                )?;
                Err("test run failed after RunStart without a result".to_string())
            }
        }
    }
}

fn update_failed_suites(watch: &mut ActiveWatch, result: &TestRunResult, scope: FailedCacheScope) {
    watch.has_completed_run = true;
    if scope == FailedCacheScope::Full {
        watch.last_failed.clear();
    }
    for suite in &result.suites {
        let selector = TestWatchSuite {
            path: watch.root.join(&suite.path),
            project: suite.project.clone(),
        };
        let identity = HostSuiteIdentity {
            path: selector.path.clone(),
            project: selector.project.clone(),
        };
        watch.last_failed.remove(&identity);
        if suite.status == TestSuiteStatus::Failed {
            watch.last_failed.insert(identity, selector);
        }
    }
}

fn prune_missing_failed_suites(watch: &mut ActiveWatch) {
    watch
        .last_failed
        .retain(|identity, _| identity.path.is_file());
}

fn cleanup_active_run(
    state: &mut SessionState,
    completion_receiver: &mpsc::Receiver<WorkerCompletion>,
) {
    let Some(active) = state.active_run.as_ref() else {
        return;
    };
    let run_id = active.run_id.clone();
    active.cancellation.cancel();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let timeout = deadline.saturating_duration_since(Instant::now());
        if timeout.is_zero() {
            let _ = state.finish_run(&run_id);
            return;
        }
        match completion_receiver.recv_timeout(timeout) {
            Ok(completion) if completion.run_id == run_id => {
                if let Some(mut active) = state.finish_run(&run_id)
                    && let Some(worker) = active.worker.take()
                {
                    let _ = worker.join();
                }
                return;
            }
            Ok(_) => {}
            Err(_) => {
                let _ = state.finish_run(&run_id);
                return;
            }
        }
    }
}

enum WorkerError {
    Test(wake_test::TestError),
    Host(String),
    HostError(HostError),
}

impl WorkerError {
    fn code(&self) -> &str {
        match self {
            Self::Test(error) => error.code(),
            Self::Host(_) => "WAKE_TEST_HOST",
            Self::HostError(error) => &error.code,
        }
    }

    fn path(&self) -> Option<&std::path::Path> {
        match self {
            Self::Test(error) => error.path(),
            Self::Host(_) => None,
            Self::HostError(error) => error.path.as_deref().map(std::path::Path::new),
        }
    }
}

impl std::fmt::Display for WorkerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Test(error) => error.fmt(formatter),
            Self::Host(message) => formatter.write_str(message),
            Self::HostError(error) => formatter.write_str(&error.message),
        }
    }
}

fn validate_id(kind: &str, value: &str) -> Result<(), HostError> {
    if value.trim().is_empty() {
        Err(host_error(
            "WAKE_TEST_HOST",
            format!("{kind} id must not be empty"),
            None,
        ))
    } else {
        Ok(())
    }
}

fn busy_error(message: impl Into<String>) -> HostError {
    host_error("WAKE_TEST_BUSY", message, None)
}

fn unknown_run_error(run_id: &str) -> HostError {
    host_error(
        "WAKE_TEST_UNKNOWN_RUN",
        format!("run `{run_id}` is not active"),
        None,
    )
}

fn unknown_watch_error(watch_id: &str) -> HostError {
    host_error(
        "WAKE_TEST_UNKNOWN_WATCH",
        format!("watch `{watch_id}` is not active"),
        None,
    )
}

fn host_error(
    code: impl Into<String>,
    message: impl Into<String>,
    path: Option<String>,
) -> HostError {
    HostError {
        code: code.into(),
        message: message.into(),
        path,
    }
}

fn write_error(
    stream: &mut TcpStream,
    request_id: u64,
    run_id: Option<String>,
    error: HostError,
    sequence: &mut u64,
) -> Result<(), String> {
    write_response(
        stream,
        request_id,
        HostResponseBody::Error { run_id, error },
        sequence,
    )
}

fn write_response(
    stream: &mut TcpStream,
    request_id: u64,
    body: HostResponseBody,
    sequence: &mut u64,
) -> Result<(), String> {
    *sequence = sequence
        .checked_add(1)
        .ok_or_else(|| "test-host response sequence overflowed".to_string())?;
    let response = HostResponse {
        protocol_version: PROTOCOL_VERSION,
        build_id: HOST_BUILD_ID.to_string(),
        request_id,
        sequence: *sequence,
        body,
    };
    write_frame(stream, &response).map_err(|error| format!("could not encode response: {error}"))
}

fn constant_time_equal(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| difference | left ^ right)
        == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use wake_test_contract::protocol::{HostResponseBody, read_frame};
    use wake_test_contract::{
        TestEnvironmentInfo, TestEnvironmentKind, TestSuiteResult, TestSuiteStatus,
        TestTerminationReason,
    };

    const TOKEN: &str = "0123456789abcdef0123456789abcdef";

    fn request(request_id: u64, token: &str, command: HostCommand) -> HostRequest {
        HostRequest {
            protocol_version: PROTOCOL_VERSION,
            build_id: HOST_BUILD_ID.to_string(),
            token: token.to_string(),
            request_id,
            command,
        }
    }

    fn connect_session(runner: TestRunner) -> (TcpStream, JoinHandle<SessionAction>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            configure_stream(&stream).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            handle_session_with_runner(stream, TOKEN, runner).unwrap()
        });
        let client = TcpStream::connect(address).unwrap();
        configure_stream(&client).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        client
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        (client, server)
    }

    fn connect_session_result(
        runner: TestRunner,
    ) -> (TcpStream, JoinHandle<Result<SessionAction, String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            configure_stream(&stream).unwrap();
            handle_session_with_runner(stream, TOKEN, runner)
        });
        let client = TcpStream::connect(address).unwrap();
        configure_stream(&client).unwrap();
        (client, server)
    }

    fn unused_runner() -> TestRunner {
        Arc::new(|_, _, _| panic!("watch-only session must not start a worker"))
    }

    fn watch_options(root: &std::path::Path) -> TestOptions {
        TestOptions {
            root: Some(root.to_path_buf()),
            ..TestOptions::default()
        }
    }

    fn test_suite(path: &str, project: Option<&str>, status: TestSuiteStatus) -> TestSuiteResult {
        TestSuiteResult {
            id: format!("{path}-{}", project.unwrap_or("root")),
            path: path.to_string(),
            name: None,
            project: project.map(str::to_string),
            environment: None,
            status,
            duration_ms: 0,
            tests: Vec::new(),
            failures: Vec::new(),
            snapshot: None,
        }
    }

    fn read_response(stream: &mut TcpStream) -> HostResponse {
        read_frame(stream).unwrap()
    }

    fn assert_error(response: HostResponse, code: &str) {
        let HostResponseBody::Error { error, .. } = response.body else {
            panic!("expected an error response");
        };
        assert_eq!(error.code, code);
    }

    #[test]
    fn state_machine_reports_busy_and_unknown_identifiers() {
        let root = tempfile::tempdir().unwrap();
        let (watch_sender, _watch_receiver) = mpsc::channel();
        let mut state = SessionState::default();
        assert_eq!(
            state.stop_watch("missing").unwrap_err().code,
            "WAKE_TEST_UNKNOWN_WATCH"
        );
        assert_eq!(
            state
                .watch_control("missing", WatchControl::Rerun)
                .unwrap_err()
                .code,
            "WAKE_TEST_UNKNOWN_WATCH"
        );
        state
            .start_watch(
                "watch-1",
                1,
                watch_options(root.path()),
                watch_sender.clone(),
            )
            .unwrap();
        let generation = state.watch.as_ref().unwrap().generation;
        let WatchNotificationAction::Fatal(fatal) =
            state.enqueue_watch_notification(WatchNotification::Fatal {
                watch_id: "watch-1".to_string(),
                generation,
                message: "synthetic native watcher failure".to_string(),
                paths: Vec::new(),
            })
        else {
            panic!("watcher failures must be fatal host notifications");
        };
        assert_eq!(fatal.request_id, 1);
        assert_eq!(fatal.error.code, "WAKE_TEST_HOST");
        assert!(
            fatal
                .error
                .message
                .contains("synthetic native watcher failure")
        );
        assert_eq!(
            state
                .start_watch("watch-2", 2, watch_options(root.path()), watch_sender)
                .unwrap_err()
                .code,
            "WAKE_TEST_BUSY"
        );
        assert_eq!(
            state.stop_watch("watch-2").unwrap_err().code,
            "WAKE_TEST_UNKNOWN_WATCH"
        );
        let cancellation = state.begin_run("run-1".to_string(), 1).unwrap();
        assert_eq!(
            state.stop_watch("watch-1").unwrap_err().code,
            "WAKE_TEST_BUSY"
        );
        state.active_run.as_mut().unwrap().watch_id = Some("watch-1".to_string());
        assert!(!cancellation.is_cancelled());
        state.enqueue_watch_notification(WatchNotification::Paths {
            watch_id: "watch-1".to_string(),
            generation,
            paths: vec![root.path().join("view.tsx")],
            rediscover: false,
        });
        assert!(cancellation.is_cancelled());
        assert!(state.active_run.as_ref().unwrap().watch_restart);
        assert_eq!(
            state.begin_run("run-2".to_string(), 2).unwrap_err().code,
            "WAKE_TEST_BUSY"
        );
        assert_eq!(
            state.cancel_run("run-2").unwrap_err().code,
            "WAKE_TEST_UNKNOWN_RUN"
        );
        state.cancel_run("run-1").unwrap();
        assert_eq!(
            state.cancel_run("run-1").unwrap_err().code,
            "WAKE_TEST_BUSY"
        );
        state.finish_run("run-1").unwrap();
        state.stop_watch("watch-1").unwrap();
        state.can_shutdown().unwrap();
    }

    #[test]
    fn watcher_filters_only_reserved_root_artifacts_not_same_named_source_directories() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path();
        let report = root.join("artifacts/results.json");
        let outputs = BTreeSet::from([report.clone()]);
        assert!(!should_watch_path(
            root,
            &outputs,
            &root.join("coverage/index.html")
        ));
        assert!(!should_watch_path(
            root,
            &outputs,
            &root.join("target/debug/wake")
        ));
        assert!(!should_watch_path(root, &outputs, &root.join(".git/index")));
        assert!(!should_watch_path(root, &outputs, &report));
        assert!(should_watch_path(
            root,
            &outputs,
            &root.join("src/coverage/math.ts")
        ));
        assert!(should_watch_path(
            root,
            &outputs,
            &root.join("packages/target/view.tsx")
        ));
        assert!(should_watch_path(
            root,
            &outputs,
            &root.join("__snapshots__/view.test.tsx.snap")
        ));
        let screenshot_directory = root.join("__screenshots__");
        assert!(should_watch_path(
            root,
            &outputs,
            &screenshot_directory.join("__diffs__/view.diff.html")
        ));
        assert!(is_owned_test_artifact(
            &[TestWatchPath {
                path: screenshot_directory,
                recursive: true,
                roles: TestWatchPathRoles::PROJECT_TREE.union(TestWatchPathRoles::BASELINE_INPUT),
            }],
            &root.join("__screenshots__/__diffs__/view.diff.html")
        ));
        assert!(should_watch_path(
            root,
            &outputs,
            &root.join("src/__diffs__/view.tsx")
        ));
    }

    #[test]
    fn normalized_watch_registrations_union_all_roles_for_one_physical_path() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().canonicalize().unwrap();
        let registrations = normalize_watch_registrations(
            &root,
            vec![
                TestWatchPath {
                    path: root.clone(),
                    recursive: true,
                    roles: TestWatchPathRoles::PROJECT_TREE,
                },
                TestWatchPath {
                    path: root.clone(),
                    recursive: false,
                    roles: TestWatchPathRoles::COMPILER_INPUT,
                },
                TestWatchPath {
                    path: root.clone(),
                    recursive: false,
                    roles: TestWatchPathRoles::BASELINE_INPUT,
                },
            ],
        );
        assert_eq!(registrations.len(), 1);
        let registration = &registrations[0];
        assert!(registration.recursive);
        assert!(
            registration
                .roles
                .contains(TestWatchPathRoles::PROJECT_TREE)
        );
        assert!(
            registration
                .roles
                .contains(TestWatchPathRoles::COMPILER_INPUT)
        );
        assert!(
            registration
                .roles
                .contains(TestWatchPathRoles::BASELINE_INPUT)
        );
        assert!(is_baseline_watch_input(
            &registrations,
            &root.join("__snapshots__/view.test.tsx.snap")
        ));
        assert!(is_owned_test_artifact(
            &registrations,
            &root.join("__snapshots__/.wake-snapshot-stage")
        ));
    }

    #[test]
    fn external_registration_matches_parent_children_and_rejects_old_generations() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("app");
        let external = fixture.path().join("shared");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&external).unwrap();
        let missing = external.join("generated/dependency.ts");
        let registrations = normalize_watch_registrations(
            &root,
            vec![TestWatchPath {
                path: missing,
                recursive: false,
                roles: TestWatchPathRoles::COMPILER_INPUT,
            }],
        );
        let anchors = watch_anchors(&root, &registrations).unwrap();
        let external = external.canonicalize().unwrap();
        assert_eq!(anchors.get(&external), Some(&false));
        assert!(watch_event_matches(
            &root,
            &registrations,
            &anchors,
            &external.join("generated")
        ));
        assert!(!watch_event_matches(
            &root,
            &registrations,
            &anchors,
            &fixture.path().join("unrelated/file.ts")
        ));

        let (sender, _receiver) = mpsc::channel();
        let mut state = SessionState::default();
        state
            .start_watch("generation", 1, watch_options(&root), sender)
            .unwrap();
        let generation = state.watch.as_ref().unwrap().generation;
        assert!(matches!(
            state.enqueue_watch_notification(WatchNotification::Paths {
                watch_id: "generation".to_string(),
                generation: generation.saturating_sub(1),
                paths: vec![root.join("stale.ts")],
                rediscover: false,
            }),
            WatchNotificationAction::Continue
        ));
        assert!(state.watch.as_ref().unwrap().pending_paths.is_empty());
        state.enqueue_watch_notification(WatchNotification::Rescan {
            watch_id: "generation".to_string(),
            generation,
        });
        assert!(state.watch.as_ref().unwrap().rescan_required);
    }

    #[test]
    fn failed_watch_cache_is_project_aware_and_partial_runs_merge() {
        let root = tempfile::tempdir().unwrap();
        let (sender, _receiver) = mpsc::channel();
        let mut state = SessionState::default();
        state
            .start_watch("failed-cache", 1, watch_options(root.path()), sender)
            .unwrap();
        let environment = TestEnvironmentInfo {
            kind: TestEnvironmentKind::Dom,
            react: None,
            react_dom: None,
            v8: "test-v8".to_string(),
            browser: None,
        };
        let mut initial = TestRunResult::empty(
            "initial-run".to_string(),
            "seed".to_string(),
            environment.clone(),
        );
        initial.suites = vec![
            test_suite("same.test.ts", Some("alpha"), TestSuiteStatus::Failed),
            test_suite("same.test.ts", Some("beta"), TestSuiteStatus::Passed),
            test_suite("other.test.ts", None, TestSuiteStatus::Failed),
        ];
        update_failed_suites(
            state.watch.as_mut().unwrap(),
            &initial,
            FailedCacheScope::Full,
        );
        assert_eq!(state.watch.as_ref().unwrap().last_failed.len(), 2);

        let mut partial =
            TestRunResult::empty("partial-run".to_string(), "seed".to_string(), environment);
        partial.suites = vec![test_suite(
            "same.test.ts",
            Some("beta"),
            TestSuiteStatus::Passed,
        )];
        update_failed_suites(
            state.watch.as_mut().unwrap(),
            &partial,
            FailedCacheScope::Partial,
        );
        let failed = state
            .watch
            .as_ref()
            .unwrap()
            .last_failed
            .values()
            .collect::<Vec<_>>();
        assert_eq!(failed.len(), 2);
        assert!(
            failed
                .iter()
                .any(|suite| suite.project.as_deref() == Some("alpha"))
        );
        assert!(
            failed
                .iter()
                .any(|suite| suite.path.ends_with("other.test.ts"))
        );
    }

    #[test]
    fn failed_watch_cache_evicts_deleted_and_renamed_suite_paths() {
        let root = tempfile::tempdir().unwrap();
        let deleted = root.path().join("deleted.test.ts");
        let renamed = root.path().join("renamed.test.ts");
        let replacement = root.path().join("replacement.test.ts");
        std::fs::write(&deleted, "").unwrap();
        std::fs::write(&renamed, "").unwrap();

        let (sender, _receiver) = mpsc::channel();
        let mut state = SessionState::default();
        state
            .start_watch(
                "failed-cache-eviction",
                1,
                watch_options(root.path()),
                sender,
            )
            .unwrap();
        let environment = TestEnvironmentInfo {
            kind: TestEnvironmentKind::Dom,
            react: None,
            react_dom: None,
            v8: "test-v8".to_string(),
            browser: None,
        };
        let mut initial = TestRunResult::empty(
            "initial-run".to_string(),
            "seed".to_string(),
            environment.clone(),
        );
        initial.suites = vec![
            test_suite("deleted.test.ts", None, TestSuiteStatus::Failed),
            test_suite("renamed.test.ts", Some("client"), TestSuiteStatus::Failed),
        ];
        let watch = state.watch.as_mut().unwrap();
        update_failed_suites(watch, &initial, FailedCacheScope::Full);
        assert_eq!(watch.last_failed.len(), 2);

        std::fs::remove_file(deleted).unwrap();
        std::fs::rename(renamed, &replacement).unwrap();
        let replacement_identity = replacement.canonicalize().unwrap();
        prune_missing_failed_suites(watch);
        assert!(watch.last_failed.is_empty());

        let mut rerun = TestRunResult::empty("rerun".to_string(), "seed".to_string(), environment);
        rerun.suites = vec![test_suite(
            "replacement.test.ts",
            Some("client"),
            TestSuiteStatus::Failed,
        )];
        update_failed_suites(watch, &rerun, FailedCacheScope::Partial);
        assert_eq!(watch.last_failed.len(), 1);
        assert!(
            watch
                .last_failed
                .values()
                .any(|suite| suite.path == replacement_identity)
        );
    }

    #[test]
    fn one_connection_handles_watch_lifecycle_and_shutdown_in_order() {
        let root = tempfile::tempdir().unwrap();
        let (mut client, server) = connect_session(unused_runner());
        write_frame(
            &mut client,
            &request(
                1,
                TOKEN,
                HostCommand::StartWatch {
                    watch_id: "watch-1".to_string(),
                    options: Box::new(watch_options(root.path())),
                },
            ),
        )
        .unwrap();
        let start = read_response(&mut client);
        assert_eq!(start.sequence, 1);
        assert!(matches!(
            start.body,
            HostResponseBody::Ack {
                command: HostAck::StartWatch { watch_id }
            } if watch_id == "watch-1"
        ));
        let ready = read_response(&mut client);
        assert_eq!(ready.sequence, 2);
        assert!(matches!(
            ready.body,
            HostResponseBody::Event {
                event
            } if matches!(&*event, HostEvent::WatchReady { watch_id, .. } if watch_id == "watch-1")
        ));

        write_frame(
            &mut client,
            &request(
                2,
                TOKEN,
                HostCommand::StopWatch {
                    watch_id: "watch-1".to_string(),
                },
            ),
        )
        .unwrap();
        let stop = read_response(&mut client);
        assert_eq!(stop.sequence, 3);
        assert!(matches!(
            stop.body,
            HostResponseBody::Ack {
                command: HostAck::StopWatch { watch_id }
            } if watch_id == "watch-1"
        ));

        write_frame(&mut client, &request(3, TOKEN, HostCommand::Shutdown)).unwrap();
        let shutdown = read_response(&mut client);
        assert_eq!(shutdown.sequence, 4);
        assert!(matches!(
            shutdown.body,
            HostResponseBody::Ack {
                command: HostAck::Shutdown
            }
        ));
        drop(client);
        assert_eq!(server.join().unwrap(), SessionAction::Shutdown);
    }

    #[test]
    fn filesystem_watch_emits_an_ordered_fresh_run() {
        let root = tempfile::tempdir().unwrap();
        let runner: TestRunner = Arc::new(|_, watch_paths, _| {
            assert!(watch_paths.is_some(), "filesystem runs carry changed paths");
            WorkerExecution::from_result(TestRunResult::empty(
                "watch-run".to_string(),
                "watch-seed".to_string(),
                TestEnvironmentInfo {
                    kind: TestEnvironmentKind::Dom,
                    react: None,
                    react_dom: None,
                    v8: "test-v8".to_string(),
                    browser: None,
                },
            ))
        });
        let (mut client, server) = connect_session(runner);
        write_frame(
            &mut client,
            &request(
                1,
                TOKEN,
                HostCommand::StartWatch {
                    watch_id: "watch-files".to_string(),
                    options: Box::new(watch_options(root.path())),
                },
            ),
        )
        .unwrap();
        assert!(matches!(
            read_response(&mut client).body,
            HostResponseBody::Ack { .. }
        ));
        assert!(matches!(
            read_response(&mut client).body,
            HostResponseBody::Event { event }
                if matches!(*event, HostEvent::WatchReady { .. })
        ));

        std::fs::write(root.path().join("value.ts"), "export const value = 1").unwrap();
        let started = read_response(&mut client);
        let run_id = match started.body {
            HostResponseBody::Event { event } => match *event {
                HostEvent::RunStart { run_id, watching } => {
                    assert!(watching);
                    run_id
                }
                other => panic!("expected watch runStart, got {other:?}"),
            },
            other => panic!("expected watch event, got {other:?}"),
        };
        let completed = read_response(&mut client);
        assert!(matches!(
            completed.body,
            HostResponseBody::Event { event }
                if matches!(&*event, HostEvent::RunComplete { run_id: completed, .. } if completed == &run_id)
        ));

        write_frame(
            &mut client,
            &request(
                2,
                TOKEN,
                HostCommand::StopWatch {
                    watch_id: "watch-files".to_string(),
                },
            ),
        )
        .unwrap();
        assert!(matches!(
            read_response(&mut client).body,
            HostResponseBody::Ack { .. }
        ));
        write_frame(&mut client, &request(3, TOKEN, HostCommand::Shutdown)).unwrap();
        assert!(matches!(
            read_response(&mut client).body,
            HostResponseBody::Ack {
                command: HostAck::Shutdown
            }
        ));
        drop(client);
        assert_eq!(server.join().unwrap(), SessionAction::Shutdown);
    }

    #[test]
    fn token_is_verified_on_every_frame() {
        let root = tempfile::tempdir().unwrap();
        let (mut client, server) = connect_session(unused_runner());
        write_frame(
            &mut client,
            &request(
                1,
                TOKEN,
                HostCommand::StartWatch {
                    watch_id: "watch-1".to_string(),
                    options: Box::new(watch_options(root.path())),
                },
            ),
        )
        .unwrap();
        assert_eq!(read_response(&mut client).sequence, 1);
        assert_eq!(read_response(&mut client).sequence, 2);

        write_frame(
            &mut client,
            &request(
                2,
                "wrong-token",
                HostCommand::StopWatch {
                    watch_id: "watch-1".to_string(),
                },
            ),
        )
        .unwrap();
        let rejected = read_response(&mut client);
        assert_eq!(rejected.sequence, 3);
        assert_error(rejected, "WAKE_TEST_HOST");
        drop(client);
        assert_eq!(server.join().unwrap(), SessionAction::Continue);
    }

    #[test]
    fn build_id_is_verified_on_every_frame() {
        let (mut client, server) = connect_session(unused_runner());
        let mut incompatible = request(1, TOKEN, HostCommand::Shutdown);
        incompatible.build_id = "wake-test-host/incompatible".to_string();
        write_frame(&mut client, &incompatible).unwrap();
        let rejected = read_response(&mut client);
        assert_eq!(rejected.sequence, 1);
        assert_error(rejected, "WAKE_TEST_HOST");
        drop(client);
        assert_eq!(server.join().unwrap(), SessionAction::Continue);
    }

    #[test]
    fn worker_error_is_a_terminal_frame_and_closes_the_protocol_session() {
        let runner: TestRunner = Arc::new(|_, _, _| {
            WorkerExecution::from_error(WorkerError::Host("synthetic worker failure".to_string()))
        });
        let (mut client, server) = connect_session_result(runner);
        write_frame(
            &mut client,
            &request(
                1,
                TOKEN,
                HostCommand::Run {
                    run_id: "run-error".to_string(),
                    options: Box::new(TestOptions::default()),
                },
            ),
        )
        .unwrap();
        assert_eq!(read_response(&mut client).sequence, 1);
        assert_eq!(read_response(&mut client).sequence, 2);
        let terminal = read_response(&mut client);
        assert_eq!(terminal.sequence, 3);
        let HostResponseBody::Error { run_id, error } = terminal.body else {
            panic!("expected a terminal worker error");
        };
        assert_eq!(run_id.as_deref(), Some("run-error"));
        assert_eq!(error.code, "WAKE_TEST_HOST");

        drop(client);
        assert!(server.join().unwrap().is_err());
    }

    #[test]
    fn started_watch_error_is_terminal_and_closes_the_protocol_session() {
        let root = tempfile::tempdir().unwrap();
        let runner: TestRunner = Arc::new(|_, _, _| {
            WorkerExecution::from_error(WorkerError::Host(
                "synthetic automatic failure".to_string(),
            ))
        });
        let (mut client, server) = connect_session_result(runner);
        write_frame(
            &mut client,
            &request(
                1,
                TOKEN,
                HostCommand::StartWatch {
                    watch_id: "fatal-watch".to_string(),
                    options: Box::new(watch_options(root.path())),
                },
            ),
        )
        .unwrap();
        assert!(matches!(
            read_response(&mut client).body,
            HostResponseBody::Ack { .. }
        ));
        assert!(matches!(
            read_response(&mut client).body,
            HostResponseBody::Event { .. }
        ));
        write_frame(
            &mut client,
            &request(
                2,
                TOKEN,
                HostCommand::WatchControl {
                    watch_id: "fatal-watch".to_string(),
                    control: WatchControl::Rerun,
                },
            ),
        )
        .unwrap();
        assert!(matches!(
            read_response(&mut client).body,
            HostResponseBody::Ack { .. }
        ));
        assert!(matches!(
            read_response(&mut client).body,
            HostResponseBody::Event { event }
                if matches!(&*event, HostEvent::RunStart { watching: true, .. })
        ));
        let HostResponseBody::WatchRunError {
            watch_id,
            run_id,
            started,
            error,
        } = read_response(&mut client).body
        else {
            panic!("expected a terminal watch error");
        };
        assert_eq!(watch_id, "fatal-watch");
        assert!(run_id.is_some());
        assert!(started);
        assert_eq!(error.code, "WAKE_TEST_HOST");
        drop(client);
        assert!(server.join().unwrap().is_err());
    }

    #[test]
    fn run_remains_responsive_to_busy_cancel_and_final_result() {
        let runner: TestRunner = Arc::new(|_, _, cancellation| {
            while !cancellation.is_cancelled() {
                std::thread::yield_now();
            }
            WorkerExecution::from_result(TestRunResult::empty(
                "cancelled-run".to_string(),
                "seed".to_string(),
                TestEnvironmentInfo {
                    kind: TestEnvironmentKind::Dom,
                    react: None,
                    react_dom: None,
                    v8: "test-v8".to_string(),
                    browser: None,
                },
            ))
        });
        let (mut client, server) = connect_session(runner);

        write_frame(
            &mut client,
            &request(
                1,
                TOKEN,
                HostCommand::Run {
                    run_id: "run-1".to_string(),
                    options: Box::new(TestOptions::default()),
                },
            ),
        )
        .unwrap();
        let accepted = read_response(&mut client);
        let started = read_response(&mut client);
        assert_eq!((accepted.sequence, started.sequence), (1, 2));
        assert!(matches!(
            accepted.body,
            HostResponseBody::Ack {
                command: HostAck::Run { run_id }
            } if run_id == "run-1"
        ));
        assert!(matches!(
            started.body,
            HostResponseBody::Event {
                event
            } if matches!(*event, HostEvent::RunStart { ref run_id, watching: false } if run_id == "run-1")
        ));

        write_frame(
            &mut client,
            &request(
                2,
                TOKEN,
                HostCommand::Run {
                    run_id: "run-2".to_string(),
                    options: Box::new(TestOptions::default()),
                },
            ),
        )
        .unwrap();
        assert_error(read_response(&mut client), "WAKE_TEST_BUSY");

        write_frame(
            &mut client,
            &request(
                3,
                TOKEN,
                HostCommand::Cancel {
                    run_id: "run-1".to_string(),
                },
            ),
        )
        .unwrap();
        let cancel = read_response(&mut client);
        assert_eq!(cancel.sequence, 4);
        assert!(matches!(
            cancel.body,
            HostResponseBody::Ack {
                command: HostAck::Cancel { run_id }
            } if run_id == "run-1"
        ));

        let completed = read_response(&mut client);
        assert_eq!(completed.sequence, 5);
        let HostResponseBody::Result { run_id, result } = completed.body else {
            panic!("expected a terminal run result");
        };
        assert_eq!(run_id, "run-1");
        assert_eq!(result.run_id, "run-1");
        assert!(!result.success);
        assert_eq!(result.termination_reason, TestTerminationReason::Cancelled);

        write_frame(
            &mut client,
            &request(
                4,
                TOKEN,
                HostCommand::Cancel {
                    run_id: "run-1".to_string(),
                },
            ),
        )
        .unwrap();
        assert_error(read_response(&mut client), "WAKE_TEST_UNKNOWN_RUN");

        write_frame(&mut client, &request(5, TOKEN, HostCommand::Shutdown)).unwrap();
        let shutdown = read_response(&mut client);
        assert_eq!(shutdown.sequence, 7);
        assert!(matches!(
            shutdown.body,
            HostResponseBody::Ack {
                command: HostAck::Shutdown
            }
        ));
        drop(client);
        assert_eq!(server.join().unwrap(), SessionAction::Shutdown);
    }
}
