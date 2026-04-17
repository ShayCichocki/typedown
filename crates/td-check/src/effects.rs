//! Effect rows: capability / policy types woven into a document's declared
//! type via intersection.
//!
//! # Why
//!
//! `Prompt<I, O>` answers *"what does this document contain?"*. Effect rows
//! answer *"what is this document authorized to do?"*. That second question
//! is what turns typedown from a lint tool into a contract language: the
//! prompt's type carries its capability set, the runtime enforces it, and
//! downstream tooling (JSON Schema vendor extensions, provider tool JSON)
//! can read the declared policy without re-parsing markdown.
//!
//! # Shape
//!
//! The effect rows are plain named references intersected into the doc
//! type:
//!
//! ```ts
//! export type Doc =
//!   & Prompt<In, Out>
//!   & Uses<["read_file", "run_tests"]>
//!   & Reads<["./src/**"]>
//!   & Writes<[]>
//!   & Model<"claude-opus-4-5">
//!   & MaxTokens<4096>
//! ```
//!
//! They live in `typedown/agents` as `= any` aliases; the type checker
//! *harvests* them via [`split_effects`] before the shape-flattening pass
//! runs. The harvest removes the effect rows from the intersection (so
//! `flatten_to_object` doesn't try to turn them into sections) and returns
//! an [`Effects`] record holding the extracted policy.
//!
//! # Diagnostics
//!
//! | code   | meaning                                                |
//! |--------|--------------------------------------------------------|
//! | td601  | malformed effect row (wrong argument shape)            |
//!
//! Unknown effect-row names are *not* an error — they're just not
//! recognized, and flow through to the normal type resolver where
//! `td403` would fire if they truly don't exist.

use serde::Serialize;
use td_ast::td::{TdType, TdPrim};
use td_core::{Diagnostics, SourceFile, Span, TdDiagnostic};

use crate::env::{LookupResult, TypeEnv};

/// Policy extracted from a doc's declared type.
///
/// Every field defaults to "unspecified" (empty vec or `None`). A runtime
/// consuming this should treat unspecified as permissive OR deny-by-default
/// — that's a policy decision, not ours. The `td-runtime` crate defaults
/// to deny-by-default for `uses`/`reads`/`writes` since those are security
/// boundaries; `models` and `max_tokens` default to permissive.
#[derive(Debug, Default, Clone, Serialize, PartialEq)]
pub struct Effects {
    pub uses: Vec<String>,
    pub reads: Vec<String>,
    pub writes: Vec<String>,
    pub models: Vec<String>,
    pub max_tokens: Option<u64>,
    /// Was any effect row seen at all? Distinguishes
    /// "declared empty" from "not declared." Runtimes may reject prompts
    /// that never opted into policy.
    pub declared: bool,
}

/// Split a declared doc type into (stripped_type, effects).
///
/// The returned `stripped_type` is safe to feed to `flatten_to_object` —
/// it has no effect-row named-refs hanging off the root intersection.
///
/// # Alias resolution
///
/// Docs typically declare effects via a user-level alias:
///
/// ```ts
/// export type Doc = Prompt<I, O> & Uses<[...]>
/// ```
///
/// and the frontmatter references `typedown: Doc`. We resolve through the
/// env's aliases **one step at a time** before stripping so effect rows
/// land in the intersection-flattening pass rather than being hidden
/// behind a named reference. One-step resolution (rather than full
/// expansion) keeps the stripped type readable for downstream tools.
pub fn split_effects(
    ty: &TdType,
    env: &TypeEnv,
    file: &SourceFile,
) -> (TdType, Effects, Diagnostics) {
    let mut effects = Effects::default();
    let mut diagnostics = Diagnostics::new();
    let resolved = resolve_alias(ty, env);
    let stripped = strip(&resolved, &mut effects, &mut diagnostics, file);
    (stripped, effects, diagnostics)
}

/// Collect effects from a type without mutating it. Equivalent to
/// `split_effects` but discards the stripped form — useful for tooling
/// that wants the policy independently of conformance checking (e.g.
/// schema export, the runtime loader).
pub fn collect_effects(
    ty: &TdType,
    env: &TypeEnv,
    file: &SourceFile,
) -> (Effects, Diagnostics) {
    let (_stripped, effects, diagnostics) = split_effects(ty, env, file);
    (effects, diagnostics)
}

