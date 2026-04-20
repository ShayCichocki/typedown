//! Inlay hint provider.
//!
//! Three hint locations:
//!   1. End of the frontmatter `typedown:` line → one-line effects summary.
//!   2. (If composition present) one hint per step with its input/output type.
//!   3. (v1.1 candidate) Top of `td` fences with merged type — skipped in
//!      v1 because a high-signal rendering requires more context than we
//!      want in this pass.

use td_ast::td::TdType;
use td_check::TypeEnv;
use tower_lsp::lsp_types::{InlayHint, InlayHintKind, InlayHintLabel, Range};

use crate::state::DocState;

pub fn hints(state: &DocState, range: Range) -> Vec<InlayHint> {
    let mut out = Vec::new();

    // --- 1. effects summary on typedown: line ---
    if let Some(fm) = &state.doc.frontmatter {
        let src = state.line_index.text();
        if let Some(td_line_end) = find_typedown_line_end(src, fm.span.start, fm.span.end) {
            let pos = state.line_index.position(td_line_end);
            if within(pos, range) && state.effects.declared {
                let summary = format_effects_summary(state);
                out.push(InlayHint {
                    position: pos,
                    label: InlayHintLabel::String(format!("  // {summary}")),
                    kind: Some(InlayHintKind::TYPE),
                    text_edits: None,
                    tooltip: None,
                    padding_left: Some(true),
                    padding_right: None,
                    data: None,
                });
            }
        }
    }

    // --- 2. pipeline step I/O ---
    if let Some(comp) = &state.composition {
        for step in &comp.steps {
            let step_span = step.input.span();
            let pos = state.line_index.position(step_span.end);
            if !within(pos, range) {
                continue;
            }
            out.push(InlayHint {
                position: pos,
                label: InlayHintLabel::String(format!(
                    " // {} : {} -> {}",
                    step.name,
                    type_brief(&step.input, &state.env),
                    type_brief(&step.output, &state.env),
                )),
                kind: Some(InlayHintKind::TYPE),
                text_edits: None,
                tooltip: None,
                padding_left: Some(true),
                padding_right: None,
                data: None,
            });
        }
    }

    out
}

fn format_effects_summary(state: &DocState) -> String {
    let e = &state.effects;
    let mut parts: Vec<String> = Vec::new();
    if !e.uses.is_empty() {
        parts.push(format!("uses={{{}}}", e.uses.join(",")));
    }
    if !e.reads.is_empty() {
        parts.push(format!("reads={{{}}}", e.reads.len()));
    }
    if !e.writes.is_empty() {
        parts.push(format!("writes={{{}}}", e.writes.len()));
    }
    if !e.models.is_empty() {
        parts.push(format!("model={}", e.models.join("|")));
    }
    if let Some(n) = e.max_tokens {
        parts.push(format!("maxTokens={n}"));
    }
    if parts.is_empty() {
        "effects: ∅".to_string()
    } else {
        format!("effects: {}", parts.join(", "))
    }
}

fn find_typedown_line_end(src: &str, fm_start: usize, fm_end: usize) -> Option<usize> {
    let slice = src.get(fm_start..fm_end)?;
    let mut cursor = fm_start;
    for line in slice.split_inclusive('\n') {
        let line_no_nl = line.strip_suffix('\n').unwrap_or(line);
        let trimmed = line_no_nl.trim_start();
        if trimmed.starts_with("typedown:") {
            return Some(cursor + line_no_nl.len());
        }
        cursor += line.len();
    }
    None
}

fn type_brief(ty: &TdType, _env: &TypeEnv) -> String {
    match ty {
        TdType::NamedRef { name, type_args, .. } if type_args.is_empty() => name.clone(),
        TdType::NamedRef { name, type_args, .. } => format!(
            "{name}<{}>",
            type_args
                .iter()
                .map(|a| type_brief(a, _env))
                .collect::<Vec<_>>()
                .join(",")
        ),
        TdType::Primitive { kind, .. } => format!("{kind:?}").to_lowercase(),
        TdType::Object(_) => "{…}".to_string(),
        TdType::Union { variants, .. } => variants
            .iter()
            .map(|v| type_brief(v, _env))
            .collect::<Vec<_>>()
            .join("|"),
        TdType::Intersection { parts, .. } => parts
            .iter()
            .map(|p| type_brief(p, _env))
            .collect::<Vec<_>>()
            .join("&"),
        TdType::Array { elem, .. } => format!("{}[]", type_brief(elem, _env)),
        TdType::Tuple { elems, .. } => format!(
            "[{}]",
            elems
                .iter()
                .map(|e| type_brief(e, _env))
                .collect::<Vec<_>>()
                .join(",")
        ),
        TdType::StringLit { value, .. } => format!("\"{value}\""),
        TdType::NumberLit { value, .. } => value.to_string(),
    }
}

fn within(pos: tower_lsp::lsp_types::Position, range: Range) -> bool {
    use std::cmp::Ordering;
    let after_start = matches!(cmp_pos(pos, range.start), Ordering::Greater | Ordering::Equal);
    let before_end = matches!(cmp_pos(pos, range.end), Ordering::Less | Ordering::Equal);
    after_start && before_end
}

fn cmp_pos(
    a: tower_lsp::lsp_types::Position,
    b: tower_lsp::lsp_types::Position,
) -> std::cmp::Ordering {
    use std::cmp::Ordering::*;
    match a.line.cmp(&b.line) {
        Equal => a.character.cmp(&b.character),
        other => other,
    }
}
