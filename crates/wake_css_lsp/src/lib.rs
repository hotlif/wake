use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tokio::time::{Duration, sleep};
use tower_lsp_server::jsonrpc::Result;
use tower_lsp_server::ls_types::notification::Notification;
use tower_lsp_server::ls_types::*;
use tower_lsp_server::{Client, LanguageServer};
use wake_common::{FileSystem, OsFileSystem, SourceFile, Span};
use wake_css_in_js::value::Scope;
use wake_css_language::{
    CompletionKind as CssCompletionKind, HostLanguage, LanguageDiagnostic, LanguageDocument,
    LanguageSeverity, SemanticKind, TextEdit as CssTextEdit,
};
use wake_resolver::{ResolutionEnvironment, ResolveErrorKind, Resolver};

const LIVE_DEBOUNCE: Duration = Duration::from_millis(150);
const CLOSED_CACHE_ENTRIES: usize = 512;
const CLOSED_CACHE_BYTES: usize = 128 * 1024 * 1024;

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct TriggerSuggestParams {
    uri: Uri,
    version: i32,
    position: Position,
}

enum TriggerSuggest {}

impl Notification for TriggerSuggest {
    type Params = TriggerSuggestParams;

    const METHOD: &'static str = "crabCss/triggerSuggest";
}

