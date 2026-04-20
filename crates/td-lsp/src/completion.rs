//! Completion provider.

use td_check::EntryOrigin;
use tower_lsp::lsp_types::{
    CompletionItem, CompletionItemKind, CompletionList, CompletionResponse, Documentation,
    InsertTextFormat, MarkupContent, MarkupKind, Position,
};

use crate::{
    resolver::{token_at, EffectKind, TokenKind},
    state::{DocState, WorkspaceState},
};

pub fn completions(
    ws: &WorkspaceState,
    state: &DocState,
    pos: Position,
) -> Option<CompletionResponse> {
    let offset = state.line_index.offset(pos);
    let tok = token_at(&state.doc, state.line_index.text(), offset);
    let items = match tok {
        Some(TokenKind::FrontmatterType { .. }) | None => frontmatter_type_items(ws, state),
        Some(TokenKind::EffectKeyword { kind, .. }) => effect_keyword_items(kind),
        Some(TokenKind::EffectStringArg { kind, .. }) => effect_string_items(kind),
        Some(TokenKind::ImportModulePath { .. }) => import_path_items(),
    };
    Some(CompletionResponse::List(CompletionList {
        is_incomplete: false,
        items,
    }))
}

fn frontmatter_type_items(ws: &WorkspaceState, state: &DocState) -> Vec<CompletionItem> {
    let mut out: Vec<CompletionItem> = Vec::new();

    // Stdlib types visible by default (Section, Prose, Prompt, …).
    for (name, path) in td_stdlib::builtin_index() {
        out.push(CompletionItem {
            label: name.to_string(),
            kind: Some(CompletionItemKind::STRUCT),
            detail: Some(format!("stdlib ({path})")),
            ..Default::default()
        });
    }

    // User-declared types in the current doc.
    for (name, entry) in &state.env.entries {
        if matches!(entry.origin, EntryOrigin::Local) {
            out.push(CompletionItem {
                label: name.clone(),
                kind: Some(CompletionItemKind::STRUCT),
                detail: Some("declared in this document".into()),
                ..Default::default()
            });
        }
    }

    // Cross-file user-declared types from the symbol index.
    for name in ws.symbol_index.all_names() {
        if state.env.entries.contains_key(name) {
            continue;
        }
        if td_stdlib::builtin_index().contains_key(name.as_str()) {
            continue;
        }
        out.push(CompletionItem {
            label: name.clone(),
            kind: Some(CompletionItemKind::STRUCT),
            detail: Some("from workspace".into()),
            ..Default::default()
        });
    }

    // Effect-row kinds as snippet-style insertions (Uses<[]>, Model<"">…).
    out.extend(effect_row_snippets());

    out
}

fn effect_row_snippets() -> Vec<CompletionItem> {
    let mk = |label: &str, snippet: &str, blurb: &str| CompletionItem {
        label: label.to_string(),
        kind: Some(CompletionItemKind::KEYWORD),
        detail: Some("effect row".into()),
        documentation: Some(Documentation::MarkupContent(MarkupContent {
            kind: MarkupKind::Markdown,
            value: blurb.to_string(),
        })),
        insert_text: Some(snippet.to_string()),
        insert_text_format: Some(InsertTextFormat::SNIPPET),
        ..Default::default()
    };
    vec![
        mk("Uses", "Uses<[$0]>", "Tools this prompt may invoke."),
        mk("Reads", "Reads<[$0]>", "Glob patterns this prompt may read."),
        mk("Writes", "Writes<[$0]>", "Glob patterns this prompt may write."),
        mk("Model", "Model<$0>", "Allowed model identifiers."),
        mk("MaxTokens", "MaxTokens<$0>", "Hard token ceiling."),
    ]
}

fn effect_keyword_items(_kind: EffectKind) -> Vec<CompletionItem> {
    // When the cursor is on the keyword itself, re-offer the keyword
    // completions so tab-selection still works.
    effect_row_snippets()
}

fn effect_string_items(kind: EffectKind) -> Vec<CompletionItem> {
    match kind {
        EffectKind::Model => model_items(),
        EffectKind::Uses => common_tool_items(),
        EffectKind::Reads | EffectKind::Writes => glob_shape_items(),
        EffectKind::MaxTokens => Vec::new(),
    }
}

fn model_items() -> Vec<CompletionItem> {
    // Lightweight hints. Keep the list conservative so it doesn't date
    // badly; users can always type a new id.
    [
        "claude-opus-4-7",
        "claude-sonnet-4-6",
        "claude-haiku-4-5",
        "openai/gpt-4o-mini",
        "openai/gpt-4o",
    ]
    .iter()
    .map(|m| CompletionItem {
        label: m.to_string(),
        kind: Some(CompletionItemKind::VALUE),
        detail: Some("model id".into()),
        ..Default::default()
    })
    .collect()
}

fn common_tool_items() -> Vec<CompletionItem> {
    ["read_file", "write_file", "run_tests", "list_files", "search_code"]
        .iter()
        .map(|t| CompletionItem {
            label: t.to_string(),
            kind: Some(CompletionItemKind::VALUE),
            detail: Some("tool name".into()),
            ..Default::default()
        })
        .collect()
}

fn glob_shape_items() -> Vec<CompletionItem> {
    [("./**", "entire workspace"), ("./src/**", "source tree"), ("./docs/**", "docs tree")]
        .iter()
        .map(|(p, d)| CompletionItem {
            label: p.to_string(),
            kind: Some(CompletionItemKind::VALUE),
            detail: Some((*d).to_string()),
            ..Default::default()
        })
        .collect()
}

fn import_path_items() -> Vec<CompletionItem> {
    td_stdlib::module_paths()
        .iter()
        .map(|p| CompletionItem {
            label: p.to_string(),
            kind: Some(CompletionItemKind::MODULE),
            detail: Some("stdlib module".into()),
            ..Default::default()
        })
        .collect()
}
