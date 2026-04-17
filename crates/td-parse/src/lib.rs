//! Two parsers stacked:
//!
//! 1. [`markdown`] — turns a `.md` file into [`td_ast::MdDoc`], pulling
//!    frontmatter out of the head if present.
//! 2. [`td_dsl`]   — turns a `td` fence's text (or a concatenation thereof)
//!    into a [`td_ast::TdModule`].
//!
//! Both parsers return `(ast, diagnostics)` rather than `Result` because
//! typedown is a diagnostics-first tool — we want to surface every problem
//! in one pass instead of bailing on the first.

pub mod markdown;
pub mod td_dsl;

pub use markdown::parse_markdown;
pub use td_dsl::parse_td_module;
