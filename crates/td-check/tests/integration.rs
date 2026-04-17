//! End-to-end tests: feed real markdown through `check_source` and assert
//! on the diagnostic codes produced. These are the tests most resistant to
//! refactors because they exercise the full pipeline.

use td_check::{check_source, resolve_doc_type, to_json_schema};
use td_core::SourceFile;

fn check(name: &str, src: &str) -> Vec<String> {
    let file = SourceFile::new(name, src.to_string());
    let (_doc, diags) = check_source(&file);
    diags.iter().map(|d| d.code.clone()).collect()
}

const GOOD_PROMPT: &str = r#"---
typedown: Prompt<In, Out>
---

# Reviewer

```td
import { Prompt, Section, Prose, OrderedList, Example } from "typedown/agents"

type In  = { x: string }
type Out = { y: string }

export type Doc = Prompt<In, Out>
```

## Role
Text here.

## Instructions

1. do a thing
2. then another

## Examples

### Example 1
**Input:** x
**Output:** y
"#;

#[test]
fn good_prompt_has_no_diagnostics() {
    let codes = check("good.md", GOOD_PROMPT);
    assert!(codes.is_empty(), "expected no diagnostics, got {codes:?}");
}

#[test]
fn missing_section_fires_td401() {
    // Remove ## Examples.
    let src = GOOD_PROMPT.replace(
        "## Examples\n\n### Example 1\n**Input:** x\n**Output:** y\n",
        "",
    );
    let codes = check("missing.md", &src);
    assert!(codes.contains(&"td401".to_string()), "codes: {codes:?}");
}

#[test]
fn unknown_section_fires_td405_warning() {
    let src = format!(
        "{GOOD_PROMPT}\n\n## Unexpected\n\nhmm\n"
    );
    let codes = check("extra.md", &src);
    assert!(codes.contains(&"td405".to_string()), "codes: {codes:?}");
}

#[test]
fn array_section_without_subsections_fires_td402() {
    let src = GOOD_PROMPT.replace(
        "### Example 1\n**Input:** x\n**Output:** y\n",
        "nothing here\n",
    );
    let codes = check("no_subs.md", &src);
    assert!(codes.contains(&"td402".to_string()), "codes: {codes:?}");
}

#[test]
fn unknown_type_fires_td403() {
    let src = r#"---
typedown: DoesNotExist
---

# Hi

```td
```
"#;
    let codes = check("unknown.md", src);
    assert!(codes.contains(&"td403".to_string()), "codes: {codes:?}");
}

#[test]
fn unknown_module_fires_td202() {
    let src = r#"---
typedown: Foo
---

# x

```td
import { Foo } from "not/real"
type Doc = { bar: string }
```

## Bar
text
"#;
    let codes = check("bad_import.md", src);
    assert!(codes.contains(&"td202".to_string()), "codes: {codes:?}");
}

#[test]
fn file_without_frontmatter_is_silent() {
    let codes = check("no_fm.md", "# Just a doc\n\nsome text\n");
    assert!(codes.is_empty(), "codes: {codes:?}");
}

#[test]
fn file_with_empty_frontmatter_warns_td301() {
    let codes = check("empty_fm.md", "---\ntitle: foo\n---\n# doc\n");
    assert!(codes.contains(&"td301".to_string()), "codes: {codes:?}");
}

// ---------------------------------------------------------------------------
// Value typing: the type parameters on `Example<I, O>` actually constrain
// the JSON / YAML fences inside the example body.
// ---------------------------------------------------------------------------

const GOOD_TYPED_EXAMPLE: &str = r#"---
typedown: Prompt<In, Out>
---

# Reviewer

```td
import { Prompt } from "typedown/agents"

type In  = { diff: string, context: string }
type Out = { approved: boolean, severity: "nit" | "suggestion" | "blocking" }

export type Doc = Prompt<In, Out>
```

## Role
Text here.

## Instructions

1. do a thing

## Examples

### Example 1

**Input:**

```json
{ "diff": "a", "context": "b" }
```

**Output:**

```json
{ "approved": true, "severity": "nit" }
```
"#;

#[test]
fn typed_example_passes_when_values_conform() {
    let codes = check("good_values.md", GOOD_TYPED_EXAMPLE);
    assert!(codes.is_empty(), "expected clean, got {codes:?}");
}

