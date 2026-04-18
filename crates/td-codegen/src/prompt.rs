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
//!
//! For pipeline documents, [`pipeline_step_prompts`] does a second
//! pass that buckets the body by `##` heading and returns a per-step
//! system prompt mapping.

use std::collections::HashMap;

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

/// Bucket a pipeline document's markdown body into per-step system
/// prompts.
///
/// For each step name (type-alias identifier like `Classify` /
/// `Answer`), we find the first level-2 heading whose text contains
/// the step name (case-insensitive substring) and take everything
/// from that heading up to — but not including — the next level-2
/// heading as that step's system prompt.
///
/// Step names are tried **longest-first** so that when both `A` and
/// `AB` are step aliases, a heading `## AB` matches `AB` rather than
/// `A`. Steps with no matching heading get an empty string; the
/// caller emits a comment noting the unmatched step so the author
/// can fix the doc.
///
/// `td` fence content and the frontmatter are stripped from every
/// returned string so the per-step prompt is safe to splice into a
/// TypeScript template literal without leaking type declarations.
pub fn pipeline_step_prompts(
    file: &SourceFile,
    doc: &MdDoc,
    step_names: &[String],
) -> HashMap<String, String> {
    let content = &file.content;
    let body_start = frontmatter_end(content);
    let td_spans = collect_td_spans(doc);

    // Gather every level-2 heading with its byte start. We use the
    // heading node's `span.start` as the range boundary so the
    // rendered prompt *includes* the heading line — that gives the
    // model context ("you are the step labeled X").
    let headings: Vec<(usize, String)> = doc
        .nodes
        .iter()
        .filter_map(|n| match &n.kind {
            MdNodeKind::Heading {
                level: 2,
                text,
                ..
            } => Some((n.span.start, text.clone())),
            _ => None,
        })
        .collect();

    let mut out = HashMap::new();

    // Sort step names by length descending so multi-word names win
    // over prefixes of themselves.
    let mut ordered = step_names.to_vec();
    ordered.sort_by_key(|n| std::cmp::Reverse(n.len()));

    // Track which heading indices have already been claimed so the
    // same section isn't attributed to two steps.
    let mut claimed = vec![false; headings.len()];

    for step_name in &ordered {
        let needle = step_name.to_ascii_lowercase();
        let matched = headings.iter().enumerate().find(|(i, (_, text))| {
            !claimed[*i] && text.to_ascii_lowercase().contains(&needle)
        });
        if let Some((idx, (start, _text))) = matched {
            claimed[idx] = true;
            // End of this step's range = start of next level-2 heading,
            // or end of file.
            let end = headings
                .get(idx + 1)
                .map(|(start, _)| *start)
                .unwrap_or(content.len());
            // Guard against the range preceding the body (shouldn't
            // happen, but defensive).
            let real_start = (*start).max(body_start);
            let text = slice_without_td_fences(content, real_start, end, &td_spans);
            out.insert(step_name.clone(), normalize_whitespace(&text));
        } else {
            out.insert(step_name.clone(), String::new());
        }
    }

    out
}

/// Take `content[start..end]` with any `td` fence ranges spliced out.
fn slice_without_td_fences(
    content: &str,
    start: usize,
    end: usize,
    td_spans: &[(usize, usize)],
) -> String {
    let mut out = String::with_capacity(end - start);
    let mut cursor = start;
    for (span_start, span_end) in td_spans {
        if *span_end <= cursor || *span_start >= end {
            continue;
        }
        let clamped_start = (*span_start).max(cursor);
        if clamped_start > cursor {
            out.push_str(&content[cursor..clamped_start]);
        }
        cursor = (*span_end).min(end);
    }
    if cursor < end {
        out.push_str(&content[cursor..end]);
    }
    out
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

    #[test]
    fn pipeline_step_prompts_bucket_by_heading() {
        let src = r#"---
typedown: Pipeline
---

# Overview

Pipeline docs have overview prose here.

## Step 1 — Classify

You are the classifier. Emit JSON matching Classification.

## Step 2 — Answer

Given the classification, produce an answer.
"#;
        let file = SourceFile::new("p.md", src.to_string());
        let (doc, _) = parse_markdown(&file.content);
        let prompts = pipeline_step_prompts(
            &file,
            &doc,
            &["Classify".to_string(), "Answer".to_string()],
        );
        assert!(prompts["Classify"].contains("You are the classifier"));
        assert!(!prompts["Classify"].contains("Given the classification"));
        assert!(prompts["Answer"].contains("Given the classification"));
        assert!(!prompts["Answer"].contains("You are the classifier"));
    }

    #[test]
    fn pipeline_step_prompts_longest_name_wins() {
        // Step name "Review" and "ReviewOutput" — a heading `## ReviewOutput`
        // should match ReviewOutput, not Review.
        let src = r#"---
typedown: Pipeline
---

## Review

Short-named step prose.

## ReviewOutput

Longer-named step prose.
"#;
        let file = SourceFile::new("p.md", src.to_string());
        let (doc, _) = parse_markdown(&file.content);
        let prompts = pipeline_step_prompts(
            &file,
            &doc,
            &["Review".to_string(), "ReviewOutput".to_string()],
        );
        assert!(prompts["Review"].contains("Short-named"));
        assert!(prompts["ReviewOutput"].contains("Longer-named"));
        assert!(!prompts["ReviewOutput"].contains("Short-named"));
    }

    #[test]
    fn pipeline_step_without_heading_gets_empty_prompt() {
        let src = r#"---
typedown: Pipeline
---

## Step 1 — Classify

Classifier only.
"#;
        let file = SourceFile::new("p.md", src.to_string());
        let (doc, _) = parse_markdown(&file.content);
        let prompts = pipeline_step_prompts(
            &file,
            &doc,
            &["Classify".to_string(), "Answer".to_string()],
        );
        assert!(!prompts["Classify"].is_empty());
        assert_eq!(prompts["Answer"], "");
    }

    #[test]
    fn pipeline_step_prompts_strip_td_fences_in_bucket() {
        let src = r#"---
typedown: Pipeline
---

## Classify

```td
type X = string
```

You are the classifier.
"#;
        let file = SourceFile::new("p.md", src.to_string());
        let (doc, _) = parse_markdown(&file.content);
        let prompts =
            pipeline_step_prompts(&file, &doc, &["Classify".to_string()]);
        assert!(prompts["Classify"].contains("You are the classifier"));
        assert!(!prompts["Classify"].contains("type X"), "got: {:?}", prompts["Classify"]);
    }
}
