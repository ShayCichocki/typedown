//! Compile typed markdown documents to executable runtime code.
//!
//! # Why
//!
//! Typedown describes a prompt's shape, types, and capability policy in
//! a declarative markdown source. Runtime platforms (Vercel AI SDK,
//! Anthropic's tools API, LangChain, custom harnesses) describe the
//! same concepts imperatively, in their own vocabularies. A code
//! generator bridges the two:
//!
//! ```text
//!   typed markdown → td-check (parse, type, verify)
//!                 → td-codegen (emit runtime source)
//!                 → target platform (executes)
//! ```
//!
//! One source, many targets. This crate is the codegen layer.
//!
//! # Backends
//!
//! * [`ai_sdk`] — Vercel AI SDK (TypeScript). Emits a ready-to-import
//!   `.ts` module with Zod schemas for every declared type, a policy
//!   constant pulled from the effect rows, the rendered system prompt,
//!   and an async function that wraps `generateText` with structured
//!   output and tool filtering.
//!
//! More backends will follow the same shape: `target.rs` exposes a
//! single `emit(unit: &CompileUnit) -> Result<String, Error>` function.

pub mod ai_sdk;
mod naming;
mod prompt;
mod zod;

use std::path::PathBuf;

use td_ast::{md::MdDoc, td::TdType};
use td_check::{Composition, Effects, TypeEnv};
use td_core::SourceFile;
use thiserror::Error;

/// Everything a codegen backend needs from a typedown-typed document.
///
/// Built by [`CompileUnit::from_source`]. Backends receive this
/// read-only — they don't reach back into typedown's internals beyond
/// what's exposed here.
pub struct CompileUnit<'a> {
    pub file: &'a SourceFile,
    pub doc: &'a MdDoc,
    pub env: &'a TypeEnv,
    /// The document's top-level type with effect rows and composition
    /// markers stripped — the pure content shape.
    pub doc_type: Option<&'a TdType>,
    pub effects: &'a Effects,
    pub composition: Option<&'a Composition>,
}

#[derive(Debug, Clone, PartialEq, Error)]
pub enum CodegenError {
    /// The document has no `typedown:` frontmatter declaration or it
    /// failed to resolve — there's nothing to compile.
    #[error("document has no declared type")]
    MissingDocType,
    /// The document's type isn't a shape we know how to emit for the
    /// requested backend (e.g. a `Readme` to the AI SDK backend).
    #[error("document shape `{shape}` is not supported by backend `{backend}`: {reason}")]
    UnsupportedShape {
        backend: &'static str,
        shape: String,
        reason: String,
    },
    /// A type referenced in the document couldn't be rendered. Includes
    /// the path through the declaration tree where rendering failed.
    #[error("could not render type at `{path}`: {reason}")]
    RenderFailed { path: String, reason: String },
}

/// Convenience wrapper: load a file, run the full typedown pipeline,
/// and produce a [`CompileUnit`] suitable for handing to a backend.
///
/// Fails fast on any error-severity diagnostic: codegen against a
/// broken contract would emit broken code.
#[derive(Debug)]
pub struct LoadedDoc {
    pub file: SourceFile,
    pub doc: MdDoc,
    pub env: TypeEnv,
    pub doc_type: Option<TdType>,
    pub effects: Effects,
    pub composition: Option<Composition>,
}

impl LoadedDoc {
    pub fn from_path(path: PathBuf) -> Result<Self, LoadError> {
        let content = std::fs::read_to_string(&path).map_err(|e| LoadError::Io {
            path: path.clone(),
            source: e,
        })?;
        let file = SourceFile::new(path, content);
        Self::from_source(file)
    }

    pub fn from_source(file: SourceFile) -> Result<Self, LoadError> {
        // Full check pipeline so we surface every diagnostic — not just
        // resolve_doc_type's subset.
        let (_doc, check_diags) = td_check::check_source(&file);
        let fatal: Vec<String> = check_diags
            .iter()
            .filter(|d| matches!(d.severity, td_core::Severity::Error))
            .map(|d| format!("[{}] {}", d.code, d.message))
            .collect();
        if !fatal.is_empty() {
            return Err(LoadError::Check(fatal));
        }
        // Now re-run for the resolved artifacts.
        let (doc, env, doc_type, effects, composition, _diags) = td_check::resolve_doc_type(&file);
        Ok(LoadedDoc {
            file,
            doc,
            env,
            doc_type,
            effects,
            composition,
        })
    }

    pub fn as_unit(&self) -> CompileUnit<'_> {
        CompileUnit {
            file: &self.file,
            doc: &self.doc,
            env: &self.env,
            doc_type: self.doc_type.as_ref(),
            effects: &self.effects,
            composition: self.composition.as_ref(),
        }
    }
}

#[derive(Debug, Error)]
pub enum LoadError {
    #[error("failed to read `{path}`: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("typedown check failed: {}", .0.join("; "))]
    Check(Vec<String>),
}
