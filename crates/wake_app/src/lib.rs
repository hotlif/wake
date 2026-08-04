//! Reusable Wake application services.
//!
//! This crate owns project/configuration orchestration. Frontends such as the
//! Rust CLI and the Node-API addon are responsible only for argument parsing,
//! presentation, and process lifecycle.

use std::path::{Component, Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::thread;
use std::time::Instant;

use serde::Serialize;
use wake_bundler::{BuildOutput, BuildRequest, BuildSession, IncrementalBundler, ResolveOptions};
use wake_common::{Diagnostic, OsFileSystem};
use wake_ecma_transform::{BrowserTarget, TargetEnv};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WakeError {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<DiagnosticInfo>,
}

impl WakeError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            path: None,
            diagnostics: Vec::new(),
        }
    }

    pub fn at(mut self, path: &Path) -> Self {
        self.path = Some(path.to_string_lossy().into_owned());
        self
    }

    pub fn with_diagnostics(mut self, diagnostics: &[Diagnostic]) -> Self {
        self.diagnostics = diagnostics.iter().map(DiagnosticInfo::from).collect();
        self
    }

    pub fn cancelled() -> Self {
        Self::new("WAKE_CANCELLED", "Wake operation was cancelled")
    }

    pub fn closed(resource: &str) -> Self {
        Self::new(
            "WAKE_INTERNAL",
            format!("{resource} has already been closed"),
        )
    }
}

