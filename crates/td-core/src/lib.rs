//! Shared primitives for typedown: spans, source files, diagnostics.
//!
//! This crate is the narrow waist every other crate depends on. Keep it tiny
//! and dependency-light.

use std::{ops::Range, path::PathBuf, sync::Arc};

use miette::{Diagnostic, NamedSource, SourceSpan};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// A byte range into the source file, inclusive start, exclusive end.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub const DUMMY: Span = Span { start: 0, end: 0 };

    pub fn new(start: usize, end: usize) -> Self {
        debug_assert!(start <= end, "Span start must be <= end");
        Self { start, end }
    }

    pub fn from_range(r: Range<usize>) -> Self {
        Self::new(r.start, r.end)
    }

    pub fn len(&self) -> usize {
        self.end - self.start
    }

    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }

    /// Merge two spans into the smallest span covering both.
    pub fn join(self, other: Span) -> Span {
        Span::new(self.start.min(other.start), self.end.max(other.end))
    }
}

impl From<Span> for SourceSpan {
    fn from(s: Span) -> Self {
        SourceSpan::new(s.start.into(), s.len())
    }
}

impl From<Range<usize>> for Span {
    fn from(r: Range<usize>) -> Self {
        Span::from_range(r)
    }
}

/// A source file backing one or more diagnostics.
///
/// Cloning a `SourceFile` is cheap (Arc-shared content).
#[derive(Debug, Clone)]
pub struct SourceFile {
    pub path: PathBuf,
    pub content: Arc<str>,
}

impl SourceFile {
    pub fn new(path: impl Into<PathBuf>, content: impl Into<Arc<str>>) -> Self {
        Self {
            path: path.into(),
            content: content.into(),
        }
    }

    pub fn display_name(&self) -> String {
        self.path.display().to_string()
    }

    pub fn named_source(&self) -> NamedSource<String> {
        NamedSource::new(self.display_name(), self.content.to_string())
    }
}

/// Severity of a typedown diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Severity {
    Error,
    Warning,
    Info,
    Hint,
}

impl From<Severity> for miette::Severity {
    fn from(s: Severity) -> Self {
        match s {
            Severity::Error => miette::Severity::Error,
            Severity::Warning => miette::Severity::Warning,
            Severity::Info | Severity::Hint => miette::Severity::Advice,
        }
    }
}

/// The canonical typedown diagnostic.
///
/// Produced by every phase (parser, type checker, lints) and rendered by
/// `td-cli` via miette. Using one enum variant plus a code string lets
/// downstream tooling filter/enable/disable rules with a stable identifier
/// (think `td001`, `missing-section`).
#[derive(Debug, Error, Diagnostic, Clone)]
#[error("{message}")]
pub struct TdDiagnostic {
    pub code: String,
    pub message: String,
    #[source_code]
    pub src: NamedSource<String>,
    #[label("{label}")]
    pub span: SourceSpan,
    pub label: String,
    #[help]
    pub help: Option<String>,
    pub severity: Severity,
}

impl TdDiagnostic {
    pub fn error(
        code: impl Into<String>,
        message: impl Into<String>,
        file: &SourceFile,
        span: Span,
        label: impl Into<String>,
    ) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            src: file.named_source(),
            span: span.into(),
            label: label.into(),
            help: None,
            severity: Severity::Error,
        }
    }

    pub fn with_help(mut self, help: impl Into<String>) -> Self {
        self.help = Some(help.into());
        self
    }

    pub fn with_severity(mut self, severity: Severity) -> Self {
        self.severity = severity;
        self
    }
}

/// Collection of diagnostics produced by a check run.
#[derive(Debug, Default)]
pub struct Diagnostics {
    items: Vec<TdDiagnostic>,
}

impl Diagnostics {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, d: TdDiagnostic) {
        self.items.push(d);
    }

    pub fn extend<I: IntoIterator<Item = TdDiagnostic>>(&mut self, it: I) {
        self.items.extend(it);
    }

    pub fn has_errors(&self) -> bool {
        self.items
            .iter()
            .any(|d| matches!(d.severity, Severity::Error))
    }

    pub fn iter(&self) -> std::slice::Iter<'_, TdDiagnostic> {
        self.items.iter()
    }

    pub fn into_vec(self) -> Vec<TdDiagnostic> {
        self.items
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn span_join_works() {
        let a = Span::new(2, 5);
        let b = Span::new(10, 12);
        assert_eq!(a.join(b), Span::new(2, 12));
    }

    #[test]
    fn diagnostics_track_errors() {
        let file = SourceFile::new("t.md", "hi");
        let mut d = Diagnostics::new();
        assert!(!d.has_errors());
        d.push(TdDiagnostic::error(
            "td001",
            "bad",
            &file,
            Span::new(0, 2),
            "here",
        ));
        assert!(d.has_errors());
    }
}
