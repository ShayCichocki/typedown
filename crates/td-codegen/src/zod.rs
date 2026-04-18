//! Compile a [`TdType`] to a Zod schema expression (TypeScript source).
//!
//! Zod is the de-facto TypeScript runtime schema library and the
//! natural interchange shape for the Vercel AI SDK
//! (`Output.object({ schema: z.object({…}) })`). This emitter turns
//! typedown's type AST into Zod call chains textually, so a caller
//! can splice them directly into a generated `.ts` file.
//!
//! # Output examples
//!
//! | TdType                                   | Zod                                |
//! |------------------------------------------|------------------------------------|
//! | `string`                                 | `z.string()`                       |
//! | `number`                                 | `z.number()`                       |
//! | `boolean`                                | `z.boolean()`                      |
//! | `null`                                   | `z.null()`                         |
//! | `any`                                    | `z.any()`                          |
//! | `"nit"`                                  | `z.literal("nit")`                 |
//! | `42`                                     | `z.literal(42)`                    |
//! | `string[]`                               | `z.array(z.string())`              |
//! | `[string, number]`                       | `z.tuple([z.string(), z.number()])`|
//! | `{ a: string, b?: number }`              | `z.object({ a: z.string(), b: z.number().optional() })` |
//! | `"a" \| "b"` (string-literal union)      | `z.enum(["a", "b"])`               |
//! | `A \| B` (mixed union)                   | `z.union([<A>, <B>])`              |
//! | `A & B` (object intersection)            | `<A>.and(<B>)`                     |
//! | `Comment` (named ref)                    | `CommentSchema`                    |
//!
//! # Named references
//!
//! When we hit a named-ref pointing to a locally-declared type, we emit
//! the identifier `<Name>Schema` — which the top-level emitter is
//! responsible for declaring. Stdlib-origin types that are structurally
//! `any` (effect rows, `Compose`, etc.) should be stripped *before*
//! this pass runs; they emit `z.any()` defensively if they leak
//! through.

use td_ast::td::{TdField, TdObjectType, TdPrim, TdType};
use td_check::{LookupResult, TypeEnv};

use crate::CodegenError;

/// Render a type as a Zod expression.
///
/// `path` is the dot-path through the declaration tree we'd report on
/// error — callers pass an empty string at the root and the emitter
/// extends it as it descends.
pub fn emit_zod(ty: &TdType, env: &TypeEnv, path: &str) -> Result<String, CodegenError> {
    match ty {
        TdType::Primitive { kind, .. } => Ok(prim_zod(*kind).to_string()),
        TdType::StringLit { value, .. } => Ok(format!("z.literal({})", js_string(value))),
        TdType::NumberLit { value, .. } => Ok(format!("z.literal({})", fmt_number(*value))),
        TdType::Array { elem, .. } => {
            let inner = emit_zod(elem, env, &push(path, "[]"))?;
            Ok(format!("z.array({inner})"))
        }
        TdType::Tuple { elems, .. } => {
            if elems.is_empty() {
                Ok("z.tuple([])".to_string())
            } else {
                let parts: Result<Vec<String>, CodegenError> = elems
                    .iter()
                    .enumerate()
                    .map(|(i, e)| emit_zod(e, env, &push(path, &format!("[{i}]"))))
                    .collect();
                Ok(format!("z.tuple([{}])", parts?.join(", ")))
            }
        }
        TdType::Object(obj) => emit_object(obj, env, path),
        TdType::Union { variants, .. } => emit_union(variants, env, path),
        TdType::Intersection { parts, .. } => emit_intersection(parts, env, path),
        TdType::NamedRef {
            name, type_args, ..
        } => emit_named_ref(name, type_args, env, path),
    }
}

fn emit_object(obj: &TdObjectType, env: &TypeEnv, path: &str) -> Result<String, CodegenError> {
    if obj.fields.is_empty() {
        return Ok("z.object({})".to_string());
    }
    let mut lines = Vec::with_capacity(obj.fields.len());
    for field in &obj.fields {
        lines.push(emit_field(field, env, path)?);
    }
    Ok(format!("z.object({{ {} }})", lines.join(", ")))
}

fn emit_field(field: &TdField, env: &TypeEnv, path: &str) -> Result<String, CodegenError> {
    let inner = emit_zod(&field.ty, env, &push(path, &field.name))?;
    let key = js_object_key(&field.name);
    let body = if field.optional {
        format!("{inner}.optional()")
    } else {
        inner
    };
    let out = if let Some(doc) = field.doc.as_deref() {
        let described = format!("{body}.describe({})", js_string(doc.trim()));
        format!("{key}: {described}")
    } else {
        format!("{key}: {body}")
    };
    Ok(out)
}

