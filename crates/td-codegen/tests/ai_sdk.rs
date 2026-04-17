//! Integration tests for the AI SDK backend.
//!
//! These exercise the full pipeline end-to-end: parse markdown, run
//! typedown check, produce the generated TS source. They're the most
//! refactor-resistant tests because they treat the codegen as a black
//! box — any failure surfaces as "the output isn't what a consumer
//! would expect."

use td_codegen::{ai_sdk, CodegenError, LoadedDoc};
use td_core::SourceFile;

fn emit_from(src: &str, filename: &str) -> Result<String, CodegenError> {
    let file = SourceFile::new(filename, src.to_string());
    let loaded = LoadedDoc::from_source(file).expect("loads");
    ai_sdk::emit(&loaded.as_unit())
}

const MINIMAL_PROMPT: &str = r#"---
typedown: Doc
---

# Minimal

```td
import { Prompt } from "typedown/agents"
type In  = { x: string }
type Out = { y: number }
export type Doc = Prompt<In, Out>
```

## Role
r.

## Instructions
1. x

## Examples

### Example 1
**Input:** hi
**Output:** 1
"#;

#[test]
fn minimal_prompt_emits_expected_shape() {
    let ts = emit_from(MINIMAL_PROMPT, "minimal.md").expect("emits");

    // Schemas in the right order (no forward refs).
    let in_pos = ts.find("export const InSchema").expect("In schema");
    let out_pos = ts.find("export const OutSchema").expect("Out schema");
    assert!(in_pos < out_pos, "declaration order matters for Zod");

    // Zod content.
    assert!(ts.contains("export const InSchema = z.object({ x: z.string() });"));
    assert!(ts.contains("export const OutSchema = z.object({ y: z.number() });"));
    assert!(ts.contains("export type In = z.infer<typeof InSchema>;"));

    // Doc is NOT emitted as a value-shape schema (it contains Prompt).
    assert!(
        !ts.contains("DocSchema"),
        "`Doc` is a content shape, shouldn't produce a Zod schema; got: {ts}"
    );

    // Policy constant.
    assert!(ts.contains("export const MinimalPolicy = {"));
    assert!(ts.contains("allowedTools: [] as const"));
    assert!(ts.contains("reads: [] as const"));
    assert!(ts.contains("writes: [] as const"));

    // System prompt captured as a template string.
    assert!(ts.contains("export const MinimalSystem = `"));
    assert!(ts.contains("## Role"));
    assert!(ts.contains("## Instructions"));
    assert!(!ts.contains("```td"), "td fences must be stripped from the system prompt");

    // Invoke function with camelCase name from filename.
    assert!(ts.contains("export async function minimal("));
    assert!(ts.contains("input: In,"));
    assert!(ts.contains("): Promise<Out> {"));
    assert!(ts.contains("generateText({"));
    assert!(ts.contains("output: Output.object({ schema: OutSchema }),"));
}

#[test]
fn inline_io_types_synthesize_schema_names() {
    // Prompt args are bare object literals instead of named types.
    let src = r#"---
typedown: Doc
---

# X

```td
import { Prompt } from "typedown/agents"
export type Doc = Prompt<{ a: string }, { b: number }>
```

## Role
r.

## Instructions
1. x

## Examples

### Example 1
**Input:** hi
**Output:** 1
"#;
    let ts = emit_from(src, "inline_io.md").expect("emits");
    // PascalCase from file name drives the synthesized names.
    assert!(ts.contains("export const InlineIoInputSchema"));
    assert!(ts.contains("export const InlineIoOutputSchema"));
    assert!(ts.contains("input: InlineIoInput,"));
    assert!(ts.contains("): Promise<InlineIoOutput>"));
}