impl std::fmt::Display for WakeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for WakeError {}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticInfo {
    pub severity: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

impl From<&Diagnostic> for DiagnosticInfo {
    fn from(value: &Diagnostic) -> Self {
        let span = value.primary_span();
        Self {
            severity: value.severity.as_str().to_string(),
            code: value.code.as_ref().map(ToString::to_string),
            message: value.message.clone(),
            start: span.map(|span| span.lo),
            end: span.map(|span| span.hi),
            notes: value.notes.clone(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ProjectOptions {
    pub cwd: Option<PathBuf>,
    pub config_path: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct BuildOptions {
    pub project: ProjectOptions,
    pub entry: Option<PathBuf>,
    pub outdir: Option<PathBuf>,
    pub cache: bool,
    pub source_map: bool,
    pub write: bool,
}

impl Default for BuildOptions {
    fn default() -> Self {
        Self {
            project: ProjectOptions::default(),
            entry: None,
            outdir: None,
            cache: false,
            source_map: false,
            write: true,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputFile {
    pub path: String,
    pub kind: String,
    pub bytes: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildResult {
    pub success: bool,
    pub module_count: usize,
    pub duration_ms: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    pub files: Vec<OutputFile>,
    pub diagnostics: Vec<DiagnosticInfo>,
}

#[derive(Debug, Clone, Default)]
pub struct CancellationToken(Arc<AtomicBool>);

impl CancellationToken {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }

    fn check(&self) -> Result<(), WakeError> {
        if self.is_cancelled() {
            Err(WakeError::cancelled())
        } else {
            Ok(())
        }
    }
}

struct PreparedBuild {
    root: PathBuf,
    entry: PathBuf,
    outdir: PathBuf,
    config: wake_config::Config,
    aliases: Vec<(String, PathBuf)>,
}

pub fn build(
    options: BuildOptions,
    cancellation: &CancellationToken,
) -> Result<BuildResult, WakeError> {
    execute_build(options, cancellation, true)
}

pub fn bundle(
    mut options: BuildOptions,
    cancellation: &CancellationToken,
) -> Result<BuildResult, WakeError> {
    options.write = false;
    execute_build(options, cancellation, false)
}

fn execute_build(
    options: BuildOptions,
    cancellation: &CancellationToken,
    project_defaults: bool,
) -> Result<BuildResult, WakeError> {
    cancellation.check()?;
    let started = Instant::now();
    let prepared = prepare_build(&options)?;
    cancellation.check()?;
    let mut bundler = create_bundler(&prepared, &options, project_defaults)?;
    let output = bundler.build(&prepared.entry);
    cancellation.check()?;
    finish_output(
        &prepared,
        &options,
        output,
        started.elapsed().as_secs_f64() * 1000.0,
    )
}

fn prepare_build(options: &BuildOptions) -> Result<PreparedBuild, WakeError> {
    let cwd = options
        .project
        .cwd
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let config_dir = resolve_config_dir(&cwd, options.project.config_path.as_deref())?;
    let config = wake_config::load(&config_dir)
        .map_err(|error| WakeError::new("WAKE_CONFIG", error.to_string()).at(&config_dir))?;
    let root = normalize_path(&config.resolved_root(&config_dir));
    if !root.is_dir() {
        return Err(WakeError::new(
            "WAKE_CONFIG",
            format!("configured project root does not exist: {}", root.display()),
        )
        .at(&root));
    }
    let aliases = prepare_aliases_and_scans(&config, &root)?;
    let entry = match &options.entry {
        Some(entry) => absolute_from(&root, entry),
        None => virtual_entry(&root, &config)?,
    };
    if !entry.is_file() {
        return Err(WakeError::new(
            "WAKE_IO",
            format!("entry file does not exist: {}", entry.display()),
        )
        .at(&entry));
    }
    let outdir = absolute_from(
        &root,
        options
            .outdir
            .as_deref()
            .unwrap_or_else(|| Path::new("dist")),
    );
    Ok(PreparedBuild {
        root,
        entry,
        outdir,
        config,
        aliases,
    })
}

fn resolve_config_dir(cwd: &Path, config_path: Option<&Path>) -> Result<PathBuf, WakeError> {
    let cwd = if cwd.is_absolute() {
        normalize_path(cwd)
    } else {
        let process_cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        normalize_path(&process_cwd.join(cwd))
    };
    if !cwd.is_dir() {
        return Err(
            WakeError::new("WAKE_CONFIG", "cwd does not exist or is not a directory").at(&cwd),
        );
    }
    if let Some(config_path) = config_path {
        let path = absolute_from(&cwd, config_path);
        if path.file_name().and_then(|name| name.to_str()) != Some(wake_config::CONFIG_FILE) {
            return Err(WakeError::new(
                "WAKE_CONFIG",
                format!("configPath must point to {}", wake_config::CONFIG_FILE),
            )
            .at(&path));
        }
        if !path.is_file() {
            return Err(
                WakeError::new("WAKE_CONFIG", "configuration file does not exist").at(&path),
            );
        }
        return Ok(path.parent().unwrap_or(&cwd).to_path_buf());
    }
    Ok(wake_config::find_root(&cwd))
}

fn prepare_aliases_and_scans(
    config: &wake_config::Config,
    root: &Path,
) -> Result<Vec<(String, PathBuf)>, WakeError> {
    let mut aliases = config.resolver_aliases(root);
    if config.component_scan.is_empty() {
        return Ok(aliases);
    }
    let scan_base = root.join(".wake").join("scan");
    std::fs::create_dir_all(&scan_base)
        .map_err(|error| WakeError::new("WAKE_IO", error.to_string()).at(&scan_base))?;
    for rule in &config.component_scan {
        let source = wake_scan::scan(&wake_scan::ScanRule {
            namespace: &rule.namespace,
            scan_dir: &root.join(&rule.cwd),
            root,
            generate_source: rule.generate_source,
            include: rule.include.as_deref(),
            exclude: rule.exclude.as_deref(),
        })
        .map_err(|error| WakeError::new("WAKE_BUILD", error.to_string()))?;
        let file = scan_base.join(format!("{}.ts", sanitize_namespace(&rule.namespace)));
        std::fs::write(&file, source)
            .map_err(|error| WakeError::new("WAKE_IO", error.to_string()).at(&file))?;
        aliases.push((format!("@@@/{}", rule.namespace), file));
    }
    Ok(aliases)
}

fn create_bundler(
    prepared: &PreparedBuild,
    options: &BuildOptions,
    project_defaults: bool,
) -> Result<IncrementalBundler, WakeError> {
    let mut bundler = IncrementalBundler::new(Arc::new(OsFileSystem));
    bundler.set_resolve_options(ResolveOptions {
        alias: prepared.aliases.clone(),
        ..ResolveOptions::default()
    });
    bundler.set_define(build_defines(&prepared.config, !project_defaults));
    bundler.set_target_env(resolve_target_env(&prepared.config, &prepared.root)?);
    bundler.set_jsx_runtime(
        false,
        Box::leak(
            prepared
                .config
                .react
                .jsx_import_source
                .clone()
                .into_boxed_str(),
        ),
    );
    bundler.enable_css_in_js();
    bundler.set_asset_inline_limit(4096);
    bundler.set_public_path(prepared.config.public_path());
    if project_defaults {
        bundler.enable_css_extraction();
        bundler.enable_dead_module_elimination();
        bundler.enable_tree_shaking();
        if options.source_map {
            bundler.enable_sourcemap();
        } else {
            bundler.enable_minify();
            bundler.enable_mangle();
            bundler.enable_code_splitting();
        }
    } else if options.source_map {
        bundler.enable_sourcemap();
    }
    if options.cache {
        let cache_dir = prepared.root.join(".wake");
        std::fs::create_dir_all(&cache_dir)
            .map_err(|error| WakeError::new("WAKE_IO", error.to_string()).at(&cache_dir))?;
        bundler.enable_persistent_cache(cache_dir.join("cache.bin"));
    }
    Ok(bundler)
}

fn create_session(
    prepared: &PreparedBuild,
    options: &BuildOptions,
    project_defaults: bool,
) -> Result<BuildSession, WakeError> {
    Ok(BuildSession::from_incremental(create_bundler(
        prepared,
        options,
        project_defaults,
    )?))
}

fn finish_output(
    prepared: &PreparedBuild,
    options: &BuildOptions,
    output: BuildOutput,
    duration_ms: f64,
) -> Result<BuildResult, WakeError> {
    let diagnostics = output
        .diagnostics
        .iter()
        .map(DiagnosticInfo::from)
        .collect::<Vec<_>>();
    if output.has_errors() {
        return Err(
            WakeError::new("WAKE_BUILD", "Wake build failed").with_diagnostics(&output.diagnostics)
        );
    }

    let mut files = output
        .chunks
        .iter()
        .map(|chunk| OutputFile {
            path: chunk.file_name.clone(),
            kind: "chunk".to_string(),
            bytes: chunk.code.len(),
        })
        .chain(output.assets.iter().map(|asset| OutputFile {
            path: asset.file_name.clone(),
            kind: if asset.is_css { "css" } else { "asset" }.to_string(),
            bytes: asset.bytes.len(),
        }))
        .collect::<Vec<_>>();

    let output_dir = if options.write {
        write_build_output(&output, &prepared.outdir)?;
        let html = emit_html(&output, &prepared.config);
        let html_path = prepared.outdir.join("index.html");
        std::fs::write(&html_path, html.as_bytes())
            .map_err(|error| WakeError::new("WAKE_IO", error.to_string()).at(&html_path))?;
        files.push(OutputFile {
            path: "index.html".to_string(),
            kind: "html".to_string(),
            bytes: html.len(),
        });
        Some(prepared.outdir.to_string_lossy().into_owned())
    } else {
        None
    };

    Ok(BuildResult {
        success: true,
        module_count: output.module_count,
        duration_ms,
        output_dir,
        code: (!options.write).then(|| output.bundle.clone()),
        files,
        diagnostics,
    })
}

fn write_build_output(output: &BuildOutput, outdir: &Path) -> Result<(), WakeError> {
    clean_outdir(outdir)?;
    std::fs::create_dir_all(outdir)
        .map_err(|error| WakeError::new("WAKE_IO", error.to_string()).at(outdir))?;
    for chunk in &output.chunks {
        let path = outdir.join(&chunk.file_name);
        std::fs::write(&path, &chunk.code)
            .map_err(|error| WakeError::new("WAKE_IO", error.to_string()).at(&path))?;
        if let Some(map) = &chunk.source_map {
            let map_path = outdir.join(format!("{}.map", chunk.file_name));
            std::fs::write(&map_path, map)
                .map_err(|error| WakeError::new("WAKE_IO", error.to_string()).at(&map_path))?;
        }
    }
    for asset in &output.assets {
        let path = outdir.join(&asset.file_name);
        std::fs::write(&path, &asset.bytes)
            .map_err(|error| WakeError::new("WAKE_IO", error.to_string()).at(&path))?;
    }
    let manifest = serde_json::json!({
        "entry": output.entry().file_name,
        "chunks": output.chunks.iter().map(|chunk| &chunk.file_name).collect::<Vec<_>>(),
        "assets": output.assets.iter().map(|asset| &asset.file_name).collect::<Vec<_>>(),
    });
    let path = outdir.join("manifest.json");
    std::fs::write(
        &path,
        serde_json::to_vec_pretty(&manifest).expect("manifest serialization"),
    )
    .map_err(|error| WakeError::new("WAKE_IO", error.to_string()).at(&path))?;
    Ok(())
}

fn emit_html(output: &BuildOutput, config: &wake_config::Config) -> String {
    let scripts = output
        .chunks
        .iter()
        .filter(|chunk| chunk.is_entry)
        .map(|chunk| chunk.file_name.clone())
        .collect::<Vec<_>>();
    let styles = output
        .assets
        .iter()
        .filter(|asset| asset.is_css)
        .map(|asset| asset.file_name.clone())
        .collect::<Vec<_>>();
    wake_html::generate(
        None,
        &wake_html::HtmlInputs {
            scripts: &scripts,
            styles: &styles,
            public_path: config.public_path(),
        },
    )
}
fn build_defines(config: &wake_config::Config, development: bool) -> Vec<(String, String)> {
    let node_env = if development {
        "\"development\""
    } else {
        "\"production\""
    };
    let mut values = vec![("process.env.NODE_ENV".to_string(), node_env.to_string())];
    for (key, value) in &config.define {
        if let Some(existing) = values.iter_mut().find(|(name, _)| name == key) {
            existing.1 = value.clone();
        } else {
            values.push((key.clone(), value.clone()));
        }
    }
    values
}

fn resolve_target_env(config: &wake_config::Config, root: &Path) -> Result<TargetEnv, WakeError> {
    let targets = config
        .resolve_browser_targets(root)
        .map_err(|error| WakeError::new("WAKE_CONFIG", error.to_string()).at(root))?
        .into_iter()
        .map(|target| BrowserTarget::new(target.name, target.version))
        .collect();
    let mut environment = TargetEnv::new(targets);
    environment
        .apply_overrides(&config.transforms.include, &config.transforms.exclude)
        .map_err(|error| WakeError::new("WAKE_CONFIG", error))?;
    Ok(environment)
}

fn virtual_entry(root: &Path, config: &wake_config::Config) -> Result<PathBuf, WakeError> {
    let target = config
        .html
        .entry
        .as_deref()
        .unwrap_or("src/entry.tsx")
        .replace('\\', "/");
    let dir = root.join(".wake");
    std::fs::create_dir_all(&dir)
        .map_err(|error| WakeError::new("WAKE_IO", error.to_string()).at(&dir))?;
    let path = dir.join("entry.tsx");
    std::fs::write(&path, format!("import(\"@@/{target}\");\n"))
        .map_err(|error| WakeError::new("WAKE_IO", error.to_string()).at(&path))?;
    Ok(path)
}

fn clean_outdir(outdir: &Path) -> Result<(), WakeError> {
    if outdir.exists() {
        if outdir.file_name().is_none() || outdir == Path::new(".") {
            return Err(WakeError::new(
                "WAKE_CONFIG",
                format!(
                    "refusing to clean unsafe output directory: {}",
                    outdir.display()
                ),
            ));
        }
        std::fs::remove_dir_all(outdir)
            .map_err(|error| WakeError::new("WAKE_IO", error.to_string()).at(outdir))?;
    }
    Ok(())
}

fn absolute_from(root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        normalize_path(path)
    } else {
        normalize_path(&root.join(path))
    }
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut output = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                output.pop();
            }
            Component::CurDir => {}
            other => output.push(other.as_os_str()),
        }
    }
    output
}

fn sanitize_namespace(namespace: &str) -> String {
    namespace
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '_'
            }
        })
        .collect()
}

enum ContextCommand {
    Rebuild {
        changed_paths: Vec<PathBuf>,
        cancellation: CancellationToken,
        response: mpsc::Sender<Result<BuildResult, WakeError>>,
    },
    Close {
        response: mpsc::Sender<()>,
    },
}

struct BuildContextInner {
    sender: mpsc::Sender<ContextCommand>,
    join: Mutex<Option<thread::JoinHandle<()>>>,
    closed: AtomicBool,
}

#[derive(Clone)]
pub struct BuildContext {
    inner: Arc<BuildContextInner>,
}

impl BuildContext {
    pub fn create(options: BuildOptions) -> Result<Self, WakeError> {
        let prepared = prepare_build(&options)?;
        let (sender, receiver) = mpsc::channel();
        let join = thread::Builder::new()
            .name("wake-build-context".to_string())
            .spawn(move || {
                let session = create_session(&prepared, &options, true);
                run_build_context(receiver, prepared, options, session);
            })
            .map_err(|error| WakeError::new("WAKE_INTERNAL", error.to_string()))?;
        Ok(Self {
            inner: Arc::new(BuildContextInner {
                sender,
                join: Mutex::new(Some(join)),
                closed: AtomicBool::new(false),
            }),
        })
    }

