//! Simplified markdown AST.
//!
//! We intentionally do NOT try to represent every CommonMark construct. The
//! checker only needs enough shape to answer questions like "is the body of
//! this heading an ordered list?" or "what's the language tag of this code
//! fence?". Everything else gets flattened into `Paragraph { inline }` blobs
//! so the AST stays small and the match-trees readable.

use serde::Serialize;
use td_core::Span;

/// A parsed markdown document, frontmatter-aware.
#[derive(Debug, Clone, Serialize)]
pub struct MdDoc {
    /// Raw frontmatter text, if any. Semantics (`typedown: ...`) are resolved
    /// later by the checker, not the parser.
    pub frontmatter: Option<Frontmatter>,
    /// Flat list of top-level block nodes in source order.
    pub nodes: Vec<MdNode>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Frontmatter {
    pub span: Span,
    pub raw: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct MdNode {
    pub span: Span,
    pub kind: MdNodeKind,
}

/// Block-level markdown constructs we model explicitly.
#[derive(Debug, Clone, Serialize)]
pub enum MdNodeKind {
    /// `# ... ######` — depth is 1-6.
    Heading {
        level: u8,
        text: String,
        /// Slugified anchor id (`## Foo Bar` → `foo-bar`).
        slug: String,
    },
    Paragraph {
        text: String,
    },
    CodeBlock {
        /// Info string after the opening fence (e.g. `rust`, `td`, `json`).
        lang: Option<String>,
        code: String,
    },
    OrderedList {
        items: Vec<ListItem>,
    },
    UnorderedList {
        items: Vec<ListItem>,
    },
    TaskList {
        items: Vec<TaskItem>,
    },
    BlockQuote {
        text: String,
    },
    ThematicBreak,
    /// Anything we don't specifically model falls here; checkers should treat
    /// it as opaque content.
    Other {
        raw: String,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct ListItem {
    pub span: Span,
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TaskItem {
    pub span: Span,
    pub checked: bool,
    pub text: String,
}

impl MdDoc {
    /// Iterate over all headings in document order.
    pub fn headings(&self) -> impl Iterator<Item = (&MdNode, u8, &str, &str)> {
        self.nodes.iter().filter_map(|n| match &n.kind {
            MdNodeKind::Heading { level, text, slug } => {
                Some((n, *level, text.as_str(), slug.as_str()))
            }
            _ => None,
        })
    }

    /// All top-level nodes belonging to the section rooted at heading index
    /// `idx` — i.e. everything until the next heading of equal-or-shallower
    /// level. Returns an empty slice if `idx` is out of range or not a heading.
    pub fn section_body(&self, idx: usize) -> &[MdNode] {
        let Some(start) = self.nodes.get(idx) else {
            return &[];
        };
        let MdNodeKind::Heading { level, .. } = start.kind else {
            return &[];
        };
        let body_start = idx + 1;
        let body_end = self.nodes[body_start..]
            .iter()
            .position(|n| matches!(n.kind, MdNodeKind::Heading { level: l, .. } if l <= level))
            .map(|p| body_start + p)
            .unwrap_or(self.nodes.len());
        &self.nodes[body_start..body_end]
    }
}
