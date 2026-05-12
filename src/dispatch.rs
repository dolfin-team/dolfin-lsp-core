//! Sync JSON-RPC dispatcher — the interface between core logic and both
//! transport layers (tower-lsp stdio and WASM Web Worker).
//!
//! All public methods take / return [`serde_json::Value`] so neither transport
//! needs to know about each other's serialisation details.

use lsp_types::{
    CodeActionOptions, CodeActionProviderCapability, CodeLensOptions, CompletionOptions,
    DocumentSymbolResponse, GotoDefinitionResponse, HoverProviderCapability, InitializeResult,
    OneOf, RenameOptions, SemanticTokensFullOptions, SemanticTokensOptions,
    SemanticTokensServerCapabilities, ServerCapabilities, TextDocumentSyncCapability,
    TextDocumentSyncKind, Url, WorkspaceFoldersServerCapabilities, WorkspaceServerCapabilities,
};
use tracing::{debug, instrument, warn};

use crate::{
    codeaction, codelens, completion, diagnostics, goto, hover, references, rename, semantic,
    symbols, world::World,
};

// ── Result type ───────────────────────────────────────────────────────────────

/// Result of dispatching a single JSON-RPC message.
pub struct DispatchResult {
    /// Serialised JSON response body (requests only; `None` for notifications).
    pub response: Option<serde_json::Value>,
    /// Server-initiated push notifications: `(method, params)` pairs.
    /// e.g. `("textDocument/publishDiagnostics", {...})`.
    pub notifications: Vec<(String, serde_json::Value)>,
}

impl DispatchResult {
    fn response_only(v: serde_json::Value) -> Self {
        Self {
            response: Some(v),
            notifications: vec![],
        }
    }

    fn notify_only(notifications: Vec<(String, serde_json::Value)>) -> Self {
        Self {
            response: None,
            notifications,
        }
    }

    fn null_response() -> Self {
        Self::response_only(serde_json::Value::Null)
    }
}

// ── Server state ──────────────────────────────────────────────────────────────

/// All mutable LSP server state in one place.
pub struct LspState {
    pub world: World,
    pub workspace_root: Option<Url>,
}

impl LspState {
    pub fn new() -> Self {
        Self {
            world: World::new(),
            workspace_root: None,
        }
    }

    // ── Request dispatcher ────────────────────────────────────────────────────

