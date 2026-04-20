//! "Token at position" resolution.
//!
//! Given a cursor byte offset and a doc's parsed state, figure out what
//! the user is pointing at: a type identifier in frontmatter, an effect
//! row keyword, a field name inside a typed example value block, or
//! nothing interesting.
//!
//! This is intentionally a small, regex-light walker over the parsed
//! `MdDoc` + frontmatter `typedown:` line — we do *not* re-parse the
//! whole doc. Hover/goto/completion all share this so they agree on
//! what a "token" means.

use td_ast::md::{Frontmatter, MdDoc};
use td_core::Span;

/// The classification of what's under the cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenKind {
    /// A type identifier inside `typedown:` frontmatter. Span covers the
    /// identifier's exact extent. String is the bare name (e.g. "Prompt").
    FrontmatterType { name: String, span: Span },
    /// One of the effect-row keywords: `Uses`, `Reads`, `Writes`, `Model`,
    /// `MaxTokens`. Span covers the keyword.
    EffectKeyword { kind: EffectKind, span: Span },
    /// The cursor is inside a string-literal argument to an effect row,
    /// e.g. `"read_file"` inside `Uses<["read_file"]>`. The span covers
    /// the string contents (without the quotes).
    EffectStringArg { kind: EffectKind, value: String, span: Span },
    /// The cursor is after `Imports<"` and before the closing quote —
    /// i.e. the user is typing a stdlib module path.
    ImportModulePath { partial: String, span: Span },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectKind {
    Uses,
    Reads,
    Writes,
    Model,
    MaxTokens,
}

impl EffectKind {
    pub fn from_name(s: &str) -> Option<Self> {
        match s {
            "Uses" => Some(Self::Uses),
            "Reads" => Some(Self::Reads),
            "Writes" => Some(Self::Writes),
            "Model" => Some(Self::Model),
            "MaxTokens" => Some(Self::MaxTokens),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Uses => "Uses",
            Self::Reads => "Reads",
            Self::Writes => "Writes",
            Self::Model => "Model",
            Self::MaxTokens => "MaxTokens",
        }
    }

    pub fn policy_blurb(&self) -> &'static str {
        match self {
            Self::Uses => "Tools this prompt is authorized to invoke. `Uses<[]>` declares no-tools.",
            Self::Reads => "Resource patterns this prompt may read. Glob strings.",
            Self::Writes => "Resource patterns this prompt may write. `Writes<[]>` declares read-only.",
            Self::Model => "Models this prompt has been validated against. Tuple or union of string literals.",
            Self::MaxTokens => "Hard token ceiling honored by the runtime.",
        }
    }
}

/// Resolve the cursor to a token, if it falls on one we know how to handle.
pub fn token_at(doc: &MdDoc, source: &str, offset: usize) -> Option<TokenKind> {
    let Some(fm) = &doc.frontmatter else {
        return None;
    };
    // Only frontmatter tokens are meaningful for v1. The frontmatter text
    // is `source[fm.span.start..fm.span.end]`, plus the `typedown:` line
    // lives inside that region.
    if offset < fm.span.start || offset >= fm.span.end {
        return None;
    }
    let (td_line_off, td_line) = typedown_line(fm, source)?;
    let abs_start = td_line_off;
    let abs_end = td_line_off + td_line.len();
    if offset < abs_start || offset > abs_end {
        return None;
    }
    let rel = offset - abs_start;
    classify_in_td_line(td_line, rel).map(|t| shift_span(t, abs_start))
}

/// Find the `typedown:` line inside the frontmatter. Returns (absolute
/// byte offset, line text without the trailing newline).
fn typedown_line<'a>(fm: &Frontmatter, source: &'a str) -> Option<(usize, &'a str)> {
    let fm_text: &str = &fm.raw;
    // `fm.raw` is the frontmatter *contents* between the fences — but
    // we need absolute offsets. Re-scan the source for the same line
    // inside fm.span.
    let slice = &source.get(fm.span.start..fm.span.end)?;
    let mut cursor = fm.span.start;
    for line in slice.split_inclusive('\n') {
        let line_no_nl = line.strip_suffix('\n').unwrap_or(line);
        let trimmed = line_no_nl.trim_start();
        if trimmed.starts_with("typedown:") {
            // We want the offset of the raw line (not the trim). cursor
            // is at the start of `line` in source coords.
            // But we only care about abs offset of the line's first char.
            let abs = cursor;
            let _ = fm_text;
            return Some((abs, line_no_nl));
        }
        cursor += line.len();
    }
    None
}

