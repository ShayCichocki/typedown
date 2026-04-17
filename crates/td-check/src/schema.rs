//! Compile a resolved [`TdType`] to JSON Schema (Draft 2020-12).
//!
//! Schema export is the other half of "types are load-bearing": once the
//! checker is doing real value typing, we want to hand that same truth to
//! everyone *outside* typedown — CI validators, OpenAPI generators, LLM
//! tool definitions, Zod translators, etc. One source of truth, many sinks.
//!
//! # Design choices
//!
//! * **Inline, not `$ref`.** For v1 we inline every named-ref's instantiated
//!   body instead of producing a `$defs` table. Inlining keeps the output
//!   self-contained and easy to diff; we pay the price of duplicated
//!   subschemas. A future `--flatten-refs` flag can move to `$defs`.
//! * **Strict objects by default.** `additionalProperties: false` mirrors
//!   the checker's `td504` warning stance. Authors declare the shape; extra
//!   keys are schema violations.
//! * **Content-shape builtins become `true`.** `Section<T>`, `Prose`,
//!   `OrderedList` aren't value types — they describe markdown structure.
//!   Exporting them as "accept anything" is the least-surprising behavior
//!   when someone asks for a schema of a document type that contains them.
//! * **Draft 2020-12.** Stable, widely supported (ajv, python-jsonschema,
//!   Zod via `json-schema-to-zod`).

use serde_json::{json, Map, Value};
use td_ast::td::{TdObjectType, TdPrim, TdType};
use td_stdlib::Builtin;

use crate::env::{EntryOrigin, LookupResult, TypeEnv};

/// Render a type as a standalone JSON Schema document.
///
/// The returned `Value` includes `$schema` and — if available — a `title`
/// from the caller. Every user-declared (non-stdlib) type from the env is
/// emitted under `$defs` so downstream consumers can reference them
/// individually — this is where the real schema value lives for most
/// docs, since top-level `Prompt<I, O>` types describe markdown shape
/// rather than values.
///
/// Use [`to_subschema`] when embedding inside a larger schema, since only
/// the root should carry `$schema`, `title`, and `$defs`.
pub fn to_json_schema(ty: &TdType, env: &TypeEnv, title: Option<&str>) -> Value {
    let mut schema = to_subschema(ty, env);

    // Only the root object-schema is allowed to grow metadata keys. For
    // scalar / array / boolean schemas we wrap in an `allOf` so we can
    // still attach `$schema` + `title` without mangling the original.
    if !matches!(schema, Value::Object(_)) {
        schema = json!({ "allOf": [schema] });
    }

    if let Value::Object(ref mut map) = schema {
        map.insert(
            "$schema".into(),
            Value::String("https://json-schema.org/draft/2020-12/schema".into()),
        );
        if let Some(t) = title {
            map.insert("title".into(), Value::String(t.to_string()));
        }
        let defs = local_defs(env);
        if !defs.is_empty() {
            map.insert("$defs".into(), Value::Object(defs));
        }
    }
    schema
}

/// Compile every locally-declared (non-stdlib) type in the env into a
/// `$defs` map. Stdlib types like `Prompt`, `Section`, `Readme` are excluded
/// because emitting them inline would bloat the output without value —
/// they already expand fully wherever they're referenced.
fn local_defs(env: &TypeEnv) -> Map<String, Value> {
    let mut defs = Map::new();
    for (name, entry) in &env.entries {
        if !matches!(entry.origin, EntryOrigin::Local) {
            continue;
        }
        let body = env.instantiate(&entry.decl, &[]);
        defs.insert(name.clone(), to_subschema(&body, env));
    }
    defs
}

/// Compile a type to a JSON Schema fragment (no `$schema` key).
pub fn to_subschema(ty: &TdType, env: &TypeEnv) -> Value {
    match ty {
        TdType::Primitive { kind, .. } => match kind {
            TdPrim::String => json!({"type": "string"}),
            TdPrim::Number => json!({"type": "number"}),
            TdPrim::Boolean => json!({"type": "boolean"}),
            TdPrim::Null => json!({"type": "null"}),
            // `any` intentionally emits the always-true schema.
            TdPrim::Any => json!(true),
        },

        TdType::StringLit { value, .. } => json!({"const": value}),
        TdType::NumberLit { value, .. } => json!({"const": value}),

        TdType::Array { elem, .. } => json!({
            "type": "array",
            "items": to_subschema(elem, env),
        }),

        TdType::Object(obj) => object_schema(obj, env),

        TdType::Union { variants, .. } => union_schema(variants, env),

        TdType::Intersection { parts, .. } => json!({
            "allOf": parts.iter().map(|p| to_subschema(p, env)).collect::<Vec<_>>(),
        }),

        TdType::NamedRef {
            name, type_args, ..
        } => named_schema(name, type_args, env),
    }
}

fn object_schema(obj: &TdObjectType, env: &TypeEnv) -> Value {
    let mut properties = Map::new();
    let mut required = Vec::new();
    for field in &obj.fields {
        let mut sub = to_subschema(&field.ty, env);
        // Promote doc comments onto the field schema as `description`. A
        // tiny thing that makes the exported schema *much* friendlier for
        // tool spec consumers.
        if let Some(doc) = &field.doc {
            if let Value::Object(ref mut m) = sub {
                m.insert("description".into(), Value::String(doc.trim().to_string()));
            }
        }
        properties.insert(field.name.clone(), sub);
        if !field.optional {
            required.push(Value::String(field.name.clone()));
        }
    }
    let mut out = Map::new();
    out.insert("type".into(), Value::String("object".into()));
    out.insert("properties".into(), Value::Object(properties));
    if !required.is_empty() {
        out.insert("required".into(), Value::Array(required));
    }
    out.insert("additionalProperties".into(), Value::Bool(false));
    Value::Object(out)
}

