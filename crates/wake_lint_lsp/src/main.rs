use std::borrow::Cow;
use std::collections::HashMap;
use std::path::PathBuf;

use tokio::sync::RwLock;
use tower_lsp_server::jsonrpc::Result;
use tower_lsp_server::ls_types::*;
use tower_lsp_server::{Client, LanguageServer, LspService, Server};
use wake_app::{LintProjectOptions, LintStdin};

const SERVER_NAME: &str = "wake-lint-language-server";

struct Document {
    version: i32,
    text: String,
}

struct Backend {
    client: Client,
    root: RwLock<PathBuf>,
    documents: RwLock<HashMap<Uri, Document>>,
}

impl Backend {
    async fn publish(&self, uri: Uri) {
        let Some(document) = self
            .documents
            .read()
            .await
            .get(&uri)
            .map(|doc| (doc.version, doc.text.clone()))
        else {
            return;
        };
        let path = uri
            .to_file_path()
            .map_or_else(|| PathBuf::from(uri.as_str()), |path| path.into_owned());
        let root = self.root.read().await.clone();
        let result = wake_app::lint_project(
            LintProjectOptions {
                root: root.clone(),
                stdin: Some(LintStdin {
                    filename: virtual_filename(&root, &path),
                    text: document.1.clone(),
                }),
                ..Default::default()
            },
            &wake_app::CancellationToken::default(),
        );
        let diagnostics = result
            .ok()
            .and_then(|result| result.files.into_iter().next())
            .map(|file| {
                file.diagnostics
                    .into_iter()
                    .filter_map(|diagnostic| to_lsp_diagnostic(&document.1, diagnostic.diagnostic))
                    .collect()
            })
            .unwrap_or_default();
        let still_current = self
            .documents
            .read()
            .await
            .get(&uri)
            .is_some_and(|current| current.version == document.0);
        if !still_current {
            return;
        }
        self.client
            .publish_diagnostics(uri, diagnostics, Some(document.0))
            .await;
    }
}

impl LanguageServer for Backend {
    async fn initialize(&self, params: InitializeParams) -> Result<InitializeResult> {
        if let Some(root) = params
            .workspace_folders
            .as_ref()
            .and_then(|folders| folders.first())
            .and_then(|folder| folder.uri.to_file_path().map(Cow::into_owned))
        {
            *self.root.write().await = root;
        }
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                code_action_provider: Some(CodeActionProviderCapability::Simple(true)),
                ..Default::default()
            },
            server_info: Some(ServerInfo {
                name: SERVER_NAME.into(),
                version: Some(env!("CARGO_PKG_VERSION").into()),
            }),
            ..Default::default()
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "Wake lint language server initialized")
            .await;
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn code_action(&self, params: CodeActionParams) -> Result<Option<CodeActionResponse>> {
        let uri = params.text_document.uri;
        let Some(document) = self
            .documents
            .read()
            .await
            .get(&uri)
            .map(|document| document.text.clone())
        else {
            return Ok(None);
        };
        let path = uri
            .to_file_path()
            .map_or_else(|| PathBuf::from(uri.as_str()), |path| path.into_owned());
        let root = self.root.read().await.clone();
        let result = wake_app::lint_project(
            LintProjectOptions {
                root: root.clone(),
                stdin: Some(LintStdin {
                    filename: virtual_filename(&root, &path),
                    text: document.clone(),
                }),
                ..Default::default()
            },
            &wake_app::CancellationToken::default(),
        );
        let Some(file) = result
            .ok()
            .and_then(|result| result.files.into_iter().next())
        else {
            return Ok(Some(Vec::new()));
        };
        let actions = file
            .diagnostics
            .into_iter()
            .filter_map(|item| {
                let fix = item.fix?;
                let start = item.diagnostic.start? as usize;
                let end = item.diagnostic.end? as usize;
                let diagnostic_range =
                    Range::new(position(&document, start), position(&document, end));
                if !ranges_overlap(diagnostic_range, params.range) {
                    return None;
                }
                let edits = fix
                    .edits
                    .into_iter()
                    .map(|edit| TextEdit {
                        range: Range::new(
                            position(&document, edit.start as usize),
                            position(&document, edit.end as usize),
                        ),
                        new_text: edit.text,
                    })
                    .collect::<Vec<_>>();
                let diagnostic = to_lsp_diagnostic(&document, item.diagnostic.clone())?;
                Some(CodeActionOrCommand::CodeAction(CodeAction {
                    title: format!(
                        "Fix {}",
                        item.diagnostic
                            .code
                            .as_deref()
                            .unwrap_or("Wake lint diagnostic")
                    ),
                    kind: Some(CodeActionKind::QUICKFIX),
                    diagnostics: Some(vec![diagnostic]),
                    edit: Some(WorkspaceEdit {
                        changes: Some(HashMap::from([(uri.clone(), edits)])),
                        ..Default::default()
                    }),
                    is_preferred: Some(true),
                    ..Default::default()
                }))
            })
            .collect();
        Ok(Some(actions))
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let item = params.text_document;
        self.documents.write().await.insert(
            item.uri.clone(),
            Document {
                version: item.version,
                text: item.text,
            },
        );
        self.publish(item.uri).await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;
        let mut documents = self.documents.write().await;
        let Some(previous) = documents.get_mut(&uri) else {
            return;
        };
        if let Some(change) = params.content_changes.into_iter().next() {
            if let TextDocumentContentChangeEvent {
                range: None, text, ..
            } = change
            {
                previous.text = text;
            } else {
                return;
            }
        }
        previous.version = params.text_document.version;
        drop(documents);
        self.publish(uri).await;
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        self.publish(params.text_document.uri).await;
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        self.documents
            .write()
            .await
            .remove(&params.text_document.uri);
        self.client
            .publish_diagnostics(params.text_document.uri, Vec::new(), None)
            .await;
    }
}