    pub fn rebuild(
        &self,
        changed_paths: Vec<PathBuf>,
        cancellation: CancellationToken,
    ) -> Result<BuildResult, WakeError> {
        if self.inner.closed.load(Ordering::Acquire) {
            return Err(WakeError::closed("BuildContext"));
        }
        let (sender, receiver) = mpsc::channel();
        self.inner
            .sender
            .send(ContextCommand::Rebuild {
                changed_paths,
                cancellation,
                response: sender,
            })
            .map_err(|_| WakeError::closed("BuildContext"))?;
        receiver
            .recv()
            .map_err(|_| WakeError::closed("BuildContext"))?
    }

    pub fn request_close(&self) {
        if !self.inner.closed.swap(true, Ordering::AcqRel) {
            let (sender, _) = mpsc::channel();
            let _ = self
                .inner
                .sender
                .send(ContextCommand::Close { response: sender });
        }
    }

    pub fn close(&self) {
        let response = if self.inner.closed.swap(true, Ordering::AcqRel) {
            None
        } else {
            let (sender, receiver) = mpsc::channel();
            let _ = self
                .inner
                .sender
                .send(ContextCommand::Close { response: sender });
            Some(receiver)
        };
        if let Some(receiver) = response {
            let _ = receiver.recv();
        }
        let mut join = self
            .inner
            .join
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(join) = join.take() {
            let _ = join.join();
        }
    }

