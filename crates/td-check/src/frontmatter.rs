//! Resolve the document's top-level type from YAML frontmatter.
//!
//! A document opts in by declaring a `typedown:` field in its frontmatter:
//!
//! ```yaml
//! ---
//! typedown: Prompt<ReviewInput, ReviewOutput>
//! ---
//! ```
//!
//! The field value is parsed with the `td` DSL expression parser as if it
//! were a standalone type expression. If no frontmatter field is present we
//! emit an info-level diagnostic (the checker short-circuits) so that
//! untyped markdown files are silently ignored — important for gradual
//! adoption.

use td_ast::md::MdDoc;
use td_ast::td::TdType;
use td_core::{Diagnostics, Severity, SourceFile, Span, TdDiagnostic};
use td_parse::parse_td_module;

pub fn doc_type_expr(
    doc: &MdDoc,
    file: &SourceFile,
    diagnostics: &mut Diagnostics,
) -> Option<TdType> {
    let Some(fm) = &doc.frontmatter else {
        // No frontmatter: file is not a typedown-typed doc. Silent skip.
        return None;
    };

    let mut td_expr: Option<(String, Span)> = None;
    for line in fm.raw.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("typedown:") {
            let value = rest.trim().trim_matches('"').to_string();
            // Approximate span: frontmatter span start + offset within raw.
            // Accurate-enough for now; we can tighten with a real YAML parse.
            td_expr = Some((value, fm.span));
            break;
        }
    }

    let Some((expr, span)) = td_expr else {
        diagnostics.push(
            TdDiagnostic::error(
                "td301",
                "frontmatter is missing `typedown:` field".to_string(),
                file,
                fm.span,
                "no typedown declaration",
            )
            .with_severity(Severity::Warning)
            .with_help(
                "add `typedown: <TypeName>` to opt this file into checking, \
                 or remove the frontmatter entirely to skip it"
                    .to_string(),
            ),
        );
        return None;
    };

    // Wrap as a fake type alias so we can reuse the module parser.
    let wrapped = format!("type __Doc = {expr}");
    let (m, diags) = parse_td_module(&wrapped, file, span.start);
    diagnostics.extend(diags.into_vec());
    let decl = m.decls.into_iter().next()?;
    match decl.kind {
        td_ast::td::TdDeclKind::TypeAlias(t) => Some(t),
        _ => None,
    }
}
