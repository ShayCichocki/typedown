//! `TdDiagnostic` → `lsp_types::Diagnostic` conversion.

use td_core::{Severity, TdDiagnostic};
use tower_lsp::lsp_types::{
    Diagnostic, DiagnosticSeverity, NumberOrString,
};

use crate::line_index::LineIndex;

pub fn to_lsp(diag: &TdDiagnostic, line_index: &LineIndex) -> Diagnostic {
    // miette SourceSpan carries offset + length; pull them back into a Range.
    let start = diag.span.offset();
    let len = diag.span.len();
    let range = line_index.range(td_core::Span::new(start, start + len));
    Diagnostic {
        range,
        severity: Some(map_severity(diag.severity)),
        code: Some(NumberOrString::String(diag.code.clone())),
        code_description: None,
        source: Some("typedown".to_string()),
        message: full_message(diag),
        related_information: None,
        tags: None,
        data: None,
    }
}

fn map_severity(s: Severity) -> DiagnosticSeverity {
    match s {
        Severity::Error => DiagnosticSeverity::ERROR,
        Severity::Warning => DiagnosticSeverity::WARNING,
        Severity::Info => DiagnosticSeverity::INFORMATION,
        Severity::Hint => DiagnosticSeverity::HINT,
    }
}

/// Combine `message`, `label`, and optional `help` into one display string.
/// LSP clients typically show only `message`, so we smash the three together
/// the way miette does for humans — but terser.
fn full_message(d: &TdDiagnostic) -> String {
    let mut out = d.message.clone();
    if !d.label.is_empty() && d.label != d.message {
        out.push_str(" — ");
        out.push_str(&d.label);
    }
    if let Some(help) = &d.help {
        out.push_str("\n\nhelp: ");
        out.push_str(help);
    }
    out
}
