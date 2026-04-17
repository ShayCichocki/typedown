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

pub mod effects;
mod env;
mod extract;
mod frontmatter;
mod rules;
pub mod schema;
pub mod value;

pub use effects::{collect_effects, split_effects, Effects};
pub use env::{LookupResult, TypeEnv};
pub use schema::{to_json_schema, to_subschema};
pub use value::{check_value, parse_value, VALUE_FENCE_LANGS};

use td_ast::{md::MdDoc, td::TdType};
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

    // Strip effect rows before shape-flattening. Effects are meta — they
    // describe what the document is authorized to do, not what it contains.
    // Leaving them in would make `flatten_to_object` treat `Uses<…>` as if
    // it were a content field and emit bogus td404s.
    let (stripped, _effects, effect_diags) = effects::split_effects(&doc_type_expr, &env, file);
    diagnostics.extend(effect_diags.into_vec());

    // Run conformance checks against the stripped shape.
    rules::check_doc(&doc, &stripped, &env, file, &mut diagnostics);

    (doc, diagnostics)
}

/// Resolve a source file to its parsed doc, type environment, declared
/// top-level type, extracted effects, and diagnostics — without running
/// conformance rules.
///
/// This is the shared prefix of every cross-cutting operation that wants
/// to reason about a doc's *type* or *policy* rather than its conformance:
/// schema export, `.d.ts` emission, `td-runtime::EnforcedPrompt::load`,
/// `td diff`, the eventual LSP hover handler. Keeping it centralized means
/// each tool pays only for what it uses and we avoid re-deriving the
/// "what type is this doc?" judgement in three places.
///
/// The returned type is the *stripped* type — with effect rows removed —
/// because that's what schemas, type-preview, and conformance all want.
/// Pass through `Effects` for the policy-aware consumers.
pub fn resolve_doc_type(
    file: &SourceFile,
) -> (MdDoc, TypeEnv, Option<TdType>, Effects, Diagnostics) {
    let (doc, mut diagnostics) = parse_markdown(&file.content);
    let (merged_module, extract_diags) = extract::extract_td_modules(&doc, file);
    diagnostics.extend(extract_diags.into_vec());
    let (env, env_diags) = TypeEnv::build(&merged_module, file);
    diagnostics.extend(env_diags.into_vec());
    let raw_ty = frontmatter::doc_type_expr(&doc, file, &mut diagnostics);
    let (stripped_ty, effects) = match raw_ty {
        Some(ty) => {
            let (stripped, fx, effect_diags) = effects::split_effects(&ty, &env, file);
            diagnostics.extend(effect_diags.into_vec());
            (Some(stripped), fx)
        }
        None => (None, Effects::default()),
    };
    (doc, env, stripped_ty, effects, diagnostics)
}