/// Given a `typedown:` line's text and a relative byte offset, classify.
fn classify_in_td_line(line: &str, rel: usize) -> Option<TokenKind> {
    // Walk tokens: strip "typedown:" prefix, then split at identifiers /
    // string literals / angle brackets / ampersands / commas.
    let prefix_end = line.find("typedown:").map(|i| i + "typedown:".len())?;
    if rel < prefix_end {
        return None;
    }
    // Within the type expression.
    let bytes = line.as_bytes();
    // Find the token that contains `rel`.
    let mut i = prefix_end;
    while i < line.len() {
        let c = bytes[i];
        if c.is_ascii_whitespace() || c == b'<' || c == b'>' || c == b'&' || c == b',' || c == b'[' || c == b']' || c == b'|' {
            i += 1;
            continue;
        }
        if c == b'"' {
            // String literal.
            let start = i + 1;
            let mut end = start;
            while end < line.len() && bytes[end] != b'"' {
                end += 1;
            }
            if rel >= start && rel <= end {
                // Need to know which effect-row we're inside. Find the
                // nearest preceding `Name<` that opens an effect row.
                let kind = containing_effect(line, i);
                if let Some(k) = kind {
                    return Some(TokenKind::EffectStringArg {
                        kind: k,
                        value: line[start..end].to_string(),
                        span: Span::new(start, end),
                    });
                }
                // Not inside an effect row — ignore.
                return None;
            }
            i = end + 1;
            continue;
        }
        if c.is_ascii_alphabetic() || c == b'_' {
            let start = i;
            let mut end = i + 1;
            while end < line.len()
                && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_')
            {
                end += 1;
            }
            if rel >= start && rel < end {
                let name = &line[start..end];
                if let Some(kind) = EffectKind::from_name(name) {
                    return Some(TokenKind::EffectKeyword {
                        kind,
                        span: Span::new(start, end),
                    });
                }
                return Some(TokenKind::FrontmatterType {
                    name: name.to_string(),
                    span: Span::new(start, end),
                });
            }
            i = end;
            continue;
        }
        i += 1;
    }
    None
}

/// Which `Name<…>` wrapper encloses the byte offset `quote_pos` inside `line`?
fn containing_effect(line: &str, quote_pos: usize) -> Option<EffectKind> {
    // Scan backwards for the most recent `<` that opens an unclosed bracket.
    let bytes = line.as_bytes();
    let mut depth: i32 = 0;
    let mut i = quote_pos;
    while i > 0 {
        i -= 1;
        match bytes[i] {
            b'>' => depth += 1,
            b'<' => {
                if depth == 0 {
                    // Found opening bracket. Walk back over identifier.
                    let end = i;
                    let start = {
                        let mut s = end;
                        while s > 0 {
                            let b = bytes[s - 1];
                            if b.is_ascii_alphanumeric() || b == b'_' {
                                s -= 1;
                            } else {
                                break;
                            }
                        }
                        s
                    };
                    let name = std::str::from_utf8(&bytes[start..end]).ok()?;
                    let _ = end;
                    return EffectKind::from_name(name);
                }
                depth -= 1;
            }
            _ => {}
        }
    }
    None
}

fn shift_span(kind: TokenKind, base: usize) -> TokenKind {
    match kind {
        TokenKind::FrontmatterType { name, span } => TokenKind::FrontmatterType {
            name,
            span: Span::new(span.start + base, span.end + base),
        },
        TokenKind::EffectKeyword { kind, span } => TokenKind::EffectKeyword {
            kind,
            span: Span::new(span.start + base, span.end + base),
        },
        TokenKind::EffectStringArg { kind, value, span } => TokenKind::EffectStringArg {
            kind,
            value,
            span: Span::new(span.start + base, span.end + base),
        },
        TokenKind::ImportModulePath { partial, span } => TokenKind::ImportModulePath {
            partial,
            span: Span::new(span.start + base, span.end + base),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use td_parse::parse_markdown;

    fn mkdoc(src: &str) -> MdDoc {
        parse_markdown(src).0
    }

    #[test]
    fn hover_on_prompt_keyword() {
        let src = "---\ntypedown: Prompt<In, Out>\n---\n\n# Hi\n";
        let doc = mkdoc(src);
        // Offset points at the `P` of `Prompt`
        let off = src.find("Prompt").unwrap() + 1;
        let tok = token_at(&doc, src, off).expect("token");
        match tok {
            TokenKind::FrontmatterType { name, .. } => assert_eq!(name, "Prompt"),
            other => panic!("wrong kind: {:?}", other),
        }
    }

    #[test]
    fn hover_on_uses_keyword() {
        let src = "---\ntypedown: Prompt<In, Out> & Uses<[]>\n---\n\n# Hi\n";
        let doc = mkdoc(src);
        let off = src.find("Uses").unwrap() + 1;
        let tok = token_at(&doc, src, off).expect("token");
        match tok {
            TokenKind::EffectKeyword { kind, .. } => assert_eq!(kind, EffectKind::Uses),
            other => panic!("wrong kind: {:?}", other),
        }
    }

    #[test]
    fn hover_on_string_inside_uses() {
        let src = "---\ntypedown: Prompt<A,B> & Uses<[\"read_file\"]>\n---\n\n# Hi\n";
        let doc = mkdoc(src);
        let off = src.find("read_file").unwrap() + 2;
        let tok = token_at(&doc, src, off).expect("token");
        match tok {
            TokenKind::EffectStringArg { kind, value, .. } => {
                assert_eq!(kind, EffectKind::Uses);
                assert_eq!(value, "read_file");
            }
            other => panic!("wrong kind: {:?}", other),
        }
    }
}
