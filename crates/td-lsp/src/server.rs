//! The `LanguageServer` impl.

use std::{
    path::PathBuf,
    sync::Arc,
};

use async_trait::async_trait;
use ignore::WalkBuilder;
use tokio::sync::RwLock;
use tower_lsp::{
    jsonrpc::Result as JsonRpcResult,
    lsp_types::{
        CompletionOptions, CompletionParams, CompletionResponse, DidChangeTextDocumentParams,
        DidCloseTextDocumentParams, DidOpenTextDocumentParams, DidSaveTextDocumentParams,
        GotoDefinitionParams, GotoDefinitionResponse, Hover, HoverParams, HoverProviderCapability,
        InitializeParams, InitializeResult, InitializedParams, InlayHint, InlayHintParams,
        Location, MessageType, OneOf, SemanticTokens, SemanticTokensFullOptions,
        SemanticTokensLegend, SemanticTokensOptions, SemanticTokensParams, SemanticTokensResult,
        SemanticTokensServerCapabilities, ServerCapabilities, TextDocumentItem,
        TextDocumentSyncCapability, TextDocumentSyncKind, Url, WorkDoneProgressOptions,
    },
    Client, LanguageServer,
};

use crate::{
    completion, diagnostics as diag_mod, hover as hover_mod, inlay, semantic, state::{DocState, WorkspaceState},
    stdlib_cache,
};

pub struct TypedownServer {
    client: Client,
    state: Arc<RwLock<WorkspaceState>>,
}

impl TypedownServer {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            state: Arc::new(RwLock::new(WorkspaceState::new())),
        }
    }

    async fn publish(&self, uri: Url) {
        let state = self.state.read().await;
        let Some(doc) = state.docs.get(&uri) else { return };
        let lsp_diags: Vec<_> = doc
            .diagnostics
            .iter()
            .map(|d| diag_mod::to_lsp(d, &doc.line_index))
            .collect();
        let version = Some(doc.version);
        drop(state);
        self.client.publish_diagnostics(uri, lsp_diags, version).await;
    }

    async fn publish_empty(&self, uri: Url) {
        self.client.publish_diagnostics(uri, Vec::new(), None).await;
    }

    async fn index_workspace(&self, roots: Vec<PathBuf>) {
        let mut state = self.state.write().await;
        state.roots = roots.clone();

        // Build stdlib cache + stdlib symbols.
        match stdlib_cache::build() {
            Ok(snap) => {
                for (name, site) in snap.decls {
                    state.symbol_index.insert_stdlib(name, site);
                }
            }
            Err(e) => {
                self.client
                    .log_message(
                        MessageType::WARNING,
                        format!("failed to materialize stdlib cache: {e}"),
                    )
                    .await;
            }
        }

        // Walk user files.
        let mut discovered: Vec<(Url, DocState)> = Vec::new();
        for root in &roots {
            for entry in WalkBuilder::new(root).hidden(false).build().flatten() {
                if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                    continue;
                }
                let path = entry.path();
                if !matches!(
                    path.extension().and_then(|s| s.to_str()),
                    Some("md") | Some("mdx") | Some("markdown")
                ) {
                    continue;
                }
                let Ok(content) = std::fs::read_to_string(path) else { continue };
                let Ok(uri) = Url::from_file_path(path) else { continue };
                let doc = DocState::build(path.to_path_buf(), content, 0);
                discovered.push((uri, doc));
            }
        }
        for (uri, doc) in discovered {
            state.upsert(uri, doc);
        }
    }
}

#[async_trait]
impl LanguageServer for TypedownServer {
    async fn initialize(&self, params: InitializeParams) -> JsonRpcResult<InitializeResult> {
        let roots = workspace_roots(&params);
        self.index_workspace(roots).await;

        let caps = ServerCapabilities {
            text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
            hover_provider: Some(HoverProviderCapability::Simple(true)),
            completion_provider: Some(CompletionOptions {
                trigger_characters: Some(vec!["<".into(), "\"".into(), " ".into()]),
                resolve_provider: None,
                ..Default::default()
            }),
            definition_provider: Some(OneOf::Left(true)),
            semantic_tokens_provider: Some(
                SemanticTokensServerCapabilities::SemanticTokensOptions(SemanticTokensOptions {
                    work_done_progress_options: WorkDoneProgressOptions::default(),
                    legend: SemanticTokensLegend {
                        token_types: semantic::TOKEN_TYPES.to_vec(),
                        token_modifiers: vec![],
                    },
                    range: Some(false),
                    full: Some(SemanticTokensFullOptions::Bool(true)),
                }),
            ),
            inlay_hint_provider: Some(OneOf::Left(true)),
            ..Default::default()
        };
        Ok(InitializeResult {
            capabilities: caps,
            server_info: Some(tower_lsp::lsp_types::ServerInfo {
                name: "typedown-lsp".into(),
                version: Some(env!("CARGO_PKG_VERSION").into()),
            }),
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "typedown LSP ready".to_string())
            .await;
        // Publish diagnostics for every already-indexed doc so editors
        // that opened folders (not individual files) still get a red
        // squiggle on broken docs before the user touches them.
        let uris: Vec<Url> = {
            let state = self.state.read().await;
            state.docs.keys().cloned().collect()
        };
        for uri in uris {
            self.publish(uri).await;
        }
    }

    async fn shutdown(&self) -> JsonRpcResult<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let TextDocumentItem { uri, version, text, .. } = params.text_document;
        let Some(path) = uri_to_path(&uri) else { return };
        let doc = DocState::build(path, text, version);
        {
            let mut state = self.state.write().await;
            state.open_uris.insert(uri.clone());
            state.upsert(uri.clone(), doc);
        }
        self.publish(uri).await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        // Full sync: last content change is authoritative.
        let uri = params.text_document.uri.clone();
        let version = params.text_document.version;
        let Some(path) = uri_to_path(&uri) else { return };
        let Some(change) = params.content_changes.into_iter().last() else { return };
        let doc = DocState::build(path, change.text, version);
        {
            let mut state = self.state.write().await;
            state.upsert(uri.clone(), doc);
        }
        self.publish(uri).await;
    }