fn to_lsp_diagnostic(source: &str, diagnostic: wake_app::DiagnosticInfo) -> Option<Diagnostic> {
    let start = diagnostic.start? as usize;
    let end = diagnostic.end? as usize;
    Some(Diagnostic {
        range: Range::new(position(source, start), position(source, end)),
        severity: Some(match diagnostic.severity.as_str() {
            "error" => DiagnosticSeverity::ERROR,
            "warning" => DiagnosticSeverity::WARNING,
            _ => DiagnosticSeverity::INFORMATION,
        }),
        code: diagnostic.code.map(NumberOrString::String),
        source: Some("wake".into()),
        message: diagnostic.message,
        ..Default::default()
    })
}

fn position(source: &str, offset: usize) -> Position {
    let offset = offset.min(source.len());
    let mut line = 0u32;
    let mut column = 0u32;
    for ch in source[..offset].chars() {
        if ch == '\n' {
            line += 1;
            column = 0;
        } else {
            column += ch.len_utf16() as u32;
        }
    }
    Position::new(line, column)
}

fn ranges_overlap(left: Range, right: Range) -> bool {
    left.start <= right.end && right.start <= left.end
}

fn virtual_filename(root: &std::path::Path, path: &std::path::Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let (service, socket) = LspService::new(|client| Backend {
        client,
        root: RwLock::new(std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))),
        documents: RwLock::new(HashMap::new()),
    });
    Server::new(stdin, stdout, socket).serve(service).await;
}

#[cfg(test)]
mod tests {
    use super::{position, ranges_overlap};
    use tower_lsp_server::ls_types::{Position, Range};

    #[test]
    fn positions_are_utf16_zero_based() {
        let source = "前😀\nvalue";
        assert_eq!(
            position(source, "前😀".len()),
            tower_lsp_server::ls_types::Position::new(0, 3)
        );
        assert_eq!(
            position(source, source.len()),
            tower_lsp_server::ls_types::Position::new(1, 5)
        );
    }

    #[test]
    fn code_actions_overlap_touching_ranges() {
        assert!(ranges_overlap(
            Range::new(Position::new(1, 2), Position::new(1, 4)),
            Range::new(Position::new(1, 4), Position::new(1, 7)),
        ));
        assert!(!ranges_overlap(
            Range::new(Position::new(1, 2), Position::new(1, 4)),
            Range::new(Position::new(1, 5), Position::new(1, 7)),
        ));
    }
}