pub const SERVER_NAME: &str = "wake-css-language-server";

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ValidationMode {
    Off,
    #[default]
    OnType,
    OnSave,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    pub enable: bool,
    pub validation: ValidationSettings,
    pub format: FormatSettings,
    pub trace: TraceSettings,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enable: true,
            validation: ValidationSettings::default(),
            format: FormatSettings::default(),
            trace: TraceSettings::default(),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ValidationSettings {
    pub mode: ValidationMode,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct FormatSettings {
    pub enable: bool,
}

impl Default for FormatSettings {
    fn default() -> Self {
        Self { enable: true }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TraceSettings {
    pub server: String,
}

struct OpenDocument {
    version: i32,
    language: HostLanguage,
    analysis: Arc<LanguageDocument>,
}

#[derive(Clone)]
struct CachedDependency {
    modified: Option<SystemTime>,
    source_len: usize,
    analysis: Arc<LanguageDocument>,
}

#[derive(Default)]
struct DependencyCache {
    entries: HashMap<PathBuf, CachedDependency>,
    order: VecDeque<PathBuf>,
    source_bytes: usize,
}

impl DependencyCache {
    fn get(
        &mut self,
        path: &Path,
        modified: Option<SystemTime>,
        source_len: usize,
    ) -> Option<Arc<LanguageDocument>> {
        let entry = self.entries.get(path)?;
        if entry.modified != modified || entry.source_len != source_len {
            self.remove(path);
            return None;
        }
        let analysis = Arc::clone(&entry.analysis);
        self.touch(path);
        Some(analysis)
    }

    fn get_immutable(&mut self, path: &Path) -> Option<Arc<LanguageDocument>> {
        let analysis = Arc::clone(&self.entries.get(path)?.analysis);
        self.touch(path);
        Some(analysis)
    }

    fn insert(&mut self, path: PathBuf, entry: CachedDependency) {
        self.remove(&path);
        self.source_bytes += entry.source_len;
        self.entries.insert(path.clone(), entry);
        self.order.push_back(path);
        while self.entries.len() > CLOSED_CACHE_ENTRIES || self.source_bytes > CLOSED_CACHE_BYTES {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            if let Some(removed) = self.entries.remove(&oldest) {
                self.source_bytes = self.source_bytes.saturating_sub(removed.source_len);
            }
        }
    }

    fn remove(&mut self, path: &Path) {
        self.order.retain(|candidate| candidate != path);
        if let Some(removed) = self.entries.remove(path) {
            self.source_bytes = self.source_bytes.saturating_sub(removed.source_len);
        }
    }

    fn touch(&mut self, path: &Path) {
        self.order.retain(|candidate| candidate != path);
        self.order.push_back(path.to_path_buf());
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
        self.source_bytes = 0;
    }
}

struct ResolverContext {
    fs: Arc<dyn FileSystem>,
    resolver: Arc<Resolver>,
    environment: ResolutionEnvironment,
}

struct WorkspaceAnalyzer {
    context: Arc<ResolverContext>,
    cache: Mutex<DependencyCache>,
    reverse_imports: Mutex<HashMap<PathBuf, HashSet<PathBuf>>>,
}

impl WorkspaceAnalyzer {
    fn new() -> Self {
        let os_fs: Arc<dyn FileSystem> = Arc::new(OsFileSystem);
        let environment = ResolutionEnvironment::new(os_fs);
        Self {
            context: Arc::new(ResolverContext {
                fs: environment.file_system(),
                resolver: environment.resolver(),
                environment,
            }),
            cache: Mutex::new(DependencyCache::default()),
            reverse_imports: Mutex::new(HashMap::new()),
        }
    }

    fn imported_scope(
        &self,
        path: &Path,
        document: &LanguageDocument,
        open: &HashMap<PathBuf, Arc<LanguageDocument>>,
    ) -> (Scope, Vec<LanguageDiagnostic>) {
        let context = self.resolver_context(path);
        let mut diagnostics = Vec::new();
        let scope = self.imported_scope_inner(
            path,
            document,
            open,
            &context,
            &mut HashSet::new(),
            &mut diagnostics,
            0,
        );
        (scope, diagnostics)
    }

    fn resolver_context(&self, _path: &Path) -> Arc<ResolverContext> {
        Arc::clone(&self.context)
    }

    fn imported_scope_inner(
        &self,
        path: &Path,
        document: &LanguageDocument,
        open: &HashMap<PathBuf, Arc<LanguageDocument>>,
        context: &ResolverContext,
        visiting: &mut HashSet<PathBuf>,
        diagnostics: &mut Vec<LanguageDiagnostic>,
        depth: usize,
    ) -> Scope {
        if depth >= CLOSED_CACHE_ENTRIES || !visiting.insert(path.to_path_buf()) {
            return Scope::default();
        }
        let mut imported = Scope::default();
        let from_dir = path.parent().unwrap_or_else(|| Path::new("."));
        for import in document.static_imports() {
            if import.specifier == "@crab-dev/css" || import.imported == "*" {
                continue;
            }
            let dependency_path = match context.resolver.resolve(&import.specifier, from_dir) {
                Ok(path) => path,
                Err(error) => {
                    if !matches!(error.kind(), ResolveErrorKind::NotFound) {
                        diagnostics.push(LanguageDiagnostic {
                            span: Span::DUMMY,
                            severity: LanguageSeverity::Error,
                            code: match error.kind() {
                                ResolveErrorKind::PnpManifest(_) => "WAKE_PNP_MANIFEST",
                                ResolveErrorKind::PnpDependency(_) => "WAKE_PNP_DEPENDENCY",
                                ResolveErrorKind::NotFound => unreachable!(),
                            }
                            .to_string(),
                            message: error.to_string(),
                        });
                    }
                    continue;
                }
            };
            self.reverse_imports
                .lock()
                .expect("reverse import graph lock")
                .entry(dependency_path.clone())
                .or_default()
                .insert(path.to_path_buf());
            let dependency = open
                .get(&dependency_path)
                .cloned()
                .or_else(|| self.load_dependency(&dependency_path, context));
            let Some(dependency) = dependency else {
                continue;
            };
            let dependency_scope = self.imported_scope_inner(
                &dependency_path,
                &dependency,
                open,
                context,
                visiting,
                diagnostics,
                depth + 1,
            );
            let exports = dependency.static_exports(&dependency_scope);
            if let Some(value) = exports.get(&import.imported) {
                imported.insert(import.local, value.clone());
            }
        }
        visiting.remove(path);
        imported
    }

    fn load_dependency(
        &self,
        path: &Path,
        context: &ResolverContext,
    ) -> Option<Arc<LanguageDocument>> {
        let metadata = std::fs::metadata(path).ok();
        if let Some(metadata) = &metadata {
            let modified = metadata.modified().ok();
            let source_len = usize::try_from(metadata.len()).unwrap_or(usize::MAX);
            if let Some(cached) = self
                .cache
                .lock()
                .expect("dependency cache lock")
                .get(path, modified, source_len)
            {
                return Some(cached);
            }
        } else if let Some(cached) = self
            .cache
            .lock()
            .expect("dependency cache lock")
            .get_immutable(path)
        {
            // Yarn cache archives are content-addressed. Workspace files remain ordinary files and
            // take the metadata-validated branch above; zip entries are immutable for this context.
            return Some(cached);
        }
        let source = context.fs.read_to_string(path).ok()?;
        let source_len = source.len();
        let modified = metadata.and_then(|metadata| metadata.modified().ok());
        let language = language_from_path(path)?;
        let analysis = Arc::new(LanguageDocument::analyze(
            path.to_string_lossy(),
            source,
            language,
        ));
        self.cache.lock().expect("dependency cache lock").insert(
            path.to_path_buf(),
            CachedDependency {
                modified,
                source_len,
                analysis: Arc::clone(&analysis),
            },
        );
        Some(analysis)
    }

    fn invalidate(&self, path: &Path) {
        let pnp_manifest_changed = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| matches!(name, ".pnp.cjs" | ".pnp.data.json" | "yarn.lock"));
        if pnp_manifest_changed {
            self.context
                .environment
                .invalidate_paths(std::iter::once(path));
            self.cache.lock().expect("dependency cache lock").clear();
        } else {
            self.cache
                .lock()
                .expect("dependency cache lock")
                .remove(path);
        }
    }

    fn affected_paths(&self, changed: &Path) -> HashSet<PathBuf> {
        let graph = self
            .reverse_imports
            .lock()
            .expect("reverse import graph lock");
        let mut affected = HashSet::from([changed.to_path_buf()]);
        let mut pending = VecDeque::from([changed.to_path_buf()]);
        while let Some(path) = pending.pop_front() {
            if let Some(importers) = graph.get(&path) {
                for importer in importers {
                    if affected.insert(importer.clone()) {
                        pending.push_back(importer.clone());
                    }
                }
            }
        }
        affected
    }
}

pub struct Backend {
    client: Client,
    documents: Arc<RwLock<HashMap<Uri, OpenDocument>>>,
    settings: Arc<RwLock<Settings>>,
    pending_live: Arc<Mutex<HashMap<Uri, JoinHandle<()>>>>,
    workspace: Arc<WorkspaceAnalyzer>,
    root: Arc<RwLock<Option<PathBuf>>>,
    compiler_compatible: Arc<AtomicBool>,
}

impl Backend {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            documents: Arc::new(RwLock::new(HashMap::new())),
            settings: Arc::new(RwLock::new(Settings::default())),
            pending_live: Arc::new(Mutex::new(HashMap::new())),
            workspace: Arc::new(WorkspaceAnalyzer::new()),
            root: Arc::new(RwLock::new(None)),
            compiler_compatible: Arc::new(AtomicBool::new(true)),
        }
    }

    async fn document(&self, uri: &Uri) -> Option<(i32, Arc<LanguageDocument>)> {
        self.documents
            .read()
            .await
            .get(uri)
            .map(|entry| (entry.version, Arc::clone(&entry.analysis)))
    }

    async fn schedule_live_diagnostics(&self, uri: Uri, version: i32) {
        let settings = self.settings.read().await.clone();
        if !settings.enable || settings.validation.mode != ValidationMode::OnType {
            return;
        }
        if let Some(task) = self
            .pending_live
            .lock()
            .expect("pending diagnostics lock")
            .remove(&uri)
        {
            task.abort();
        }
        let client = self.client.clone();
        let documents = Arc::clone(&self.documents);
        let task_uri = uri.clone();
        let task = tokio::spawn(async move {
            sleep(LIVE_DEBOUNCE).await;
            let analysis = {
                let documents = documents.read().await;
                let Some(entry) = documents.get(&task_uri) else {
                    return;
                };
                if entry.version != version {
                    return;
                }
                Arc::clone(&entry.analysis)
            };
            client
                .publish_diagnostics(
                    task_uri,
                    to_lsp_diagnostics(analysis.source(), analysis.diagnostics()),
                    Some(version),
                )
                .await;
        });
        self.pending_live
            .lock()
            .expect("pending diagnostics lock")
            .insert(uri, task);
    }

    async fn publish_saved_diagnostics(&self, uri: &Uri) {
        let settings = self.settings.read().await.clone();
        if !settings.enable || settings.validation.mode == ValidationMode::Off {
            self.client
                .publish_diagnostics(uri.clone(), Vec::new(), None)
                .await;
            return;
        }
        let open = open_path_snapshot(&self.documents).await;
        let affected = uri.to_file_path().map_or_else(HashSet::new, |path| {
            self.workspace.affected_paths(path.as_ref())
        });
        let targets = self
            .documents
            .read()
            .await
            .iter()
            .filter_map(|(document_uri, entry)| {
                let path = document_uri.to_file_path();
                let selected = document_uri == uri
                    || path
                        .as_ref()
                        .is_some_and(|path| affected.contains(path.as_ref()));
                selected.then(|| {
                    (
                        document_uri.clone(),
                        entry.version,
                        Arc::clone(&entry.analysis),
                        path.map(|path| path.into_owned()),
                    )
                })
            })
            .collect::<Vec<_>>();
        for (document_uri, version, analysis, path) in targets {
            let Some(path) = path else {
                if document_uri == *uri {
                    self.client
                        .publish_diagnostics(
                            document_uri.clone(),
                            to_lsp_diagnostics(analysis.source(), analysis.diagnostics()),
                            Some(version),
                        )
                        .await;
                }
                continue;
            };
            let workspace = Arc::clone(&self.workspace);
            let open = open.clone();
            let analysis_for_task = Arc::clone(&analysis);
            let path_for_task = path.clone();
            let compatible = self.compiler_compatible.load(Ordering::Acquire);
            let mut diagnostics = tokio::task::spawn_blocking(move || {
                let mut diagnostics = analysis_for_task.diagnostics().to_vec();
                if compatible {
                    let (imported, resolution_diagnostics) =
                        workspace.imported_scope(&path_for_task, &analysis_for_task, &open);
                    diagnostics.extend(resolution_diagnostics);
                    diagnostics.extend(
                        analysis_for_task
                            .compiler_diagnostics(&path_for_task.to_string_lossy(), &imported),
                    );
                }
                diagnostics
            })
            .await
            .unwrap_or_else(|_| analysis.diagnostics().to_vec());
            diagnostics.sort_by_key(|diagnostic| (diagnostic.span.lo, diagnostic.span.hi));
            diagnostics.dedup_by(|left, right| {
                left.span == right.span && left.code == right.code && left.message == right.message
            });
            if self
                .document(&document_uri)
                .await
                .is_some_and(|(current_version, _)| current_version == version)
            {
                self.client
                    .publish_diagnostics(
                        document_uri,
                        to_lsp_diagnostics(analysis.source(), &diagnostics),
                        Some(version),
                    )
                    .await;
            }
        }
    }
}

impl LanguageServer for Backend {
    async fn initialize(&self, params: InitializeParams) -> Result<InitializeResult> {
        let root = initialize_root(&params);
        *self.root.write().await = root;
        if let Some(options) = params.initialization_options
            && let Ok(settings) = serde_json::from_value::<Settings>(options)
        {
            *self.settings.write().await = settings;
        }
        Ok(InitializeResult {
            capabilities: server_capabilities(),
            server_info: Some(ServerInfo {
                name: SERVER_NAME.to_string(),
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
            }),
            ..InitializeResult::default()
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "Crab CSS language server initialized")
            .await;
        if let Some(root) = self.root.read().await.clone()
            && let Some(version) = crab_css_version(&root)
            && !version_is_supported(&version)
        {
            self.compiler_compatible.store(false, Ordering::Release);
            self.client
                .show_message(
                    MessageType::WARNING,
                    format!(
                        "@crab-dev/css {version} is outside the supported range >=0.1.0 <0.2.0; exact compiler diagnostics are disabled"
                    ),
                )
                .await;
        }
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let item = params.text_document;
        let language = language_from_id(&item.language_id);
        let analysis = Arc::new(LanguageDocument::analyze(
            item.uri.to_string(),
            item.text,
            language,
        ));
        self.documents.write().await.insert(
            item.uri.clone(),
            OpenDocument {
                version: item.version,
                language,
                analysis,
            },
        );
        self.schedule_live_diagnostics(item.uri, item.version).await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;
        let version = params.text_document.version;
        let completion_position = automatic_completion_position(&params.content_changes);
        let mut documents = self.documents.write().await;
        let Some(previous) = documents.get(&uri) else {
            return;
        };
        let text = apply_content_changes(previous.analysis.source(), params.content_changes);
        let language = previous.language;
        let analysis = Arc::new(LanguageDocument::analyze(uri.to_string(), text, language));
        let should_trigger_completion = completion_position.is_some_and(|position| {
            let offset = position_to_offset(analysis.source(), position);
            analysis
                .completions(offset)
                .is_some_and(|items| !items.is_empty())
        });
        documents.insert(
            uri.clone(),
            OpenDocument {
                version,
                language,
                analysis,
            },
        );
        drop(documents);
        if should_trigger_completion {
            self.client
                .send_notification::<TriggerSuggest>(TriggerSuggestParams {
                    uri: uri.clone(),
                    version,
                    position: completion_position.expect("checked completion position"),
                })
                .await;
        }
        self.schedule_live_diagnostics(uri, version).await;
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        if let Some(path) = params.text_document.uri.to_file_path() {
            self.workspace.invalidate(path.as_ref());
        }
        self.publish_saved_diagnostics(&params.text_document.uri)
            .await;
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        if let Some(task) = self
            .pending_live
            .lock()
            .expect("pending diagnostics lock")
            .remove(&params.text_document.uri)
        {
            task.abort();
        }
        self.documents
            .write()
            .await
            .remove(&params.text_document.uri);
        self.client
            .publish_diagnostics(params.text_document.uri, Vec::new(), None)
            .await;
    }

    async fn did_change_configuration(&self, params: DidChangeConfigurationParams) {
        if let Ok(settings) = serde_json::from_value::<Settings>(params.settings) {
            *self.settings.write().await = settings;
        }
    }

    async fn did_change_watched_files(&self, params: DidChangeWatchedFilesParams) {
        for change in params.changes {
            if let Some(path) = change.uri.to_file_path() {
                self.workspace.invalidate(path.as_ref());
                if self.documents.read().await.contains_key(&change.uri) {
                    self.publish_saved_diagnostics(&change.uri).await;
                }
            }
        }
    }

    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        let uri = &params.text_document_position.text_document.uri;
        let Some((_, document)) = self.document(uri).await else {
            return Ok(None);
        };
        let offset = position_to_offset(document.source(), params.text_document_position.position);
        let Some(completions) = document.completions(offset) else {
            return Ok(None);
        };
        let items = completions
            .into_iter()
            .enumerate()
            .map(|(sort_index, item)| CompletionItem {
                label: item.label,
                kind: Some(match item.kind {
                    CssCompletionKind::Property => CompletionItemKind::PROPERTY,
                    CssCompletionKind::Value => CompletionItemKind::VALUE,
                    CssCompletionKind::Keyword => CompletionItemKind::KEYWORD,
                }),
                detail: Some(item.detail),
                documentation: Some(Documentation::MarkupContent(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value: item.documentation,
                })),
                insert_text: Some(item.insert_text),
                sort_text: Some(format!("{sort_index:04}")),
                ..CompletionItem::default()
            })
            .collect();
        Ok(Some(CompletionResponse::Array(items)))
    }

    async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let Some((_, document)) = self.document(uri).await else {
            return Ok(None);
        };
        let offset = position_to_offset(
            document.source(),
            params.text_document_position_params.position,
        );
        Ok(document.hover(offset).map(|hover| Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: hover.markdown,
            }),
            range: Some(span_to_range(document.source(), hover.span)),
        }))
    }

    async fn semantic_tokens_full(
        &self,
        params: SemanticTokensParams,
    ) -> Result<Option<SemanticTokensResult>> {
        let Some((_, document)) = self.document(&params.text_document.uri).await else {
            return Ok(None);
        };
        let data = encode_semantic_tokens(&document);
        Ok(Some(SemanticTokensResult::Tokens(SemanticTokens {
            result_id: None,
            data,
        })))
    }

    #[allow(deprecated)]
    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> Result<Option<DocumentSymbolResponse>> {
        let Some((_, document)) = self.document(&params.text_document.uri).await else {
            return Ok(None);
        };
        let symbols = document
            .symbols()
            .into_iter()
            .map(|symbol| SymbolInformation {
                name: symbol.name,
                kind: SymbolKind::OBJECT,
                tags: None,
                deprecated: None,
                location: Location {
                    uri: params.text_document.uri.clone(),
                    range: span_to_range(document.source(), symbol.span),
                },
                container_name: Some("Crab CSS".to_string()),
            })
            .collect();
        Ok(Some(DocumentSymbolResponse::Flat(symbols)))
    }

    async fn folding_range(&self, params: FoldingRangeParams) -> Result<Option<Vec<FoldingRange>>> {
        let Some((_, document)) = self.document(&params.text_document.uri).await else {
            return Ok(None);
        };
        Ok(Some(
            document
                .folding_ranges()
                .into_iter()
                .map(|span| {
                    let range = span_to_range(document.source(), span);
                    FoldingRange {
                        start_line: range.start.line,
                        start_character: Some(range.start.character),
                        end_line: range.end.line,
                        end_character: Some(range.end.character),
                        kind: Some(FoldingRangeKind::Region),
                        collapsed_text: None,
                    }
                })
                .collect(),
        ))
    }

    async fn selection_range(
        &self,
        params: SelectionRangeParams,
    ) -> Result<Option<Vec<SelectionRange>>> {
        let Some((_, document)) = self.document(&params.text_document.uri).await else {
            return Ok(None);
        };
        Ok(Some(
            params
                .positions
                .into_iter()
                .map(|position| {
                    let offset = position_to_offset(document.source(), position);
                    let span = document
                        .selection_span(offset)
                        .unwrap_or_else(|| Span::at(offset));
                    SelectionRange {
                        range: span_to_range(document.source(), span),
                        parent: None,
                    }
                })
                .collect(),
        ))
    }

    async fn document_color(&self, params: DocumentColorParams) -> Result<Vec<ColorInformation>> {
        let Some((_, document)) = self.document(&params.text_document.uri).await else {
            return Ok(Vec::new());
        };
        Ok(document
            .colors()
            .into_iter()
            .map(|color| ColorInformation {
                range: span_to_range(document.source(), color.span),
                color: Color {
                    red: color.red,
                    green: color.green,
                    blue: color.blue,
                    alpha: color.alpha,
                },
            })
            .collect())
    }

    async fn color_presentation(
        &self,
        params: ColorPresentationParams,
    ) -> Result<Vec<ColorPresentation>> {
        let color = params.color;
        let red = (color.red * 255.0).round() as u8;
        let green = (color.green * 255.0).round() as u8;
        let blue = (color.blue * 255.0).round() as u8;
        let alpha = (color.alpha * 255.0).round() as u8;
        let label = if alpha == u8::MAX {
            format!("#{red:02x}{green:02x}{blue:02x}")
        } else {
            format!("#{red:02x}{green:02x}{blue:02x}{alpha:02x}")
        };
        Ok(vec![ColorPresentation {
            label: label.clone(),
            text_edit: Some(TextEdit {
                range: params.range,
                new_text: label,
            }),
            additional_text_edits: None,
        }])
    }

    async fn formatting(&self, params: DocumentFormattingParams) -> Result<Option<Vec<TextEdit>>> {
        let settings = self.settings.read().await.clone();
        if !settings.enable || !settings.format.enable {
            return Ok(None);
        }
        let Some((_, document)) = self.document(&params.text_document.uri).await else {
            return Ok(None);
        };
        Ok(Some(to_lsp_text_edits(&document, document.format(None))))
    }

    async fn range_formatting(
        &self,
        params: DocumentRangeFormattingParams,
    ) -> Result<Option<Vec<TextEdit>>> {
        let settings = self.settings.read().await.clone();
        if !settings.enable || !settings.format.enable {
            return Ok(None);
        }
        let Some((_, document)) = self.document(&params.text_document.uri).await else {
            return Ok(None);
        };
        let range = range_to_span(document.source(), params.range);
        Ok(Some(to_lsp_text_edits(
            &document,
            document.format(Some(range)),
        )))
    }

    async fn code_action(&self, params: CodeActionParams) -> Result<Option<CodeActionResponse>> {
        let Some((_, document)) = self.document(&params.text_document.uri).await else {
            return Ok(None);
        };
        let mut actions = Vec::new();
        for diagnostic in params.context.diagnostics {
            let Some(code) = diagnostic.code.as_ref().and_then(NumberOrString::as_str) else {
                continue;
            };
            let (title, edit) = match code {
                "CSS_UNEXPECTED_BRACE" => (
                    "Remove unexpected closing brace",
                    TextEdit {
                        range: diagnostic.range,
                        new_text: String::new(),
                    },
                ),
                "CSS_PROPERTY_CASE" => {
                    let span = range_to_span(document.source(), diagnostic.range);
                    (
                        "Normalize CSS property casing",
                        TextEdit {
                            range: diagnostic.range,
                            new_text: span.slice(document.source().src()).to_ascii_lowercase(),
                        },
                    )
                }
                "CSS_UNCLOSED_BLOCK" => {
                    let offset = range_to_span(document.source(), diagnostic.range).hi;
                    let Some(template) = document
                        .virtual_documents()
                        .iter()
                        .find(|template| template.template_span.contains_offset(offset))
                    else {
                        continue;
                    };
                    let insert = template
                        .segments
                        .last()
                        .map_or(offset, |segment| segment.host.hi);
                    (
                        "Close CSS block",
                        TextEdit {
                            range: span_to_range(document.source(), Span::at(insert)),
                            new_text: "}".to_string(),
                        },
                    )
                }
                _ => continue,
            };
            let mut changes = HashMap::new();
            changes.insert(params.text_document.uri.clone(), vec![edit]);
            actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                title: title.to_string(),
                kind: Some(CodeActionKind::QUICKFIX),
                diagnostics: Some(vec![diagnostic]),
                edit: Some(WorkspaceEdit {
                    changes: Some(changes),
                    document_changes: None,
                    change_annotations: None,
                }),
                is_preferred: Some(true),
                ..CodeAction::default()
            }));
        }
        Ok(Some(actions))
    }
}

