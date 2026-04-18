// ────────────────────────────────────────────────────────────────────────
//  support_triage.workflow.ts
//
//  Six-step durable workflow built on top of the LLM pipeline typedown
//  generated from `support_triage.md`. This file is the *runtime*; the
//  markdown is the *contract*.
//
//  Regenerate the typedown half with:
//      typedown export --format ai-sdk examples/support_triage.md \
//        -o examples/generated/support_triage.ts
//
//  The six "steps" workflow-sdk reports on its observability dashboard:
//
//    ┌────────────────────────────┐
//    │  1. fetchCustomer          │ ───┐
//    ├────────────────────────────┤    │  parallel — each runs as its
//    │  2. fetchOrders            │ ───┤  own retriable checkpoint
//    ├────────────────────────────┤    │
//    │  3. searchKb               │ ───┘
//    ├────────────────────────────┤
//    │  4. classify   (typedown)  │   sequential — typedown's
//    │  5. draft      (typedown)  │   compose-verified I/O flow
//    │  6. review     (typedown)  │
//    ├────────────────────────────┤
//    │     logToCrm (housekeeping)│
//    └────────────────────────────┘
//
//  Why this split?
//
//  * typedown owns the LLM *contracts* — per-step Zod schemas, tool
//    allowlists, model / token ceilings, structurally-verified I/O
//    flow between the three LLM steps. That file is declarative and
//    regeneratable. It never touches your network.
//
//  * workflow-sdk owns the *orchestration* — durability, retries,
//    suspension, resumption, observability, parallelism, and every
//    step that talks to an external system. It knows nothing about
//    typedown's type-system; it just sees three async functions to
//    await.
//
//  The composition is literally a two-line import. No adapter,
//  no runtime bridge, no wrapper crate.
// ────────────────────────────────────────────────────────────────────────

import { FatalError } from "workflow";

// The four symbols below come from typedown codegen. Run
// `typedown export --format ai-sdk` to regenerate them. Every type
// and function used here was structurally verified by `typedown
// check` when the markdown was written.
import {
  classify,
  draft,
  review,
  type InboundMessage,
  type CustomerContext,
  type OrderSummary,
  type KbArticle,
  type Triaged,
  type Drafted,
  type ReviewResult,
} from "./generated/support_triage";

// ═══════════════════════════════════════════════════════════════════════
//  External-system steps — one "use step" per durable unit
// ═══════════════════════════════════════════════════════════════════════

/**
 * Fetch the customer's profile from the CRM. Retriable on 5xx; bails
 * with `FatalError` on 404 so workflow-sdk stops retrying a
 * fundamentally-broken request.
 */
async function fetchCustomer(customerId: string): Promise<CustomerContext> {
  "use step";

  const res = await fetch(
    `${process.env.CRM_URL}/customers/${encodeURIComponent(customerId)}`,
    {
      headers: { Authorization: `Bearer ${process.env.CRM_TOKEN!}` },
    },
  );
  if (res.status === 404) {
    throw new FatalError(`no such customer: ${customerId}`);
  }
  if (!res.ok) {
    // Non-fatal — workflow-sdk will retry.
    throw new Error(`CRM fetch failed: ${res.status} ${res.statusText}`);
  }
  return (await res.json()) as CustomerContext;
}

/**
 * Fetch recent order and billing summary from the billing service.
 */
async function fetchOrders(customerId: string): Promise<OrderSummary> {
  "use step";

  const res = await fetch(
    `${process.env.BILLING_URL}/customers/${encodeURIComponent(customerId)}/summary`,
    {
      headers: { Authorization: `Bearer ${process.env.BILLING_TOKEN!}` },
    },
  );
  if (!res.ok) {
    throw new Error(`billing fetch failed: ${res.status} ${res.statusText}`);
  }
  return (await res.json()) as OrderSummary;
}

/**
 * Vector search against the KB. Returns the top-5 articles ranked by
 * embedding similarity to the customer's message body.
 */
async function searchKb(query: string): Promise<KbArticle[]> {
  "use step";

  const res = await fetch(`${process.env.KB_URL}/search`, {
    method: "POST",
    headers: {
      Authorization: `Bearer ${process.env.KB_TOKEN!}`,
      "Content-Type": "application/json",
    },
    body: JSON.stringify({ query, topK: 5 }),
  });
  if (!res.ok) {
    throw new Error(`KB search failed: ${res.status}`);
  }
  const { results } = (await res.json()) as { results: KbArticle[] };
  return results;
}

/**
 * Persist the final triage record to the CRM ticket log. Idempotent
 * on `message.messageId` — safe to retry.
 */
async function logToCrm(
  messageId: string,
  customerId: string,
  record: { triaged: Triaged; drafted: Drafted; reviewed: ReviewResult },
): Promise<void> {
  "use step";

  const res = await fetch(
    `${process.env.CRM_URL}/customers/${encodeURIComponent(customerId)}/tickets`,
    {
      method: "POST",
      headers: {
        Authorization: `Bearer ${process.env.CRM_TOKEN!}`,
        "Content-Type": "application/json",
        "Idempotency-Key": `triage:${messageId}`,
      },
      body: JSON.stringify({
        sourceMessageId: messageId,
        triage: record.triaged.triage,
        draftBody: record.drafted.draft.body,
        reviewDecision: record.reviewed,
      }),
    },
  );
  if (!res.ok) {
    throw new Error(`CRM log failed: ${res.status}`);
  }
}

/**
 * Page a human agent via PagerDuty. Called when `ReviewResult.mustEscalate`
 * fires or the customer hit a critical-tier policy tripwire.
 */
