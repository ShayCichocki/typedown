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

pub mod compose;
pub mod effects;
mod env;
mod extract;
mod frontmatter;
mod rules;
pub mod schema;
pub mod value;

pub use compose::{split_compose, Composition, ComposedStep};
pub use effects::{collect_effects, split_effects, Effects};
pub use env::{EntryOrigin, EnvEntry, LookupResult, TypeEnv};
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

    // Harvest composition markers first. A `Compose<[…]>` pipeline
    // declaration verifies end-to-end I/O flow and the effect-row algebra
    // across its children; we run the verification regardless of whether
    // the rest of the type has a content shape.
    let (after_compose, composition, compose_diags) =
        compose::split_compose(&doc_type_expr, &env, file);
    diagnostics.extend(compose_diags.into_vec());

    // Strip effect rows before shape-flattening. Effects are meta — they
    // describe what the document is authorized to do, not what it contains.
    // Leaving them in would make `flatten_to_object` treat `Uses<…>` as if
    // it were a content field and emit bogus td404s.
    let (stripped, _effects, effect_diags) = effects::split_effects(&after_compose, &env, file);
    diagnostics.extend(effect_diags.into_vec());

    // Content conformance. Pure pipeline docs (a `Compose<…>` declaration
    // with nothing else intersected in) reduce to `any` after stripping —
    // there's no markdown shape to enforce, and running the rules would
    // emit spurious td404s. We skip in that case.
    let is_pure_pipeline = composition.is_some() && matches!(&stripped, td_ast::td::TdType::Primitive { kind: td_ast::td::TdPrim::Any, .. });
    if !is_pure_pipeline {
        rules::check_doc(&doc, &stripped, &env, file, &mut diagnostics);
    }

    (doc, diagnostics)
}

/// Resolve a source file to its parsed doc, type environment, declared
/// top-level type, extracted effects, composition, and diagnostics —
/// without running conformance rules.
///
/// This is the shared prefix of every cross-cutting operation that wants
/// to reason about a doc's *type*, *policy*, or *pipeline structure*
/// rather than its conformance: schema export, `.d.ts` emission,
/// `td-runtime::EnforcedPrompt::load`, `td diff`, the LSP. Centralizing
/// means each tool pays only for what it uses and we avoid re-deriving
/// the "what type is this doc?" judgement in three places.
///
/// The returned type is the *fully stripped* type — with both effect rows
/// AND composition markers removed. For pure pipeline docs the stripped
/// type is `any`.
pub fn resolve_doc_type(
    file: &SourceFile,
) -> (
    MdDoc,
    TypeEnv,
    Option<TdType>,
    Effects,
    Option<Composition>,
    Diagnostics,
) {
    let (doc, mut diagnostics) = parse_markdown(&file.content);
    let (merged_module, extract_diags) = extract::extract_td_modules(&doc, file);
    diagnostics.extend(extract_diags.into_vec());
    let (env, env_diags) = TypeEnv::build(&merged_module, file);
    diagnostics.extend(env_diags.into_vec());
    let raw_ty = frontmatter::doc_type_expr(&doc, file, &mut diagnostics);
    let (stripped_ty, effects, composition) = match raw_ty {
        Some(ty) => {
            let (after_compose, comp, compose_diags) = compose::split_compose(&ty, &env, file);
            diagnostics.extend(compose_diags.into_vec());
            let (stripped, fx, effect_diags) =
                effects::split_effects(&after_compose, &env, file);
            diagnostics.extend(effect_diags.into_vec());
            (Some(stripped), fx, comp)
        }
        None => (None, Effects::default(), None),
    };
    (doc, env, stripped_ty, effects, composition, diagnostics)
}
