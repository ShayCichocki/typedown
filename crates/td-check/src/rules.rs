//! Conformance rules: given a resolved [`TdType`] and an [`MdDoc`], emit
//! diagnostics for every place the markdown fails to match the declared
//! structure.
//!
//! # Diagnostic codes
//!
//! | code   | meaning                                   |
//! |--------|-------------------------------------------|
//! | td401  | required section is missing              |
//! | td402  | section body does not match expected type |
//! | td403  | unknown type referenced in declaration    |
//! | td404  | expected an object-shaped doc type        |
//! | td405  | extra top-level heading not in schema     |
//!
//! # Shape model
//!
//! The top-level document type is expected to collapse into an **object**
//! (fields → sections). Intersections of objects are flattened; named refs
//! are resolved through the type environment. Unions at the top level
//! aren't supported yet — a document has one shape, not several.

use std::collections::{BTreeMap, HashSet};

use td_ast::{
    md::{MdDoc, MdNode, MdNodeKind},
    td::{TdField, TdObjectType, TdType},
};
use td_core::{Diagnostics, Severity, SourceFile, Span, TdDiagnostic};
use td_stdlib::Builtin;

use crate::env::{LookupResult, TypeEnv};
use crate::value::{check_value, parse_value, VALUE_FENCE_LANGS};

pub fn check_doc(
    doc: &MdDoc,
    doc_type: &TdType,
    env: &TypeEnv,
    file: &SourceFile,
    diagnostics: &mut Diagnostics,
) {
    let anchor_span = doc_type.span();
    // Collapse the declared type into a flat list of fields (name → Section<T>).
    let Some(object) = flatten_to_object(doc_type, env, file, diagnostics) else {
        // Error already emitted.
        return;
    };

    // Gather all top-level h2 sections, indexed by slug.
    let sections = collect_level_sections(doc, 2);

    // Track what the schema covers so we can flag extras.
    let mut covered: HashSet<String> = HashSet::new();

    for field in &object.fields {
        let field_slug = slugify(&field.name);
        covered.insert(field_slug.clone());

        match sections.get(&field_slug) {
            Some(section_idx) => {
                let heading_node = &doc.nodes[*section_idx];
                let body = doc.section_body(*section_idx);
                check_section_body(
                    &field.name,
                    heading_node,
                    body,
                    &field.ty,
                    env,
                    file,
                    diagnostics,
                );
            }
            None if field.optional => { /* fine, it's optional */ }
            None => {
                let span = pick_anchor(field.span, anchor_span, &file.content);
                diagnostics.push(
                    TdDiagnostic::error(
                        "td401",
                        format!("missing required section `{}`", pretty(&field.name)),
                        file,
                        span,
                        "required by document type",
                    )
                    .with_help(format!(
                        "add a `## {}` section to this document",
                        pretty(&field.name)
                    )),
                );
            }
        }
    }

    // Extra headings flagged as warnings, not errors — unknown sections may
    // just be prose the author wants.
    for (slug, idx) in &sections {
        if covered.contains(slug) {
            continue;
        }
        let node = &doc.nodes[*idx];
        let MdNodeKind::Heading { text, .. } = &node.kind else {
            continue;
        };
        diagnostics.push(
            TdDiagnostic::error(
                "td405",
                format!("section `{text}` is not declared in the document type"),
                file,
                node.span,
                "unknown section",
            )
            .with_severity(Severity::Warning)
            .with_help(
                "either add this field to the `td` declaration, \
                 or remove the heading"
                    .to_string(),
            ),
        );
    }
}