/// Emit unions as either `enum` (when every variant is a string literal) or
/// `anyOf`. The enum path is important because it's what consumers recognize
/// as "this is a discriminated string set," the form LLM tool JSON expects.
fn union_schema(variants: &[TdType], env: &TypeEnv) -> Value {
    if variants
        .iter()
        .all(|v| matches!(v, TdType::StringLit { .. }))
    {
        let values: Vec<Value> = variants
            .iter()
            .filter_map(|v| match v {
                TdType::StringLit { value, .. } => Some(Value::String(value.clone())),
                _ => None,
            })
            .collect();
        return json!({"type": "string", "enum": values});
    }
    if variants
        .iter()
        .all(|v| matches!(v, TdType::NumberLit { .. }))
    {
        let values: Vec<Value> = variants
            .iter()
            .filter_map(|v| match v {
                TdType::NumberLit { value, .. } => serde_json::Number::from_f64(*value).map(Value::Number),
                _ => None,
            })
            .collect();
        return json!({"type": "number", "enum": values});
    }
    json!({
        "anyOf": variants.iter().map(|v| to_subschema(v, env)).collect::<Vec<_>>(),
    })
}

fn named_schema(name: &str, type_args: &[TdType], env: &TypeEnv) -> Value {
    match env.lookup(name) {
        LookupResult::Decl(entry) => {
            let expanded = env.instantiate(&entry.decl, type_args);
            to_subschema(&expanded, env)
        }
        LookupResult::Builtin(b) => builtin_schema(b, type_args, env),
        LookupResult::Missing => json!({
            // Preserve the reference for downstream tooling. Consumers see
            // an unresolved $ref and know something went wrong upstream.
            "$comment": format!("unresolved type `{name}`"),
        }),
    }
}

fn builtin_schema(b: Builtin, type_args: &[TdType], env: &TypeEnv) -> Value {
    match b {
        // Value-shaped builtin: Example<I, O> == { input: I, output: O }.
        Builtin::Example => {
            let i = type_args.first().cloned().unwrap_or(TdType::Primitive {
                span: td_core::Span::DUMMY,
                kind: TdPrim::Any,
            });
            let o = type_args.get(1).cloned().unwrap_or(TdType::Primitive {
                span: td_core::Span::DUMMY,
                kind: TdPrim::Any,
            });
            json!({
                "type": "object",
                "properties": {
                    "input":  to_subschema(&i, env),
                    "output": to_subschema(&o, env),
                },
                "required": ["input", "output"],
                "additionalProperties": false,
            })
        }
        // Content-shape builtins describe markdown, not values. The most
        // honest JSON Schema representation is "permits anything" plus a
        // comment so readers aren't confused about why their `Section<T>`
        // field accepts `42`.
        Builtin::Section
        | Builtin::Prose
        | Builtin::OrderedList
        | Builtin::UnorderedList
        | Builtin::TaskList
        | Builtin::CodeBlock
        | Builtin::Heading => json!({
            "$comment": format!(
                "`{}` is a markdown content-shape type; it has no value-level schema",
                b.display()
            ),
        }),
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

    fn type_of(env: &TypeEnv, name: &str) -> TdType {
        match env.lookup(name) {
            LookupResult::Decl(e) => env.instantiate(&e.decl, &[]),
            _ => panic!("missing `{name}`"),
        }
    }

    #[test]
    fn primitive_string() {
        let env = env_from("type T = string");
        let s = to_subschema(&type_of(&env, "T"), &env);
        assert_eq!(s, json!({"type": "string"}));
    }

    #[test]
    fn simple_object() {
        let env = env_from("type T = { x: string, y?: number }");
        let s = to_subschema(&type_of(&env, "T"), &env);
        assert_eq!(s["type"], json!("object"));
        assert_eq!(s["required"], json!(["x"]));
        assert_eq!(s["additionalProperties"], json!(false));
        assert_eq!(s["properties"]["x"], json!({"type": "string"}));
        assert_eq!(s["properties"]["y"], json!({"type": "number"}));
    }

    #[test]
    fn string_literal_union_as_enum() {
        let env = env_from(r#"type Sev = "nit" | "suggestion" | "blocking""#);
        let s = to_subschema(&type_of(&env, "Sev"), &env);
        assert_eq!(s["type"], json!("string"));
        assert_eq!(s["enum"], json!(["nit", "suggestion", "blocking"]));
    }

    #[test]
    fn array_of_named_type() {
        let env = env_from(
            r#"
            type Item = { id: number }
            type T = Item[]
            "#,
        );
        let s = to_subschema(&type_of(&env, "T"), &env);
        assert_eq!(s["type"], json!("array"));
        assert_eq!(s["items"]["type"], json!("object"));
        assert_eq!(s["items"]["required"], json!(["id"]));
    }

    #[test]
    fn any_is_true() {
        let env = env_from("type T = any");
        let s = to_subschema(&type_of(&env, "T"), &env);
        assert_eq!(s, json!(true));
    }

    #[test]
    fn intersection_becomes_allof() {
        let env = env_from(
            r#"
            type A = { a: string }
            type B = { b: number }
            type T = A & B
            "#,
        );
        let s = to_subschema(&type_of(&env, "T"), &env);
        assert!(s["allOf"].is_array());
        assert_eq!(s["allOf"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn root_schema_has_metadata() {
        let env = env_from("type T = { x: string }");
        let s = to_json_schema(&type_of(&env, "T"), &env, Some("Doc"));
        assert_eq!(s["title"], json!("Doc"));
        assert_eq!(
            s["$schema"],
            json!("https://json-schema.org/draft/2020-12/schema")
        );
    }
}