fn server_capabilities() -> ServerCapabilities {
    ServerCapabilities {
        position_encoding: Some(PositionEncodingKind::UTF16),
        text_document_sync: Some(TextDocumentSyncCapability::Options(
            TextDocumentSyncOptions {
                open_close: Some(true),
                change: Some(TextDocumentSyncKind::INCREMENTAL),
                save: Some(TextDocumentSyncSaveOptions::Supported(true)),
                ..TextDocumentSyncOptions::default()
            },
        )),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        completion_provider: Some(CompletionOptions {
            trigger_characters: Some(vec![":", "@", "-"].into_iter().map(String::from).collect()),
            ..CompletionOptions::default()
        }),
        selection_range_provider: Some(SelectionRangeProviderCapability::Simple(true)),
        document_symbol_provider: Some(OneOf::Left(true)),
        code_action_provider: Some(CodeActionProviderCapability::Simple(true)),
        document_formatting_provider: Some(OneOf::Left(true)),
        document_range_formatting_provider: Some(OneOf::Left(true)),
        color_provider: Some(ColorProviderCapability::Simple(true)),
        folding_range_provider: Some(FoldingRangeProviderCapability::Simple(true)),
        semantic_tokens_provider: Some(
            SemanticTokensOptions {
                work_done_progress_options: WorkDoneProgressOptions::default(),
                legend: SemanticTokensLegend {
                    token_types: vec![
                        SemanticTokenType::PROPERTY,
                        SemanticTokenType::KEYWORD,
                        SemanticTokenType::NUMBER,
                        SemanticTokenType::STRING,
                        SemanticTokenType::FUNCTION,
                        SemanticTokenType::new("crabCssValue"),
                    ],
                    token_modifiers: Vec::new(),
                },
                range: Some(false),
                full: Some(SemanticTokensFullOptions::Bool(true)),
            }
            .into(),
        ),
        workspace: Some(WorkspaceServerCapabilities {
            workspace_folders: Some(WorkspaceFoldersServerCapabilities {
                supported: Some(true),
                change_notifications: Some(OneOf::Left(true)),
            }),
            file_operations: None,
        }),
        ..ServerCapabilities::default()
    }
}