/// Walk one layer of aliases: if `ty` is a NamedRef to a user decl (NOT a
/// builtin or effect marker), expand it once. Repeat until we hit a
/// non-alias type, a built-in, or an effect marker. This is *not* full
/// generic instantiation — it's the minimal unwrap needed to reach the
/// underlying intersection.
fn resolve_alias(ty: &TdType, env: &TypeEnv) -> TdType {
    let mut current = ty.clone();
    // Bound the loop to prevent pathological cycles; six levels is far more
    // than any real document declares.
    for _ in 0..6 {
        let TdType::NamedRef {
            name, type_args, ..
        } = &current
        else {
            break;
        };
        // Don't unwrap builtins or effect markers.
        if effect_kind(&current).is_some() {
            break;
        }
        match env.lookup(name) {
            LookupResult::Decl(entry) => {
                current = env.instantiate(&entry.decl, type_args);
            }
            _ => break,
        }
    }
    current
}

// ---------------------------------------------------------------------------
// Core stripping logic.
// ---------------------------------------------------------------------------

/// Recursive strip: if `ty` is an intersection, drop any part that's a
/// recognized effect row (after harvesting it into `effects`). Otherwise,
/// if `ty` itself is an effect row, consume it and return `any`. Any other
/// type passes through unchanged.
///
/// We intentionally don't descend into sub-types (object field types,
/// array elements, etc.) — effect rows only live at the top-level
/// intersection by convention. Nesting them would be incoherent.
fn strip(
    ty: &TdType,
    effects: &mut Effects,
    diagnostics: &mut Diagnostics,
    file: &SourceFile,
) -> TdType {
    match ty {
        TdType::Intersection { parts, span } => {
            let mut kept = Vec::new();
            for p in parts {
                if let Some(kind) = effect_kind(p) {
                    absorb(kind, p, effects, diagnostics, file);
                } else {
                    kept.push(p.clone());
                }
            }
            match kept.len() {
                0 => TdType::Primitive {
                    span: *span,
                    kind: TdPrim::Any,
                },
                1 => kept.into_iter().next().unwrap(),
                _ => TdType::Intersection {
                    span: *span,
                    parts: kept,
                },
            }
        }
        other => {
            if let Some(kind) = effect_kind(other) {
                absorb(kind, other, effects, diagnostics, file);
                TdType::Primitive {
                    span: other.span(),
                    kind: TdPrim::Any,
                }
            } else {
                other.clone()
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum EffectKind {
    Uses,
    Reads,
    Writes,
    Model,
    MaxTokens,
}

fn effect_kind(ty: &TdType) -> Option<EffectKind> {
    let TdType::NamedRef { name, .. } = ty else {
        return None;
    };
    match name.as_str() {
        "Uses" => Some(EffectKind::Uses),
        "Reads" => Some(EffectKind::Reads),
        "Writes" => Some(EffectKind::Writes),
        "Model" => Some(EffectKind::Model),
        "MaxTokens" => Some(EffectKind::MaxTokens),
        _ => None,
    }
}

fn absorb(
    kind: EffectKind,
    ty: &TdType,
    effects: &mut Effects,
    diagnostics: &mut Diagnostics,
    file: &SourceFile,
) {
    effects.declared = true;
    let TdType::NamedRef { type_args, span, name } = ty else {
        return;
    };
    let first = type_args.first();
    match kind {
        EffectKind::Uses => {
            effects.uses.extend(decode_string_list(name, first, *span, file, diagnostics));
        }
        EffectKind::Reads => {
            effects.reads.extend(decode_string_list(name, first, *span, file, diagnostics));
        }
        EffectKind::Writes => {
            effects.writes.extend(decode_string_list(name, first, *span, file, diagnostics));
        }
        EffectKind::Model => {
            effects.models.extend(decode_string_list(name, first, *span, file, diagnostics));
        }
        EffectKind::MaxTokens => {
            if let Some(n) = decode_number(name, first, *span, file, diagnostics) {
                effects.max_tokens = Some(n);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Decoders.
// ---------------------------------------------------------------------------

/// Decode the arg of `Uses<>` / `Reads<>` / `Writes<>` / `Model<>` into a
/// flat `Vec<String>`. Accepts, in order of preference:
///
/// * a `Tuple` of string literals   →  `["a", "b"]`
/// * a `Union` of string literals   →  `"a" | "b"`       (for Model)
/// * a bare string literal          →  `"a"`             (singleton)
///
/// Anything else emits `td601` and returns an empty vec so partial
/// policies at least surface the good entries.
fn decode_string_list(
    marker_name: &str,
    arg: Option<&TdType>,
    anchor: Span,
    file: &SourceFile,
    diagnostics: &mut Diagnostics,
) -> Vec<String> {
    let Some(arg) = arg else {
        diagnostics.push(malformed(
            marker_name,
            "expected a tuple of string literals, e.g. `[\"tool_a\", \"tool_b\"]`",
            anchor,
            file,
        ));
        return Vec::new();
    };
    match arg {
        TdType::Tuple { elems, .. } => {
            let mut out = Vec::with_capacity(elems.len());
            for el in elems {
                match el {
                    TdType::StringLit { value, .. } => out.push(value.clone()),
                    other => {
                        diagnostics.push(malformed(
                            marker_name,
                            "tuple elements must all be string literals",
                            other.span(),
                            file,
                        ));
                    }
                }
            }
            out
        }
        TdType::Union { variants, .. } => {
            let mut out = Vec::with_capacity(variants.len());
            for v in variants {
                match v {
                    TdType::StringLit { value, .. } => out.push(value.clone()),
                    other => {
                        diagnostics.push(malformed(
                            marker_name,
                            "union variants must all be string literals",
                            other.span(),
                            file,
                        ));
                    }
                }
            }
            out
        }
        TdType::StringLit { value, .. } => vec![value.clone()],
        other => {
            diagnostics.push(malformed(
                marker_name,
                "expected a tuple `[...]`, a union `\"a\" | \"b\"`, or a single string literal",
                other.span(),
                file,
            ));
            Vec::new()
        }
    }
}

/// Decode the arg of `MaxTokens<>` into a `u64`. Accepts only a number
/// literal. Fractional values are floored with a warning; negative values
/// emit `td601`.
fn decode_number(
    marker_name: &str,
    arg: Option<&TdType>,
    anchor: Span,
    file: &SourceFile,
    diagnostics: &mut Diagnostics,
) -> Option<u64> {
    let Some(arg) = arg else {
        diagnostics.push(malformed(
            marker_name,
            "expected a number literal, e.g. `MaxTokens<4096>`",
            anchor,
            file,
        ));
        return None;
    };
    match arg {
        TdType::NumberLit { value, span } => {
            if *value < 0.0 || !value.is_finite() {
                diagnostics.push(malformed(
                    marker_name,
                    "expected a non-negative finite number",
                    *span,
                    file,
                ));
                None
            } else {
                Some(*value as u64)
            }
        }
        other => {
            diagnostics.push(malformed(
                marker_name,
                "expected a number literal",
                other.span(),
                file,
            ));
            None
        }
    }
}

fn malformed(marker_name: &str, help: &str, span: Span, file: &SourceFile) -> TdDiagnostic {
    TdDiagnostic::error(
        "td601",
        format!("malformed effect row `{marker_name}<…>`"),
        file,
        span,
        "invalid effect row argument",
    )
    .with_help(help.to_string())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use td_parse::parse_td_module;

    fn ty_env(src: &str) -> (TdType, TypeEnv, SourceFile) {
        let file = SourceFile::new("t.td", src.to_string());
        let (module, _) = parse_td_module(src, &file, 0);
        let (env, _) = TypeEnv::build(&module, &file);
        let decl = module.decls.into_iter().next().expect("a decl");
        let ty = match decl.kind {
            td_ast::td::TdDeclKind::TypeAlias(t) => t,
            td_ast::td::TdDeclKind::Interface(o) => TdType::Object(o),
        };
        (ty, env, file)
    }

    fn codes(d: &Diagnostics) -> Vec<String> {
        d.iter().map(|x| x.code.clone()).collect()
    }

    #[test]
    fn extracts_uses_tuple() {
        let (ty, env, file) = ty_env(r#"type Doc = Uses<["a", "b"]>"#);
        let (_, fx, diags) = split_effects(&ty, &env, &file);
        assert!(diags.is_empty(), "{:?}", codes(&diags));
        assert_eq!(fx.uses, vec!["a".to_string(), "b".to_string()]);
        assert!(fx.declared);
    }

    #[test]
    fn extracts_empty_writes() {
        let (ty, env, file) = ty_env(r#"type Doc = Writes<[]>"#);
        let (_, fx, diags) = split_effects(&ty, &env, &file);
        assert!(diags.is_empty(), "{:?}", codes(&diags));
        assert!(fx.writes.is_empty());
        assert!(fx.declared);
    }

    #[test]
    fn extracts_model_union() {
        let (ty, env, file) =
            ty_env(r#"type Doc = Model<"claude-opus-4-5" | "claude-sonnet-4-5">"#);
        let (_, fx, diags) = split_effects(&ty, &env, &file);
        assert!(diags.is_empty(), "{:?}", codes(&diags));
        assert_eq!(fx.models.len(), 2);
    }

    #[test]
    fn extracts_max_tokens() {
        let (ty, env, file) = ty_env("type Doc = MaxTokens<4096>");
        let (_, fx, diags) = split_effects(&ty, &env, &file);
        assert!(diags.is_empty(), "{:?}", codes(&diags));
        assert_eq!(fx.max_tokens, Some(4096));
    }

    #[test]
    fn intersection_with_object_strips_effect() {
        let (ty, env, file) = ty_env(
            r#"type Doc = { x: string } & Uses<["a"]> & Writes<[]>"#,
        );
        let (stripped, fx, diags) = split_effects(&ty, &env, &file);
        assert!(diags.is_empty(), "{:?}", codes(&diags));
        assert_eq!(fx.uses, vec!["a".to_string()]);
        // Stripped should collapse to the single remaining object.
        assert!(matches!(stripped, TdType::Object(_)));
    }

    #[test]
    fn malformed_uses_nonstring_emits_td601() {
        let (ty, env, file) = ty_env(r#"type Doc = Uses<[1, 2]>"#);
        let (_, _fx, diags) = split_effects(&ty, &env, &file);
        assert!(codes(&diags).iter().any(|c| c == "td601"), "{:?}", codes(&diags));
    }

    #[test]
    fn malformed_max_tokens_string_emits_td601() {
        let (ty, env, file) = ty_env(r#"type Doc = MaxTokens<"4096">"#);
        let (_, fx, diags) = split_effects(&ty, &env, &file);
        assert!(codes(&diags).iter().any(|c| c == "td601"), "{:?}", codes(&diags));
        assert_eq!(fx.max_tokens, None);
    }

    #[test]
    fn declared_false_when_no_effects() {
        let (ty, env, file) = ty_env("type Doc = { x: string }");
        let (_, fx, diags) = split_effects(&ty, &env, &file);
        assert!(diags.is_empty());
        assert!(!fx.declared);
    }

    #[test]
    fn singleton_string_arg_is_accepted() {
        let (ty, env, file) = ty_env(r#"type Doc = Uses<"only_tool">"#);
        let (_, fx, diags) = split_effects(&ty, &env, &file);
        assert!(diags.is_empty(), "{:?}", codes(&diags));
        assert_eq!(fx.uses, vec!["only_tool".to_string()]);
    }

    #[test]
    fn resolves_through_alias_before_stripping() {
        // Effects are behind a user alias — resolve_alias should unwrap it.
        let (_, env, file) = ty_env(
            r#"type Inner = { x: string } & Uses<["read"]>
               type Doc = Inner"#,
        );
        let outer = TdType::NamedRef {
            span: Span::DUMMY,
            name: "Doc".into(),
            type_args: vec![],
        };
        let (stripped, fx, diags) = split_effects(&outer, &env, &file);
        assert!(diags.is_empty(), "{:?}", codes(&diags));
        assert_eq!(fx.uses, vec!["read".to_string()]);
        // Stripped should be the object (Inner without the effect).
        assert!(matches!(stripped, TdType::Object(_)), "stripped: {stripped:?}");
    }
}