fn emit_union(variants: &[TdType], env: &TypeEnv, path: &str) -> Result<String, CodegenError> {
    // Promote string-literal unions to z.enum; the Vercel AI SDK docs
    // consistently use this form and downstream tool specs (OpenAI,
    // Anthropic) recognize it as a discriminated set.
    if variants.iter().all(|v| matches!(v, TdType::StringLit { .. })) {
        let items: Vec<String> = variants
            .iter()
            .filter_map(|v| match v {
                TdType::StringLit { value, .. } => Some(js_string(value)),
                _ => None,
            })
            .collect();
        return Ok(format!("z.enum([{}])", items.join(", ")));
    }
    // Zod requires at least two variants for z.union; a one-variant
    // "union" collapses to the inner type.
    if variants.len() == 1 {
        return emit_zod(&variants[0], env, path);
    }
    let parts: Result<Vec<String>, CodegenError> = variants
        .iter()
        .enumerate()
        .map(|(i, v)| emit_zod(v, env, &push(path, &format!("|{i}"))))
        .collect();
    Ok(format!("z.union([{}])", parts?.join(", ")))
}

fn emit_intersection(
    parts: &[TdType],
    env: &TypeEnv,
    path: &str,
) -> Result<String, CodegenError> {
    // Zod exposes `.and(other)` for intersection. It fluently composes
    // but reads poorly for 3+ parts; render as a left-associative fold.
    // Single-part intersections (degenerate) collapse to the inner.
    if parts.len() == 1 {
        return emit_zod(&parts[0], env, path);
    }
    let rendered: Result<Vec<String>, CodegenError> = parts
        .iter()
        .enumerate()
        .map(|(i, p)| emit_zod(p, env, &push(path, &format!("&{i}"))))
        .collect();
    let rendered = rendered?;
    let mut iter = rendered.into_iter();
    let first = iter.next().unwrap();
    Ok(iter.fold(first, |acc, next| format!("{acc}.and({next})")))
}

fn emit_named_ref(
    name: &str,
    type_args: &[TdType],
    env: &TypeEnv,
    path: &str,
) -> Result<String, CodegenError> {
    // Locally-declared types are emitted by the module-level emitter as
    // `<Name>Schema` constants; we just reference them here. Type args
    // aren't instantiated — v1 Zod emission assumes zero-arg local
    // decls, which matches the pattern in real typedown documents.
    match env.lookup(name) {
        LookupResult::Decl(entry) => {
            if entry.decl.generics.is_empty() && type_args.is_empty() {
                Ok(format!("{name}Schema"))
            } else {
                // Generic local decls get inlined (instantiate, then
                // recurse). Rare in practice, but keeps us correct.
                let expanded = env.instantiate(&entry.decl, type_args);
                emit_zod(&expanded, env, path)
            }
        }
        // Stdlib builtins (Section, Prose, …) are content-shape and
        // aren't value types. They'd only leak here if a caller forgot
        // to strip them. Emit z.any() with a breadcrumb to help debug.
        LookupResult::Builtin(_) => Ok("z.any()".to_string()),
        LookupResult::Missing => Err(CodegenError::RenderFailed {
            path: path.to_string(),
            reason: format!("unknown type `{name}` in value position"),
        }),
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn prim_zod(p: TdPrim) -> &'static str {
    match p {
        TdPrim::String => "z.string()",
        TdPrim::Number => "z.number()",
        TdPrim::Boolean => "z.boolean()",
        TdPrim::Null => "z.null()",
        TdPrim::Any => "z.any()",
    }
}

/// Render a JS string literal. Escapes backslashes, double quotes,
/// and control characters so the output is safe to splice into source.
pub fn js_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// `camelCaseKey` / `snake_case_key` are bare; anything else gets
/// quoted so Zod accepts it.
pub fn js_object_key(name: &str) -> String {
    if is_bare_identifier(name) {
        name.to_string()
    } else {
        js_string(name)
    }
}

fn is_bare_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_' || first == '$') {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
}

