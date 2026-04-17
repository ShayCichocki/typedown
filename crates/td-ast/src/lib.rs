//! The two AST families that power typedown.
//!
//! * [`md`] — a simplified markdown AST produced by `td-parse`. Only the
//!   structural bits typedown cares about (headings, lists, code fences,
//!   paragraphs) are first-class; the rest collapses into `Inline`/`Text`.
//! * [`td`] — the TypeScript-ish DSL that lives inside ```` ```td ```` fences
//!   and drives the checker.
//!
//! Both ASTs carry spans back into the original source so diagnostics can
//! point at exactly the right byte range.

pub mod md;
pub mod td;

pub use md::{MdDoc, MdNode, MdNodeKind};
pub use td::{TdDecl, TdModule, TdType};