    #[instrument(skip(self, params), fields(method))]
    pub fn handle_request(
        &mut self,
        _id: u32,
        method: &str,
        params: serde_json::Value,
    ) -> DispatchResult {
        debug!(method, "request");
        match method {
            "initialize" => self.on_initialize(params),
            "shutdown" => DispatchResult::null_response(),

            "textDocument/hover" => {
                let p = match deser::<lsp_types::HoverParams>(params) {
                    Some(p) => p,
                    None => return DispatchResult::null_response(),
                };
                let uri = &p.text_document_position_params.text_document.uri;
                let pos = p.text_document_position_params.position;
                let result = self
                    .world
                    .with_doc(uri, |doc| hover::provide(doc, pos))
                    .flatten();
                DispatchResult::response_only(to_value(result))
            }

            "textDocument/completion" => {
                let p = match deser::<lsp_types::CompletionParams>(params) {
                    Some(p) => p,
                    None => return DispatchResult::null_response(),
                };
                let uri = &p.text_document_position.text_document.uri;
                let pos = p.text_document_position.position;
                let result = self
                    .world
                    .with_doc(uri, |doc| completion::provide(doc, pos));
                DispatchResult::response_only(to_value(result))
            }

            "textDocument/semanticTokens/full" => {
                let p = match deser::<lsp_types::SemanticTokensParams>(params) {
                    Some(p) => p,
                    None => return DispatchResult::null_response(),
                };
                let uri = &p.text_document.uri;
                let result = self
                    .world
                    .with_doc(uri, |doc| semantic::full_tokens(doc))
                    .flatten();
                if result.is_none() {
                    warn!("semantic_tokens_full: no analysis for document");
                }
                DispatchResult::response_only(to_value(result))
            }

            "textDocument/definition" => {
                let p = match deser::<lsp_types::GotoDefinitionParams>(params) {
                    Some(p) => p,
                    None => return DispatchResult::null_response(),
                };
                let uri = p
                    .text_document_position_params
                    .text_document
                    .uri
                    .clone();
                let pos = p.text_document_position_params.position;
                let result: Option<GotoDefinitionResponse> = self
                    .world
                    .with_doc(&uri, |doc| goto::provide(doc, &uri, pos))
                    .flatten();
                DispatchResult::response_only(to_value(result))
            }

            "textDocument/references" => {
                let p = match deser::<lsp_types::ReferenceParams>(params) {
                    Some(p) => p,
                    None => return DispatchResult::null_response(),
                };
                let uri = &p.text_document_position.text_document.uri;
                let pos = p.text_document_position.position;

                // Step 1: find symbol name while holding doc lock.
                let target = self
                    .world
                    .with_doc(uri, |doc| references::target_at(doc, pos))
                    .flatten();

                let locs = if let Some(t) = target {
                    // Step 2: scan all ASTs (lock released).
                    let all_asts = self.world.all_file_asts();
                    references::search(&all_asts, &t)
                } else {
                    vec![]
                };

                let result: Option<Vec<_>> = if locs.is_empty() { None } else { Some(locs) };
                DispatchResult::response_only(to_value(result))
            }

            "textDocument/documentSymbol" => {
                let p = match deser::<lsp_types::DocumentSymbolParams>(params) {
                    Some(p) => p,
                    None => return DispatchResult::null_response(),
                };
                let uri = &p.text_document.uri;
                let result: Option<DocumentSymbolResponse> = self
                    .world
                    .with_doc(uri, |doc| {
                        DocumentSymbolResponse::Nested(symbols::provide(doc))
                    });
                DispatchResult::response_only(to_value(result))
            }

            "textDocument/codeAction" => {
                let p = match deser::<lsp_types::CodeActionParams>(params) {
                    Some(p) => p,
                    None => return DispatchResult::null_response(),
                };
                let uri = &p.text_document.uri.clone();
                let range = p.range;
                let result = self
                    .world
                    .with_doc(uri, |doc| codeaction::provide(doc, uri, range))
                    .unwrap_or_default();
                DispatchResult::response_only(to_value(result))
            }

            "textDocument/prepareRename" => {
                let p = match deser::<lsp_types::TextDocumentPositionParams>(params) {
                    Some(p) => p,
                    None => return DispatchResult::null_response(),
                };
                let uri = &p.text_document.uri;
                let pos = p.position;
                let result = self
                    .world
                    .with_doc(uri, |doc| rename::prepare(doc, pos))
                    .flatten();
                DispatchResult::response_only(to_value(result))
            }

            "textDocument/rename" => {
                let p = match deser::<lsp_types::RenameParams>(params) {
                    Some(p) => p,
                    None => return DispatchResult::null_response(),
                };
                let uri = p.text_document_position.text_document.uri.clone();
                let pos = p.text_document_position.position;
                let result = rename::rename(&self.world, &uri, pos, &p.new_name);
                DispatchResult::response_only(to_value(result))
            }

            "textDocument/codeLens" => {
                let p = match deser::<lsp_types::CodeLensParams>(params) {
                    Some(p) => p,
                    None => return DispatchResult::null_response(),
                };
                let uri = p.text_document.uri.clone();
                // Acquire all_asts BEFORE with_doc to avoid lock ordering issues.
                let all_asts = self.world.all_file_asts();
                let result = self
                    .world
                    .with_doc(&uri, |doc| codelens::provide(&all_asts, &uri, doc));
                DispatchResult::response_only(to_value(result))
            }

            other => {
                debug!(method = other, "unhandled request");
                DispatchResult::null_response()
            }
        }
    }

