//! Hover provider.

use td_check::LookupResult;
use tower_lsp::lsp_types::{Hover, HoverContents, MarkupContent, MarkupKind, Position};

use crate::{
    resolver::{token_at, EffectKind, TokenKind},
    state::{DocState, WorkspaceState},
};

pub fn hover(ws: &WorkspaceState, state: &DocState, pos: Position) -> Option<Hover> {
    let offset = state.line_index.offset(pos);
    let tok = token_at(&state.doc, state.line_index.text(), offset)?;
    let md = match tok {
        TokenKind::FrontmatterType { name, .. } => frontmatter_type_md(ws, state, &name)?,
        TokenKind::EffectKeyword { kind, .. } => effect_keyword_md(kind, state),
        TokenKind::EffectStringArg { kind, value, .. } => effect_string_md(kind, &value),
        TokenKind::ImportModulePath { .. } => return None,
    };
    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: md,
        }),
        range: None,
    })
}

fn frontmatter_type_md(_ws: &WorkspaceState, state: &DocState, name: &str) -> Option<String> {
    // Try local env first, then builtins.
    match state.env.lookup(name) {
        LookupResult::Decl(entry) => Some(render_decl(entry, state)),
        LookupResult::Builtin(b) => Some(render_builtin(b)),
        LookupResult::Missing => Some(format!("`{name}` — unresolved")),
    }
}

fn render_decl(entry: &td_check::EnvEntry, _state: &DocState) -> String {
    use td_ast::td::TdDeclKind;
    let origin = match &entry.origin {
        td_check::EntryOrigin::Local => "local",
        td_check::EntryOrigin::Stdlib(path) => path,
    };
    let kw = match entry.decl.kind {
        TdDeclKind::TypeAlias(_) => "type",
        TdDeclKind::Interface(_) => "interface",
    };
    let generics = if entry.decl.generics.is_empty() {
        String::new()
    } else {
        format!("<{}>", entry.decl.generics.join(", "))
    };
    format!(
        "```typedown\n{kw} {name}{generics}\n```\n\n*from {origin}*",
        name = entry.decl.name
    )
}

fn render_builtin(b: td_stdlib::Builtin) -> String {
    format!(
        "```typedown\nbuiltin {name}\n```\n\n*content-shape primitive*",
        name = b.display()
    )
}

fn effect_keyword_md(kind: EffectKind, state: &DocState) -> String {
    use std::fmt::Write;
    let mut s = String::new();
    let _ = writeln!(s, "**`{}<…>`** — effect row", kind.as_str());
    let _ = writeln!(s);
    let _ = writeln!(s, "{}", kind.policy_blurb());
    let _ = writeln!(s);
    // Show the current declared value for this effect, if any.
    let current = match kind {
        EffectKind::Uses => render_list(&state.effects.uses),
        EffectKind::Reads => render_list(&state.effects.reads),
        EffectKind::Writes => render_list(&state.effects.writes),
        EffectKind::Model => render_list(&state.effects.models),
        EffectKind::MaxTokens => state
            .effects
            .max_tokens
            .map(|n| n.to_string())
            .unwrap_or_else(|| "—".to_string()),
    };
    let _ = writeln!(s, "**declared:** `{current}`");
    s
}

fn render_list(xs: &[String]) -> String {
    if xs.is_empty() {
        "∅ (deny-all)".to_string()
    } else {
        xs.join(", ")
    }
}

fn effect_string_md(kind: EffectKind, value: &str) -> String {
    format!(
        "`{value}` — entry in **`{}<…>`** ({})",
        kind.as_str(),
        kind.policy_blurb()
    )
}