async function escalateToHuman(
  messageId: string,
  customerId: string,
  reviewed: ReviewResult,
): Promise<void> {
  "use step";

  const res = await fetch(`${process.env.PAGERDUTY_URL}/incidents`, {
    method: "POST",
    headers: {
      Authorization: `Token token=${process.env.PAGERDUTY_TOKEN!}`,
      "Content-Type": "application/json",
      "Idempotency-Key": `escalate:${messageId}`,
    },
    body: JSON.stringify({
      incident: {
        type: "incident",
        title: `[support-triage] ${customerId} — ${reviewed.risks.join(", ") || "auto-review rejected"}`,
        service: { id: process.env.PAGERDUTY_SERVICE_ID!, type: "service_reference" },
        urgency: reviewed.mustEscalate ? "high" : "low",
        body: {
          type: "incident_body",
          details: reviewed.reviewerNotes,
        },
      },
    }),
  });
  if (!res.ok && res.status !== 409 /* already exists, idempotent */) {
    throw new Error(`PagerDuty escalation failed: ${res.status}`);
  }
}

// ═══════════════════════════════════════════════════════════════════════
//  LLM steps — thin "use step" wrappers around the typedown-generated
//  invocation functions. Wrapping makes each call appear as a
//  discrete durable checkpoint in the workflow dashboard.
// ═══════════════════════════════════════════════════════════════════════

async function classifyStep(
  input: Parameters<typeof classify>[0],
): Promise<Triaged> {
  "use step";
  return await classify(input);
}

async function draftStep(input: Triaged): Promise<Drafted> {
  "use step";
  return await draft(input);
}

async function reviewStep(input: Drafted): Promise<ReviewResult> {
  "use step";
  return await review(input);
}

// ═══════════════════════════════════════════════════════════════════════
//  The durable six-step workflow
// ═══════════════════════════════════════════════════════════════════════

/**
 * Orchestrate the full customer-support triage. Returns the review
 * decision — the send layer is responsible for acting on
 * `reviewed.approved` (dispatch the `redactedBody`) vs
 * `reviewed.mustEscalate` (wait for the PagerDuty incident the
 * escalateToHuman step opens).
 *
 * Durability semantics:
 *
 *   * Each numbered step above is a workflow-sdk checkpoint. If the
 *     process crashes after `fetchOrders` but before `classify`, a
 *     retry picks up from the checkpoint rather than re-billing the
 *     CRM.
 *   * Steps 1–3 run in parallel because they don't depend on each
 *     other. `Promise.all` inside `"use workflow"` preserves per-step
 *     checkpointing.
 *   * Steps 4–6 are strictly sequential — typedown verified the
 *     I/O flow, so `await draftStep(triaged)` can't fail a type
 *     check. The LLM itself might fail; workflow-sdk retries
 *     automatically.
 *   * `logToCrm` and `escalateToHuman` are idempotent by
 *     `messageId` — re-runs won't double-log.
 *
 * The typedown pipeline's effect row policy is enforced at generation
 * time (no `Writes<…>` were declared; no filesystem writes can escape
 * the LLM tool layer). workflow-sdk's policies are orthogonal — you
 * still want a separate rate-limiter / budget-cap layer at the
 * platform level.
 */
export async function supportTriageWorkflow(
  message: InboundMessage,
): Promise<ReviewResult> {
  "use workflow";

  // ── 1–3. Three parallel external fetches. ────────────────────────
  const [customer, orders, kbArticles] = await Promise.all([
    fetchCustomer(message.customerId),
    fetchOrders(message.customerId),
    searchKb(message.body),
  ]);

  // ── 4. LLM classification. ───────────────────────────────────────
  // typedown verified: `Prompt<TriageInput, Triaged>`. The compiler
  // rejected any version of this workflow that passed the wrong
  // shape here; at this point it's just an import.
  const triaged = await classifyStep({ message, customer, orders, kbArticles });

  // Short-circuit obvious escalations — if the classifier is
  // already confident this needs a human, skip the draft+review
  // cost and route directly.
  if (triaged.triage.escalateToHuman && triaged.triage.urgency === "critical") {
    const skeleton: Drafted = {
      ...triaged,
      draft: {
        body: "",
        citedArticleIds: [],
        proposedTags: [],
        suggestedMacroIds: [],
        confidence: 0,
        flagsForReviewer: [],
        reviewerNotes: "Auto-routed to human on critical escalation signal.",
      },
    };
    const reviewed: ReviewResult = {
      approved: false,
      redactedBody: "",
      risks: [],
      mustEscalate: true,
      reviewerNotes: triaged.triage.escalationReason ?? "critical escalation",
      finalTagsForCrm: triaged.triage.suggestedTags,
    };
    await escalateToHuman(message.messageId, message.customerId, reviewed);
    await logToCrm(message.messageId, message.customerId, {
      triaged,
      drafted: skeleton,
      reviewed,
    });
    return reviewed;
  }

  // ── 5. LLM drafting — may invoke `search_kb` / `get_customer_macros`
  //      tools, both of which typedown pre-filtered to the declared
  //      allowlist. ───────────────────────────────────────────────
  const drafted = await draftStep(triaged);

  // ── 6. LLM safety / policy review. ───────────────────────────────
  const reviewed = await reviewStep(drafted);

  // Housekeeping: always log, escalate when the reviewer demands it.
  await logToCrm(message.messageId, message.customerId, {
    triaged,
    drafted,
    reviewed,
  });

  if (reviewed.mustEscalate) {
    await escalateToHuman(message.messageId, message.customerId, reviewed);
  }

  return reviewed;
}
