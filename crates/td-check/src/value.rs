//! Value typing: the other half of typedown's judgement.
//!
//! Until now the checker only asked "does the document have the right
//! *shape* of markdown nodes for the declared type?". That makes type
//! parameters phantom — `Prompt<ReviewInput, ReviewOutput>` and
//! `Prompt<Foo, Bar>` produce the same diagnostics as long as the markdown
//! skeleton matches. This module closes that loop.
//!
//! We parse JSON / YAML code fences that appear inside `### Example N`
//! subsections and check the resulting values against their declared types
//! (`I`, `O` from `Example<I, O>`). Every mismatch is reported with a JSON
//! path so authors can locate the field in the fence without us needing a
//! byte-accurate span (those will come later via a spanned parser).
//!
//! # Diagnostic codes
//!
//! | code   | meaning                                              |
//! |--------|------------------------------------------------------|
//! | td501  | value fence failed to parse (JSON / YAML syntax)     |
//! | td502  | value does not match declared type                   |
//! | td504  | value has extra field not present in declared type   |
//!
//! # Why `serde_json::Value`?
//!
//! Keeping the in-memory value representation in JSON (rather than YAML)
//! means the checker deals with one tree shape regardless of fence lang.
//! YAML parses straight into `serde_json::Value` via serde because every
//! JSON tree is a legal YAML tree. Non-JSON-compatible YAML (e.g. non-
//! string object keys) surfaces as a td501.

use serde_json::Value;
use td_ast::td::{TdObjectType, TdPrim, TdType};
use td_core::{Diagnostics, Severity, SourceFile, Span, TdDiagnostic};
use td_stdlib::Builtin;

use crate::env::{LookupResult, TypeEnv};

/// Languages we understand as "typed value" fences.
///
/// The checker will attempt to typecheck the fence body against the adjacent
/// input / output type. Unknown langs (`rust`, `ts`, arbitrary prose) are
/// ignored — only these two are semantically meaningful today.
pub const VALUE_FENCE_LANGS: &[&str] = &["json", "jsonc", "yaml", "yml"];

/// Parse a fenced code block body into a [`serde_json::Value`].
///
/// Returns `Err(msg)` with the parser error. The caller surfaces it as
/// `td501`; we don't push diagnostics here because `value.rs` has no
/// opinion about where the error should anchor.
pub fn parse_value(lang: &str, src: &str) -> Result<Value, String> {
    match lang {
        "json" | "jsonc" => {
            // `jsonc` is JSON-with-comments. serde_json doesn't support them
            // natively; we do a lightweight strip of `//` and `/* */` blocks
            // before parsing so authors can annotate example payloads.
            let cleaned = if lang == "jsonc" {
                strip_jsonc_comments(src)
            } else {
                src.to_string()
            };
            serde_json::from_str(&cleaned).map_err(|e| e.to_string())
        }
        "yaml" | "yml" => {
            // serde_yaml can deserialize directly into serde_json::Value since
            // the value models overlap for JSON-compatible YAML documents.
            serde_yaml::from_str::<Value>(src).map_err(|e| e.to_string())
        }
        other => Err(format!("unsupported value fence language `{other}`")),
    }
}