#[test]
fn effect_rows_surface_on_policy() {
    let src = r#"---
typedown: Doc
---

# R

```td
import { Prompt, Uses, Reads, Writes, Model, MaxTokens } from "typedown/agents"
type In  = { x: string }
type Out = { y: string }

export type Doc =
  & Prompt<In, Out>
  & Uses<["a", "b"]>
  & Reads<["./data/**"]>
  & Writes<[]>
  & Model<"m1" | "m2">
  & MaxTokens<2048>
```

## Role
r.

## Instructions
1. x

## Examples

### Example 1
**Input:** hi
**Output:** 1
"#;
    let ts = emit_from(src, "with_effects.md").expect("emits");
    assert!(ts.contains(r#"model: "m1" as const"#));
    assert!(ts.contains(r#"allowedModels: ["m1", "m2"] as const"#));
    assert!(ts.contains("maxOutputTokens: 2048"));
    assert!(ts.contains(r#"allowedTools: ["a", "b"] as const"#));
    assert!(ts.contains(r#"reads: ["./data/**"] as const"#));
    assert!(ts.contains("writes: [] as const"));
    // Tool-filtering branch must be present when uses are declared.
    assert!(ts.contains("Object.entries(options.tools).filter"));
}

#[test]
fn topo_sort_orders_dependencies_before_dependents() {
    // ReviewOutput references Comment which is declared later in source.
    // The emitter must emit CommentSchema before ReviewOutputSchema.
    let src = r#"---
typedown: Doc
---

# T

```td
import { Prompt } from "typedown/agents"
type ReviewInput  = { diff: string }
type ReviewOutput = { comments: Comment[] }
interface Comment { file: string, line: number }
export type Doc = Prompt<ReviewInput, ReviewOutput>
```

## Role
r.

## Instructions
1. x

## Examples

### Example 1
**Input:** hi
**Output:** 1
"#;
    let ts = emit_from(src, "topo.md").expect("emits");
    let comment_pos = ts.find("export const CommentSchema").expect("Comment");
    let output_pos = ts.find("export const ReviewOutputSchema").expect("Out");
    assert!(
        comment_pos < output_pos,
        "Comment must come before ReviewOutput to avoid TDZ error.\n{ts}"
    );
}

#[test]
fn pipeline_doc_is_unsupported_in_v1() {
    let src = r#"---
typedown: Pipeline
---

# P

```td
import { Prompt } from "typedown/agents"
import { Compose } from "typedown/workflows"

type A = { a: string }
type B = { b: string }
type Step = Prompt<A, B>

export type Pipeline = Compose<[Step]>
```
"#;
    let file = SourceFile::new("pipeline.md", src.to_string());
    let loaded = LoadedDoc::from_source(file).expect("loads");
    let err = ai_sdk::emit(&loaded.as_unit()).unwrap_err();
    match err {
        CodegenError::UnsupportedShape { backend, shape, .. } => {
            assert_eq!(backend, "ai-sdk");
            assert!(shape.contains("pipeline"), "shape: {shape}");
        }
        other => panic!("expected UnsupportedShape, got {other}"),
    }
}

#[test]
fn readme_doc_is_unsupported() {
    // A plain Readme has no Prompt<I, O> — the AI SDK backend can't
    // compile it.
    let src = r#"---
typedown: Readme
---

# r

```td
import { Readme } from "typedown/docs"
```

## Overview
overview.

## Installation
1. install

## Usage
use.
"#;
    let file = SourceFile::new("r.md", src.to_string());
    let loaded = LoadedDoc::from_source(file).expect("loads");
    let err = ai_sdk::emit(&loaded.as_unit()).unwrap_err();
    assert!(matches!(err, CodegenError::UnsupportedShape { .. }));
}

#[test]
fn broken_doc_fails_to_load() {
    let src = r#"---
typedown: CompletelyUnknown
---

# x

```td
```
"#;
    let file = SourceFile::new("broken.md", src.to_string());
    let err = LoadedDoc::from_source(file).unwrap_err();
    match err {
        td_codegen::LoadError::Check(diags) => {
            assert!(
                diags.iter().any(|d| d.contains("td403")),
                "expected td403: {diags:?}"
            );
        }
        other => panic!("expected Check error, got {other}"),
    }
}