/// Collapse an arbitrary TdType into a single object describing the top-level
/// document fields. Supports:
///
/// * named-refs (resolved then re-entered recursively)
/// * intersections of objects (fields merged; duplicates: last wins with warn)
/// * bare object types
///
/// Anything else yields `td404` and `None`.
fn flatten_to_object(
    ty: &TdType,
    env: &TypeEnv,
    file: &SourceFile,
    diagnostics: &mut Diagnostics,
) -> Option<TdObjectType> {
    match ty {
        TdType::Object(o) => Some(o.clone()),

        TdType::Intersection { parts, span } => {
            let mut fields: BTreeMap<String, TdField> = BTreeMap::new();
            let mut last_span = *span;
            for p in parts {
                if let Some(o) = flatten_to_object(p, env, file, diagnostics) {
                    for f in o.fields {
                        last_span = last_span.join(f.span);
                        fields.insert(f.name.clone(), f);
                    }
                }
            }
            Some(TdObjectType {
                span: last_span,
                fields: fields.into_values().collect(),
            })
        }

        TdType::NamedRef {
            name,
            type_args,
            span,
        } => match env.lookup(name) {
            LookupResult::Decl(entry) => {
                let expanded = env.instantiate(&entry.decl, type_args);
                flatten_to_object(&expanded, env, file, diagnostics)
            }
            LookupResult::Builtin(_) => {
                diagnostics.push(TdDiagnostic::error(
                    "td404",
                    format!("cannot use built-in `{name}` as a top-level document type"),
                    file,
                    *span,
                    "not a document shape",
                ).with_help(
                    "top-level document types must be an object or composite \
                     of objects (e.g. `Prompt<I, O>`, `Readme`, a custom type)"
                        .to_string(),
                ));
                None
            }
            LookupResult::Missing => {
                diagnostics.push(TdDiagnostic::error(
                    "td403",
                    format!("unknown type `{name}`"),
                    file,
                    *span,
                    "not declared or imported",
                ).with_help(
                    "declare it in a ```td fence or import it from `typedown/agents` / `typedown/docs`"
                        .to_string(),
                ));
                None
            }
        },

        other => {
            diagnostics.push(TdDiagnostic::error(
                "td404",
                "document type must be an object or an intersection of objects".to_string(),
                file,
                other.span(),
                "not a document shape",
            ));
            None
        }
    }
}

/// Check that a section's body matches the field's declared type.
///
/// The field type is typically `Section<T>` (from `typedown/agents`). We
/// unwrap that one layer and delegate to [`check_body_against`].
fn check_section_body(
    field_name: &str,
    heading_node: &MdNode,
    body: &[MdNode],
    field_ty: &TdType,
    env: &TypeEnv,
    file: &SourceFile,
    diagnostics: &mut Diagnostics,
) {
    let inner = unwrap_section(field_ty, env).unwrap_or(field_ty.clone());
    check_body_against(field_name, heading_node, body, &inner, env, file, diagnostics);
}

