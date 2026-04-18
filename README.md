# typedown

**Statically typed markdown.** A lint + type checker + contract runtime
for markdown files aimed at agent-facing documents (prompts, tool specs,
runbooks, `AGENTS.md`).

Markdown is load-bearing infrastructure for LLMs now, but it's `any`-typed.
typedown gives it types — and through **effect rows**, a capability
contract that a runtime can enforce and a code generator can compile
into executable TypeScript.

![typedown CLI output](docs/typedown.png)

---

## Contents

- [Quickstart](#quickstart)
- [Examples walkthrough](#examples-walkthrough)
- [Concept](#concept)
- [Typed example values](#typed-example-values)
- [Effect rows — prompts as contracts](#effect-rows--prompts-as-contracts)
- [Typed composition — pipelines](#typed-composition--pipelines-where-io-and-effects-flow-statically)
- [Compiling to Vercel AI SDK TypeScript](#compiling-to-runtime-code--vercel-ai-sdk)
- [Runtime enforcement](#runtime-enforcement-td-runtime)
- [CLI reference](#cli-reference)
- [Diagnostic codes](#diagnostic-codes)
- [Stdlib](#stdlib)
- [Layout](#layout)
- [Status & roadmap](#status)

---

## Quickstart

**Prerequisites:** Rust 1.80+ (stable). No npm / Node required to build
typedown — the CLI is a single Rust binary. Node is only needed when you
want to *run* the TypeScript the AI SDK backend generates.

```sh
git clone https://github.com/ShayCichocki/typedown
cd typedown

# Build the CLI (release mode for speed; omit --release while iterating).
cargo build --release -p td-cli

# Alias for convenience in the rest of this README.
alias typedown="$(pwd)/target/release/typedown"

# Lint every .md under examples/ — should report 5 files checked,
# 7 errors and 2 warnings across the two deliberately broken examples.
typedown check examples/
```

Or use `cargo run` directly if you don't want an alias:

```sh
cargo run --release -p td-cli -- check examples/
```

## Examples walkthrough

Five examples ship in [`examples/`](examples/), in increasing order of
ambition. Every command below is copy-paste runnable from the repo root.

### 1. `code_reviewer_prompt.md` — typed prompt with effect rows

A single-prompt document declaring `Prompt<ReviewInput, ReviewOutput>`
intersected with a full capability policy (`Uses`, `Reads`, `Writes`,
`Model`, `MaxTokens`).

```sh
# Conformance check — should be clean.
typedown check examples/code_reviewer_prompt.md

# Print the declared policy.
typedown effects examples/code_reviewer_prompt.md
#   uses:       read_file, run_tests
#   reads:      ./src/**, ./tests/**
#   writes:     ∅ (deny-all)
#   model:      claude-opus-4-5, claude-sonnet-4-5
#   max tokens: 4096

# Export to JSON Schema (with x-typedown-effects vendor extension).
typedown export examples/code_reviewer_prompt.md -o reviewer.schema.json

# Compile to Vercel AI SDK TypeScript.
typedown export --format ai-sdk \
  examples/code_reviewer_prompt.md -o reviewer.ts
```

The generated `reviewer.ts` is a ~110-line module you can import into
any Next.js / Node project that has `ai` and `zod` installed:

```ts
import { codeReviewerPrompt } from "./reviewer";
const result = await codeReviewerPrompt({ diff, context });
```

### 2. `code_reviewer_prompt_broken.md` — diagnostic showcase (prompt level)

A deliberately broken variant of the reviewer. Demonstrates four
diagnostic codes:

```sh
typedown check examples/code_reviewer_prompt_broken.md
```

Expected codes: **td502** (value-type mismatch — `"diff": 42` where a
string is declared), **td502** again (`"approved": "yes"` where a
boolean is declared), **td504** (extra field in the example output),
**td601** (malformed effect row — `MaxTokens<"nope">`), **td405**
(undeclared section).

These are catches the type system makes *before* you ever run the
prompt — five separate bug classes, pinpointed with miette-rendered
source spans.

### 3. `support_pipeline.md` — typed pipeline with composition algebra

A two-step pipeline (Classify → Answer) declared via
`Compose<[Classify, Answer]>`. Each step has its own policy; the
pipeline's declared effect rows form the ceiling.

```sh
# Clean — adjacent I/O matches, every child effect fits inside the parent.
typedown check examples/support_pipeline.md

# Inspect the declared pipeline structure.
typedown pipeline examples/support_pipeline.md
# examples/support_pipeline.md — pipeline (2 steps)
#   [1] Classify
#       input:      Query
#       output:     Classification
#       uses:       ∅ (deny-all)
#       model:      openai/gpt-4o-mini
#       max tokens: 512
#   [2] Answer
#       input:      Classification
#       output:     Response
#       uses:       retrieve_kb
#       reads:      ./kb/**
#       model:      anthropic/claude-sonnet-4.5
#       max tokens: 2048

# Full schema export including x-typedown-pipeline vendor extension.
typedown export examples/support_pipeline.md | jq .x-typedown-pipeline

# Compile the pipeline to a typed orchestrator function.
typedown export --format ai-sdk \
  examples/support_pipeline.md -o support_pipeline.ts
```

The generated `support_pipeline.ts` exposes per-step invocations
(`classify`, `answer`) AND an orchestrator that sequences them with
statically-typed I/O flow:

```ts
import { supportPipeline } from "./support_pipeline";
const response = await supportPipeline({ text, customerId });
// Internally: classify(input) → answer(classification) — each step's
// schemas and policy are enforced at its own boundary.
```

### 4. `support_pipeline_broken.md` — composition diagnostic showcase

The broken companion to `support_pipeline.md`. Violates every
composition rule at once: a step whose output doesn't match the next
step's input, a step whose `Uses` includes a tool the pipeline never
authorized, a step using a model not in the pipeline's allowlist, and
a step requesting more tokens than the pipeline permits.

```sh
typedown check examples/support_pipeline_broken.md
```

Expected codes: **td702** (I/O mismatch between adjacent steps),
**td703** (child `Uses` entry not in parent ceiling), **td704** (child
`Model` not in parent's allowlist), **td705** (child `MaxTokens`
exceeds parent). Four type-level bugs caught before execution.

### 5. `support_triage.md` + `support_triage.workflow.ts` — production-shape integration with Vercel Workflow SDK

A realistic customer-support triage. Three chonky LLM steps typed by
typedown (`Classify` → `Draft` → `Review`), plus three external
fetches orchestrated by [Vercel Workflow SDK](https://workflow-sdk.dev):
`fetchCustomer`, `fetchOrders`, `searchKb`. The two files compose into
a single **six-step durable workflow**:

```
┌─────────────────┐  ┌─────────────────┐  ┌─────────────────┐
│ fetchCustomer   │  │ fetchOrders     │  │ searchKb        │
│ (workflow-sdk)  │  │ (workflow-sdk)  │  │ (workflow-sdk)  │
└────────┬────────┘  └────────┬────────┘  └────────┬────────┘
         └───────────┬────────┴─────────┬──────────┘
                    ┌▼─────────┐  ┌─────▼──┐  ┌────────┐
                    │ Classify │→ │ Draft  │→ │ Review │      (typedown)
                    └──────────┘  └────────┘  └────────┘
                     (LLM)         (LLM)       (LLM)
```

The division of labour:

| concern | owner |
|---|---|
| LLM I/O schemas + pipeline I/O flow verification | **typedown** |
| Per-step effect-row policy (tools, models, token caps) | **typedown** |
| Durable retries, suspension, observability, resumption | **workflow-sdk** |
| External HTTP / DB / vector fetches | **workflow-sdk** |

Each step in the markdown file has a **~500 word system prompt**
covering voice, tone, grounding rules, escalation triggers, policy
rejection conditions, and output-field semantics — genuinely
production-shape instruction content, not placeholder prose.

```sh
# Check the typed pipeline.
typedown check examples/support_triage.md

# Inspect the 3 typedown-owned LLM steps.
typedown pipeline examples/support_triage.md
# examples/support_triage.md — pipeline (3 steps)
#   [1] Classify
#       input:      TriageInput
#       output:     Triaged
#       uses:       ∅ (deny-all)
#       model:      openai/gpt-4o-mini
#       max tokens: 1024
#   [2] Draft
#       input:      Triaged
#       output:     Drafted
#       uses:       search_kb, get_customer_macros
#       reads:      ./kb/**, ./macros/**
#       model:      anthropic/claude-sonnet-4.5
#       max tokens: 4096
#   [3] Review
#       input:      Drafted
#       output:     ReviewResult
#       ...

# Compile to TypeScript. The output is ~380 lines of fully typed
# Zod schemas + per-step invocation functions + orchestrator.
mkdir -p examples/generated
typedown export --format ai-sdk examples/support_triage.md \
  -o examples/generated/support_triage.ts
```

Then the companion
[`examples/support_triage.workflow.ts`](examples/support_triage.workflow.ts)
imports the typedown-generated symbols and wraps them in a
`"use workflow"` durable orchestrator:

```ts
import {
  classify, draft, review,
  type InboundMessage, type ReviewResult,
} from "./generated/support_triage";
import { FatalError } from "workflow";

export async function supportTriageWorkflow(
  message: InboundMessage,
): Promise<ReviewResult> {
  "use workflow";

  // 1–3: parallel external fetches, each a durable checkpoint.
  const [customer, orders, kbArticles] = await Promise.all([
    fetchCustomer(message.customerId),
    fetchOrders(message.customerId),
    searchKb(message.body),
  ]);

  // 4–6: typedown-verified LLM chain. Type flow was checked at
  // `typedown check` time, so `triaged → drafted → reviewed` can't
  // fail a schema check — only the LLM itself can, in which case
  // workflow-sdk retries automatically.
  const triaged  = await classifyStep({ message, customer, orders, kbArticles });
  const drafted  = await draftStep(triaged);
  const reviewed = await reviewStep(drafted);

  await logToCrm(message.messageId, message.customerId, { triaged, drafted, reviewed });
  if (reviewed.mustEscalate) await escalateToHuman(message.messageId, message.customerId, reviewed);

  return reviewed;
}
```

The companion file is ~280 lines of real orchestration code: CRM and
billing fetches, vector KB search, PagerDuty escalation, CRM ticket
logging with idempotency keys, a critical-path short-circuit that
routes severe escalations straight to a human before paying for the
draft+review LLM calls. Every external call declares
`"use step"` so workflow-sdk observes and retries each independently.

**The headline:** typedown and workflow-sdk compose in two lines
(an `import` and an `await`). typedown types the contracts;
workflow-sdk types the orchestration; together they describe an
agent system with compile-time correctness, runtime policy
enforcement, and durable execution end-to-end.

### Clean up

```sh
rm -f reviewer.schema.json reviewer.ts support_pipeline.ts
rm -rf examples/generated
```

---

## Concept

Declare a document's type in frontmatter and author types inline with a
TypeScript-flavored DSL in ``` ```td ``` fences. The document's type is
both a **content shape** (what sections and values belong) and a **policy**
(what tools it may invoke, what paths it may read/write, which models it
was validated against):

`````md
---
typedown: Doc
---

# Code Reviewer

```td
import {
  Prompt, Uses, Reads, Writes, Model, MaxTokens,
} from "typedown/agents"

type ReviewInput  = { diff: string, context: string }
type ReviewOutput = { approved: boolean, comments: Comment[] }

interface Comment {
  file: string
  line: number
  severity: "nit" | "suggestion" | "blocking"
  body: string
}

export type Doc =
  & Prompt<ReviewInput, ReviewOutput>
  & Uses<["read_file", "run_tests"]>
  & Reads<["./src/**", "./tests/**"]>
  & Writes<[]>
  & Model<"claude-opus-4-5" | "claude-sonnet-4-5">
  & MaxTokens<4096>
```

## Role
You are a rigorous reviewer…

## Instructions
1. …

## Examples

### Example 1

**Input:**

```json
{ "diff": "…", "context": "src/auth.ts" }
```

**Output:**

```json
{ "approved": false, "comments": [
  { "file": "src/auth.ts", "line": 42,
    "severity": "blocking", "body": "null check missing" }
] }
```
`````

Run `typedown check docs/` and the checker verifies:

- every field of the declared shape has a `##` heading
- `## Instructions` body is actually an ordered list
- `## Examples` contains `### Example N` sub-sections
- each example has `Input:` and `Output:` markers
- **every `json` / `yaml` value fence in an example is type-checked
  against the declared `I` / `O`** — wrong primitives, missing required
  fields, enum violations, malformed JSON all get pinpointed diagnostics
- no undeclared `##` sections slip in
- every effect row (`Uses`, `Reads`, `Writes`, `Model`, `MaxTokens`) is
  well-formed — tuple args of the right shape, number literals for
  ceilings, and so on

## Typed example values

`Example<I, O>` is load-bearing. Write your examples with `json` or
`yaml` value fences and typedown type-checks the payloads against
`I` / `O`:

````md
### Example 1

**Input:**

```json
{ "diff": "...", "context": "src/auth.ts" }
```

**Output:**

```yaml
approved: false
comments:
  - file: src/auth.ts
    line: 42
    severity: blocking
    body: null check missing
```
````

Prose-only examples (no value fences) continue to work — value typing is
strictly opt-in.

## Effect rows — prompts as contracts

A typed prompt's declared type can carry a **capability policy** alongside
its content shape. Five markers ship in `typedown/agents`:

| marker           | meaning                                                     |
|------------------|-------------------------------------------------------------|
| `Uses<T>`        | tuple of tool names this prompt may invoke                  |
| `Reads<T>`       | tuple of glob patterns this prompt may read                 |
| `Writes<T>`      | tuple of glob patterns this prompt may write                |
| `Model<T>`       | tuple or string-union of model identifiers it was validated against |
| `MaxTokens<N>`   | number literal: hard ceiling the runtime enforces           |

You opt in by intersecting them into the document's declared type:

```ts
export type Doc =
  & Prompt<In, Out>
  & Uses<["read_file", "run_tests"]>
  & Reads<["./src/**", "./tests/**"]>
  & Writes<[]>                                  // explicit: cannot write
  & Model<"claude-opus-4-5" | "claude-sonnet-4-5">
  & MaxTokens<4096>
```

Effects flow through every downstream tool:

- `typedown check` validates effect-row arguments (tuples of string
  literals, numbers for `MaxTokens`, etc.) — malformed rows fire **td601**.
- `typedown export` emits them as the `x-typedown-effects` vendor
  extension on the root JSON Schema, so downstream consumers (OpenAPI,
  provider tool-call specs, etc.) preserve the policy.
- `typedown effects <file>` prints the declared policy table.
- `td-runtime`'s `EnforcedPrompt` refuses unauthorized tool calls,
  reads, writes, models, and over-budget token requests, and validates
  concrete JSON input/output against `I` and `O`.
- `typedown export --format ai-sdk` folds the policy into a typed
  `<Title>Policy` constant in the generated TypeScript module.

## Typed composition — pipelines where I/O and effects flow statically

Single prompts are atoms; real agents are graphs of prompts calling
prompts calling tools. `Compose<[…]>` types that graph end-to-end:

```ts
import { Prompt, Uses, Model, MaxTokens } from "typedown/agents"
import { Compose } from "typedown/workflows"

type Classify =
  & Prompt<Query, Classification>
  & Uses<[]>
  & Model<"openai/gpt-4o-mini">
  & MaxTokens<512>

type Answer =
  & Prompt<Classification, Response>
  & Uses<["retrieve_kb"]>
  & Model<"anthropic/claude-sonnet-4.5">
  & MaxTokens<2048>

// The pipeline's type is both the plan AND the policy ceiling.
export type Pipeline =
  & Compose<[Classify, Answer]>
  & Uses<["retrieve_kb"]>
  & Model<"openai/gpt-4o-mini" | "anthropic/claude-sonnet-4.5">
  & MaxTokens<4096>
```

`typedown check` verifies:

1. **I/O flow.** `Classify`'s output (`Classification`) must be
   structurally equivalent to `Answer`'s input. Rename a field, change
   a type, reorder the steps without updating the adjacent one — you
   get a pinpointed **td702** on the PR that introduces it.
2. **Effect-row algebra.** Every child's effects must fit inside the
   pipeline's declared ceiling:
   - `∪ child.Uses   ⊆ parent.Uses`   (**td703**)
   - `∪ child.Reads  ⊆ parent.Reads`  (**td703**)
   - `∪ child.Writes ⊆ parent.Writes` (**td703**)
   - each `child.Model  ⊆ parent.Model` (**td704**)
   - each `child.MaxTokens ≤ parent.MaxTokens` (**td705**)

You cannot accidentally compose a subagent that uses a tool the parent
didn't authorize. The type system refuses before runtime.

Schema export embeds the full pipeline as an `x-typedown-pipeline`
vendor extension alongside `x-typedown-effects`, so orchestrators
(AI SDK codegen, Workflow DevKit, a custom runner) get stepwise I/O +
per-step policy in one document.

## Compiling to runtime code — Vercel AI SDK

Typedown documents compile to executable TypeScript targeting the
[Vercel AI SDK](https://ai-sdk.dev). The backend handles two shapes:

### Single prompts

`Prompt<I, O>` docs become a ready-to-import module with:

* A **Zod schema** and inferred TypeScript type for every declared
  value-shape type (topologically sorted so forward references are
  impossible).
* A **policy constant** derived from the effect rows — the same
  capability data `td-runtime` enforces server-side.
* The rendered **system prompt** as a template string (markdown body
  minus the `td` fences).
* An **async invocation function** wrapping `generateText` with
  structured output (`Output.object({ schema })`) and runtime tool
  allowlist filtering.

```sh
typedown export --format ai-sdk examples/code_reviewer_prompt.md \
  -o generated/reviewer.ts
```

```ts
import { codeReviewerPrompt } from "./generated/reviewer";
const result = await codeReviewerPrompt({ diff, context });
```

### Pipelines

`Compose<[…]>` docs become a module with **one invocation function
per step** plus a **typed orchestrator** that chains them:

```sh
typedown export --format ai-sdk examples/support_pipeline.md \
  -o generated/support_pipeline.ts
```

```ts
import { supportPipeline, classify, answer } from "./generated/support_pipeline";

// Run the full pipeline:
const response = await supportPipeline({ text, customerId });

// Or drive individual steps directly — still with full type safety:
const classification = await classify({ text, customerId });
const answered = await answer(classification);
```

The orchestrator reads like hand-written code:

```ts
export async function supportPipeline(
  input: Query,
  options?: { tools?: Record<string, Tool>; abortSignal?: AbortSignal },
): Promise<Response> {
  const step1Out = await classify(input, options);
  const step2Out = await answer(step1Out, options);
  return step2Out;
}
```

Each step's system prompt is bucketed from the markdown body by
matching level-2 headings against the step's type-alias name (case-
insensitive substring, longest-name wins). `## Step 1 — Classify`
feeds the `Classify` step; `## Step 2 — Answer` feeds `Answer`; an
unmatched step gets an empty system prompt plus a comment noting the
gap so the author can fix the doc.

### Guarantees

The generated module is plain TypeScript — no runtime dependency on
typedown or Rust. The compile target preserves every constraint the
type system verified:

* `allowedTools` const is a `readonly` string-literal tuple TypeScript
  can exhaustively check at compile time.
* `Model<"a" | "b">` becomes `allowedModels: ["a", "b"]` with `model`
  defaulting to the first member.
* `Writes<[]>` emits `writes: []` — the same deny-all signal the
  runtime honors.
* Pipeline orchestrators chain steps in declared order; because
  adjacent-step I/O was verified at `typedown check` time (td702), no
  runtime validation is needed between steps.
* Codegen **refuses to run** if the document wouldn't pass `typedown
  check`. Shipping TS generated from a broken contract just moves the
  failure downstream.

## Runtime enforcement (`td-runtime`)

When you want server-side enforcement in Rust rather than compiled-TS,
use `td-runtime` directly:

```rust
use td_runtime::EnforcedPrompt;

let prompt = EnforcedPrompt::load("prompts/reviewer.md")?;

// Before letting the model invoke a tool, ask the contract.
prompt.authorize_tool("read_file")?;                    // Ok
prompt.authorize_tool("shell_exec").unwrap_err();       // deny

// Path policy is compiled to a GlobSet at load time.
prompt.authorize_read("./src/auth/user.ts")?;           // Ok
prompt.authorize_write("./src/auth/user.ts").unwrap_err(); // Writes<[]>

// Input / output validation uses the same judgement as `typedown check`.
prompt.validate_input(&serde_json::json!({
    "diff": "...", "context": "src/auth.ts",
}))?;

prompt.check_token_limit(4096)?;                        // Ok
prompt.check_token_limit(4097).unwrap_err();            // over ceiling
```

`EnforcedPrompt::load` **refuses to construct** if the doc wouldn't pass
`typedown check`. Silently enforcing an empty policy on a broken contract
is a security anti-pattern.

## CLI reference

```sh
typedown check <paths…>              # lint one or more files / directories
typedown types                       # print stdlib modules
typedown effects <file> [--json]     # print the declared policy
typedown pipeline <file> [--json]    # print pipeline structure
typedown export <file> [-o <out>]    # JSON Schema (default)
typedown export --format ai-sdk <file> [-o <out>]
                                     # Vercel AI SDK TypeScript module
```

Every command honors `.gitignore` when walking directories.

## Diagnostic codes

| code   | severity | meaning                                          |
|--------|----------|--------------------------------------------------|
| td101  | error    | syntax error in ` ```td ` fence                  |
| td201  | error    | duplicate type declaration                       |
| td202  | error    | imported module not found                        |
| td203  | error    | symbol not exported from module                  |
| td299  | error    | internal: stdlib module failed to parse          |
| td301  | warning  | frontmatter missing `typedown:` field            |
| td401  | error    | required section is missing                      |
| td402  | error    | section body does not match expected type        |
| td403  | error    | unknown type referenced in declaration           |
| td404  | error    | document type must be an object                  |
| td405  | warning  | undeclared section present in document           |
| td501  | error    | value fence failed to parse (JSON / YAML syntax) |
| td502  | error    | value does not match declared type               |
| td504  | warning  | value has extra field not declared in the type   |
| td601  | error    | malformed effect row (bad argument shape)        |
| td701  | error    | pipeline step doesn't resolve to `Prompt<I, O>`  |
| td702  | error    | pipeline I/O mismatch (Out[N] ≠ In[N+1])         |
| td703  | error    | child Uses/Reads/Writes not in pipeline ceiling  |
| td704  | error    | child `Model<>` not in pipeline's model set      |
| td705  | error    | child `MaxTokens<>` exceeds pipeline ceiling     |

## Stdlib

Three modules ship out of the box:

- **`typedown/agents`** —
  - Content types: `Prompt<I, O>`, `Tool<A, R>`, `Runbook`, `Example<I, O>`
  - Effect rows: `Uses<T>`, `Reads<T>`, `Writes<T>`, `Model<T>`, `MaxTokens<N>`
- **`typedown/docs`** — `Readme`, `AgentsMd`
- **`typedown/workflows`** — `Compose<Steps>`, `Sequential<Steps>`
  (typed multi-step pipelines with effect-row algebra)

Plus implicit content-shape primitives usable without import:
`Section<T>`, `Prose`, `OrderedList`, `UnorderedList`, `TaskList`,
`CodeBlock<Lang>`, `Heading<Level>`.

## Layout

```
crates/
  td-core/     diagnostics + spans
  td-ast/      markdown & td-DSL ASTs
  td-parse/    markdown parser + td-DSL parser
  td-check/    type env, conformance, value typing, effect rows,
               pipeline composition, JSON Schema
  td-stdlib/   built-in types (Section, Prose, Prompt, Tool, Runbook,
               Uses, Reads, Writes, Model, MaxTokens, Compose, …)
  td-runtime/  EnforcedPrompt: refuse unauthorized tool calls / reads / writes
               at runtime from a typed-markdown contract; expose pipeline
               structure to orchestrators
  td-codegen/  compile typed docs to runtime code (Vercel AI SDK
               backend — TypeScript + Zod — today)
  td-cli/      `typedown` binary: check / types / export / effects / pipeline
```

## Status

Shipping today:

- Markdown + td-DSL parsing (intersections, unions, tuples, generics,
  TS-style leading `&` / `|` operators)
- Generic instantiation & intersection flattening
- Full conformance check for `Prompt<I, O>` and `Readme` / `AgentsMd`
- **Value typing**: JSON / YAML fences inside `Example<I, O>` are parsed
  and checked against `I` / `O` — generic parameters are no longer phantom
- **Schema export**: `typedown export` emits JSON Schema (Draft 2020-12)
  with every local type declaration under `$defs`, effect rows under
  `x-typedown-effects`, and pipelines under `x-typedown-pipeline`
- **Effect rows**: `Uses<>`, `Reads<>`, `Writes<>`, `Model<>`,
  `MaxTokens<>` intersected into the doc's type declare a capability
  policy surface
- **Typed composition**: `Compose<[A, B, C]>` statically checks
  adjacent-step I/O flow AND enforces effect-row subset algebra so
  children must fit inside the pipeline's ceiling
- **`td-runtime`**: `EnforcedPrompt::load` refuses unauthorized tool
  calls, reads, writes, models, and over-budget token requests;
  validates concrete JSON I/O against `I` and `O`; exposes pipeline
  structure for orchestrators
- **`td-codegen` / AI SDK backend**: `typedown export --format ai-sdk`
  compiles typed prompts AND typed pipelines to ready-to-import
  Vercel AI SDK TypeScript modules (Zod schemas, policy constants,
  tool allowlist filtering, `generateText` wrapper, and — for
  pipelines — per-step invocations plus a typed orchestrator)
- CLI: `check` / `types` / `export` / `effects` / `pipeline`
- **126 tests across 8 crates**

Roadmap:

- Additional codegen backends: Workflow DevKit, Anthropic tool JSON,
  OpenAI function specs, Pydantic
- `td diff` for semver-style compatibility checks between doc versions
- Reference Anthropic / OpenAI client wrappers that consume `EnforcedPrompt`
- LSP server (`td-lsp`) for in-editor diagnostics
- User-authored `.td` modules via import paths
- Executable code-fence checking (tsc / rustc / shellcheck on blocks)
- Watch mode, incremental parsing, formatter

## License

MIT OR Apache-2.0
