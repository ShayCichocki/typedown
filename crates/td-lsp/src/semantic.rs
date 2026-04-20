//! Semantic tokens provider.
//!
//! Delivers syntax-coloring hints over LSP for typedown-specific regions:
//! frontmatter type expressions, effect-row keywords, td fence contents.
//! Uses only the standard LSP token types so editors render sensible
//! defaults without needing custom theming.

use td_ast::md::MdNodeKind;
use tower_lsp::lsp_types::{
    SemanticToken, SemanticTokenType, SemanticTokens,
};

use crate::{resolver::EffectKind, state::DocState};

/// Token types, in the order we register them with the client. Indices
/// into this list are what LSP tokens carry on the wire.
pub const TOKEN_TYPES: &[SemanticTokenType] = &[
    SemanticTokenType::KEYWORD,   // 0 — effect-row keywords
    SemanticTokenType::TYPE,      // 1 — type identifiers in frontmatter + td fences
    SemanticTokenType::STRING,    // 2 — quoted effect-row arguments
    SemanticTokenType::NAMESPACE, // 3 — td fence infostring
    SemanticTokenType::COMMENT,   // 4 — not used yet, reserved
];

const T_KEYWORD: u32 = 0;
const T_TYPE: u32 = 1;
const T_STRING: u32 = 2;
const T_NAMESPACE: u32 = 3;

pub fn tokens_full(state: &DocState) -> SemanticTokens {
    // LSP semantic tokens are delta-encoded: (deltaLine, deltaStartChar,
    // length, tokenType, tokenModifiers). We build them in source order
    // and encode at the end.
    let mut flat: Vec<FlatToken> = Vec::new();

    // Frontmatter `typedown:` line
    if let Some(fm) = &state.doc.frontmatter {
        scan_typedown_line(state.line_index.text(), fm.span.start, fm.span.end, &mut flat);
    }

    // `td` fences — color the infostring and any `export type X` / `interface X` identifiers.
    for node in &state.doc.nodes {
        if let MdNodeKind::CodeBlock { lang: Some(lang), code } = &node.kind {
            if lang.trim_start() == "td" {
                // Find the `td` infostring offset in the raw source.
                let block_start = node.span.start;
                let src = state.line_index.text();
                if let Some(info_off) = find_infostring(src, block_start, "td") {
                    flat.push(FlatToken {
                        start: info_off,
                        len: 2,
                        token_type: T_NAMESPACE,
                    });
                }
                // The body of the code block starts after the infostring
                // line; we approximate its position by scanning `code`.
                // Byte offsets of `code` within source are recoverable
                // because pulldown-cmark preserves block ranges; but
                // td-parse doesn't expose that directly. Fall back to a
                // search for each `export type` inside the block's span.
                let body_search = src.get(block_start..node.span.end).unwrap_or("");
                scan_td_body(body_search, block_start, &mut flat);
                let _ = code;
            }
        }
    }

    flat.sort_by_key(|t| t.start);
    encode(state, flat)
}

struct FlatToken {
    start: usize,
    len: usize,
    token_type: u32,
}

fn scan_typedown_line(src: &str, fm_start: usize, fm_end: usize, out: &mut Vec<FlatToken>) {
    let slice = match src.get(fm_start..fm_end) {
        Some(s) => s,
        None => return,
    };
    let mut cursor = fm_start;
    for line in slice.split_inclusive('\n') {
        let line_no_nl = line.strip_suffix('\n').unwrap_or(line);
        let trimmed = line_no_nl.trim_start();
        if trimmed.starts_with("typedown:") {
            let abs = cursor;
            scan_type_expr(line_no_nl, abs, out);
            return;
        }
        cursor += line.len();
    }
}