/// Type-check a JSON-ish value against a declared [`TdType`].
///
/// `anchor` is the span we attach to every emitted diagnostic (typically
/// the fenced code block that produced the value). `path` is the running
/// JSON-pointer-style trail ("", "/comments/0/severity") we use so authors
/// can find the offending field without byte-accurate spans.
pub fn check_value(
    value: &Value,
    ty: &TdType,
    env: &TypeEnv,
    file: &SourceFile,
    anchor: Span,
    path: &str,
    diagnostics: &mut Diagnostics,
) {
    match ty {
        // ------------------------------------------------------------------
        // Primitives
        // ------------------------------------------------------------------
        TdType::Primitive { kind, .. } => match kind {
            TdPrim::String if value.is_string() => {}
            TdPrim::Number if value.is_number() => {}
            TdPrim::Boolean if value.is_boolean() => {}
            TdPrim::Null if value.is_null() => {}
            TdPrim::Any => {}
            _ => push_mismatch(
                diagnostics,
                file,
                anchor,
                path,
                &prim_name(*kind),
                &value_kind(value),
            ),
        },

        // Literal primitives narrow their primitive one step further.
        TdType::StringLit { value: expected, .. } => match value {
            Value::String(s) if s == expected => {}
            _ => push_mismatch(
                diagnostics,
                file,
                anchor,
                path,
                &format!("\"{expected}\""),
                &display_value(value),
            ),
        },
        TdType::NumberLit { value: expected, .. } => match value.as_f64() {
            Some(n) if n == *expected => {}
            _ => push_mismatch(
                diagnostics,
                file,
                anchor,
                path,
                &format!("{expected}"),
                &display_value(value),
            ),
        },

        // ------------------------------------------------------------------
        // Composites
        // ------------------------------------------------------------------
        TdType::Array { elem, .. } => match value {
            Value::Array(items) => {
                for (i, item) in items.iter().enumerate() {
                    let child = push_path(path, &i.to_string());
                    check_value(item, elem, env, file, anchor, &child, diagnostics);
                }
            }
            _ => push_mismatch(
                diagnostics,
                file,
                anchor,
                path,
                "array",
                &value_kind(value),
            ),
        },

        TdType::Object(obj) => check_object(obj, value, env, file, anchor, path, diagnostics),

        // A union passes if ANY variant passes. We try each variant into a
        // scratch buffer; if any stays empty we commit no diagnostics.
        // Otherwise we emit a single summary diagnostic naming the variants.
        TdType::Union { variants, .. } => {
            // String-literal unions get a nicer "enum" message ("one of
            // \"nit\" | \"suggestion\" | …"). Detect that case first.
            if variants.iter().all(|v| matches!(v, TdType::StringLit { .. })) {
                let matched = variants.iter().any(|v| matches!(
                    (v, value),
                    (TdType::StringLit { value: lit, .. }, Value::String(s)) if lit == s,
                ));
                if !matched {
                    let expected = variants
                        .iter()
                        .filter_map(|v| match v {
                            TdType::StringLit { value, .. } => Some(format!("\"{value}\"")),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join(" | ");
                    push_mismatch(
                        diagnostics,
                        file,
                        anchor,
                        path,
                        &format!("one of {expected}"),
                        &display_value(value),
                    );
                }
                return;
            }

            let mut matched = false;
            for v in variants {
                let mut scratch = Diagnostics::new();
                check_value(value, v, env, file, anchor, path, &mut scratch);
                if scratch.is_empty() {
                    matched = true;
                    break;
                }
            }
            if !matched {
                push_mismatch(
                    diagnostics,
                    file,
                    anchor,
                    path,
                    "value matching any variant of the union",
                    &display_value(value),
                );
            }
        }

        // Intersections require *every* part to pass.
        TdType::Intersection { parts, .. } => {
            for p in parts {
                check_value(value, p, env, file, anchor, path, diagnostics);
            }
        }

        // ------------------------------------------------------------------
        // Named references: resolve through the environment.
        // ------------------------------------------------------------------
        TdType::NamedRef {
            name,
            type_args,
            span,
        } => match env.lookup(name) {
            LookupResult::Decl(entry) => {
                let expanded = env.instantiate(&entry.decl, type_args);
                check_value(value, &expanded, env, file, anchor, path, diagnostics);
            }
            LookupResult::Builtin(b) => {
                // Content-shape builtins have no meaning at the value level.
                // Authors who type-check an example value against `Prose` or
                // `Section<T>` have a type mistake — catch it once per check.
                if matches!(b, Builtin::Example) {
                    // Shouldn't land here (check_builtin unwraps Example
                    // before descending to values), but be defensive.
                    return;
                }
                diagnostics.push(TdDiagnostic::error(
                    "td502",
                    format!(
                        "cannot typecheck a value against content-shape type `{name}`"
                    ),
                    file,
                    pick_anchor(*span, anchor),
                    "not a value type",
                ).with_help(
                    "content-shape primitives (Section, Prose, OrderedList, …) describe \
                     markdown structure, not JSON values — use a value type like \
                     `string`, a literal, or an object here".to_string(),
                ));
            }
            LookupResult::Missing => {
                diagnostics.push(TdDiagnostic::error(
                    "td403",
                    format!("unknown type `{name}` in value position"),
                    file,
                    pick_anchor(*span, anchor),
                    "not declared or imported",
                ));
            }
        },
    }
}

fn check_object(
    obj: &TdObjectType,
    value: &Value,
    env: &TypeEnv,
    file: &SourceFile,
    anchor: Span,
    path: &str,
    diagnostics: &mut Diagnostics,
) {
    let map = match value {
        Value::Object(m) => m,
        _ => {
            push_mismatch(
                diagnostics,
                file,
                anchor,
                path,
                "object",
                &value_kind(value),
            );
            return;
        }
    };

    // Required vs. optional field tracking.
    for field in &obj.fields {
        match map.get(&field.name) {
            Some(v) => {
                let child = push_path(path, &field.name);
                check_value(v, &field.ty, env, file, anchor, &child, diagnostics);
            }
            None if field.optional => {}
            None => {
                diagnostics.push(
                    TdDiagnostic::error(
                        "td502",
                        format!(
                            "{} is missing required field `{}`",
                            pretty_path(path),
                            field.name
                        ),
                        file,
                        anchor,
                        "required field missing",
                    )
                    .with_help(format!(
                        "add `{}` to the example value (declared at field `{}`)",
                        field.name, field.name
                    )),
                );
            }
        }
    }

    // Extras are warnings: the checker's stance is "types are authoritative
    // but an unexpected extra is often just a doc author learning the
    // schema". Matches our `td405` posture for extra sections.
    let declared: std::collections::HashSet<&str> =
        obj.fields.iter().map(|f| f.name.as_str()).collect();
    for key in map.keys() {
        if !declared.contains(key.as_str()) {
            diagnostics.push(
                TdDiagnostic::error(
                    "td504",
                    format!(
                        "{} has extra field `{}` not declared in the type",
                        pretty_path(path),
                        key
                    ),
                    file,
                    anchor,
                    "extra field",
                )
                .with_severity(Severity::Warning)
                .with_help(
                    "either add this field to the declared type or remove \
                     it from the example value"
                        .to_string(),
                ),
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn push_mismatch(
    diagnostics: &mut Diagnostics,
    file: &SourceFile,
    anchor: Span,
    path: &str,
    expected: &str,
    got: &str,
) {
    diagnostics.push(
        TdDiagnostic::error(
            "td502",
            format!(
                "{}: expected {expected}, got {got}",
                pretty_path(path)
            ),
            file,
            anchor,
            "value does not match type",
        )
        .with_help(
            "check the example payload against the declared `I` / `O` types \
             in the `td` fence"
                .to_string(),
        ),
    );
}

fn prim_name(p: TdPrim) -> String {
    match p {
        TdPrim::String => "string".into(),
        TdPrim::Number => "number".into(),
        TdPrim::Boolean => "boolean".into(),
        TdPrim::Null => "null".into(),
        TdPrim::Any => "any".into(),
    }
}

fn value_kind(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Bool(_) => "boolean".into(),
        Value::Number(_) => "number".into(),
        Value::String(_) => "string".into(),
        Value::Array(_) => "array".into(),
        Value::Object(_) => "object".into(),
    }
}

/// Render a concrete value compactly for diagnostic messages. Clamped so a
/// huge payload doesn't blow up the error.
fn display_value(v: &Value) -> String {
    let raw = match v {
        Value::String(s) => format!("\"{s}\""),
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        other => serde_json::to_string(other).unwrap_or_else(|_| "<unrenderable>".into()),
    };
    if raw.len() > 60 {
        format!("{}…", &raw[..60])
    } else {
        raw
    }
}

fn push_path(parent: &str, child: &str) -> String {
    if parent.is_empty() {
        format!("/{child}")
    } else {
        format!("{parent}/{child}")
    }
}

fn pretty_path(path: &str) -> String {
    if path.is_empty() {
        "value".to_string()
    } else {
        format!("at `{path}`")
    }
}

fn pick_anchor(span: Span, fallback: Span) -> Span {
    if span == Span::DUMMY {
        fallback
    } else {
        span
    }
}

fn strip_jsonc_comments(src: &str) -> String {
    // Minimal stripper: remove `//…\n` and `/* … */` blocks without touching
    // comment-like substrings inside strings. Good enough for doc examples;
    // a full JSONC grammar can wait until someone hits a real edge case.
    let bytes = src.as_bytes();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    let mut in_string = false;
    let mut escape = false;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if in_string {
            out.push(c);
            if escape {
                escape = false;
            } else if c == '\\' {
                escape = true;
            } else if c == '"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        if c == '"' {
            in_string = true;
            out.push(c);
            i += 1;
            continue;
        }
        if c == '/' && i + 1 < bytes.len() {
            let next = bytes[i + 1] as char;
            if next == '/' {
                // Line comment. Skip until \n (preserve the \n for line-numbering).
                let mut j = i + 2;
                while j < bytes.len() && bytes[j] as char != '\n' {
                    j += 1;
                }
                i = j;
                continue;
            }
            if next == '*' {
                // Block comment. Skip until */.
                let mut j = i + 2;
                while j + 1 < bytes.len() && !(bytes[j] as char == '*' && bytes[j + 1] as char == '/') {
                    j += 1;
                }
                i = j + 2;
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use td_ast::td::{TdField, TdModule};
    use td_parse::parse_td_module;

    fn sf(src: &str) -> SourceFile {
        SourceFile::new("test.md", src.to_string())
    }

    /// Build a type env from a td-module source string. Returns the env and
    /// the named type's resolved body (instantiated with no args).
    fn env_with(ty_src: &str) -> (TypeEnv, TdModule) {
        let file = sf(ty_src);
        let (module, _diags) = parse_td_module(ty_src, &file, 0);
        let (env, _env_diags) = TypeEnv::build(&module, &file);
        (env, module)
    }

    fn lookup_ty(env: &TypeEnv, name: &str) -> TdType {
        match env.lookup(name) {
            LookupResult::Decl(entry) => env.instantiate(&entry.decl, &[]),
            _ => panic!("missing type `{name}`"),
        }
    }

    fn codes(diags: &Diagnostics) -> Vec<String> {
        diags.iter().map(|d| d.code.clone()).collect()
    }

    #[test]
    fn string_passes() {
        let (env, _) = env_with("type T = string");
        let ty = lookup_ty(&env, "T");
        let v: Value = serde_json::from_str("\"hi\"").unwrap();
        let mut d = Diagnostics::new();
        let file = sf("");
        check_value(&v, &ty, &env, &file, Span::DUMMY, "", &mut d);
        assert!(d.is_empty(), "{:?}", codes(&d));
    }

    #[test]
    fn string_rejected() {
        let (env, _) = env_with("type T = string");
        let ty = lookup_ty(&env, "T");
        let v: Value = serde_json::from_str("42").unwrap();
        let mut d = Diagnostics::new();
        let file = sf("");
        check_value(&v, &ty, &env, &file, Span::DUMMY, "", &mut d);
        assert_eq!(codes(&d), vec!["td502"]);
    }

    #[test]
    fn object_missing_required() {
        let (env, _) = env_with("type T = { x: string, y: number }");
        let ty = lookup_ty(&env, "T");
        let v: Value = serde_json::from_str(r#"{"x":"hi"}"#).unwrap();
        let mut d = Diagnostics::new();
        let file = sf("");
        check_value(&v, &ty, &env, &file, Span::DUMMY, "", &mut d);
        assert_eq!(codes(&d), vec!["td502"]);
    }

    #[test]
    fn object_optional_ok() {
        let (env, _) = env_with("type T = { x: string, y?: number }");
        let ty = lookup_ty(&env, "T");
        let v: Value = serde_json::from_str(r#"{"x":"hi"}"#).unwrap();
        let mut d = Diagnostics::new();
        let file = sf("");
        check_value(&v, &ty, &env, &file, Span::DUMMY, "", &mut d);
        assert!(d.is_empty(), "{:?}", codes(&d));
    }

    #[test]
    fn object_extra_field_warns() {
        let (env, _) = env_with("type T = { x: string }");
        let ty = lookup_ty(&env, "T");
        let v: Value = serde_json::from_str(r#"{"x":"hi","y":1}"#).unwrap();
        let mut d = Diagnostics::new();
        let file = sf("");
        check_value(&v, &ty, &env, &file, Span::DUMMY, "", &mut d);
        assert_eq!(codes(&d), vec!["td504"]);
        assert_eq!(d.iter().next().unwrap().severity, Severity::Warning);
    }

    #[test]
    fn array_of_primitives() {
        let (env, _) = env_with("type T = string[]");
        let ty = lookup_ty(&env, "T");
        let bad: Value = serde_json::from_str(r#"["a", 2, "c"]"#).unwrap();
        let mut d = Diagnostics::new();
        let file = sf("");
        check_value(&bad, &ty, &env, &file, Span::DUMMY, "", &mut d);
        assert_eq!(codes(&d), vec!["td502"]);
        assert!(
            d.iter().next().unwrap().message.contains("/1"),
            "message should include path: {}",
            d.iter().next().unwrap().message
        );
    }

    #[test]
    fn string_literal_union_enum() {
        let (env, _) = env_with(r#"type T = "a" | "b" | "c""#);
        let ty = lookup_ty(&env, "T");
        let bad: Value = serde_json::from_str("\"d\"").unwrap();
        let mut d = Diagnostics::new();
        let file = sf("");
        check_value(&bad, &ty, &env, &file, Span::DUMMY, "", &mut d);
        let msg = &d.iter().next().unwrap().message;
        assert!(msg.contains("\"a\""), "msg: {msg}");
        assert!(msg.contains("\"d\""), "msg: {msg}");
    }

    #[test]
    fn nested_object_path() {
        let (env, _) = env_with(
            r#"
            type Inner = { severity: "nit" | "blocking" }
            type T = { comments: Inner[] }
            "#,
        );
        let ty = lookup_ty(&env, "T");
        let bad: Value =
            serde_json::from_str(r#"{"comments":[{"severity":"critical"}]}"#).unwrap();
        let mut d = Diagnostics::new();
        let file = sf("");
        check_value(&bad, &ty, &env, &file, Span::DUMMY, "", &mut d);
        let msg = &d.iter().next().unwrap().message;
        assert!(
            msg.contains("/comments/0/severity"),
            "expected nested path in: {msg}"
        );
    }

    #[test]
    fn yaml_parses_as_json() {
        let v = parse_value("yaml", "x: 1\ny: hi\n").unwrap();
        assert_eq!(v["x"], serde_json::json!(1));
        assert_eq!(v["y"], serde_json::json!("hi"));
    }

    #[test]
    fn jsonc_strips_comments() {
        let v = parse_value(
            "jsonc",
            r#"{
                // inline comment
                "x": 1, /* block */ "y": "hi"
            }"#,
        )
        .unwrap();
        assert_eq!(v["x"], serde_json::json!(1));
        assert_eq!(v["y"], serde_json::json!("hi"));
    }

    // Suppress unused-import warning under cfg(test).
    #[allow(dead_code)]
    fn _touch_field(_: &TdField) {}
}
