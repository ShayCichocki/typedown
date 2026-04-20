//! Workspace state: the in-memory picture of every indexed typedown doc.
//!
//! One `WorkspaceState` per server. Held behind a tokio `RwLock` by
//! [`TypedownServer`](crate::server::TypedownServer). Every LSP request
//! reads from here; every mutation (didOpen/didChange/watcher) replaces
//! the affected `DocState` and reconciles the `SymbolIndex`.

use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
};

use td_ast::{md::MdDoc, td::TdType};
use td_check::{check_source, resolve_doc_type, Composition, Effects, TypeEnv};
use td_core::{SourceFile, TdDiagnostic};
use tower_lsp::lsp_types::Url;

use crate::{line_index::LineIndex, symbol::SymbolIndex};

/// Fully resolved state for a single typedown document.
pub struct DocState {
    pub file: SourceFile,
    pub doc: MdDoc,
    pub env: TypeEnv,
    pub doc_type: Option<TdType>,
    pub effects: Effects,
    pub composition: Option<Composition>,
    pub diagnostics: Vec<TdDiagnostic>,
    pub line_index: LineIndex,
    pub version: i32,
}

impl DocState {
    /// Build from raw source. Runs the full resolve + check pipeline.
    pub fn build(path: PathBuf, content: String, version: i32) -> Self {
        let line_index = LineIndex::new(content.clone());
        let file = SourceFile::new(path, content);
        let (doc, env, doc_type, effects, composition, _resolve_diags) =
            resolve_doc_type(&file);
        // resolve_doc_type runs a subset; check_source runs the full
        // pipeline and produces the user-visible diagnostics set. We rely
        // on check_source for diagnostics (matches CLI behavior exactly)
        // and on resolve_doc_type for the structured derivations we need
        // for hover / inlay / goto.
        let (_doc_check, diagnostics) = check_source(&file);
        Self {
            file,
            doc,
            env,
            doc_type,
            effects,
            composition,
            diagnostics: diagnostics.into_vec(),
            line_index,
            version,
        }
    }
}

pub struct WorkspaceState {
    pub roots: Vec<PathBuf>,
    pub docs: HashMap<Url, DocState>,
    pub symbol_index: SymbolIndex,
    /// URIs currently open in the LSP client's editor. These are
    /// authoritative over file-watcher events to avoid a duelling-update
    /// race between the client's `didChange` and an `Event::Modify` from
    /// `notify`.
    pub open_uris: HashSet<Url>,
}

impl WorkspaceState {
    pub fn new() -> Self {
        Self {
            roots: Vec::new(),
            docs: HashMap::new(),
            symbol_index: SymbolIndex::new(),
            open_uris: HashSet::new(),
        }
    }

    /// Replace (or insert) a doc's state and reconcile the symbol index.
    pub fn upsert(&mut self, uri: Url, state: DocState) {
        self.symbol_index.evict_file(&uri);
        self.symbol_index.ingest(&uri, &state);
        self.docs.insert(uri, state);
    }

    /// Drop a doc entirely (file deleted from disk, closed, etc.).
    pub fn remove(&mut self, uri: &Url) -> Option<DocState> {
        self.symbol_index.evict_file(uri);
        self.docs.remove(uri)
    }
}

impl Default for WorkspaceState {
    fn default() -> Self {
        Self::new()
    }
}
