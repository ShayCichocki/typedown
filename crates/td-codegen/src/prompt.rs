//! Extract the prose system prompt from a typed markdown document.
//!
//! The generated code needs the document's *body* (Role, Instructions,
//! etc.) as a TypeScript string literal — but without the frontmatter
//! YAML or the `td` fenced type declarations. Both of those are
//! metadata, not instructions to the model.
//!
//! Strategy: work on the raw file content.
//!
//! 1. Skip past the second `---` frontmatter delimiter (if present).
//! 2. Collect byte ranges of every ` ```td ` code block via
//!    [`MdDoc`] node spans.
//! 3. Emit the remaining content, splicing out those ranges.
//! 4. Trim trailing whitespace and collapse sequences of blank lines
//!    introduced by the splicing.

use td_ast::md::{MdDoc, MdNodeKind};
use td_core::SourceFile;

/// Produce the system-prompt string for codegen.
pub fn system_prompt(file: &SourceFile, doc: &MdDoc) -> String {
    let content = &file.content;
    let body_start = frontmatter_end(content);
    let td_spans = collect_td_spans(doc);

    let mut out = String::with_capacity(content.len());
    let mut cursor = body_start;
    for (start, end) in td_spans {
        // Guard against spans that precede the current cursor (shouldn't
        // happen, but defensive in case MdDoc ordering is not start-sorted).
        if start < cursor {
            continue;
        }
        out.push_str(&content[cursor..start]);
        cursor = end;
    }
    if cursor < content.len() {
        out.push_str(&content[cursor..]);
    }
    normalize_whitespace(&out)
}

/// Return the byte offset immediately after the closing `---` of a
/// YAML frontmatter block. If no frontmatter is present, returns 0.
///
/// Only the *first* pair of `---` at the very start of the file is
/// recognized as frontmatter; anything else is body content.
fn frontmatter_end(content: &str) -> usize {
    let trimmed = content.trim_start_matches(|c: char| c == '\u{FEFF}');
    if !trimmed.starts_with("---") {
        return 0;
    }
    // Find the closing `---` after the opening line.
    let bytes = content.as_bytes();
    // Skip the opening delimiter line.
    let mut i = 0;
    while i < bytes.len() && bytes[i] != b'\n' {
        i += 1;
    }
    if i >= bytes.len() {
        return 0;
    }
    i += 1; // past the newline

    // Scan line-by-line for a lone `---`.
    while i < bytes.len() {
        let line_start = i;
        while i < bytes.len() && bytes[i] != b'\n' {
            i += 1;
        }
        let line = content[line_start..i].trim_end_matches('\r');
        if line == "---" || line == "..." {
            // Advance past the newline and return.
            if i < bytes.len() {
                i += 1;
            }
            return i;
        }
        if i < bytes.len() {
            i += 1;
        }
    }
    // Malformed frontmatter (no closing delimiter). Leave the body alone.
    0
}

/// Collect `(start, end)` byte ranges of every ` ```td ` code block in
/// document order. Non-`td` fences are preserved in the output — users
/// often include `json` examples etc. that belong in the system prompt.
fn collect_td_spans(doc: &MdDoc) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    for node in &doc.nodes {
        if let MdNodeKind::CodeBlock { lang: Some(lang), .. } = &node.kind {
            if lang == "td" {
                spans.push((node.span.start, node.span.end));
            }
        }
    }
    spans.sort_by_key(|s| s.0);
    spans
}

/// Trim leading/trailing whitespace and collapse runs of 3+ consecutive
/// newlines to exactly 2 — splicing out td fences tends to leave behind
/// gaps that would otherwise render awkwardly in the system prompt.
fn normalize_whitespace(s: &str) -> String {
    let trimmed = s.trim();
    let mut out = String::with_capacity(trimmed.len());
    let mut newlines = 0;
    for c in trimmed.chars() {
        if c == '\n' {
            newlines += 1;
            if newlines <= 2 {
                out.push(c);
            }
        } else {
            newlines = 0;
            out.push(c);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use td_parse::parse_markdown;

    fn extract(src: &str) -> String {
        let file = SourceFile::new("t.md", src.to_string());
        let (doc, _) = parse_markdown(&file.content);
        system_prompt(&file, &doc)
    }

    #[test]
    fn strips_frontmatter() {
        let out = extract("---\ntypedown: Doc\n---\n\n# Title\n\nbody\n");
        assert_eq!(out, "# Title\n\nbody");
    }

    #[test]
    fn strips_td_fences() {
        let src = "---\ntypedown: Doc\n---\n\n# Title\n\nBefore.\n\n```td\ntype X = string\n```\n\nAfter.\n";
        let out = extract(src);
        assert!(out.contains("# Title"));
        assert!(out.contains("Before."));
        assert!(out.contains("After."));
        assert!(!out.contains("type X"), "got: {out}");
    }

    #[test]
    fn preserves_non_td_fences() {
        let src = r#"---
typedown: Doc
---

# Hi

```json
{ "x": 1 }
```
"#;
        let out = extract(src);
        assert!(out.contains("```json"), "got: {out}");
        assert!(out.contains("\"x\": 1"));
    }

    #[test]
    fn no_frontmatter_keeps_everything() {
        let out = extract("# Just a title\n\nhello\n");
        assert_eq!(out, "# Just a title\n\nhello");
    }

    #[test]
    fn collapses_blank_runs_from_splicing() {
        let src = "---\ntypedown: Doc\n---\n\n# A\n\n```td\ntype T = string\n```\n\n\n\n\n## B\n";
        let out = extract(src);
        // Never more than one blank line between sections after stripping.
        assert!(!out.contains("\n\n\n"), "got: {out:?}");
    }
}
