---
typedown: Pipeline
---

# Broken pipeline — demonstrates composition diagnostics

This example is intentionally broken on four axes. It's the companion
to `support_pipeline.md` and exists to show what `typedown check` says
when a pipeline's type-level contract is violated. Run:

```sh
typedown check examples/support_pipeline_broken.md
```

You should see **td702** (I/O mismatch), **td703** (tool not in
parent's ceiling), **td704** (model not in parent's set), and
**td705** (token ceiling exceeded) — four pinpointed diagnostics
before any token is spent.

```td
import {
  Prompt,
  Uses,
  Model,
  MaxTokens,
} from "typedown/agents"
import { Compose } from "typedown/workflows"

type A = { a: string }
type B = { b: string }
type C = { c: string }

// Violations in Step1:
//   * `Uses<["shell_exec"]>` — parent only allows `retrieve_kb`.
//   * `Model<"xai/grok-5">`  — parent whitelists a different pair.
//   * `MaxTokens<8192>`      — parent's ceiling is 4096.
type Step1 =
  & Prompt<A, B>
  & Uses<["shell_exec"]>
  & Model<"xai/grok-5">
  & MaxTokens<8192>

// Violation in the pipeline itself:
//   * Step1 outputs `B`, but Step2 claims to take `C`.
//     (td702 — pipeline I/O mismatch between adjacent steps.)
type Step2 =
  & Prompt<C, A>
  & Uses<[]>

export type Pipeline =
  & Compose<[Step1, Step2]>
  & Uses<["retrieve_kb"]>
  & Model<"openai/gpt-4o-mini" | "anthropic/claude-sonnet-4.5">
  & MaxTokens<4096>
```

## Overview

Intentionally broken to showcase the full composition diagnostic set.
Fix any one violation by bringing the child step's declaration into
line with the pipeline's ceiling — or widening the pipeline if the
child's capability requirements are legitimate.