    async fn did_save(&self, _: DidSaveTextDocumentParams) {}

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;
        // Keep the indexed state (so cross-file hover/goto still resolve),
        // just clear the "open" flag and optionally re-read from disk to
        // drop any unsaved-in-editor state — we choose to leave the last
        // in-editor version in the index until a watcher event tells us
        // otherwise. Publish empty diagnostics so editors clear their UI.
        {
            let mut state = self.state.write().await;
            state.open_uris.remove(&uri);
        }
        self.publish_empty(uri).await;
    }

    async fn hover(&self, params: HoverParams) -> JsonRpcResult<Option<Hover>> {
        let uri = params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let state = self.state.read().await;
        let Some(doc) = state.docs.get(&uri) else { return Ok(None) };
        Ok(hover_mod::hover(&state, doc, pos))
    }

    async fn completion(
        &self,
        params: CompletionParams,
    ) -> JsonRpcResult<Option<CompletionResponse>> {
        let uri = params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;
        let state = self.state.read().await;
        let Some(doc) = state.docs.get(&uri) else { return Ok(None) };
        Ok(completion::completions(&state, doc, pos))
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> JsonRpcResult<Option<GotoDefinitionResponse>> {
        let uri = params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let state = self.state.read().await;
        let Some(doc) = state.docs.get(&uri) else { return Ok(None) };
        let offset = doc.line_index.offset(pos);
        let Some(tok) = crate::resolver::token_at(&doc.doc, doc.line_index.text(), offset) else {
            return Ok(None);
        };
        let name = match tok {
            crate::resolver::TokenKind::FrontmatterType { name, .. } => name,
            _ => return Ok(None),
        };
        let Some(site) = state.symbol_index.lookup(&name) else {
            return Ok(None);
        };
        // If the site is in an indexed doc, use its line_index for range
        // conversion; for stdlib-cache files, rebuild a LineIndex from
        // disk on the fly (cheap — stdlib modules are small).
        let range = match state.docs.get(&site.uri) {
            Some(target_doc) => target_doc.line_index.range(site.span),
            None => match uri_to_path(&site.uri).and_then(|p| std::fs::read_to_string(&p).ok()) {
                Some(content) => crate::LineIndex::new(content).range(site.span),
                None => return Ok(None),
            },
        };
        Ok(Some(GotoDefinitionResponse::Scalar(Location {
            uri: site.uri.clone(),
            range,
        })))
    }

    async fn semantic_tokens_full(
        &self,
        params: SemanticTokensParams,
    ) -> JsonRpcResult<Option<SemanticTokensResult>> {
        let uri = params.text_document.uri;
        let state = self.state.read().await;
        let Some(doc) = state.docs.get(&uri) else { return Ok(None) };
        let tokens: SemanticTokens = semantic::tokens_full(doc);
        Ok(Some(SemanticTokensResult::Tokens(tokens)))
    }

    async fn inlay_hint(&self, params: InlayHintParams) -> JsonRpcResult<Option<Vec<InlayHint>>> {
        let uri = params.text_document.uri;
        let range = params.range;
        let state = self.state.read().await;
        let Some(doc) = state.docs.get(&uri) else { return Ok(None) };
        Ok(Some(inlay::hints(doc, range)))
    }
}

fn workspace_roots(params: &InitializeParams) -> Vec<PathBuf> {
    if let Some(folders) = &params.workspace_folders {
        return folders
            .iter()
            .filter_map(|f| uri_to_path(&f.uri))
            .collect();
    }
    #[allow(deprecated)]
    if let Some(root) = params.root_uri.as_ref() {
        if let Some(p) = uri_to_path(root) {
            return vec![p];
        }
    }
    #[allow(deprecated)]
    if let Some(path) = params.root_path.as_ref() {
        return vec![PathBuf::from(path)];
    }
    Vec::new()
}

fn uri_to_path(uri: &Url) -> Option<PathBuf> {
    if uri.scheme() != "file" {
        return None;
    }
    uri.to_file_path().ok()
}