#[allow(deprecated)]
fn initialize_root(params: &InitializeParams) -> Option<PathBuf> {
    params
        .workspace_folders
        .as_ref()
        .and_then(|folders| folders.first())
        .and_then(|folder| folder.uri.to_file_path())
        .map(|path| path.into_owned())
        .or_else(|| {
            params
                .root_uri
                .as_ref()
                .and_then(|uri| uri.to_file_path().map(|path| path.into_owned()))
        })
}

async fn open_path_snapshot(
    documents: &RwLock<HashMap<Uri, OpenDocument>>,
) -> HashMap<PathBuf, Arc<LanguageDocument>> {
    documents
        .read()
        .await
        .iter()
        .filter_map(|(uri, entry)| {
            uri.to_file_path()
                .map(|path| (path.into_owned(), Arc::clone(&entry.analysis)))
        })
        .collect()
}

fn apply_content_changes(
    source: &SourceFile,
    changes: Vec<TextDocumentContentChangeEvent>,
) -> String {
    let mut text = source.src().to_string();
    for change in changes {
        let Some(range) = change.range else {
            text = change.text;
            continue;
        };
        let current = SourceFile::new(source.name(), text);
        let span = range_to_span(&current, range);
        text = current.src().to_string();
        text.replace_range(span.lo as usize..span.hi as usize, &change.text);
    }
    text
}