    pub fn is_closed(&self) -> bool {
        self.inner.closed.load(Ordering::Acquire)
    }
}

impl Drop for BuildContextInner {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::Release);
        let (sender, _) = mpsc::channel();
        let _ = self.sender.send(ContextCommand::Close { response: sender });
    }
}

fn run_build_context(
    receiver: mpsc::Receiver<ContextCommand>,
    prepared: PreparedBuild,
    options: BuildOptions,
    session: Result<BuildSession, WakeError>,
) {
    let mut session = match session {
        Ok(session) => session,
        Err(error) => {
            while let Ok(command) = receiver.recv() {
                match command {
                    ContextCommand::Rebuild { response, .. } => {
                        let _ = response.send(Err(error.clone()));
                    }
                    ContextCommand::Close { response } => {
                        let _ = response.send(());
                        break;
                    }
                }
            }
            return;
        }
    };
    while let Ok(command) = receiver.recv() {
        match command {
            ContextCommand::Rebuild {
                changed_paths,
                cancellation,
                response,
            } => {
                let result = if let Err(error) = cancellation.check() {
                    Err(error)
                } else {
                    let started = Instant::now();
                    let mut paths = changed_paths
                        .iter()
                        .map(|path| absolute_from(&prepared.root, path))
                        .collect::<Vec<_>>();
                    match prepare_aliases_and_scans(&prepared.config, &prepared.root) {
                        Ok(aliases) => {
                            paths.extend(aliases.into_iter().filter_map(|(name, path)| {
                                name.starts_with("@@@/").then_some(path)
                            }))
                        }
                        Err(error) => {
                            let _ = response.send(Err(error));
                            continue;
                        }
                    }
                    paths.sort();
                    paths.dedup();
                    if paths.is_empty() {
                        session.invalidate_filesystem();
                    } else {
                        session.invalidate_paths(&paths, true);
                    }
                    let output = session.build_current(BuildRequest::new(&prepared.entry));
                    cancellation.check().and_then(|()| {
                        finish_output(
                            &prepared,
                            &options,
                            output,
                            started.elapsed().as_secs_f64() * 1000.0,
                        )
                    })
                };
                let _ = response.send(result);
            }
            ContextCommand::Close { response } => {
                let _ = response.send(());
                break;
            }
        }
    }
}
#[derive(Debug, Clone, Default)]
pub struct DevServerOptions {
    pub project: ProjectOptions,
    pub entry: Option<PathBuf>,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub open: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum DevServerEvent {
    RebuildStart { changed_paths: Vec<String> },
    Rebuilt { modules: usize, duration_ms: f64 },
    Diagnostic { message: String },
    Closed,
}

#[derive(Clone)]
pub struct DevServer {
    handle: wake_dev_server::ServerHandle,
    events: Arc<Mutex<mpsc::Receiver<wake_dev_server::ServerEvent>>>,
}

impl DevServer {
    pub fn url(&self) -> &str {
        self.handle.url()
    }

