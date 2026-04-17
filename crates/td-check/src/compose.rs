//! Typed composition: pipelines where I/O and effects flow statically.
//!
//! `Compose<[A, B, C]>` is typedown's way of describing multi-step agent
//! workflows while keeping every seam typechecked. This module is the
//! pass that harvests those composition markers and verifies:
//!
//! 1. Each step resolves to a `Prompt<I, O>` shape.
//! 2. Adjacent steps' I/O line up: `Output(N) ≡ Input(N+1)`.
//! 3. Each child's effect rows *fit inside* the parent's. The subset
//!    relations are the interesting bit:
//!
//!    | Effect       | Parent is a …             | Rule                  |
//!    |--------------|---------------------------|-----------------------|
//!    | `Uses`       | ceiling (allowlist)       | `∪ child ⊆ parent`    |
//!    | `Reads`      | ceiling (glob set)        | `∪ child ⊆ parent`    |
//!    | `Writes`     | ceiling (glob set)        | `∪ child ⊆ parent`    |
//!    | `Model`      | model allowlist           | `child ⊆ parent`      |
//!    | `MaxTokens`  | per-step ceiling          | `child ≤ parent`      |
//!
//! The algebra is *monotone*: a child cannot widen what the parent
//! allows. You cannot accidentally compose an agent into a pipeline and
//! end up with *more* capabilities than the pipeline declared.
//!
//! # Diagnostic codes
//!
//! | code   | meaning                                                   |
//! |--------|-----------------------------------------------------------|
//! | td701  | step type doesn't resolve to a `Prompt<I, O>` shape       |
//! | td702  | adjacent step I/O mismatch                                |
//! | td703  | child Uses / Reads / Writes not covered by parent         |
//! | td704  | child `Model<>` not in parent's model set                 |
//! | td705  | child `MaxTokens<>` exceeds parent's ceiling              |

use serde::Serialize;
use td_ast::td::{TdField, TdObjectType, TdPrim, TdType};
use td_core::{Diagnostics, SourceFile, Span, TdDiagnostic};

