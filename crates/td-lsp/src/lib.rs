//! Language Server Protocol implementation for typedown.
//!
//! Reuses `td-check::check_source` / `resolve_doc_type` to produce the same
//! diagnostics the CLI does, plus hover / completion / goto-definition /
//! semantic tokens / inlay hints.
//!
//! Entry point: [`run_stdio`]. The `td-cli` crate exposes this as
//! `typedown lsp`.

mod completion;
mod diagnostics;
mod hover;
mod inlay;
mod line_index;
mod resolver;
mod semantic;
mod server;
mod state;
mod stdlib_cache;
mod symbol;
mod watcher;

pub use line_index::LineIndex;
pub use server::TypedownServer;

use std::io;

use tower_lsp::{LspService, Server};

/// Start the LSP on stdio. Runs until the client issues `shutdown` + `exit`
/// or the stdio stream closes.
pub fn run_stdio() -> io::Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    runtime.block_on(async {
        let stdin = tokio::io::stdin();
        let stdout = tokio::io::stdout();
        let (service, socket) = LspService::build(TypedownServer::new).finish();
        Server::new(stdin, stdout, socket).serve(service).await;
    });

    Ok(())
}
