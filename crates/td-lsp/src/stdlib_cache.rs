//! Lazy on-disk cache of stdlib module sources.
//!
//! Stdlib `.td` sources live as string constants inside `td-stdlib` (baked
//! into the binary). For goto-definition to work across all LSP clients,
//! we need a real filesystem URI — so on first stdlib resolution we dump
//! each known module to `$XDG_CACHE_HOME/typedown/stdlib-<version>/` and
//! hand back `file://` URLs into that directory.
//!
//! The cache is versioned by the build's crate version, so stdlib changes
//! between releases don't leak stale files.
//!
//! The cache is populated at `initialize` time; a single pass writes
//! every module and builds a `name → (uri, span)` map the symbol index
//! can import directly.

use std::{
    fs,
    path::{Path, PathBuf},
};

use td_core::Span;
use tower_lsp::lsp_types::Url;

use crate::symbol::DeclSite;

/// All stdlib symbols, with file URLs into the on-disk cache.
pub struct StdlibSnapshot {
    /// Map symbol name (e.g. "Prompt") → declaration site (stdlib cache URL).
    pub decls: Vec<(String, DeclSite)>,
}

/// Materialize the stdlib to a cache dir and return its symbols.
///
/// The cache dir is created on first call and left alone on subsequent
/// calls; rebuild is not attempted since stdlib is fixed per binary.
pub fn build() -> std::io::Result<StdlibSnapshot> {
    let dir = cache_dir()?;
    fs::create_dir_all(&dir)?;

    let mut decls: Vec<(String, DeclSite)> = Vec::new();

    for &path in td_stdlib::module_paths() {
        let Some(src) = td_stdlib::module_source(path) else {
            continue;
        };
        let file_name = format!("{}.td", path.replace('/', "__"));
        let file_path = dir.join(&file_name);
        // Idempotent: write only if content differs (cheap stat avoids
        // touching mtime for no reason, which matters if the user has a
        // filesystem-watched IDE open over this path somehow).
        let current = fs::read_to_string(&file_path).ok();
        if current.as_deref() != Some(src) {
            fs::write(&file_path, src)?;
        }

        let uri = file_url(&file_path);
        // Scan source for `export type NAME` / `export interface NAME`
        // and record a span pointing at the identifier. Matches the
        // `builtin_index` logic in td-stdlib verbatim.
        for entry in scan_decls(src) {
            let site = DeclSite {
                uri: uri.clone(),
                span: entry.span,
                origin_path: Some(path),
            };
            decls.push((entry.name, site));
        }
    }

    Ok(StdlibSnapshot { decls })
}

struct DeclScan {
    name: String,
    span: Span,
}

/// Locate every `export type Name` / `export interface Name` in a stdlib
/// module source and record the span of the identifier.
fn scan_decls(src: &str) -> Vec<DeclScan> {
    let mut out = Vec::new();
    for (line_start, line) in line_offsets(src) {
        let trimmed = line.trim_start();
        let leading_ws = line.len() - trimmed.len();
        let (prefix_len, rest) = if let Some(r) = trimmed.strip_prefix("export type ") {
            ("export type ".len(), r)
        } else if let Some(r) = trimmed.strip_prefix("export interface ") {
            ("export interface ".len(), r)
        } else {
            continue;
        };
        let name_end = rest
            .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
            .unwrap_or(rest.len());
        let name = rest[..name_end].to_string();
        if name.is_empty() {
            continue;
        }
        let abs_start = line_start + leading_ws + prefix_len;
        let abs_end = abs_start + name_end;
        out.push(DeclScan {
            name,
            span: Span::new(abs_start, abs_end),
        });
    }
    out
}

/// Yield (byte offset of line start, line text without the terminator) for every line.
fn line_offsets(src: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut off = 0usize;
    src.split_inclusive('\n').map(move |chunk| {
        let start = off;
        off += chunk.len();
        let line = chunk.strip_suffix('\n').unwrap_or(chunk);
        let line = line.strip_suffix('\r').unwrap_or(line);
        (start, line)
    })
}

fn cache_dir() -> std::io::Result<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| {
                let mut p = PathBuf::from(h);
                p.push(".cache");
                p
            })
        })
        .unwrap_or_else(|| std::env::temp_dir());
    let version = env!("CARGO_PKG_VERSION");
    Ok(base.join("typedown").join(format!("stdlib-{version}")))
}

fn file_url(path: &Path) -> Url {
    // `Url::from_file_path` is infallible only on absolute paths; our cache
    // dir is always absolute (starts with HOME, XDG_CACHE_HOME, or tmp).
    // If this somehow fails we emit a synthetic URL that at least doesn't
    // panic — goto-def will just fail to open the file gracefully.
    Url::from_file_path(path)
        .unwrap_or_else(|_| Url::parse("typedown-stdlib:///unavailable").unwrap())
}
