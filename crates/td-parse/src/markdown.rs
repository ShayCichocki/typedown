//! Markdown → [`MdDoc`].
//!
//! Strategy: run `pulldown-cmark` with offsets and fold events into our
//! simplified AST. We treat `---\n...\n---` at the very top as YAML
//! frontmatter and strip it before handing the rest to the parser.

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use td_ast::md::{Frontmatter, ListItem, MdDoc, MdNode, MdNodeKind, TaskItem};
use td_core::{Diagnostics, Span};

/// Parse a whole markdown document.
///
/// `source_offset` is added to every span so that code-fence re-parsers can
/// preserve absolute byte offsets relative to the original file.
pub fn parse_markdown(input: &str) -> (MdDoc, Diagnostics) {
    let diagnostics = Diagnostics::new();
    let (frontmatter, body, body_offset) = split_frontmatter(input);

    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_TASKLISTS);
    options.insert(Options::ENABLE_STRIKETHROUGH);

    let parser = Parser::new_ext(body, options).into_offset_iter();

    let mut nodes: Vec<MdNode> = Vec::new();
    let mut stack: Vec<Frame> = Vec::new();

    for (event, range) in parser {
        let span = Span::new(body_offset + range.start, body_offset + range.end);
        match event {
            Event::Start(tag) => stack.push(Frame::new(tag, span)),
            Event::End(end) => {
                if let Some(frame) = stack.pop() {
                    // Item frames don't produce their own node; they
                    // contribute a ListItem/TaskItem to the enclosing List.
                    if frame.is_list_item {
                        let item_span = frame.span.join(span);
                        let text = frame.text.trim().to_string();
                        if let Some(list) = stack.last_mut() {
                            if matches!(list.tag, Tag::List(_)) {
                                match frame.task_marker {
                                    Some(checked) => list.task_items.push(TaskItem {
                                        span: item_span,
                                        checked,
                                        text,
                                    }),
                                    None => list.list_items.push(ListItem {
                                        span: item_span,
                                        text,
                                    }),
                                }
                            }
                        }
                    } else if is_inline_tag(&frame.tag) {
                        // Inline constructs (emphasis, strong, link, image,
                        // inline-code already handled elsewhere) don't produce
                        // their own block node — their text joins the parent.
                        if let Some(parent) = stack.last_mut() {
                            parent.text.push_str(&frame.text);
                        }
                    } else if let Some(node) = frame.finish(end, span) {
                        push_node(&mut nodes, &mut stack, node);
                    }
                }
            }
            Event::Text(t) => {
                if let Some(frame) = stack.last_mut() {
                    frame.text.push_str(&t);
                }
            }
            Event::Code(t) => {
                if let Some(frame) = stack.last_mut() {
                    frame.text.push('`');
                    frame.text.push_str(&t);
                    frame.text.push('`');
                }
            }
            Event::SoftBreak => {
                if let Some(frame) = stack.last_mut() {
                    frame.text.push(' ');
                }
            }
            Event::HardBreak => {
                if let Some(frame) = stack.last_mut() {
                    frame.text.push('\n');
                }
            }
            Event::Rule => {
                nodes.push(MdNode {
                    span,
                    kind: MdNodeKind::ThematicBreak,
                });
            }
            Event::TaskListMarker(checked) => {
                if let Some(frame) = stack.last_mut() {
                    frame.task_marker = Some(checked);
                }
            }
            _ => {}
        }
    }

    (
        MdDoc {
            frontmatter,
            nodes,
        },
        diagnostics,
    )
}

/// Separate optional YAML frontmatter from the markdown body.
///
/// Returns `(frontmatter, body, body_offset_in_original)`.
fn split_frontmatter(input: &str) -> (Option<Frontmatter>, &str, usize) {
    // Must start with exactly `---\n` (or `---\r\n`) at byte 0.
    let rest = if let Some(r) = input.strip_prefix("---\n") {
        r
    } else if let Some(r) = input.strip_prefix("---\r\n") {
        r
    } else {
        return (None, input, 0);
    };

    // Find the closing fence on its own line.
    let mut search_from = 0;
    while let Some(nl) = rest[search_from..].find('\n') {
        let line_start = search_from;
        let line_end = search_from + nl;
        let line = rest[line_start..line_end].trim_end_matches('\r');
        if line == "---" {
            let raw = rest[..line_start].trim_end_matches('\n').to_string();
            let fm_end = input.len() - rest.len() + line_end + 1; // +1 skip newline
            return (
                Some(Frontmatter {
                    span: Span::new(0, fm_end),
                    raw,
                }),
                &input[fm_end..],
                fm_end,
            );
        }
        search_from = line_end + 1;
    }

    // Unterminated — treat whole input as body, no frontmatter.
    (None, input, 0)
}

/// In-flight block we're collecting into.
struct Frame {
    tag: Tag<'static>,
    span: Span,
    text: String,
    lang: Option<String>,
    list_items: Vec<ListItem>,
    task_items: Vec<TaskItem>,
    /// Heading depth if we're inside one.
    heading_level: Option<u8>,
    /// `true` = ordered, `false` = unordered, `None` = not a list frame.
    list_ordered: Option<bool>,
    /// `Some(checked)` if this list item started with a task marker.
    task_marker: Option<bool>,
    /// Whether this frame is a list-item frame.
    is_list_item: bool,
}