fn automatic_completion_position(changes: &[TextDocumentContentChangeEvent]) -> Option<Position> {
    let [change] = changes else {
        return None;
    };
    let range = change.range?;
    let is_identifier_insertion = range.start == range.end
        && !change.text.is_empty()
        && change
            .text
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'));
    let is_property_completion = change.text.strip_suffix(": ").is_some_and(|property| {
        !property.is_empty()
            && property.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '_' | '-')
            })
    });
    if change.text.len() > 64 || (!is_identifier_insertion && !is_property_completion) {
        return None;
    }
    Some(Position::new(
        range.start.line,
        range.start.character + change.text.encode_utf16().count() as u32,
    ))
}

fn position_to_offset(source: &SourceFile, position: Position) -> u32 {
    source.offset0_utf16(position.line, position.character)
}

fn span_to_range(source: &SourceFile, span: Span) -> Range {
    let (start_line, start_character) = source.location0_utf16(span.lo);
    let (end_line, end_character) = source.location0_utf16(span.hi);
    Range {
        start: Position::new(start_line, start_character),
        end: Position::new(end_line, end_character),
    }
}

fn range_to_span(source: &SourceFile, range: Range) -> Span {
    Span::new(
        position_to_offset(source, range.start),
        position_to_offset(source, range.end),
    )
}

