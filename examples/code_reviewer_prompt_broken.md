---
typedown: Doc
---

# Code Reviewer (broken)

```td
import { Prompt, MaxTokens } from "typedown/agents"

type ReviewInput  = { diff: string }
type ReviewOutput = { approved: boolean }

// Broken on two axes now: the input example value has the wrong type,
// AND MaxTokens gets a string instead of a number — both will fire
// diagnostics (`td502` and `td601`) at check time.
export type Doc =
  & Prompt<ReviewInput, ReviewOutput>
  & MaxTokens<"nope">
```

## Role

You are a code reviewer.

## Instructions

You should just eyeball things and see what looks off.

## Examples

### Example 1

**Input:**

```json
{ "diff": 42 }
```

**Output:**

```json
{ "approved": "yes", "extra": "oops" }
```

## Random Thoughts

Sometimes I wonder if we're all just vibes-reviewing at this point.
