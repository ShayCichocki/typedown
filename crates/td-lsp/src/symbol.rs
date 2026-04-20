//! Cross-file symbol index for goto-definition and cross-file hover.
//!
//! Builds one entry per named declaration found in a doc's `td` fences
//! (via `TypeEnv.entries`). Stdlib symbols are added separately, keyed
//! off the stdlib cache URL.

use std::collections::HashMap;

use td_check::EntryOrigin;
use td_core::Span;
use tower_lsp::lsp_types::Url;

use crate::state::DocState;

#[derive(Debug, Clone)]
pub struct DeclSite {
    pub uri: Url,
    pub span: Span,
    pub origin_path: Option<&'static str>,
}

#[derive(Debug, Default)]
pub struct SymbolIndex {
    /// Key: symbol name (module-less for now; collisions across files
    /// go to the first one indexed). Value: the canonical decl site.
    decls: HashMap<String, DeclSite>,
    /// Reverse index: which URIs contributed which symbol names. Used
    /// to evict entries on file update/removal.
    per_file: HashMap<Url, Vec<String>>,
}

impl SymbolIndex {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn ingest(&mut self, uri: &Url, state: &DocState) {
        let mut added: Vec<String> = Vec::new();
        for (name, entry) in &state.env.entries {
            // Only index locally-declared symbols per file. Stdlib entries
            // are resolved via `stdlib_cache`, not via the per-file index.
            if !matches!(entry.origin, EntryOrigin::Local) {
                continue;
            }
            let site = DeclSite {
                uri: uri.clone(),
                span: entry.decl.span,
                origin_path: None,
            };
            // First-come-first-served if two files declare the same name;
            // we also track the "per-file" list so evict works.
            self.decls.entry(name.clone()).or_insert(site);
            added.push(name.clone());
        }
        if !added.is_empty() {
            self.per_file.insert(uri.clone(), added);
        }
    }

    pub fn evict_file(&mut self, uri: &Url) {
        let Some(names) = self.per_file.remove(uri) else {
            return;
        };
        for name in names {
            if let Some(site) = self.decls.get(&name) {
                if &site.uri == uri {
                    self.decls.remove(&name);
                }
            }
        }
    }

    pub fn lookup(&self, name: &str) -> Option<&DeclSite> {
        self.decls.get(name)
    }

    /// Register a stdlib-backed symbol. Called once per module source
    /// at server init by `stdlib_cache::build`.
    pub fn insert_stdlib(&mut self, name: String, site: DeclSite) {
        self.decls.entry(name).or_insert(site);
    }

    pub fn all_names(&self) -> impl Iterator<Item = &String> {
        self.decls.keys()
    }
}
