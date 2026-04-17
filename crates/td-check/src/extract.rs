//! Pull every ```td fence out of a markdown doc and parse them as a single
//! merged [`TdModule`].
//!
//! Stitching multiple fences into one module lets authors split definitions
//! across sections without losing cross-references, and keeps the checker's
//! symbol table construction to a single pass.

use td_ast::{
    md::{MdDoc, MdNodeKind},
    TdModule,
};
use td_core::{Diagnostics, SourceFile, Span};
use td_parse::parse_td_module;

pub fn extract_td_modules(doc: &MdDoc, file: &SourceFile) -> (TdModule, Diagnostics) {
    let mut diagnostics = Diagnostics::new();
    let mut merged = TdModule::default();

    for node in &doc.nodes {
        let MdNodeKind::CodeBlock { lang, code } = &node.kind else {
            continue;
        };
        if lang.as_deref() != Some("td") {
            continue;
        }

        // We need the byte offset of the code body within the original
        // source. `node.span` covers the entire fence including backticks +
        // info string. Locate the code body by finding the first newline
        // after the opening fence. If we can't, fall back to span.start.
        let fence_start = node.span.start;
        let content = &file.content[fence_start..node.span.end];
        let body_offset = content
            .find('\n')
            .map(|nl| fence_start + nl + 1)
            .unwrap_or(fence_start);

        let (module, diags) = parse_td_module(code, file, body_offset);
        diagnostics.extend(diags.into_vec());

        if merged.span == Span::DUMMY {
            merged.span = module.span;
        } else {
            merged.span = merged.span.join(module.span);
        }
        merged.imports.extend(module.imports);
        merged.decls.extend(module.decls);
    }

    (merged, diagnostics)
}