fn to_lsp_diagnostics(source: &SourceFile, diagnostics: &[LanguageDiagnostic]) -> Vec<Diagnostic> {
    diagnostics
        .iter()
        .map(|diagnostic| Diagnostic {
            range: span_to_range(source, diagnostic.span),
            severity: Some(match diagnostic.severity {
                LanguageSeverity::Error => DiagnosticSeverity::ERROR,
                LanguageSeverity::Warning => DiagnosticSeverity::WARNING,
                LanguageSeverity::Information => DiagnosticSeverity::INFORMATION,
            }),
            code: Some(NumberOrString::String(diagnostic.code.clone())),
            source: Some("crab-css".to_string()),
            message: diagnostic.message.clone(),
            ..Diagnostic::default()
        })
        .collect()
}

fn to_lsp_text_edits(document: &LanguageDocument, edits: Vec<CssTextEdit>) -> Vec<TextEdit> {
    edits
        .into_iter()
        .map(|edit| TextEdit {
            range: span_to_range(document.source(), edit.span),
            new_text: edit.replacement,
        })
        .collect()
}

fn encode_semantic_tokens(document: &LanguageDocument) -> Vec<SemanticToken> {
    let mut previous_line = 0;
    let mut previous_start = 0;
    document
        .semantic_tokens()
        .into_iter()
        .filter_map(|token| {
            let range = span_to_range(document.source(), token.span);
            (range.start.line == range.end.line).then(|| {
                let delta_line = range.start.line - previous_line;
                let delta_start = if delta_line == 0 {
                    range.start.character - previous_start
                } else {
                    range.start.character
                };
                previous_line = range.start.line;
                previous_start = range.start.character;
                SemanticToken {
                    delta_line,
                    delta_start,
                    length: range.end.character - range.start.character,
                    token_type: match token.kind {
                        SemanticKind::Property => 0,
                        SemanticKind::Value => 5,
                        SemanticKind::Keyword => 1,
                        SemanticKind::Number => 2,
                        SemanticKind::String => 3,
                        SemanticKind::Function => 4,
                    },
                    token_modifiers_bitset: 0,
                }
            })
        })
        .collect()
}

fn language_from_id(language_id: &str) -> HostLanguage {
    match language_id {
        "javascriptreact" => HostLanguage::JavaScriptReact,
        "typescript" => HostLanguage::TypeScript,
        "typescriptreact" => HostLanguage::TypeScriptReact,
        _ => HostLanguage::JavaScript,
    }
}