#[test]
fn wrong_primitive_in_input_fires_td502() {
    // diff is declared string, pass a number instead.
    let src = GOOD_TYPED_EXAMPLE.replace(
        r#"{ "diff": "a", "context": "b" }"#,
        r#"{ "diff": 42, "context": "b" }"#,
    );
    let codes = check("bad_prim.md", &src);
    assert!(codes.contains(&"td502".to_string()), "codes: {codes:?}");
}

#[test]
fn missing_required_field_fires_td502() {
    // drop `context` from the input.
    let src = GOOD_TYPED_EXAMPLE.replace(
        r#"{ "diff": "a", "context": "b" }"#,
        r#"{ "diff": "a" }"#,
    );
    let codes = check("missing_field.md", &src);
    assert!(codes.contains(&"td502".to_string()), "codes: {codes:?}");
}

#[test]
fn extra_field_in_value_warns_td504() {
    let src = GOOD_TYPED_EXAMPLE.replace(
        r#"{ "diff": "a", "context": "b" }"#,
        r#"{ "diff": "a", "context": "b", "note": "hi" }"#,
    );
    let codes = check("extra_field.md", &src);
    assert!(codes.contains(&"td504".to_string()), "codes: {codes:?}");
}

#[test]
fn value_outside_string_literal_enum_fires_td502() {
    let src = GOOD_TYPED_EXAMPLE.replace(r#""severity": "nit""#, r#""severity": "critical""#);
    let codes = check("bad_enum.md", &src);
    assert!(codes.contains(&"td502".to_string()), "codes: {codes:?}");
}

#[test]
fn malformed_json_fires_td501() {
    let src = GOOD_TYPED_EXAMPLE.replace(
        r#"{ "diff": "a", "context": "b" }"#,
        r#"{ "diff": "a", "context":"#,
    );
    let codes = check("bad_json.md", &src);
    assert!(codes.contains(&"td501".to_string()), "codes: {codes:?}");
}

#[test]
fn yaml_fence_also_typechecks() {
    let src = GOOD_TYPED_EXAMPLE.replace(
        "```json\n{ \"diff\": \"a\", \"context\": \"b\" }\n```",
        "```yaml\ndiff: a\ncontext: b\n```",
    );
    let codes = check("yaml.md", &src);
    assert!(codes.is_empty(), "expected clean, got {codes:?}");
}

#[test]
fn prose_only_example_still_passes_v0_style() {
    // No value fences — falls back to the substring structural check.
    let codes = check(
        "prose.md",
        r#"---
typedown: Prompt<In, Out>
---

# Reviewer

```td
import { Prompt } from "typedown/agents"
type In  = { x: string }
type Out = { y: string }
export type Doc = Prompt<In, Out>
```

## Role
r.

## Instructions
1. x

## Examples

### Example 1

**Input:** prose description of the input.

**Output:** prose description of the output.
"#,
    );
    assert!(codes.is_empty(), "codes: {codes:?}");
}

// ---------------------------------------------------------------------------
// Schema export: typed docs compile to JSON Schema.
// ---------------------------------------------------------------------------

#[test]
fn schema_export_roundtrips_example_shape() {
    let file = SourceFile::new("doc.md", GOOD_TYPED_EXAMPLE.to_string());
    let (_doc, env, ty, _diags) = resolve_doc_type(&file);
    let schema = to_json_schema(&ty.expect("doc type"), &env, Some("Doc"));
    assert_eq!(schema["title"], serde_json::json!("Doc"));
    assert_eq!(schema["type"], serde_json::json!("object"));
    // Prompt<In, Out> expands to { role, instructions, examples }.
    let props = schema["properties"].as_object().expect("props");
    assert!(props.contains_key("role"), "schema: {schema}");
    assert!(props.contains_key("instructions"), "schema: {schema}");
    assert!(props.contains_key("examples"), "schema: {schema}");
}

#[test]
fn schema_export_includes_local_defs() {
    let file = SourceFile::new("doc.md", GOOD_TYPED_EXAMPLE.to_string());
    let (_doc, env, ty, _diags) = resolve_doc_type(&file);
    let schema = to_json_schema(&ty.expect("doc type"), &env, Some("Doc"));
    let defs = schema["$defs"].as_object().expect("$defs on root");
    // Both user-declared types should surface as usable value schemas.
    let in_schema = &defs["In"];
    assert_eq!(in_schema["type"], serde_json::json!("object"));
    assert_eq!(in_schema["required"], serde_json::json!(["diff", "context"]));
    let out_schema = &defs["Out"];
    assert_eq!(out_schema["properties"]["severity"]["enum"],
        serde_json::json!(["nit", "suggestion", "blocking"]));
}
