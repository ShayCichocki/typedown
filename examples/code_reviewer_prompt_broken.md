---
typedown: Prompt<ReviewInput, ReviewOutput>
---

# Code Reviewer (broken)

```td
import { Prompt, Section, Prose, OrderedList, Example } from "typedown/agents"

type ReviewInput  = { diff: string }
type ReviewOutput = { approved: boolean }

export type Doc = Prompt<ReviewInput, ReviewOutput>
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
