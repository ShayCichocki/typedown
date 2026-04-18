//! Identifier derivation for codegen output.
//!
//! Turning a file path into a stable TypeScript identifier is fiddly;
//! centralize the rules so every backend derives names the same way
//! and users can rely on them.
//!
//! Rules (applied in order):
//!
//! 1. Take the file stem (`code_reviewer_prompt.md` → `code_reviewer_prompt`).
//! 2. Split on `_` / `-` / whitespace / `.` into word tokens.
//! 3. Discard empty tokens and tokens that start with a digit if they'd
//!    become the leading character.
//! 4. Capitalize each token → PascalCase.
//! 5. Lowercase the first character for the camelCase variant.
//!
//! If the resulting identifier is empty or starts with a digit, fall
//! back to `"Document"` / `"document"`.

use std::path::Path;

/// `code_reviewer_prompt.md` → `CodeReviewerPrompt`.
pub fn pascal_case_from_path(path: &Path) -> String {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    pascal_case(stem)
}

/// `code_reviewer_prompt.md` → `codeReviewerPrompt`.
pub fn camel_case_from_path(path: &Path) -> String {
    let pascal = pascal_case_from_path(path);
    lower_first(&pascal)
}

pub fn pascal_case(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for token in tokens(input) {
        let mut chars = token.chars();
        if let Some(c) = chars.next() {
            for cap in c.to_uppercase() {
                out.push(cap);
            }
            for rest in chars {
                out.push(rest.to_ascii_lowercase());
            }
        }
    }
    if out.is_empty() || out.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        "Document".to_string()
    } else {
        out
    }
}

fn lower_first(input: &str) -> String {
    let mut chars = input.chars();
    match chars.next() {
        Some(c) => {
            let mut out = String::with_capacity(input.len());
            for lc in c.to_lowercase() {
                out.push(lc);
            }
            out.push_str(chars.as_str());
            out
        }
        None => "document".to_string(),
    }
}

fn tokens(input: &str) -> Vec<&str> {
    input
        .split(|c: char| c == '_' || c == '-' || c == '.' || c.is_whitespace())
        .filter(|s| !s.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn underscore_path_pascal() {
        assert_eq!(
            pascal_case_from_path(&PathBuf::from("code_reviewer_prompt.md")),
            "CodeReviewerPrompt"
        );
    }

    #[test]
    fn dash_path_pascal() {
        assert_eq!(
            pascal_case_from_path(&PathBuf::from("code-reviewer-prompt.md")),
            "CodeReviewerPrompt"
        );
    }

    #[test]
    fn camel_case_variant() {
        assert_eq!(
            camel_case_from_path(&PathBuf::from("code_reviewer_prompt.md")),
            "codeReviewerPrompt"
        );
    }

    #[test]
    fn plain_stem() {
        assert_eq!(pascal_case_from_path(&PathBuf::from("reviewer.md")), "Reviewer");
        assert_eq!(camel_case_from_path(&PathBuf::from("reviewer.md")), "reviewer");
    }

    #[test]
    fn empty_falls_back() {
        assert_eq!(pascal_case(""), "Document");
        assert_eq!(pascal_case("123"), "Document");
    }
}
