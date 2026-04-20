//! End-to-end check that the LSP produces the same diagnostic codes the
//! CLI does for `examples/code_reviewer_prompt_broken.md`.
//!
//! Drives the LSP path via `DocState::build` (the same function the
//! server calls on didOpen/didChange). If this test regresses, the
//! server-level diagnostics will regress too.

use std::{collections::HashSet, path::PathBuf};

use td_lsp::LineIndex;

#[path = "../src/diagnostics.rs"]
mod diagnostics;
// Re-exposing private modules via `#[path]` is intentional: we want to
// test the conversion function without widening its visibility in the
// production API.
#[path = "../src/line_index.rs"]
mod line_index;
#[path = "../src/symbol.rs"]
mod symbol;
#[path = "../src/stdlib_cache.rs"]
mod stdlib_cache;
#[path = "../src/state.rs"]
mod state;

fn broken_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("examples/code_reviewer_prompt_broken.md")
}

#[test]
fn docstate_produces_expected_diagnostic_codes() {
    let path = broken_path();
    let content = std::fs::read_to_string(&path).expect("read fixture");
    let state = state::DocState::build(path, content, 0);
    let codes: HashSet<String> = state
        .diagnostics
        .iter()
        .map(|d| d.code.clone())
        .collect();
    for expected in ["td405", "td502", "td504", "td601"] {
        assert!(
            codes.contains(expected),
            "expected {expected} in diagnostics; got {codes:?}"
        );
    }
    // td502 fires twice (two value-type mismatches); make sure we keep
    // both — losing one would silently weaken coverage.
    let td502_count = state
        .diagnostics
        .iter()
        .filter(|d| d.code == "td502")
        .count();
    assert!(
        td502_count >= 2,
        "expected at least 2 td502 diagnostics; got {td502_count}"
    );
}

#[test]
fn diagnostic_ranges_convert_to_valid_lsp_ranges() {
    let path = broken_path();
    let content = std::fs::read_to_string(&path).expect("read fixture");
    let state = state::DocState::build(path, content, 0);
    for d in &state.diagnostics {
        let lsp = diagnostics::to_lsp(d, &state.line_index);
        // Range must have start <= end (by line, then by character).
        let ok = lsp.range.start.line < lsp.range.end.line
            || (lsp.range.start.line == lsp.range.end.line
                && lsp.range.start.character <= lsp.range.end.character);
        assert!(ok, "range out of order for {}: {:?}", d.code, lsp.range);
    }
}

#[test]
fn line_index_reexport_smoke() {
    // The crate's public API exposes LineIndex; make sure callers can
    // use it without pulling internals.
    let li = LineIndex::new("abc");
    assert_eq!(li.text(), "abc");
}