/// Tokenize a type expression line into identifiers and string literals.
fn scan_type_expr(line: &str, abs_start: usize, out: &mut Vec<FlatToken>) {
    let bytes = line.as_bytes();
    let prefix_end = match line.find("typedown:") {
        Some(p) => p + "typedown:".len(),
        None => return,
    };
    let mut i = prefix_end;
    while i < line.len() {
        let c = bytes[i];
        if c.is_ascii_alphabetic() || c == b'_' {
            let start = i;
            while i < line.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            let tok_len = i - start;
            let name = &line[start..i];
            let tok = if EffectKind::from_name(name).is_some() {
                T_KEYWORD
            } else {
                T_TYPE
            };
            out.push(FlatToken {
                start: abs_start + start,
                len: tok_len,
                token_type: tok,
            });
            continue;
        }
        if c == b'"' {
            let q_start = i;
            i += 1;
            while i < line.len() && bytes[i] != b'"' {
                i += 1;
            }
            // Include closing quote if present
            let end = if i < line.len() { i + 1 } else { i };
            out.push(FlatToken {
                start: abs_start + q_start,
                len: end - q_start,
                token_type: T_STRING,
            });
            i = end;
            continue;
        }
        i += 1;
    }
}

fn find_infostring(src: &str, block_start: usize, lang: &str) -> Option<usize> {
    // Block starts with ```lang or ~~~lang followed by a newline.
    let slice = src.get(block_start..)?;
    let marker = slice.find("```").unwrap_or(0);
    let after = &slice[marker + 3..];
    let trimmed = after.trim_start_matches(|c: char| c == ' ' || c == '\t');
    if trimmed.starts_with(lang) {
        let lead = after.len() - trimmed.len();
        return Some(block_start + marker + 3 + lead);
    }
    None
}

fn scan_td_body(src: &str, base_off: usize, out: &mut Vec<FlatToken>) {
    for (off_in_src, line) in line_offsets(src) {
        let trimmed = line.trim_start();
        let lead = line.len() - trimmed.len();
        let (prefix, kind) = if let Some(r) = trimmed.strip_prefix("export type ") {
            ("export type ", Some((r, T_TYPE)))
        } else if let Some(r) = trimmed.strip_prefix("export interface ") {
            ("export interface ", Some((r, T_TYPE)))
        } else if let Some(r) = trimmed.strip_prefix("type ") {
            ("type ", Some((r, T_TYPE)))
        } else if let Some(r) = trimmed.strip_prefix("interface ") {
            ("interface ", Some((r, T_TYPE)))
        } else {
            ("", None)
        };
        let Some((rest, tok_type)) = kind else { continue };
        let name_end = rest
            .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
            .unwrap_or(rest.len());
        if name_end == 0 {
            continue;
        }
        let ident_start = base_off + off_in_src + lead + prefix.len();
        out.push(FlatToken {
            start: ident_start,
            len: name_end,
            token_type: tok_type,
        });
    }
}

fn line_offsets(src: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut off = 0usize;
    src.split_inclusive('\n').map(move |chunk| {
        let start = off;
        off += chunk.len();
        let line = chunk.strip_suffix('\n').unwrap_or(chunk);
        let line = line.strip_suffix('\r').unwrap_or(line);
        (start, line)
    })
}

fn encode(state: &DocState, tokens: Vec<FlatToken>) -> SemanticTokens {
    let mut data: Vec<SemanticToken> = Vec::with_capacity(tokens.len());
    let mut prev_line: u32 = 0;
    let mut prev_start: u32 = 0;
    for t in tokens {
        let pos = state.line_index.position(t.start);
        // Length in UTF-16 units of the region [t.start, t.start + t.len).
        let end_pos = state.line_index.position(t.start + t.len);
        let length = if pos.line == end_pos.line {
            end_pos.character - pos.character
        } else {
            // Token crosses a newline — clamp to end of start line.
            // For our tokens this shouldn't happen, but be defensive.
            0
        };
        let (delta_line, delta_start) = if pos.line == prev_line {
            (0u32, pos.character - prev_start)
        } else {
            (pos.line - prev_line, pos.character)
        };
        data.push(SemanticToken {
            delta_line,
            delta_start,
            length,
            token_type: t.token_type,
            token_modifiers_bitset: 0,
        });
        prev_line = pos.line;
        prev_start = pos.character;
    }
    SemanticTokens {
        result_id: None,
        data,
    }
}