impl Frame {
    fn new(tag: Tag<'_>, span: Span) -> Self {
        let (lang, heading_level, list_ordered, is_list_item) = match &tag {
            Tag::CodeBlock(CodeBlockKind::Fenced(info)) => {
                let tag = info.split_whitespace().next().unwrap_or("").to_string();
                (
                    (!tag.is_empty()).then_some(tag),
                    None,
                    None,
                    false,
                )
            }
            Tag::CodeBlock(CodeBlockKind::Indented) => (None, None, None, false),
            Tag::Heading { level, .. } => (None, Some(heading_to_u8(*level)), None, false),
            Tag::List(start) => (None, None, Some(start.is_some()), false),
            Tag::Item => (None, None, None, true),
            _ => (None, None, None, false),
        };

        Frame {
            tag: tag.into_static(),
            span,
            text: String::new(),
            lang,
            list_items: Vec::new(),
            task_items: Vec::new(),
            heading_level,
            list_ordered,
            task_marker: None,
            is_list_item,
        }
    }

    /// Convert an in-flight frame into a complete `MdNode`, if this frame
    /// corresponds to a block-level construct we model.
    fn finish(self, _end: TagEnd, end_span: Span) -> Option<MdNode> {
        let span = self.span.join(end_span);
        match self.tag {
            Tag::Heading { .. } => {
                let level = self.heading_level.unwrap_or(1);
                let text = self.text.trim().to_string();
                let slug = slugify(&text);
                Some(MdNode {
                    span,
                    kind: MdNodeKind::Heading { level, text, slug },
                })
            }
            Tag::Paragraph => Some(MdNode {
                span,
                kind: MdNodeKind::Paragraph {
                    text: self.text.trim().to_string(),
                },
            }),
            Tag::CodeBlock(_) => Some(MdNode {
                span,
                kind: MdNodeKind::CodeBlock {
                    lang: self.lang,
                    code: self.text,
                },
            }),
            Tag::BlockQuote(_) => Some(MdNode {
                span,
                kind: MdNodeKind::BlockQuote {
                    text: self.text.trim().to_string(),
                },
            }),
            Tag::List(_) => {
                if !self.task_items.is_empty() {
                    Some(MdNode {
                        span,
                        kind: MdNodeKind::TaskList {
                            items: self.task_items,
                        },
                    })
                } else if self.list_ordered == Some(true) {
                    Some(MdNode {
                        span,
                        kind: MdNodeKind::OrderedList {
                            items: self.list_items,
                        },
                    })
                } else {
                    Some(MdNode {
                        span,
                        kind: MdNodeKind::UnorderedList {
                            items: self.list_items,
                        },
                    })
                }
            }
            Tag::Item => None, // handled by parent list via `push_node`
            _ => None,
        }
    }
}

// (Tag::into_static is provided by pulldown-cmark 0.12+)

/// Tags that describe inline runs (emphasis, strong, link, etc.) rather than
/// block-level structure. For these we don't produce our own node — their
/// text is absorbed into whichever paragraph/heading/item is their parent.
fn is_inline_tag(tag: &Tag<'static>) -> bool {
    matches!(
        tag,
        Tag::Emphasis
            | Tag::Strong
            | Tag::Strikethrough
            | Tag::Link { .. }
            | Tag::Image { .. }
    )
}

fn push_node(nodes: &mut Vec<MdNode>, stack: &mut [Frame], node: MdNode) {
    // For nodes finishing inside a list-item frame (i.e. loose lists where
    // pulldown wraps content in `<p>`), fold their text into the item's
    // accumulator so the final ListItem has one combined text body.
    if let Some(parent) = stack.last_mut() {
        if parent.is_list_item {
            if let MdNodeKind::Paragraph { text } = &node.kind {
                if !parent.text.is_empty() {
                    parent.text.push('\n');
                }
                parent.text.push_str(text);
            }
            return;
        }
    }
    nodes.push(node);
}

fn heading_to_u8(h: HeadingLevel) -> u8 {
    match h {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

/// Deterministic GitHub-ish slug for a heading.
fn slugify(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_was_dash = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            last_was_dash = false;
        } else if c.is_whitespace() || c == '-' || c == '_' {
            if !last_was_dash && !out.is_empty() {
                out.push('-');
                last_was_dash = true;
            }
        }
    }
    out.trim_end_matches('-').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_frontmatter() {
        let src = "---\ntypedown: Prompt\n---\n# Hi\n";
        let (fm, body, off) = split_frontmatter(src);
        assert!(fm.is_some());
        assert_eq!(fm.unwrap().raw, "typedown: Prompt");
        assert!(body.starts_with("# Hi"));
        assert_eq!(off, "---\ntypedown: Prompt\n---\n".len());
    }

    #[test]
    fn parses_heading_and_paragraph() {
        let (doc, diags) = parse_markdown("# Title\n\nhello world\n");
        assert!(diags.is_empty());
        assert_eq!(doc.nodes.len(), 2);
        matches!(doc.nodes[0].kind, MdNodeKind::Heading { level: 1, .. });
        matches!(doc.nodes[1].kind, MdNodeKind::Paragraph { .. });
    }

    #[test]
    fn parses_ordered_list() {
        let (doc, _) = parse_markdown("1. alpha\n2. beta\n");
        assert_eq!(doc.nodes.len(), 1);
        match &doc.nodes[0].kind {
            MdNodeKind::OrderedList { items } => assert_eq!(items.len(), 2),
            other => panic!("expected OrderedList, got {other:?}"),
        }
    }

    #[test]
    fn parses_code_fence_lang() {
        let (doc, _) = parse_markdown("```rust\nfn main(){}\n```\n");
        match &doc.nodes[0].kind {
            MdNodeKind::CodeBlock { lang, .. } => {
                assert_eq!(lang.as_deref(), Some("rust"));
            }
            other => panic!("expected code block: {other:?}"),
        }
    }
}
