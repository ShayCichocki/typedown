//! Byte-offset ↔ LSP position math.
//!
//! `td-core::Span` is byte-offset-based. LSP `Position` is
//! (line, UTF-16 column). This module owns the conversion so every handler
//! can assume clean, tested math.

use td_core::Span;
use tower_lsp::lsp_types::{Position, Range};

/// Precomputed line starts for a source text.
#[derive(Debug, Clone)]
pub struct LineIndex {
    /// Byte offset where each line begins. `line_starts[0] == 0` always.
    /// Has `nlines + 1` entries if you count the implicit past-end offset,
    /// but we store `nlines` and treat offsets >= text.len() as the last line.
    line_starts: Vec<usize>,
    /// Full source text, needed to count UTF-16 units on a given line.
    text: String,
}

impl LineIndex {
    pub fn new(text: impl Into<String>) -> Self {
        let text = text.into();
        let mut line_starts = vec![0usize];
        // `\r\n` is one logical line break. `memchr` would be faster but
        // this runs once per didChange — fine.
        let bytes = text.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            match bytes[i] {
                b'\n' => {
                    line_starts.push(i + 1);
                    i += 1;
                }
                b'\r' => {
                    // CRLF counts as one line terminator.
                    if i + 1 < bytes.len() && bytes[i + 1] == b'\n' {
                        line_starts.push(i + 2);
                        i += 2;
                    } else {
                        line_starts.push(i + 1);
                        i += 1;
                    }
                }
                _ => i += 1,
            }
        }
        Self { line_starts, text }
    }

    /// Convert a byte offset to an LSP `Position` (UTF-16 column).
    pub fn position(&self, byte_offset: usize) -> Position {
        let offset = byte_offset.min(self.text.len());
        let line = self
            .line_starts
            .binary_search(&offset)
            .unwrap_or_else(|ins| ins.saturating_sub(1));
        let line_start = self.line_starts[line];
        let col_bytes = &self.text.as_bytes()[line_start..offset];
        let col_str = std::str::from_utf8(col_bytes).unwrap_or("");
        let col_utf16: u32 = col_str
            .chars()
            .map(|c| c.len_utf16() as u32)
            .sum();
        Position {
            line: line as u32,
            character: col_utf16,
        }
    }

    /// Convert an LSP `Position` back to a byte offset. Clamps to text length.
    pub fn offset(&self, pos: Position) -> usize {
        let line = (pos.line as usize).min(self.line_starts.len().saturating_sub(1));
        let line_start = self.line_starts[line];
        let line_end = self
            .line_starts
            .get(line + 1)
            .copied()
            .unwrap_or_else(|| self.text.len());
        let line_bytes = &self.text.as_bytes()[line_start..line_end];
        let line_str = std::str::from_utf8(line_bytes).unwrap_or("");
        let mut utf16_seen: u32 = 0;
        let target = pos.character;
        for (byte_idx, ch) in line_str.char_indices() {
            if utf16_seen >= target {
                return line_start + byte_idx;
            }
            utf16_seen += ch.len_utf16() as u32;
        }
        line_end
    }

    /// Convert a `Span` to an LSP `Range`.
    pub fn range(&self, span: Span) -> Range {
        Range {
            start: self.position(span.start),
            end: self.position(span.end),
        }
    }

    /// Total text length in bytes. Useful for bounds-checking in handlers.
    pub fn text_len(&self) -> usize {
        self.text.len()
    }

    /// Access the underlying source text.
    pub fn text(&self) -> &str {
        &self.text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_roundtrip() {
        let li = LineIndex::new("hello\nworld");
        let p = li.position(6);
        assert_eq!(p, Position { line: 1, character: 0 });
        assert_eq!(li.offset(p), 6);
        let p = li.position(11);
        assert_eq!(p, Position { line: 1, character: 5 });
    }

    #[test]
    fn crlf_is_one_break() {
        let li = LineIndex::new("a\r\nb");
        let p = li.position(3);
        assert_eq!(p, Position { line: 1, character: 0 });
        let p = li.position(4);
        assert_eq!(p, Position { line: 1, character: 1 });
    }

    #[test]
    fn multibyte_utf8_utf16_column() {
        // "é" is 2 bytes UTF-8, 1 UTF-16 unit
        let li = LineIndex::new("é-x");
        let p_dash = li.position(2);
        assert_eq!(p_dash, Position { line: 0, character: 1 });
        let back = li.offset(p_dash);
        assert_eq!(back, 2);

        // Emoji (outside BMP) is 4 bytes UTF-8, 2 UTF-16 units (surrogate pair)
        let li = LineIndex::new("😀z");
        let p_z = li.position(4);
        assert_eq!(p_z, Position { line: 0, character: 2 });
        assert_eq!(li.offset(p_z), 4);
    }

    #[test]
    fn out_of_bounds_clamps() {
        let li = LineIndex::new("a\nb\n");
        let p = li.position(999);
        // past-end clamps to end of text
        assert_eq!(p.line, 2);
        let back = li.offset(Position { line: 99, character: 99 });
        assert_eq!(back, li.text_len());
    }

    #[test]
    fn span_to_range() {
        let li = LineIndex::new("abc\ndef");
        let r = li.range(Span::new(4, 7));
        assert_eq!(r.start, Position { line: 1, character: 0 });
        assert_eq!(r.end, Position { line: 1, character: 3 });
    }
}