fn fmt_number(n: f64) -> String {
    if n.fract() == 0.0 && n.is_finite() && n.abs() < 1e15 {
        // Prefer "42" over "42.0" for clean JS.
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

fn push(path: &str, segment: &str) -> String {
    if path.is_empty() {
        segment.to_string()
    } else {
        format!("{path}.{segment}")
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use td_core::SourceFile;
    use td_parse::parse_td_module;

    fn env_from(src: &str) -> TypeEnv {
        let file = SourceFile::new("t.td", src.to_string());
        let (module, _) = parse_td_module(src, &file, 0);
        let (env, _) = TypeEnv::build(&module, &file);
        env
    }

    fn ty(env: &TypeEnv, name: &str) -> TdType {
        match env.lookup(name) {
            LookupResult::Decl(e) => env.instantiate(&e.decl, &[]),
            _ => panic!("missing `{name}`"),
        }
    }

    fn emit(src: &str, name: &str) -> String {
        let env = env_from(src);
        emit_zod(&ty(&env, name), &env, "").unwrap()
    }

    #[test]
    fn primitives() {
        assert_eq!(emit("type T = string", "T"), "z.string()");
        assert_eq!(emit("type T = number", "T"), "z.number()");
        assert_eq!(emit("type T = boolean", "T"), "z.boolean()");
        assert_eq!(emit("type T = null", "T"), "z.null()");
        assert_eq!(emit("type T = any", "T"), "z.any()");
    }

    #[test]
    fn string_literal() {
        assert_eq!(emit(r#"type T = "nit""#, "T"), r#"z.literal("nit")"#);
    }

    #[test]
    fn number_literal_integer() {
        assert_eq!(emit("type T = 42", "T"), "z.literal(42)");
    }

    #[test]
    fn array() {
        assert_eq!(emit("type T = string[]", "T"), "z.array(z.string())");
    }

    #[test]
    fn tuple() {
        assert_eq!(
            emit("type T = [string, number]", "T"),
            "z.tuple([z.string(), z.number()])"
        );
    }

    #[test]
    fn empty_tuple() {
        assert_eq!(emit("type T = []", "T"), "z.tuple([])");
    }

    #[test]
    fn simple_object() {
        assert_eq!(
            emit("type T = { a: string, b: number }", "T"),
            "z.object({ a: z.string(), b: z.number() })"
        );
    }

    #[test]
    fn optional_field() {
        assert_eq!(
            emit("type T = { a: string, b?: number }", "T"),
            "z.object({ a: z.string(), b: z.number().optional() })"
        );
    }

    #[test]
    fn string_literal_union_becomes_enum() {
        assert_eq!(
            emit(r#"type T = "nit" | "suggestion" | "blocking""#, "T"),
            r#"z.enum(["nit", "suggestion", "blocking"])"#
        );
    }

    #[test]
    fn mixed_union_is_zunion() {
        assert_eq!(
            emit(r#"type T = string | number"#, "T"),
            "z.union([z.string(), z.number()])"
        );
    }

    #[test]
    fn object_intersection_uses_and() {
        let got = emit(
            r#"type A = { a: string }
               type B = { b: number }
               type T = A & B"#,
            "T",
        );
        assert_eq!(got, "ASchema.and(BSchema)");
    }

    #[test]
    fn named_ref_renders_as_schema_identifier() {
        let env = env_from(
            r#"type Comment = { body: string }
               type T = { comments: Comment[] }"#,
        );
        let got = emit_zod(&ty(&env, "T"), &env, "").unwrap();
        assert_eq!(got, "z.object({ comments: z.array(CommentSchema) })");
    }

    #[test]
    fn doc_comments_render_as_describe() {
        let env = env_from(
            r#"type T = {
                 /// The diff to review.
                 diff: string
               }"#,
        );
        let got = emit_zod(&ty(&env, "T"), &env, "").unwrap();
        assert!(got.contains(".describe(\"The diff to review.\")"), "got: {got}");
    }

    #[test]
    fn js_object_key_quotes_non_identifiers() {
        // Direct helper test — the td DSL doesn't permit string-keyed
        // fields, so we exercise `js_object_key` rather than parsing.
        assert_eq!(js_object_key("foo"), "foo");
        assert_eq!(js_object_key("_bar"), "_bar");
        assert_eq!(js_object_key("weird-key"), "\"weird-key\"");
        assert_eq!(js_object_key("123"), "\"123\"");
    }

    #[test]
    fn string_literal_escapes_quotes() {
        assert_eq!(
            emit(r#"type T = "say \"hi\"""#, "T"),
            r#"z.literal("say \"hi\"")"#
        );
    }

    #[test]
    fn missing_type_errors() {
        let env = env_from("type T = NonExistent");
        let err = emit_zod(&ty(&env, "T"), &env, "").unwrap_err();
        assert!(matches!(err, CodegenError::RenderFailed { .. }));
    }
}
