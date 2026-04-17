//! The typedown type checker.
//!
//! Flow:
//!
//! ```text
//!   markdown source
//!        │
//!        ▼
//!   td_parse::parse_markdown  ──►  MdDoc
//!        │
//!        ▼                        (td fences)
//!   extract_td_modules     ──►  TdModule(s)  ──► merged module
//!        │
//!        ▼
//!   TypeEnv (user decls + stdlib + imports)
//!        │
//!        ▼
//!   pick doc type from frontmatter `typedown:` field
//!        │
//!        ▼
//!   conform(doc_type, MdDoc) ──►  Diagnostics
//! ```
//!
//! The checker is intentionally monolithic right now — we're earning the
//! right to refactor by getting one end-to-end slice shipping.

mod env;
mod extract;
mod frontmatter;
mod rules;

pub use env::TypeEnv;

use td_ast::md::MdDoc;
use td_core::{Diagnostics, SourceFile};
use td_parse::parse_markdown;

/// Entry point: check a single markdown file.
///
/// Returns the parsed [`MdDoc`] alongside every diagnostic produced across
/// all phases (markdown parse, td parse, resolve, conform).
pub fn check_source(file: &SourceFile) -> (MdDoc, Diagnostics) {
    let (doc, mut diagnostics) = parse_markdown(&file.content);

    // Extract and parse every ```td fence in the document.
    let (merged_module, extract_diags) = extract::extract_td_modules(&doc, file);
    diagnostics.extend(extract_diags.into_vec());

    // Build the type environment (user decls + imported stdlib modules).
    let (env, env_diags) = TypeEnv::build(&merged_module, file);
    diagnostics.extend(env_diags.into_vec());

    // Pick the document's top-level type from frontmatter.
    let Some(doc_type_expr) = frontmatter::doc_type_expr(&doc, file, &mut diagnostics) else {
        return (doc, diagnostics);
    };

    // Run conformance checks.
    rules::check_doc(&doc, &doc_type_expr, &env, file, &mut diagnostics);

    (doc, diagnostics)
}