    // ── Notification dispatcher ───────────────────────────────────────────────

    #[instrument(skip(self, params), fields(method))]
    pub fn handle_notification(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> DispatchResult {
        debug!(method, "notification");
        match method {
            "initialized" => self.on_initialized(),

            "textDocument/didOpen" => {
                let p = match deser::<lsp_types::DidOpenTextDocumentParams>(params) {
                    Some(p) => p,
                    None => return DispatchResult::notify_only(vec![]),
                };
                self.world.open(p.text_document.uri, p.text_document.text);
                DispatchResult::notify_only(self.collect_all_diagnostics())
            }

            "textDocument/didChange" => {
                let p = match deser::<lsp_types::DidChangeTextDocumentParams>(params) {
                    Some(p) => p,
                    None => return DispatchResult::notify_only(vec![]),
                };
                if let Some(change) = p.content_changes.into_iter().last() {
                    self.world.update(&p.text_document.uri, change.text);
                }
                DispatchResult::notify_only(self.collect_all_diagnostics())
            }

            "textDocument/didClose" => {
                let p = match deser::<lsp_types::DidCloseTextDocumentParams>(params) {
                    Some(p) => p,
                    None => return DispatchResult::notify_only(vec![]),
                };
                // Clear diagnostics for the closed file before closing it.
                let clear = publish_diagnostics_notification(p.text_document.uri.clone(), vec![]);
                self.world.close(&p.text_document.uri);
                let mut notifs = vec![clear];
                notifs.extend(self.collect_all_diagnostics());
                DispatchResult::notify_only(notifs)
            }

            // Extension-provided workspace files (WASM transport, no filesystem access).
            "dolfin/indexFiles" => {
                #[derive(serde::Deserialize)]
                struct IndexFilesParams {
                    root: Url,
                    files: Vec<IndexFileEntry>,
                }
                #[derive(serde::Deserialize)]
                struct IndexFileEntry {
                    uri: Url,
                    text: String,
                }
                if let Some(p) = deser::<IndexFilesParams>(params) {
                    let files = p.files.into_iter().map(|e| (e.uri, e.text)).collect();
                    self.world.index_background_files(p.root, files);
                }
                DispatchResult::notify_only(self.collect_all_diagnostics())
            }

            // Native-only: watched file changes arrive here.
            #[cfg(not(target_arch = "wasm32"))]
            "workspace/didChangeWatchedFiles" => {
                let p = match deser::<lsp_types::DidChangeWatchedFilesParams>(params) {
                    Some(p) => p,
                    None => return DispatchResult::notify_only(vec![]),
                };
                for change in &p.changes {
                    match change.typ {
                        lsp_types::FileChangeType::CREATED
                        | lsp_types::FileChangeType::CHANGED => {
                            self.world.reload_from_disk(&change.uri);
                        }
                        lsp_types::FileChangeType::DELETED => {
                            self.world.remove_disk_file(&change.uri);
                        }
                        _ => {}
                    }
                }
                DispatchResult::notify_only(self.collect_all_diagnostics())
            }

            "workspace/didChangeWorkspaceFolders" => {
                let p = match deser::<lsp_types::DidChangeWorkspaceFoldersParams>(params) {
                    Some(p) => p,
                    None => return DispatchResult::notify_only(vec![]),
                };
                if let Some(folder) = p.event.added.into_iter().next() {
                    self.workspace_root = Some(folder.uri.clone());
                    #[cfg(feature = "fs")]
                    if let Ok(path) = folder.uri.to_file_path() {
                        self.world.scan_workspace(&path);
                    }
                }
                DispatchResult::notify_only(self.collect_all_diagnostics())
            }

            other => {
                debug!(method = other, "unhandled notification");
                DispatchResult::notify_only(vec![])
            }
        }
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn on_initialize(&mut self, params: serde_json::Value) -> DispatchResult {
        if let Some(p) = deser::<lsp_types::InitializeParams>(params) {
            let root = p
                .workspace_folders
                .as_ref()
                .and_then(|f| f.first())
                .map(|f| f.uri.clone());
            if let Some(uri) = root {
                self.workspace_root = Some(uri);
            }
        }
        let result = InitializeResult {
            capabilities: server_capabilities(),
            ..Default::default()
        };
        DispatchResult::response_only(to_value(result))
    }

    fn on_initialized(&mut self) -> DispatchResult {
        #[cfg(feature = "fs")]
        if let Some(root) = &self.workspace_root.clone() {
            if let Ok(path) = root.to_file_path() {
                self.world.scan_workspace(&path);
            } else {
                warn!("workspace root is not a file URI; skipping scan");
            }
        }
        DispatchResult::notify_only(self.collect_all_diagnostics())
    }

    /// Build `textDocument/publishDiagnostics` notifications for all open files.
    fn collect_all_diagnostics(&self) -> Vec<(String, serde_json::Value)> {
        self.world
            .all_uris()
            .into_iter()
            .filter_map(|uri| {
                let diags = self
                    .world
                    .with_doc(&uri, |doc| diagnostics::collect(doc))?;
                Some(publish_diagnostics_notification(uri, diags))
            })
            .collect()
    }
}

impl Default for LspState {
    fn default() -> Self {
        Self::new()
    }
}

// ── Server capabilities ───────────────────────────────────────────────────────

fn server_capabilities() -> ServerCapabilities {
    ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Kind(
            TextDocumentSyncKind::FULL,
        )),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        completion_provider: Some(CompletionOptions {
            trigger_characters: Some(vec![" ".into(), ":".into(), ".".into()]),
            ..Default::default()
        }),
        definition_provider: Some(OneOf::Left(true)),
        references_provider: Some(OneOf::Left(true)),
        document_symbol_provider: Some(OneOf::Left(true)),
        code_action_provider: Some(CodeActionProviderCapability::Options(CodeActionOptions {
            code_action_kinds: Some(vec![lsp_types::CodeActionKind::QUICKFIX]),
            resolve_provider: Some(false),
            ..Default::default()
        })),
        rename_provider: Some(OneOf::Right(RenameOptions {
            prepare_provider: Some(true),
            work_done_progress_options: Default::default(),
        })),
        code_lens_provider: Some(CodeLensOptions {
            resolve_provider: Some(false),
        }),
        semantic_tokens_provider: Some(
            SemanticTokensServerCapabilities::SemanticTokensOptions(
                SemanticTokensOptions {
                    legend: semantic::legend(),
                    full: Some(SemanticTokensFullOptions::Bool(true)),
                    range: Some(false),
                    ..Default::default()
                },
            )
        ),
        workspace: Some(WorkspaceServerCapabilities {
            workspace_folders: Some(WorkspaceFoldersServerCapabilities {
                supported: Some(true),
                change_notifications: Some(OneOf::Left(true)),
            }),
            ..Default::default()
        }),
        ..Default::default()
    }
}

// ── Utilities ─────────────────────────────────────────────────────────────────

fn deser<T: serde::de::DeserializeOwned>(v: serde_json::Value) -> Option<T> {
    match serde_json::from_value(v) {
        Ok(t) => Some(t),
        Err(e) => {
            warn!("failed to deserialise LSP params: {e}");
            None
        }
    }
}

fn to_value<T: serde::Serialize>(t: T) -> serde_json::Value {
    serde_json::to_value(t).unwrap_or(serde_json::Value::Null)
}

fn publish_diagnostics_notification(
    uri: Url,
    diagnostics: Vec<lsp_types::Diagnostic>,
) -> (String, serde_json::Value) {
    let params = serde_json::json!({
        "uri": uri.as_str(),
        "diagnostics": diagnostics,
    });
    ("textDocument/publishDiagnostics".to_string(), params)
}
