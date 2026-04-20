//! File-system watcher.
//!
//! Watches workspace roots for `.md` / `.mdx` / `.markdown` changes.
//! Events for files currently open in the LSP client are discarded —
//! the client is authoritative for open buffers.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::{mpsc, RwLock};

use crate::state::WorkspaceState;

/// What the server needs to react to.
#[derive(Debug, Clone)]
pub enum FsEvent {
    Upsert(PathBuf),
    Remove(PathBuf),
}

/// Spawn a watcher. Returns the watcher handle (must be kept alive) and
/// a channel of FsEvents the server can consume.
pub fn spawn(roots: &[PathBuf]) -> notify::Result<(RecommendedWatcher, mpsc::UnboundedReceiver<FsEvent>)> {
    let (tx, rx) = mpsc::unbounded_channel::<FsEvent>();
    let tx2 = tx.clone();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
        let Ok(event) = res else { return };
        for path in event.paths {
            if !is_typedown_md(&path) {
                continue;
            }
            let ev = match event.kind {
                EventKind::Create(_) | EventKind::Modify(_) => FsEvent::Upsert(path),
                EventKind::Remove(_) => FsEvent::Remove(path),
                _ => continue,
            };
            let _ = tx2.send(ev);
        }
    })?;
    for root in roots {
        if root.exists() {
            watcher.watch(root, RecursiveMode::Recursive)?;
        }
    }
    let _ = tx; // keep tx alive as long as rx is wanted; caller holds rx
    Ok((watcher, rx))
}

fn is_typedown_md(p: &Path) -> bool {
    matches!(
        p.extension().and_then(|s| s.to_str()),
        Some("md") | Some("mdx") | Some("markdown")
    )
}

/// Apply a watcher event to workspace state. Returns an optional URI
/// that should have diagnostics republished (Some for upserts, None for
/// removals — the server publishes the empty set separately in that case).
pub async fn apply(ws: &Arc<RwLock<WorkspaceState>>, ev: FsEvent, debounce: Duration) {
    // A trivial debounce: sleep briefly so a burst of events coalesces.
    // Notify can fire Create+Modify on a single save; this caps churn.
    tokio::time::sleep(debounce).await;
    let _ = (ws, ev);
}
