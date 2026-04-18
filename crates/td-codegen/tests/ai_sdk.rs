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

// ---------------------------------------------------------------------------
// Pipeline codegen
// ---------------------------------------------------------------------------

const PIPELINE_SRC: &str = r#"---
typedown: Pipeline
---

# Support pipeline

```td
import { Prompt, Uses, Reads, Model, MaxTokens } from "typedown/agents"
import { Compose } from "typedown/workflows"

type Query = { text: string }
type Class = { kind: string }
type Response = { answer: string }

type Classify = Prompt<Query, Class> & Uses<[]> & Model<"m1"> & MaxTokens<512>
type Answer   = Prompt<Class, Response> & Uses<["retrieve"]> & Reads<["./kb/**"]> & Model<"m2"> & MaxTokens<2048>

export type Pipeline =
  & Compose<[Classify, Answer]>
  & Uses<["retrieve"]>
  & Reads<["./kb/**"]>
  & Model<"m1" | "m2">
  & MaxTokens<4096>
```

## Overview

Route queries through classify then answer.

## Classify

You are the classifier. Emit Class.

## Answer

Given Class, produce Response.
"#;

#[test]
fn pipeline_emits_steps_policy_and_orchestrator() {
    let file = SourceFile::new("support_pipeline.md", PIPELINE_SRC.to_string());
    let loaded = LoadedDoc::from_source(file).expect("pipeline loads");
    let ts = ai_sdk::emit(&loaded.as_unit()).expect("pipeline compiles");

    // Shared local schemas, topo-sorted, emitted before any step block.
    assert!(ts.contains("export const QuerySchema"));
    assert!(ts.contains("export const ClassSchema"));
    assert!(ts.contains("export const ResponseSchema"));

    // Per-step policy + system + function for each step.
    assert!(ts.contains("export const ClassifyPolicy = {"));
    assert!(ts.contains("export const ClassifySystem = `"));
    assert!(ts.contains("export async function classify("));
    assert!(ts.contains("export const AnswerPolicy = {"));
    assert!(ts.contains("export const AnswerSystem = `"));
    assert!(ts.contains("export async function answer("));

    // Per-step system bodies are bucketed from the right heading.
    let classify_sys_start = ts.find("export const ClassifySystem").unwrap();
    let answer_sys_start = ts.find("export const AnswerSystem").unwrap();
    let classify_body = &ts[classify_sys_start..answer_sys_start];
    assert!(
        classify_body.contains("You are the classifier"),
        "classify system should contain classifier prose; got: {classify_body}"
    );
    assert!(
        !classify_body.contains("Given Class, produce"),
        "classify system must not include the answer heading body"
    );

    // Pipeline-level ceiling is emitted after the step blocks.
    assert!(ts.contains("export const SupportPipelinePolicy = {"));

    // Orchestrator chains classify → answer with typed I/O.
    let orch = ts.find("export async function supportPipeline").unwrap();
    let orch_body = &ts[orch..];
    assert!(
        orch_body.contains("input: Query,"),
        "orchestrator input type must be the first step's input"
    );
    assert!(
        orch_body.contains("): Promise<Response>"),
        "orchestrator output type must be the last step's output"
    );
    assert!(
        orch_body.contains("const step1Out = await classify(input, options);"),
        "orchestrator must call classify first with `input`"
    );
    assert!(
        orch_body.contains("const step2Out = await answer(step1Out, options);"),
        "orchestrator must thread step1 output into step2"
    );
    assert!(
        orch_body.contains("return step2Out;"),
        "orchestrator must return the final step's output"
    );
}

#[test]
fn pipeline_step_without_heading_gets_warning_comment() {
    // Step `Phantom` has no heading that contains its name — the
    // emitter should emit a comment noting the gap and fall back to
    // an empty system.
    let src = r#"---
typedown: Pipeline
---

# p

```td
import { Prompt, Model } from "typedown/agents"
import { Compose } from "typedown/workflows"

type A = { a: string }
type B = { b: string }

type Phantom = Prompt<A, B> & Model<"m">

export type Pipeline =
  & Compose<[Phantom]>
  & Model<"m">
```

## Unrelated heading

No mention of the step's name.
"#;
    let file = SourceFile::new("phantom_pipeline.md", src.to_string());
    let loaded = LoadedDoc::from_source(file).expect("loads");
    let ts = ai_sdk::emit(&loaded.as_unit()).expect("compiles");
    // Warning comment about the unmatched step.
    assert!(
        ts.contains("No `##` heading matched step `Phantom`"),
        "expected warning comment; got: {ts}"
    );
    // Empty template string.
    assert!(ts.contains("export const PhantomSystem = ``;"));
}

#[test]
fn empty_pipeline_is_unsupported() {
    // A Compose<[]> declaration is rejected at codegen time — there's
    // no Prompt<I, O> to derive the orchestrator's I/O from. (The type
    // checker doesn't currently forbid empty tuples; the codegen is
    // the layer that cares.)
    let src = r#"---
typedown: Pipeline
---

# empty

```td
import { Compose } from "typedown/workflows"

export type Pipeline = Compose<[]>
```
"#;
    let file = SourceFile::new("empty.md", src.to_string());
    let loaded = LoadedDoc::from_source(file).expect("loads");
    let err = ai_sdk::emit(&loaded.as_unit()).unwrap_err();
    assert!(matches!(err, CodegenError::UnsupportedShape { .. }));
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