    pub fn request_close(&self) {
        self.handle.request_close();
    }

    pub fn close(&self) -> Result<(), WakeError> {
        self.handle
            .close()
            .map_err(|error| WakeError::new("WAKE_IO", error.to_string()))
    }

    pub fn wait_until_closed(&self) -> Result<(), WakeError> {
        self.handle
            .wait()
            .map_err(|error| WakeError::new("WAKE_IO", error.to_string()))
    }
    pub fn drain_events(&self) -> Vec<DevServerEvent> {
        let receiver = self
            .events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        receiver
            .try_iter()
            .map(|event| match event {
                wake_dev_server::ServerEvent::RebuildStart { changed_paths } => {
                    DevServerEvent::RebuildStart {
                        changed_paths: changed_paths
                            .into_iter()
                            .map(|path| path.to_string_lossy().into_owned())
                            .collect(),
                    }
                }
                wake_dev_server::ServerEvent::Rebuilt {
                    modules,
                    duration_ms,
                } => DevServerEvent::Rebuilt {
                    modules,
                    duration_ms,
                },
                wake_dev_server::ServerEvent::Diagnostic { message } => {
                    DevServerEvent::Diagnostic { message }
                }
                wake_dev_server::ServerEvent::Closed => DevServerEvent::Closed,
            })
            .collect()
    }
}

pub fn start_dev_server(options: DevServerOptions) -> Result<DevServer, WakeError> {
    let build_options = BuildOptions {
        project: options.project,
        entry: options.entry,
        write: false,
        ..BuildOptions::default()
    };
    let prepared = prepare_build(&build_options)?;
    let config = &prepared.config;
    let server = &config.dev_server;
    let port = options.port.or(server.port).unwrap_or(5173);
    let host = options
        .host
        .or_else(|| server.host.clone())
        .unwrap_or_else(|| "127.0.0.1".to_string());
    let proxies = server
        .proxy
        .iter()
        .map(|proxy| wake_dev_server::ProxyRule {
            context: proxy.context.clone(),
            target: proxy.target.clone(),
            path_rewrite: proxy
                .path_rewrite
                .iter()
                .map(|(pattern, replacement)| (pattern.clone(), replacement.clone()))
                .collect(),
            change_origin: proxy.change_origin,
        })
        .collect();
    let watch_roots = config
        .component_scan
        .iter()
        .map(|rule| prepared.root.join(&rule.cwd))
        .collect();
    let scan_root = prepared.root.clone();
    let scan_config = config.clone();
    let before_rebuild: wake_dev_server::BeforeRebuild = Arc::new(move |_| {
        prepare_aliases_and_scans(&scan_config, &scan_root)
            .map(|aliases| {
                aliases
                    .into_iter()
                    .filter_map(|(name, path)| name.starts_with("@@@/").then_some(path))
                    .collect()
            })
            .map_err(|error| error.to_string())
    });
    let (event_tx, event_rx) = mpsc::channel();
    let event_handler: wake_dev_server::EventHandler = Arc::new(move |event| {
        let _ = event_tx.send(event);
    });
    let serve_options = wake_dev_server::ServeOptions {
        entry: prepared.entry,
        resolve_options: ResolveOptions {
            alias: prepared.aliases,
            ..ResolveOptions::default()
        },
        define: build_defines(config, true),
        host,
        open: options.open.unwrap_or(server.open),
        proxy: proxies,
        target_env: resolve_target_env(config, &prepared.root)?,
        jsx_import_source: config.react.jsx_import_source.clone(),
        watch_roots,
        before_rebuild: Some(before_rebuild),
        quiet: true,
        event_handler: Some(event_handler),
    };
    let handle = wake_dev_server::start(&prepared.root, port, serve_options)
        .map_err(|error| WakeError::new("WAKE_IO", error.to_string()))?;
    Ok(DevServer {
        handle,
        events: Arc::new(Mutex::new(event_rx)),
    })
}

#[derive(Debug, Clone, Default)]
pub struct DocsBuildOptions {
    pub project: ProjectOptions,
    pub outdir: Option<PathBuf>,
    pub base_path: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocsBuildResult {
    #[serde(flatten)]
    pub build: BuildResult,
    pub routes: Vec<wake_docs::RouteInfo>,
}

pub fn build_docs(
    options: DocsBuildOptions,
    cancellation: &CancellationToken,
) -> Result<DocsBuildResult, WakeError> {
    cancellation.check()?;
    let started = Instant::now();
    let (mut prepared, docs, routes) = prepare_docs(&options, wake_docs::BuildMode::Production)?;
    prepared.outdir = absolute_from(
        &prepared.root,
        options
            .outdir
            .as_deref()
            .unwrap_or_else(|| Path::new("docs-dist")),
    );
    prepared.config.public_path = Some(normalize_public_path(&docs.base_path));
    let build_options = BuildOptions {
        project: options.project,
        entry: Some(prepared.entry.clone()),
        outdir: Some(prepared.outdir.clone()),
        write: true,
        ..BuildOptions::default()
    };
    let mut bundler = create_bundler(&prepared, &build_options, true)?;
    let output = bundler.build(&prepared.entry);
    cancellation.check()?;
    if output.has_errors() {
        return Err(
            WakeError::new("WAKE_BUILD", "Wake documentation build failed")
                .with_diagnostics(&output.diagnostics),
        );
    }
    let scripts = output
        .chunks
        .iter()
        .filter(|chunk| chunk.is_entry)
        .map(|chunk| chunk.file_name.clone())
        .collect::<Vec<_>>();
    let styles = output
        .assets
        .iter()
        .filter(|asset| asset.is_css)
        .map(|asset| asset.file_name.clone())
        .collect::<Vec<_>>();
    let html = wake_html::generate(
        None,
        &wake_html::HtmlInputs {
            scripts: &scripts,
            styles: &styles,
            public_path: prepared.config.public_path(),
        },
    );
    let result = finish_output(
        &prepared,
        &build_options,
        output,
        started.elapsed().as_secs_f64() * 1000.0,
    )?;
    wake_docs::write_route_shells(
        &prepared.outdir,
        &routes,
        &html,
        &docs.title,
        &docs.description,
        &docs.locale,
    )
    .map_err(|error| WakeError::new("WAKE_BUILD", error.to_string()))?;
    wake_docs::copy_public_assets(&prepared.root, &prepared.outdir)
        .map_err(|error| WakeError::new("WAKE_IO", error.to_string()))?;
    Ok(DocsBuildResult {
        build: result,
        routes,
    })
}

pub fn start_docs_dev_server(options: DevServerOptions) -> Result<DevServer, WakeError> {
    let docs_options = DocsBuildOptions {
        project: options.project.clone(),
        outdir: None,
        base_path: None,
    };
    let (prepared, docs, _routes) = prepare_docs(&docs_options, wake_docs::BuildMode::Development)?;
    let config = &prepared.config;
    let port = options.port.or(config.dev_server.port).unwrap_or(5173);
    let host = options
        .host
        .or_else(|| config.dev_server.host.clone())
        .unwrap_or_else(|| "127.0.0.1".to_string());
    let (event_tx, event_rx) = mpsc::channel();
    let event_handler: wake_dev_server::EventHandler = Arc::new(move |event| {
        let _ = event_tx.send(event);
    });
    let docs_root = prepared.root.clone();
    let docs_scan_config = config.clone();
    let before_rebuild: wake_dev_server::BeforeRebuild = Arc::new(move |_| {
        let mut changed = wake_docs::generate(&docs_root, &docs, wake_docs::BuildMode::Development)
            .map(|generated| generated.changed_files)
            .map_err(|error| error.to_string())?;
        changed.extend(
            prepare_aliases_and_scans(&docs_scan_config, &docs_root)
                .map_err(|error| error.to_string())?
                .into_iter()
                .filter_map(|(name, path)| name.starts_with("@@@/").then_some(path)),
        );
        Ok(changed)
    });
    let serve_options = wake_dev_server::ServeOptions {
        entry: prepared.entry,
        resolve_options: ResolveOptions {
            alias: prepared.aliases,
            ..ResolveOptions::default()
        },
        define: build_defines(config, true),
        host,
        open: options.open.unwrap_or(config.dev_server.open),
        proxy: config
            .dev_server
            .proxy
            .iter()
            .map(|proxy| wake_dev_server::ProxyRule {
                context: proxy.context.clone(),
                target: proxy.target.clone(),
                path_rewrite: proxy
                    .path_rewrite
                    .iter()
                    .map(|(pattern, replacement)| (pattern.clone(), replacement.clone()))
                    .collect(),
                change_origin: proxy.change_origin,
            })
            .collect(),
        target_env: resolve_target_env(config, &prepared.root)?,
        jsx_import_source: config.react.jsx_import_source.clone(),
        watch_roots: {
            let mut roots = vec![
                prepared.root.join(&config.docs.source_dir),
                prepared.root.join("src"),
            ];
            roots.extend(
                config
                    .component_scan
                    .iter()
                    .map(|rule| prepared.root.join(&rule.cwd)),
            );
            roots.sort();
            roots.dedup();
            roots
        },
        before_rebuild: Some(before_rebuild),
        quiet: true,
        event_handler: Some(event_handler),
    };
    let handle = wake_dev_server::start(&prepared.root, port, serve_options)
        .map_err(|error| WakeError::new("WAKE_IO", error.to_string()))?;
    Ok(DevServer {
        handle,
        events: Arc::new(Mutex::new(event_rx)),
    })
}

fn prepare_docs(
    options: &DocsBuildOptions,
    mode: wake_docs::BuildMode,
) -> Result<
    (
        PreparedBuild,
        wake_docs::DocsOptions,
        Vec<wake_docs::RouteInfo>,
    ),
    WakeError,
> {
    let cwd = options
        .project
        .cwd
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let config_dir = resolve_config_dir(&cwd, options.project.config_path.as_deref())?;
    let config = wake_config::load(&config_dir)
        .map_err(|error| WakeError::new("WAKE_CONFIG", error.to_string()).at(&config_dir))?;
    let root = normalize_path(&config.resolved_root(&config_dir));
    let mut aliases = prepare_aliases_and_scans(&config, &root)?;
    let docs = docs_options(&config, options.base_path.as_deref());
    let generated = wake_docs::generate(&root, &docs, mode)
        .map_err(|error| WakeError::new("WAKE_BUILD", error.to_string()))?;
    aliases.retain(|(name, _)| name != "@wake/docs" && name != "@wake/docs-project");
    aliases.extend(generated.aliases);
    let routes = generated.routes;
    Ok((
        PreparedBuild {
            root: root.clone(),
            entry: generated.entry,
            outdir: root.join("docs-dist"),
            config,
            aliases,
        },
        docs,
        routes,
    ))
}

fn docs_options(config: &wake_config::Config, base_path: Option<&str>) -> wake_docs::DocsOptions {
    let docs = &config.docs;
    wake_docs::DocsOptions {
        source_dir: PathBuf::from(&docs.source_dir),
        title: docs.title.clone(),
        description: docs.description.clone(),
        locale: docs.locale.clone(),
        logo: docs.logo.clone(),
        repository_url: docs.repository_url.clone(),
        base_path: base_path.unwrap_or(&docs.base_path).to_string(),
        preview: docs.preview.as_deref().map(PathBuf::from),
        theme_css: docs.theme_css.as_deref().map(PathBuf::from),
        default_theme: docs.default_theme.clone(),
        accent_color: docs.accent_color.clone(),
    }
}

fn normalize_public_path(path: &str) -> String {
    if path.trim().is_empty() || path == "/" {
        "/".to_string()
    } else {
        format!("/{}/", path.trim_matches('/'))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

    static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);

    struct Fixture(PathBuf);

    impl Fixture {
        fn new(label: &str) -> Self {
            let sequence = NEXT_FIXTURE.fetch_add(1, AtomicOrdering::Relaxed);
            let root = std::env::temp_dir().join(format!(
                "wake-app-{label}-{}-{sequence}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(root.join("src")).unwrap();
            std::fs::write(
                root.join(wake_config::CONFIG_FILE),
                "[html]\nentry = \"src/index.js\"\n",
            )
            .unwrap();
            std::fs::write(root.join("src/index.js"), "export const value = 42;\n").unwrap();
            Self(root)
        }

        fn project(&self) -> ProjectOptions {
            ProjectOptions {
                cwd: Some(self.0.clone()),
                config_path: None,
            }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn build_bundle_and_context_share_the_application_layer() {
        let fixture = Fixture::new("build");
        let options = BuildOptions {
            project: fixture.project(),
            outdir: Some(PathBuf::from("dist-node")),
            ..BuildOptions::default()
        };
        let result = build(options.clone(), &CancellationToken::default()).unwrap();
        assert!(result.success);
        assert!(result.files.iter().any(|file| file.kind == "html"));

        let bundled = bundle(options.clone(), &CancellationToken::default()).unwrap();
        assert!(bundled.code.as_deref().is_some_and(|code| !code.is_empty()));
        assert!(bundled.output_dir.is_none());

        let context = BuildContext::create(options).unwrap();
        let first = context.clone();
        let second = context.clone();
        let first = thread::spawn(move || first.rebuild(Vec::new(), CancellationToken::default()));
        let second =
            thread::spawn(move || second.rebuild(Vec::new(), CancellationToken::default()));
        assert!(first.join().unwrap().unwrap().success);
        assert!(second.join().unwrap().unwrap().success);

        let cancelled = CancellationToken::default();
        cancelled.cancel();
        assert_eq!(
            context.rebuild(Vec::new(), cancelled).unwrap_err().code,
            "WAKE_CANCELLED"
        );
        let first_close = context.clone();
        let second_close = context.clone();
        let first_close = thread::spawn(move || first_close.close());
        let second_close = thread::spawn(move || second_close.close());
        first_close.join().unwrap();
        second_close.join().unwrap();
        context.close();
        assert_eq!(
            context
                .rebuild(Vec::new(), CancellationToken::default())
                .unwrap_err()
                .code,
            "WAKE_INTERNAL"
        );
    }

    #[test]
    fn dev_server_close_is_idempotent_and_releases_its_port() {
        let fixture = Fixture::new("dev");
        let reservation = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = reservation.local_addr().unwrap().port();
        drop(reservation);

        let server = start_dev_server(DevServerOptions {
            project: fixture.project(),
            port: Some(port),
            ..DevServerOptions::default()
        })
        .unwrap();
        assert_eq!(server.url(), format!("http://127.0.0.1:{port}/"));
        let closing = server.clone();
        let waiting = server.clone();
        let closing = thread::spawn(move || closing.close());
        let waiting = thread::spawn(move || waiting.wait_until_closed());
        closing.join().unwrap().unwrap();
        waiting.join().unwrap().unwrap();
        server.close().unwrap();
        assert!(
            server
                .drain_events()
                .iter()
                .any(|event| matches!(event, DevServerEvent::Closed))
        );
        let rebound = TcpListener::bind(("127.0.0.1", port)).unwrap();
        drop(rebound);
    }

    #[test]
    fn dev_server_bind_failure_returns_without_leaving_a_watcher() {
        let fixture = Fixture::new("bind-failure");
        let reservation = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = reservation.local_addr().unwrap().port();
        let error = match start_dev_server(DevServerOptions {
            project: fixture.project(),
            port: Some(port),
            ..DevServerOptions::default()
        }) {
            Ok(server) => {
                let _ = server.close();
                panic!("occupied port unexpectedly accepted")
            }
            Err(error) => error,
        };
        assert_eq!(error.code, "WAKE_IO");
        drop(reservation);
    }
}
