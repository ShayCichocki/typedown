---
typedown: Pipeline
---

# Customer Support Pipeline

A two-step agent pipeline: classify the customer query, then answer it.
Typedown verifies that Step 1's output type matches Step 2's input type,
and that every tool / model / token-ceiling declared on a child step
fits within the pipeline's overall policy. Change any step's I/O without
updating its neighbor, or let a step authorize a tool the pipeline
didn't, and `typedown check` fails before a single token is spent.

```td
import {
  Prompt,
  Uses,
  Reads,
  Writes,
  Model,
  MaxTokens,
} from "typedown/agents"
import { Compose } from "typedown/workflows"

// ── Wire types ────────────────────────────────────────────────────────

type Query = {
  text: string
  customerId: string
}

type Classification = {
  intent: "general" | "refund" | "technical"
  urgency: "low" | "medium" | "high"
  reasoning: string
}

type Response = {
  answer: string
  handoffRequired: boolean
}

// ── Step types ────────────────────────────────────────────────────────

// Classifier uses a small fast model and no tools.
type Classify =
  & Prompt<Query, Classification>
  & Uses<[]>
  & Model<"openai/gpt-4o-mini">
  & MaxTokens<512>

// Answerer may retrieve knowledge-base articles and uses the large model.
type Answer =
  & Prompt<Classification, Response>
  & Uses<["retrieve_kb"]>
  & Reads<["./kb/**"]>
  & Model<"anthropic/claude-sonnet-4.5">
  & MaxTokens<2048>

// ── The pipeline: composition + policy ceiling ────────────────────────

// The pipeline is the policy ceiling. Every child step's Uses / Reads /
// Writes / Model / MaxTokens must fit inside what's declared here.
export type Pipeline =
  & Compose<[Classify, Answer]>
  & Uses<["retrieve_kb"]>
  & Reads<["./kb/**"]>
  & Writes<[]>
  & Model<"openai/gpt-4o-mini" | "anthropic/claude-sonnet-4.5">
  & MaxTokens<4096>
```

## Overview

Given a customer query, produce a structured response. The classifier
decides intent and urgency; the answerer drafts the response with
optional retrieval from the knowledge base.

## Step 1 — Classify

Read the customer's message and emit a classification. Do not attempt to
answer the question at this stage. Output JSON matching the declared
`Classification` shape.

## Step 2 — Answer

Given the classification, draft a customer-facing answer. You MAY call
`retrieve_kb` to ground your response in documented policy, but MUST NOT
call any other tool. If the intent is `refund` and urgency is `high`,
set `handoffRequired` to `true`.