use crate::effects::{collect_effects, Effects};
use crate::env::{LookupResult, TypeEnv};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// A typed pipeline extracted from a document's declared type.
#[derive(Debug, Clone, Serialize)]
pub struct Composition {
    /// Ordered steps, each with its own resolved I/O and effects.
    pub steps: Vec<ComposedStep>,
    /// Pipeline-level I/O synthesized from the first and last step.
    pub input: Option<TdType>,
    pub output: Option<TdType>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ComposedStep {
    /// User-facing name of the step (the type-alias name when referenced
    /// by NamedRef, otherwise "step N"). Used in diagnostics.
    pub name: String,
    pub input: TdType,
    pub output: TdType,
    pub effects: Effects,
    pub span: Span,
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Harvest `Compose<[...]>` from the declared type.
///
/// Returns the type with the composition marker removed (so downstream
/// passes treat a pure pipeline doc as untyped content), the extracted
/// [`Composition`], and any diagnostics emitted during verification.
pub fn split_compose(
    ty: &TdType,
    env: &TypeEnv,
    file: &SourceFile,
) -> (TdType, Option<Composition>, Diagnostics) {
    let mut diagnostics = Diagnostics::new();

    // Collect effect rows on the *parent* — we need them as the ceiling
    // to verify children against. Discard the emitted diagnostics here:
    // the effects pass runs right after us in `check_source` and will
    // surface the same td601s. Double-emitting is just noise.
    let (parent_effects, _parent_effect_diags) = collect_effects(ty, env, file);

    // Resolve one level of user aliases before searching for the Compose
    // marker. Pipelines are typically declared as a user alias
    // (`typedown: Pipeline` where `Pipeline = Compose<[…]> & …`) and the
    // frontmatter hands us the bare NamedRef — we need to expand it
    // before we can see the intersection inside.
    let ty = resolve_alias(ty, env);

    // Locate the Compose marker and extract its step list.
    let (stripped, step_types) = match find_and_strip_compose(&ty) {
        Some(result) => result,
        None => return (ty, None, diagnostics),
    };

    // Resolve each step reference to a concrete Prompt<I, O> + its effects.
    let mut steps = Vec::with_capacity(step_types.len());
    for (idx, step_ty) in step_types.iter().enumerate() {
        let step_name = step_display_name(step_ty, idx);
        match resolve_step(step_ty, env, file, &mut diagnostics) {
            Some(step) => steps.push(ComposedStep {
                name: step_name,
                input: step.input,
                output: step.output,
                effects: step.effects,
                span: step_ty.span(),
            }),
            None => {
                // `resolve_step` already pushed a td701. Keep going so the
                // user sees every malformed step, not just the first.
            }
        }
    }

    // Verify invariants across collected steps.
    verify_io_flow(&steps, file, &mut diagnostics);
    verify_effect_subsets(&parent_effects, &steps, file, &mut diagnostics);

    let input = steps.first().map(|s| s.input.clone());
    let output = steps.last().map(|s| s.output.clone());

    (
        stripped,
        Some(Composition {
            steps,
            input,
            output,
        }),
        diagnostics,
    )
}

// ---------------------------------------------------------------------------
// Finding `Compose<[...]>` in the declared type
// ---------------------------------------------------------------------------

/// Walk the type; if it contains a top-level `Compose<[...]>` named-ref,
/// remove it and return `(stripped_type, step_types)`. Otherwise return
/// `None`. Only handles top-level intersections; nested `Compose` is not
/// supported (it would mean "a step is itself a pipeline," which works
/// fine by naming the inner pipeline with a type alias and referencing it).
fn find_and_strip_compose(ty: &TdType) -> Option<(TdType, Vec<TdType>)> {
    match ty {
        TdType::NamedRef {
            name, type_args, ..
        } if is_compose(name) => {
            let steps = extract_step_types(type_args.first());
            // Pure composition — no remaining type.
            let stripped = TdType::Primitive {
                span: ty.span(),
                kind: TdPrim::Any,
            };
            Some((stripped, steps))
        }
        TdType::Intersection { parts, span } => {
            let mut kept = Vec::new();
            let mut steps: Option<Vec<TdType>> = None;
            for p in parts {
                if let TdType::NamedRef {
                    name, type_args, ..
                } = p
                {
                    if is_compose(name) {
                        steps = Some(extract_step_types(type_args.first()));
                        continue;
                    }
                }
                kept.push(p.clone());
            }
            let steps = steps?;
            let stripped = match kept.len() {
                0 => TdType::Primitive {
                    span: *span,
                    kind: TdPrim::Any,
                },
                1 => kept.into_iter().next().unwrap(),
                _ => TdType::Intersection {
                    span: *span,
                    parts: kept,
                },
            };
            Some((stripped, steps))
        }
        _ => None,
    }
}

fn is_compose(name: &str) -> bool {
    matches!(name, "Compose" | "Sequential")
}

/// Expand user aliases one step at a time until we hit a non-alias type,
/// a stdlib builtin (including the `Compose`/effect-row markers), or run
/// out of budget. Mirrors `effects::resolve_alias` — should probably be
/// factored into a shared helper once we add a third consumer.
fn resolve_alias(ty: &TdType, env: &TypeEnv) -> TdType {
    let mut current = ty.clone();
    for _ in 0..6 {
        let TdType::NamedRef {
            name, type_args, ..
        } = &current
        else {
            break;
        };
        // Stop on compose markers; they're meta, not aliases to unwrap.
        if is_compose(name) {
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

/// Extract the tuple of step types from the `Compose<T>` argument.
///
/// Accepts a Tuple `[A, B, C]` (the canonical form) or — for ergonomics —
/// a bare NamedRef treated as a single-step pipeline. Anything else
/// produces an empty step list and falls through to `verify_io_flow`
/// where the lack of steps isn't considered an error (zero-step
/// pipelines are trivially valid).
fn extract_step_types(arg: Option<&TdType>) -> Vec<TdType> {
    match arg {
        Some(TdType::Tuple { elems, .. }) => elems.clone(),
        Some(other) => vec![other.clone()],
        None => Vec::new(),
    }
}

fn step_display_name(ty: &TdType, idx: usize) -> String {
    match ty {
        TdType::NamedRef { name, .. } => name.clone(),
        _ => format!("step {}", idx + 1),
    }
}

// ---------------------------------------------------------------------------
// Resolving an individual step
// ---------------------------------------------------------------------------

struct ResolvedStep {
    input: TdType,
    output: TdType,
    effects: Effects,
}

/// A step reference must resolve to something that contains a
/// `Prompt<I, O>` somewhere in its declaration (optionally combined
/// with effect rows). We walk through user aliases and intersections
/// to find the Prompt node, then pull its type args.
fn resolve_step(
    ty: &TdType,
    env: &TypeEnv,
    file: &SourceFile,
    diagnostics: &mut Diagnostics,
) -> Option<ResolvedStep> {
    // Collect effects present on this step's declared type. We have to
    // walk the alias tree to find them, same as the parent pass.
    let (effects, _effect_diags) = collect_effects(ty, env, file);
    // We intentionally discard effect diagnostics — they'll also be
    // emitted by the parent-level pass, so surfacing them twice is noise.

    // Search for a `Prompt<I, O>` named-ref along the alias chain.
    let Some((input, output)) = find_prompt_io(ty, env) else {
        diagnostics.push(
            TdDiagnostic::error(
                "td701",
                format!(
                    "step `{}` does not resolve to a `Prompt<I, O>` shape",
                    step_display_name(ty, 0)
                ),
                file,
                ty.span(),
                "not a prompt",
            )
            .with_help(
                "every element of `Compose<[…]>` must be a typed prompt — \
                 declare it as `type X = Prompt<In, Out> & …` and reference \
                 it by name".to_string(),
            ),
        );
        return None;
    };

    Some(ResolvedStep {
        input,
        output,
        effects,
    })
}

/// Search for a `Prompt<I, O>` reference by walking aliases and
/// intersections without ever *expanding* `Prompt` itself (which would
/// destroy the NamedRef we're trying to find, since `Prompt` in the
/// stdlib resolves to an object body).
///
/// Walk rules:
/// * Hit `Prompt<I, O>` → return `(I, O)`.
/// * Hit any other NamedRef → resolve it through the env one step and
///   recurse. Missing decls and non-alias lookups stop the walk.
/// * Hit an intersection → try each part.
/// * Anything else → not found.
///
/// Bounded depth to prevent pathological cycles in user type graphs.
fn find_prompt_io(ty: &TdType, env: &TypeEnv) -> Option<(TdType, TdType)> {
    fn go(ty: &TdType, env: &TypeEnv, depth: usize) -> Option<(TdType, TdType)> {
        if depth == 0 {
            return None;
        }
        match ty {
            TdType::NamedRef {
                name, type_args, ..
            } if name == "Prompt" && type_args.len() >= 2 => {
                Some((type_args[0].clone(), type_args[1].clone()))
            }
            TdType::NamedRef {
                name, type_args, ..
            } => match env.lookup(name) {
                LookupResult::Decl(entry) => {
                    let expanded = env.instantiate(&entry.decl, type_args);
                    go(&expanded, env, depth - 1)
                }
                _ => None,
            },
            TdType::Intersection { parts, .. } => parts.iter().find_map(|p| go(p, env, depth - 1)),
            _ => None,
        }
    }
    go(ty, env, 8)
}

// ---------------------------------------------------------------------------
// I/O flow verification
// ---------------------------------------------------------------------------

fn verify_io_flow(
    steps: &[ComposedStep],
    file: &SourceFile,
    diagnostics: &mut Diagnostics,
) {
    for window in steps.windows(2) {
        let prev = &window[0];
        let next = &window[1];
        if !types_equivalent(&prev.output, &next.input) {
            diagnostics.push(
                TdDiagnostic::error(
                    "td702",
                    format!(
                        "pipeline I/O mismatch: `{}`'s output does not match `{}`'s input",
                        prev.name, next.name,
                    ),
                    file,
                    next.span,
                    "step input type doesn't match previous step's output",
                )
                .with_help(format!(
                    "adjust `{}`'s output type or `{}`'s input type so they \
                     describe the same shape; structural equality is required",
                    prev.name, next.name,
                )),
            );
        }
    }
}

/// Structural equivalence of two types for the purposes of I/O flow.
///
/// v1 semantics:
/// * Primitives equal iff same kind.
/// * String / number literals equal iff same value.
/// * Arrays equal iff element types equal.
/// * Tuples equal iff same arity and pairwise elements equal.
/// * Objects equal iff same set of field names AND field-by-field equal
///   (optional flags must match too; widening/narrowing is v2 work).
/// * Unions / intersections equal iff arity + pairwise equal (order-
///   independent would be nicer but v1 compares positionally for speed).
/// * Named refs equal iff same name + same type args equal.
///
/// This is deliberately strict. Relaxing to subtyping (covariant
/// outputs, contravariant inputs) is the natural next move but
/// introduces real complexity. Ship strict first, relax with data.
pub fn types_equivalent(a: &TdType, b: &TdType) -> bool {
    use TdType::*;
    match (a, b) {
        (Primitive { kind: ka, .. }, Primitive { kind: kb, .. }) => ka == kb,
        (StringLit { value: va, .. }, StringLit { value: vb, .. }) => va == vb,
        (NumberLit { value: va, .. }, NumberLit { value: vb, .. }) => va == vb,
        (Array { elem: ea, .. }, Array { elem: eb, .. }) => types_equivalent(ea, eb),
        (Tuple { elems: ea, .. }, Tuple { elems: eb, .. }) => {
            ea.len() == eb.len()
                && ea.iter().zip(eb.iter()).all(|(x, y)| types_equivalent(x, y))
        }
        (Object(oa), Object(ob)) => objects_equivalent(oa, ob),
        (
            Union {
                variants: va, ..
            },
            Union {
                variants: vb, ..
            },
        ) => {
            va.len() == vb.len()
                && va.iter().zip(vb.iter()).all(|(x, y)| types_equivalent(x, y))
        }
        (
            Intersection { parts: pa, .. },
            Intersection { parts: pb, .. },
        ) => {
            pa.len() == pb.len()
                && pa.iter().zip(pb.iter()).all(|(x, y)| types_equivalent(x, y))
        }
        (
            NamedRef {
                name: na,
                type_args: aa,
                ..
            },
            NamedRef {
                name: nb,
                type_args: ab,
                ..
            },
        ) => {
            na == nb
                && aa.len() == ab.len()
                && aa.iter().zip(ab.iter()).all(|(x, y)| types_equivalent(x, y))
        }
        _ => false,
    }
}

fn objects_equivalent(a: &TdObjectType, b: &TdObjectType) -> bool {
    if a.fields.len() != b.fields.len() {
        return false;
    }
    // Field-name-indexed comparison so source order doesn't matter.
    let a_by_name: std::collections::HashMap<&str, &TdField> =
        a.fields.iter().map(|f| (f.name.as_str(), f)).collect();
    for bf in &b.fields {
        let Some(af) = a_by_name.get(bf.name.as_str()) else {
            return false;
        };
        if af.optional != bf.optional {
            return false;
        }
        if !types_equivalent(&af.ty, &bf.ty) {
            return false;
        }
    }
    true
}

// ---------------------------------------------------------------------------
// Effect subset verification
// ---------------------------------------------------------------------------

fn verify_effect_subsets(
    parent: &Effects,
    steps: &[ComposedStep],
    file: &SourceFile,
    diagnostics: &mut Diagnostics,
) {
    // If the parent never declared effects, we have no ceiling to enforce;
    // skip rather than flag every child as unauthorized.
    if !parent.declared {
        return;
    }

    for step in steps {
        if !step.effects.declared {
            // Child without declared effects inherits the parent's ceiling
            // implicitly — deny nothing specifically at the child level.
            continue;
        }

        // `Uses`: child ⊆ parent.
        let missing_uses: Vec<&String> = step
            .effects
            .uses
            .iter()
            .filter(|u| !parent.uses.contains(u))
            .collect();
        if !missing_uses.is_empty() {
            diagnostics.push(subset_violation(
                "Uses",
                &step.name,
                &missing_uses,
                step.span,
                file,
            ));
        }

        // `Reads`: child patterns must each appear verbatim in parent's
        // declared Reads list. String equality (not glob containment)
        // keeps the check local and predictable; glob-semantic subset
        // checking is a v2 refinement (requires a subset decision
        // procedure for globs).
        let missing_reads: Vec<&String> = step
            .effects
            .reads
            .iter()
            .filter(|r| !parent.reads.contains(r))
            .collect();
        if !missing_reads.is_empty() {
            diagnostics.push(subset_violation(
                "Reads",
                &step.name,
                &missing_reads,
                step.span,
                file,
            ));
        }

        let missing_writes: Vec<&String> = step
            .effects
            .writes
            .iter()
            .filter(|w| !parent.writes.contains(w))
            .collect();
        if !missing_writes.is_empty() {
            diagnostics.push(subset_violation(
                "Writes",
                &step.name,
                &missing_writes,
                step.span,
                file,
            ));
        }

        // `Model`: each child-declared model must appear in parent's set.
        // When the parent didn't declare a Model, any child model is fine.
        if !parent.models.is_empty() {
            let missing_models: Vec<&String> = step
                .effects
                .models
                .iter()
                .filter(|m| !parent.models.contains(m))
                .collect();
            if !missing_models.is_empty() {
                diagnostics.push(
                    TdDiagnostic::error(
                        "td704",
                        format!(
                            "step `{}` declares model(s) {} not in the pipeline's `Model<>`",
                            step.name,
                            missing_models
                                .iter()
                                .map(|s| format!("`{s}`"))
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                        file,
                        step.span,
                        "model not authorized by parent",
                    )
                    .with_help(
                        "add the model to the pipeline's `Model<…>` set or \
                         change the step to use a permitted model"
                            .to_string(),
                    ),
                );
            }
        }

        // `MaxTokens`: child ≤ parent. When parent doesn't declare a
        // ceiling the child is unrestricted.
        if let (Some(child_max), Some(parent_max)) =
            (step.effects.max_tokens, parent.max_tokens)
        {
            if child_max > parent_max {
                diagnostics.push(
                    TdDiagnostic::error(
                        "td705",
                        format!(
                            "step `{}` declares `MaxTokens<{}>` which exceeds \
                             the pipeline's ceiling of {}",
                            step.name, child_max, parent_max
                        ),
                        file,
                        step.span,
                        "token budget exceeds parent",
                    )
                    .with_help(
                        "raise the pipeline's `MaxTokens<…>` or lower the \
                         step's ceiling".to_string(),
                    ),
                );
            }
        }
    }
}

fn subset_violation(
    effect_kind: &str,
    step_name: &str,
    missing: &[&String],
    span: Span,
    file: &SourceFile,
) -> TdDiagnostic {
    TdDiagnostic::error(
        "td703",
        format!(
            "step `{step_name}` declares `{effect_kind}<>` entries not in \
             the pipeline's ceiling: {}",
            missing
                .iter()
                .map(|s| format!("`{s}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        file,
        span,
        "capability not authorized by parent pipeline",
    )
    .with_help(format!(
        "either add these entries to the pipeline's `{effect_kind}<…>` \
         declaration or remove them from the step"
    ))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use td_parse::parse_td_module;

    fn ty_env(src: &str, doc_type: &str) -> (TdType, TypeEnv, SourceFile) {
        let file = SourceFile::new("t.td", src.to_string());
        let (module, _) = parse_td_module(src, &file, 0);
        let (env, _) = TypeEnv::build(&module, &file);
        let ty = match env.lookup(doc_type) {
            LookupResult::Decl(e) => env.instantiate(&e.decl, &[]),
            _ => panic!("missing `{doc_type}`"),
        };
        (ty, env, file)
    }

    fn codes(d: &Diagnostics) -> Vec<String> {
        d.iter().map(|x| x.code.clone()).collect()
    }

    const PIPE_OK: &str = r#"
        import { Prompt, Uses, Reads, Writes, Model, MaxTokens } from "typedown/agents"
        import { Compose } from "typedown/workflows"

        type Query = { text: string }
        type Class = { kind: string }
        type Response = { answer: string }

        type Classify = Prompt<Query, Class> & Uses<[]> & MaxTokens<512>
        type Answer   = Prompt<Class, Response> & Uses<["retrieve"]> & MaxTokens<2048>

        export type Doc =
          & Compose<[Classify, Answer]>
          & Uses<["retrieve"]>
          & MaxTokens<4096>
    "#;

    #[test]
    fn clean_pipeline_has_no_diagnostics() {
        let (ty, env, file) = ty_env(PIPE_OK, "Doc");
        let (_, comp, d) = split_compose(&ty, &env, &file);
        assert!(d.is_empty(), "codes: {:?}", codes(&d));
        let c = comp.expect("composition");
        assert_eq!(c.steps.len(), 2);
        assert_eq!(c.steps[0].name, "Classify");
        assert_eq!(c.steps[1].name, "Answer");
        // Pipeline I/O = first.in, last.out.
        assert!(matches!(c.input, Some(TdType::NamedRef { ref name, .. }) if name == "Query"));
        assert!(matches!(c.output, Some(TdType::NamedRef { ref name, .. }) if name == "Response"));
    }

    #[test]
    fn mismatched_io_fires_td702() {
        let src = r#"
            import { Prompt } from "typedown/agents"
            import { Compose } from "typedown/workflows"

            type A = { a: string }
            type B = { b: string }
            type C = { c: string }

            type StepA = Prompt<A, B>
            // StepB *claims* to take C but the previous step outputs B.
            type StepB = Prompt<C, A>

            export type Doc = Compose<[StepA, StepB]>
        "#;
        let (ty, env, file) = ty_env(src, "Doc");
        let (_, _, d) = split_compose(&ty, &env, &file);
        assert!(codes(&d).contains(&"td702".to_string()), "codes: {:?}", codes(&d));
    }

    #[test]
    fn non_prompt_step_fires_td701() {
        let src = r#"
            import { Prompt } from "typedown/agents"
            import { Compose } from "typedown/workflows"

            type NotAPrompt = { foo: string }
            type Good = Prompt<{ x: string }, { y: string }>

            export type Doc = Compose<[NotAPrompt, Good]>
        "#;
        let (ty, env, file) = ty_env(src, "Doc");
        let (_, _, d) = split_compose(&ty, &env, &file);
        assert!(codes(&d).contains(&"td701".to_string()), "codes: {:?}", codes(&d));
    }

    #[test]
    fn child_uses_not_in_parent_fires_td703() {
        let src = r#"
            import { Prompt, Uses } from "typedown/agents"
            import { Compose } from "typedown/workflows"

            type A = { a: string }
            type B = { b: string }

            type Step1 = Prompt<A, B> & Uses<["shell_exec"]>

            // Parent only authorizes read_file; child wants shell_exec.
            export type Doc =
              & Compose<[Step1]>
              & Uses<["read_file"]>
        "#;
        let (ty, env, file) = ty_env(src, "Doc");
        let (_, _, d) = split_compose(&ty, &env, &file);
        assert!(codes(&d).contains(&"td703".to_string()), "codes: {:?}", codes(&d));
        let msg = &d.iter().find(|x| x.code == "td703").unwrap().message;
        assert!(msg.contains("shell_exec"), "msg: {msg}");
    }

    #[test]
    fn child_uses_covered_is_clean() {
        let src = r#"
            import { Prompt, Uses } from "typedown/agents"
            import { Compose } from "typedown/workflows"

            type A = { a: string }
            type B = { b: string }

            type Step1 = Prompt<A, B> & Uses<["read_file"]>
            type Step2 = Prompt<B, A> & Uses<["run_tests"]>

            export type Doc =
              & Compose<[Step1, Step2]>
              & Uses<["read_file", "run_tests", "extra_tool"]>
        "#;
        let (ty, env, file) = ty_env(src, "Doc");
        let (_, _, d) = split_compose(&ty, &env, &file);
        assert!(
            !codes(&d).contains(&"td703".to_string()),
            "should not fire; codes: {:?}",
            codes(&d)
        );
    }

    #[test]
    fn child_model_not_in_parent_fires_td704() {
        let src = r#"
            import { Prompt, Model } from "typedown/agents"
            import { Compose } from "typedown/workflows"

            type A = { a: string }
            type B = { b: string }

            type Step1 = Prompt<A, B> & Model<"gpt-5-pro">

            export type Doc =
              & Compose<[Step1]>
              & Model<"claude-opus-4-5">
        "#;
        let (ty, env, file) = ty_env(src, "Doc");
        let (_, _, d) = split_compose(&ty, &env, &file);
        assert!(codes(&d).contains(&"td704".to_string()), "codes: {:?}", codes(&d));
    }

    #[test]
    fn child_max_tokens_exceeds_parent_fires_td705() {
        let src = r#"
            import { Prompt, MaxTokens } from "typedown/agents"
            import { Compose } from "typedown/workflows"

            type A = { a: string }
            type B = { b: string }

            type Step1 = Prompt<A, B> & MaxTokens<8192>

            export type Doc =
              & Compose<[Step1]>
              & MaxTokens<4096>
        "#;
        let (ty, env, file) = ty_env(src, "Doc");
        let (_, _, d) = split_compose(&ty, &env, &file);
        assert!(codes(&d).contains(&"td705".to_string()), "codes: {:?}", codes(&d));
    }

    #[test]
    fn child_max_tokens_within_parent_is_clean() {
        let src = r#"
            import { Prompt, MaxTokens } from "typedown/agents"
            import { Compose } from "typedown/workflows"

            type A = { a: string }
            type B = { b: string }

            type Step1 = Prompt<A, B> & MaxTokens<1024>

            export type Doc =
              & Compose<[Step1]>
              & MaxTokens<4096>
        "#;
        let (ty, env, file) = ty_env(src, "Doc");
        let (_, _, d) = split_compose(&ty, &env, &file);
        assert!(
            !codes(&d).contains(&"td705".to_string()),
            "should not fire; codes: {:?}",
            codes(&d)
        );
    }

    #[test]
    fn sequential_alias_works() {
        let src = r#"
            import { Prompt } from "typedown/agents"
            import { Sequential } from "typedown/workflows"

            type A = { a: string }
            type B = { b: string }

            type Step1 = Prompt<A, B>

            export type Doc = Sequential<[Step1]>
        "#;
        let (ty, env, file) = ty_env(src, "Doc");
        let (_, comp, d) = split_compose(&ty, &env, &file);
        assert!(d.is_empty(), "codes: {:?}", codes(&d));
        assert_eq!(comp.expect("composition").steps.len(), 1);
    }

    #[test]
    fn no_compose_returns_none() {
        let src = r#"
            import { Prompt } from "typedown/agents"
            type A = { a: string }
            type B = { b: string }
            export type Doc = Prompt<A, B>
        "#;
        let (ty, env, file) = ty_env(src, "Doc");
        let (_, comp, d) = split_compose(&ty, &env, &file);
        assert!(d.is_empty(), "codes: {:?}", codes(&d));
        assert!(comp.is_none());
    }

    #[test]
    fn structural_object_equivalence() {
        let file = SourceFile::new("t.td", String::new());
        let (module_a, _) = parse_td_module("type A = { x: string, y: number }", &file, 0);
        let (module_b, _) = parse_td_module("type B = { y: number, x: string }", &file, 0);
        let (env_a, _) = TypeEnv::build(&module_a, &file);
        let (env_b, _) = TypeEnv::build(&module_b, &file);
        let a = match env_a.lookup("A") {
            LookupResult::Decl(e) => env_a.instantiate(&e.decl, &[]),
            _ => panic!("missing A"),
        };
        let b = match env_b.lookup("B") {
            LookupResult::Decl(e) => env_b.instantiate(&e.decl, &[]),
            _ => panic!("missing B"),
        };
        // Field order shouldn't matter for equivalence.
        assert!(types_equivalent(&a, &b));
    }
}