fn language_from_path(path: &Path) -> Option<HostLanguage> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "jsx" => Some(HostLanguage::JavaScriptReact),
        "ts" | "mts" | "cts" => Some(HostLanguage::TypeScript),
        "tsx" => Some(HostLanguage::TypeScriptReact),
        "js" | "mjs" | "cjs" => Some(HostLanguage::JavaScript),
        _ => None,
    }
}

fn crab_css_version(root: &Path) -> Option<String> {
    let package: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(root.join("package.json")).ok()?).ok()?;
    ["dependencies", "devDependencies", "peerDependencies"]
        .into_iter()
        .find_map(|section| {
            package
                .get(section)?
                .get("@crab-dev/css")?
                .as_str()
                .map(str::to_string)
        })
}

fn version_is_supported(version: &str) -> bool {
    let normalized = version.trim_start_matches(['^', '~', '=', 'v', ' ']);
    normalized == "workspace:*" || normalized.starts_with("0.1.")
}

trait NumberOrStringExt {
    fn as_str(&self) -> Option<&str>;
}

impl NumberOrStringExt for NumberOrString {
    fn as_str(&self) -> Option<&str> {
        match self {
            NumberOrString::String(value) => Some(value),
            NumberOrString::Number(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wake_css_in_js::StaticValue;

    #[test]
    fn applies_incremental_utf16_edits() {
        let source = SourceFile::new("a.ts", "const 𝒳 = 'red';\r\nnext");
        let changed = apply_content_changes(
            &source,
            vec![TextDocumentContentChangeEvent {
                range: Some(Range::new(Position::new(0, 12), Position::new(0, 15))),
                range_length: None,
                text: "blue".to_string(),
            }],
        );
        assert_eq!(changed, "const 𝒳 = 'blue';\r\nnext");
    }

    #[test]
    fn derives_automatic_completion_positions_only_from_identifier_insertions() {
        let insertion = TextDocumentContentChangeEvent {
            range: Some(Range::new(Position::new(4, 8), Position::new(4, 8))),
            range_length: Some(0),
            text: "disp".to_string(),
        };
        assert_eq!(
            automatic_completion_position(std::slice::from_ref(&insertion)),
            Some(Position::new(4, 12))
        );

        let property_completion = TextDocumentContentChangeEvent {
            range: Some(Range::new(Position::new(4, 8), Position::new(4, 12))),
            range_length: Some(4),
            text: "display: ".to_string(),
        };
        assert_eq!(
            automatic_completion_position(&[property_completion]),
            Some(Position::new(4, 17))
        );

        let unrelated_replacement = TextDocumentContentChangeEvent {
            range: Some(Range::new(Position::new(4, 8), Position::new(4, 9))),
            range_length: Some(1),
            text: "d".to_string(),
        };
        assert_eq!(
            automatic_completion_position(&[unrelated_replacement]),
            None
        );
        assert_eq!(automatic_completion_position(&[]), None);
        assert_eq!(
            automatic_completion_position(&[insertion.clone(), insertion]),
            None
        );
    }

    #[test]
    fn semantic_tokens_are_monotonic_and_utf16_encoded() {
        let source = "// 𝒳\nimport { css } from '@crab-dev/css';\nconst box = css`display: grid;`;";
        let document = LanguageDocument::analyze("a.ts", source, HostLanguage::TypeScript);
        let tokens = encode_semantic_tokens(&document);
        assert!(!tokens.is_empty());
        assert!(tokens.iter().all(|token| token.length > 0));
        assert!(tokens.iter().any(|token| token.token_type == 5));
    }

    #[test]
    fn dependency_cache_is_bounded_and_invalidates_changed_files() {
        let mut cache = DependencyCache::default();
        for index in 0..CLOSED_CACHE_ENTRIES + 5 {
            let source = format!("export const value = {index};");
            cache.insert(
                PathBuf::from(format!("module-{index}.ts")),
                CachedDependency {
                    modified: None,
                    source_len: source.len(),
                    analysis: Arc::new(LanguageDocument::analyze(
                        format!("module-{index}.ts"),
                        source,
                        HostLanguage::TypeScript,
                    )),
                },
            );
        }
        assert_eq!(cache.entries.len(), CLOSED_CACHE_ENTRIES);
        assert!(!cache.entries.contains_key(Path::new("module-0.ts")));
    }

    #[test]
    fn supported_version_range_is_strict_for_zero_major() {
        assert!(version_is_supported("0.1.16"));
        assert!(version_is_supported("^0.1.0"));
        assert!(!version_is_supported("0.2.0"));
        assert!(!version_is_supported("1.0.0"));
    }

    #[test]
    fn capabilities_do_not_compete_with_typescript_navigation() {
        let capabilities = server_capabilities();
        let Some(SemanticTokensServerCapabilities::SemanticTokensOptions(options)) =
            capabilities.semantic_tokens_provider
        else {
            panic!("semantic token options");
        };
        assert_eq!(
            options.legend.token_types,
            vec![
                SemanticTokenType::PROPERTY,
                SemanticTokenType::KEYWORD,
                SemanticTokenType::NUMBER,
                SemanticTokenType::STRING,
                SemanticTokenType::FUNCTION,
                SemanticTokenType::new("crabCssValue"),
            ]
        );
        let completion = capabilities
            .completion_provider
            .expect("completion provider options");
        let triggers = completion
            .trigger_characters
            .expect("completion trigger characters");
        assert_eq!(triggers, vec![":", "@", "-"]);
        assert!(capabilities.definition_provider.is_none());
        assert!(capabilities.references_provider.is_none());
        assert!(capabilities.rename_provider.is_none());
    }

    #[test]
    fn saved_analysis_resolves_static_exports_and_tracks_reverse_imports() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("wake-css-lsp-{unique}"));
        std::fs::create_dir_all(&root).unwrap();
        let tokens = root.join("tokens.ts");
        let component = root.join("component.ts");
        std::fs::write(&tokens, "export const color = 'red';").unwrap();
        let source = "import { color } from './tokens';\n\
            import { css } from '@crab-dev/css';\n\
            export const box = css`color: ${color};`;";
        std::fs::write(&component, source).unwrap();
        let document = Arc::new(LanguageDocument::analyze(
            component.to_string_lossy(),
            source,
            HostLanguage::TypeScript,
        ));
        let workspace = WorkspaceAnalyzer::new();
        let open = HashMap::from([(component.clone(), Arc::clone(&document))]);
        let (imported, resolution_diagnostics) =
            workspace.imported_scope(&component, &document, &open);
        assert!(resolution_diagnostics.is_empty());
        assert_eq!(
            imported.get("color"),
            Some(&StaticValue::Str("red".to_string()))
        );
        assert!(
            document
                .compiler_diagnostics(&component.to_string_lossy(), &imported)
                .is_empty()
        );
        assert!(workspace.affected_paths(&tokens).contains(&component));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn saved_analysis_resolves_static_exports_from_pnp_workspace_packages() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("wake-css-lsp-pnp-{unique}"));
        let button = root.join("packages/button");
        let spin = root.join("packages/spin");
        std::fs::create_dir_all(button.join("src")).unwrap();
        std::fs::create_dir_all(spin.join("esm")).unwrap();
        std::fs::write(
            root.join(".pnp.cjs"),
            "module.exports = require('./.pnp.data.json');",
        )
        .unwrap();
        std::fs::write(
            root.join(".pnp.data.json"),
            r#"{
                "enableTopLevelFallback": false,
                "fallbackExclusionList": [],
                "fallbackPool": [],
                "packageRegistryData": [
                    [null, [[null, {
                        "packageLocation": "./",
                        "packageDependencies": [["@crab-dev/rc-button", "workspace:packages/button"]],
                        "linkType": "SOFT"
                    }]]],
                    ["@crab-dev/rc-button", [["workspace:packages/button", {
                        "packageLocation": "./packages/button/",
                        "packageDependencies": [
                            ["@crab-dev/rc-button", "workspace:packages/button"],
                            ["@crab-dev/rc-spin", "workspace:packages/spin"]
                        ],
                        "linkType": "SOFT"
                    }]]],
                    ["@crab-dev/rc-spin", [["workspace:packages/spin", {
                        "packageLocation": "./packages/spin/",
                        "packageDependencies": [["@crab-dev/rc-spin", "workspace:packages/spin"]],
                        "linkType": "SOFT"
                    }]]]
                ]
            }"#,
        )
        .unwrap();
        std::fs::write(
            spin.join("package.json"),
            r#"{"exports":{".":{"import":"./esm/index.mjs"}}}"#,
        )
        .unwrap();
        std::fs::write(
            spin.join("esm/index.mjs"),
            "import { defineTokens } from '@crab-dev/css'; const v = defineTokens({'ring.indicator-color': '--spin-ring-indicator-color'}); export { v as vars };",
        )
        .unwrap();
        let component = button.join("src/button.tsx");
        let source = "import { css } from '@crab-dev/css';\n\
            import { vars as spinVars } from '@crab-dev/rc-spin';\n\
            export const loading = css`${spinVars['ring.indicator-color']}: currentColor;`;";
        std::fs::write(&component, source).unwrap();
        let document = Arc::new(LanguageDocument::analyze(
            component.to_string_lossy(),
            source,
            HostLanguage::TypeScriptReact,
        ));
        let workspace = WorkspaceAnalyzer::new();
        let context = workspace.resolver_context(&component);
        let resolved = context
            .resolver
            .resolve("@crab-dev/rc-spin", component.parent().unwrap());
        let expected = spin.join("esm/index.mjs");
        assert_eq!(resolved.as_deref(), Ok(expected.as_path()));
        let dependency = workspace.load_dependency(&expected, &context).unwrap();
        let exports = dependency.static_exports(&Scope::default());
        assert!(
            matches!(exports.get("vars"), Some(StaticValue::Frozen(_))),
            "defineTokens export must be frozen: {exports:?}"
        );
        let open = HashMap::from([(component.clone(), Arc::clone(&document))]);
        let (imported, resolution_diagnostics) =
            workspace.imported_scope(&component, &document, &open);
        assert!(resolution_diagnostics.is_empty());
        assert!(
            matches!(imported.get("spinVars"), Some(StaticValue::Frozen(_))),
            "PnP workspace export must enter the compiler scope: {imported:?}"
        );
        assert!(
            document
                .compiler_diagnostics(&component.to_string_lossy(), &imported)
                .is_empty(),
            "PnP workspace static interpolation must not be reported as dynamic"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn broken_pnp_manifest_is_reported_instead_of_using_the_default_resolver() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("wake-css-lsp-broken-pnp-{unique}"));
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join(".pnp.cjs"), "module.exports = {").unwrap();
        std::fs::create_dir_all(root.join("node_modules/ghost")).unwrap();
        std::fs::write(
            root.join("node_modules/ghost/index.js"),
            "export const color = 'red';",
        )
        .unwrap();
        let component = root.join("src/component.ts");
        let source = "import { color } from 'ghost'; export const value = color;";
        std::fs::write(&component, source).unwrap();
        let document = Arc::new(LanguageDocument::analyze(
            component.to_string_lossy(),
            source,
            HostLanguage::TypeScript,
        ));
        let workspace = WorkspaceAnalyzer::new();
        let open = HashMap::from([(component.clone(), Arc::clone(&document))]);
        let (_, diagnostics) = workspace.imported_scope(&component, &document, &open);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code, "WAKE_PNP_MANIFEST");
        assert!(diagnostics[0].message.contains(".pnp.cjs"));
        std::fs::remove_dir_all(&root).unwrap();
    }
}