fn unwrap_section(ty: &TdType, env: &TypeEnv) -> Option<TdType> {
    let TdType::NamedRef {
        name, type_args, ..
    } = ty
    else {
        return None;
    };
    match env.lookup(name) {
        LookupResult::Builtin(Builtin::Section) => {
            type_args
                .first()
                .cloned()
                .or_else(|| {
                    Some(TdType::NamedRef {
                        span: Span::DUMMY,
                        name: "Prose".into(),
                        type_args: vec![],
                    })
                })
        }
        LookupResult::Decl(entry) => {
            // Instantiate then try again in case the user aliased Section.
            let expanded = env.instantiate(&entry.decl, type_args);
            match &expanded {
                TdType::NamedRef { .. } => unwrap_section(&expanded, env),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Check body nodes against a content type.
fn check_body_against(
    field_name: &str,
    heading_node: &MdNode,
    body: &[MdNode],
    ty: &TdType,
    env: &TypeEnv,
    file: &SourceFile,
    diagnostics: &mut Diagnostics,
) {
    match ty {
        TdType::NamedRef {
            name,
            type_args,
            span,
        } => match env.lookup(name) {
            LookupResult::Builtin(b) => check_builtin(
                b,
                type_args,
                field_name,
                heading_node,
                body,
                env,
                file,
                diagnostics,
                *span,
            ),
            LookupResult::Decl(entry) => {
                let expanded = env.instantiate(&entry.decl, type_args);
                check_body_against(field_name, heading_node, body, &expanded, env, file, diagnostics)
            }
            LookupResult::Missing => {
                diagnostics.push(TdDiagnostic::error(
                    "td403",
                    format!("unknown type `{name}`"),
                    file,
                    *span,
                    "not declared or imported",
                ));
            }
        },

        TdType::Array { elem, .. } => check_array_body(
            field_name,
            heading_node,
            body,
            elem,
            env,
            file,
            diagnostics,
        ),

        TdType::Union { variants, .. } => {
            // Any variant matches → clean. Otherwise emit a summary error.
            let mut tried_diags = Vec::new();
            for v in variants {
                let mut scratch = Diagnostics::new();
                check_body_against(
                    field_name,
                    heading_node,
                    body,
                    v,
                    env,
                    file,
                    &mut scratch,
                );
                if scratch.is_empty() {
                    return;
                }
                tried_diags.push(scratch);
            }
            diagnostics.push(TdDiagnostic::error(
                "td402",
                format!(
                    "section `{}` does not match any variant of its declared union type",
                    pretty(field_name)
                ),
                file,
                heading_node.span,
                "no variant matched",
            ).with_help(
                "check each allowed shape in the type declaration".to_string(),
            ));
        }

        TdType::Intersection { parts, .. } => {
            for p in parts {
                check_body_against(field_name, heading_node, body, p, env, file, diagnostics);
            }
        }

        TdType::Object(_) => {
            // Object inside a section body means "this section contains
            // sub-sections, one per field". Recurse with `###` headings.
            let sub_fields = match flatten_to_object(ty, env, file, diagnostics) {
                Some(o) => o,
                None => return,
            };
            let sub_sections = collect_subsections_under(body, 3);
            for f in &sub_fields.fields {
                let field_slug = slugify(&f.name);
                match sub_sections.get(&field_slug) {
                    Some((heading, body)) => check_section_body(
                        &f.name,
                        heading,
                        body,
                        &f.ty,
                        env,
                        file,
                        diagnostics,
                    ),
                    None if f.optional => {}
                    None => {
                        diagnostics.push(TdDiagnostic::error(
                            "td401",
                            format!("missing sub-section `{}`", pretty(&f.name)),
                            file,
                            f.span,
                            "required by type",
                        ));
                    }
                }
            }
        }

        // Primitives at the body level aren't meaningful (they describe
        // values, not markdown bodies). Quietly accept — they're typically
        // used inside Example<I, O> code-fence values.
        TdType::Primitive { .. } | TdType::StringLit { .. } | TdType::NumberLit { .. } => {}
    }
}

fn check_array_body(
    field_name: &str,
    heading_node: &MdNode,
    body: &[MdNode],
    elem: &TdType,
    env: &TypeEnv,
    file: &SourceFile,
    diagnostics: &mut Diagnostics,
) {
    // Each element becomes one `### ...` subsection. If the element type is
    // an object or named ref that resolves to one, we recurse. Otherwise we
    // match against the whole sub-body.
    let subs = collect_subsections_under(body, 3);
    if subs.is_empty() {
        diagnostics.push(TdDiagnostic::error(
            "td402",
            format!(
                "section `{}` must contain one or more `###` sub-sections",
                pretty(field_name)
            ),
            file,
            heading_node.span,
            "no items found",
        ));
        return;
    }
    for (_slug, (heading, body)) in subs {
        let name = match &heading.kind {
            MdNodeKind::Heading { text, .. } => text.clone(),
            _ => String::new(),
        };
        check_body_against(&name, heading, body, elem, env, file, diagnostics);
    }
}

#[allow(clippy::too_many_arguments)]
fn check_builtin(
    b: Builtin,
    type_args: &[TdType],
    field_name: &str,
    heading_node: &MdNode,
    body: &[MdNode],
    env: &TypeEnv,
    file: &SourceFile,
    diagnostics: &mut Diagnostics,
    ref_span: Span,
) {
    use MdNodeKind::*;
    let ok = |nodes: &[MdNode], pred: fn(&MdNodeKind) -> bool| nodes.iter().any(|n| pred(&n.kind));

    match b {
        Builtin::Prose => {
            if !ok(body, |k| matches!(k, Paragraph { .. })) {
                diagnostics.push(shape_error(
                    field_name,
                    "at least one paragraph of prose",
                    heading_node,
                    file,
                ));
            }
        }
        Builtin::OrderedList => {
            if !ok(body, |k| matches!(k, OrderedList { .. })) {
                diagnostics.push(shape_error(
                    field_name,
                    "an ordered list (`1.`, `2.`, …)",
                    heading_node,
                    file,
                ));
            }
        }
        Builtin::UnorderedList => {
            if !ok(body, |k| matches!(k, UnorderedList { .. })) {
                diagnostics.push(shape_error(
                    field_name,
                    "an unordered list (`- …`)",
                    heading_node,
                    file,
                ));
            }
        }
        Builtin::TaskList => {
            if !ok(body, |k| matches!(k, TaskList { .. })) {
                diagnostics.push(shape_error(
                    field_name,
                    "a task list (`- [ ] …`)",
                    heading_node,
                    file,
                ));
            }
        }
        Builtin::CodeBlock => {
            let required_lang = type_args.first().and_then(|t| match t {
                TdType::StringLit { value, .. } => Some(value.as_str()),
                _ => None,
            });
            let matched = body.iter().any(|n| match &n.kind {
                CodeBlock { lang, .. } => match required_lang {
                    Some(req) => lang.as_deref() == Some(req),
                    None => true,
                },
                _ => false,
            });
            if !matched {
                let descr = match required_lang {
                    Some(l) => format!("a fenced code block (```{l})"),
                    None => "a fenced code block".into(),
                };
                diagnostics.push(shape_error(field_name, &descr, heading_node, file));
            }
        }
        Builtin::Heading => {
            // Heading at body level is unusual — just accept.
        }
        Builtin::Section => {
            // Should have been unwrapped already.
            diagnostics.push(TdDiagnostic::error(
                "td402",
                "nested `Section<…>` inside section body is not yet supported".to_string(),
                file,
                ref_span,
                "unexpected Section here",
            ));
        }
        Builtin::Example => {
            // Example<I, O> at body level == { input: I, output: O }.
            //
            // We run *two* checks here, in order:
            //
            //   1. Structural. The body must mention `Input` and `Output`,
            //      either as prose markers ("**Input:** …") or as a typed
            //      value fence labeled with the nearest marker. This keeps
            //      prose-only examples working unchanged (backward compat
            //      with v0) while enabling value typing when authors opt in.
            //
            //   2. Value-level. For every ```json / ```yaml fence we can
            //      associate with an Input or Output marker, we type-check
            //      the parsed value against I or O respectively. This is
            //      what turns `Example<I, O>`'s type parameters from phantom
            //      into load-bearing.
            check_example_body(
                type_args,
                field_name,
                heading_node,
                body,
                env,
                file,
                diagnostics,
            );
        }
    }
}

fn shape_error(
    field_name: &str,
    expected: &str,
    heading: &MdNode,
    file: &SourceFile,
) -> TdDiagnostic {
    TdDiagnostic::error(
        "td402",
        format!(
            "section `{}` should contain {expected}",
            pretty(field_name)
        ),
        file,
        heading.span,
        "shape mismatch",
    )
}

/// Build a map from heading slug → node index for all headings of the given
/// level at the top of the document.
fn collect_level_sections(doc: &MdDoc, level: u8) -> BTreeMap<String, usize> {
    let mut out = BTreeMap::new();
    for (idx, n) in doc.nodes.iter().enumerate() {
        if let MdNodeKind::Heading { level: l, slug, .. } = &n.kind {
            if *l == level {
                out.entry(slug.clone()).or_insert(idx);
            }
        }
    }
    out
}

/// Given a section body, collect every `level` heading as a sub-section
/// (returning heading node + its own body slice).
fn collect_subsections_under(body: &[MdNode], level: u8) -> BTreeMap<String, (&MdNode, &[MdNode])> {
    let mut out: BTreeMap<String, (&MdNode, &[MdNode])> = BTreeMap::new();
    let mut i = 0;
    while i < body.len() {
        let MdNodeKind::Heading { level: l, slug, .. } = &body[i].kind else {
            i += 1;
            continue;
        };
        if *l != level {
            i += 1;
            continue;
        }
        // Find the end of this sub-section.
        let body_start = i + 1;
        let body_end = body[body_start..]
            .iter()
            .position(|n| matches!(&n.kind, MdNodeKind::Heading { level: hl, .. } if *hl <= *l))
            .map(|p| body_start + p)
            .unwrap_or(body.len());
        out.entry(slug.clone())
            .or_insert((&body[i], &body[body_start..body_end]));
        i = body_end;
    }
    out
}

/// Turn a field name like `reviewInput` or `review_input` into a slug
/// comparable with a markdown heading's slug. We match both conventions
/// because frontmatter/type authors reasonably use camelCase while heading
/// authors use Title Case.
fn slugify(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut prev_was_upper = false;
    for (i, c) in name.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 && !prev_was_upper {
                out.push('-');
            }
            out.push(c.to_ascii_lowercase());
            prev_was_upper = true;
        } else if c == '_' || c == '-' || c.is_whitespace() {
            if !out.ends_with('-') && !out.is_empty() {
                out.push('-');
            }
            prev_was_upper = false;
        } else {
            out.push(c);
            prev_was_upper = false;
        }
    }
    out.trim_matches('-').to_string()
}

/// Render a slug/identifier as a human-readable Title Case heading.
fn pretty(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut capitalize = true;
    for c in slugify(name).chars() {
        if c == '-' {
            out.push(' ');
            capitalize = true;
        } else if capitalize {
            out.extend(c.to_uppercase());
            capitalize = false;
        } else {
            out.push(c);
        }
    }
    out
}

/// If `span` is inside `content`, use it; otherwise fall back to `fallback`.
/// Stdlib-sourced type decls carry spans relative to their source string,
/// which is useless to surface to the user.
fn pick_anchor(span: Span, fallback: Span, content: &str) -> Span {
    if span.end <= content.len() {
        span
    } else {
        fallback
    }
}

/// Which role a value fence plays in an `### Example N` subsection.
#[derive(Debug, Clone, Copy, PartialEq)]
enum ExampleSlot {
    Input,
    Output,
}

/// Full conformance check for a single `### Example N` body against the
/// declared `Example<I, O>` type args.
///
/// The pairing rule: walk the body in order. Paragraphs containing
/// "Input"/"Output" flip the current slot. Every subsequent value fence
/// (lang ∈ [`VALUE_FENCE_LANGS`]) is parsed and type-checked against
/// whichever of `I` / `O` the current slot points at. A fence appearing
/// before any marker is treated as Input (common pattern: fence-first
/// examples). Prose examples with no value fences continue to pass as
/// long as the markers exist — the v0 behavior — so adopting typed
/// values is a strictly-opt-in upgrade.
fn check_example_body(
    type_args: &[TdType],
    field_name: &str,
    heading_node: &MdNode,
    body: &[MdNode],
    env: &TypeEnv,
    file: &SourceFile,
    diagnostics: &mut Diagnostics,
) {
    use MdNodeKind::*;

    let input_ty = type_args.first();
    let output_ty = type_args.get(1);

    // Scan for markers + value fences and pair them up.
    let mut seen_input_marker = false;
    let mut seen_output_marker = false;
    let mut current_slot: Option<ExampleSlot> = None;
    let mut input_value_seen = false;
    let mut output_value_seen = false;

    for node in body {
        match &node.kind {
            Paragraph { text } | BlockQuote { text } => {
                let lower = text.to_ascii_lowercase();
                // Detect markers. We require the word to appear near a
                // colon to avoid false positives in prose ("the input…").
                let has_input_marker = has_marker(&lower, "input");
                let has_output_marker = has_marker(&lower, "output");
                if has_input_marker {
                    seen_input_marker = true;
                    current_slot = Some(ExampleSlot::Input);
                }
                if has_output_marker {
                    seen_output_marker = true;
                    current_slot = Some(ExampleSlot::Output);
                }
                // Also accept loose mentions (not colon-adjacent) as a
                // marker hit for the structural check only, preserving v0
                // behavior where any "input" substring sufficed.
                if !has_input_marker && lower.contains("input") {
                    seen_input_marker = true;
                }
                if !has_output_marker && lower.contains("output") {
                    seen_output_marker = true;
                }
            }
            CodeBlock { lang, code } => {
                let Some(l) = lang.as_deref() else { continue };
                if !VALUE_FENCE_LANGS.contains(&l) {
                    continue;
                }
                let slot = current_slot.unwrap_or(ExampleSlot::Input);
                let ty = match slot {
                    ExampleSlot::Input => input_ty,
                    ExampleSlot::Output => output_ty,
                };
                match slot {
                    ExampleSlot::Input => input_value_seen = true,
                    ExampleSlot::Output => output_value_seen = true,
                }
                // Parse.
                let value = match parse_value(l, code) {
                    Ok(v) => v,
                    Err(err) => {
                        diagnostics.push(TdDiagnostic::error(
                            "td501",
                            format!(
                                "failed to parse `{l}` {slot_label} value in example `{}`: {err}",
                                pretty(field_name),
                                slot_label = slot_label(slot),
                            ),
                            file,
                            node.span,
                            "malformed value",
                        ).with_help(
                            "check the JSON / YAML syntax of this code fence".to_string(),
                        ));
                        continue;
                    }
                };
                // Typecheck (if we have a type arg for this slot).
                if let Some(ty) = ty {
                    let path_prefix = slot_label(slot); // "input" / "output"
                    check_value(
                        &value,
                        ty,
                        env,
                        file,
                        node.span,
                        &format!("/{path_prefix}"),
                        diagnostics,
                    );
                }
                // After a value fence we reset the slot so a later fence
                // without a marker is treated as "no slot" rather than
                // re-binding to the last one. This avoids silently
                // double-counting an unlabeled fence.
                current_slot = None;
            }
            _ => {}
        }
    }

    // Structural gate: every Example must reference both Input and Output,
    // either through prose markers or through value fences. Value-fence
    // presence implies a marker was seen (otherwise the slot defaulted to
    // Input, which handles the fence-first case above).
    if !(seen_input_marker || input_value_seen) {
        diagnostics.push(shape_error(
            field_name,
            "an `Input:` marker followed by the input value",
            heading_node,
            file,
        ));
    }
    if !(seen_output_marker || output_value_seen) {
        diagnostics.push(shape_error(
            field_name,
            "an `Output:` marker followed by the output value",
            heading_node,
            file,
        ));
    }
}

/// Match "input" / "output" as a field-style marker (`**Input:** …`,
/// `Input -`, `Input —`). Bare occurrences of the word in prose aren't
/// treated as a marker but are still counted as a structural mention
/// elsewhere for v0 compatibility.
fn has_marker(text_lower: &str, word: &str) -> bool {
    let Some(pos) = text_lower.find(word) else {
        return false;
    };
    // Look for a colon or dash within a few chars after the word.
    let after = &text_lower[pos + word.len()..];
    let probe: String = after.chars().take(4).collect();
    probe.contains(':') || probe.contains('-') || probe.contains('—')
}

fn slot_label(slot: ExampleSlot) -> &'static str {
    match slot {
        ExampleSlot::Input => "input",
        ExampleSlot::Output => "output",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_matches_heading() {
        assert_eq!(slugify("role"), "role");
        assert_eq!(slugify("ReviewInput"), "review-input");
        assert_eq!(slugify("review_input"), "review-input");
        assert_eq!(slugify("example 1"), "example-1");
    }
}
