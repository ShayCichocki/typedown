# typedown

**Statically typed markdown.** A lint + type checker for markdown files aimed
at agent-facing documents (prompts, tool specs, runbooks, AGENTS.md).

Markdown is load-bearing infrastructure for LLMs now, but it's `any`-typed.
typedown gives it types.

## Concept

Declare a document's type in frontmatter and author types inline with a
TypeScript-flavored DSL in ``` ```td ``` fences:

```md
---
typedown: Prompt<ReviewInput, ReviewOutput>
---

# Code Reviewer

` ``td
import { Prompt, Example } from "typedown/agents"

type ReviewInput  = { diff: string, context: string }
type ReviewOutput = { approved: boolean, comments: Comment[] }

interface Comment {
  file: string
  line: number
  severity: "nit" | "suggestion" | "blocking"
}

export type Doc = Prompt<ReviewInput, ReviewOutput>
` ``

## Role
You are a rigorous reviewer…

## Instructions
1. …

## Examples
### Example 1
**Input:** …
**Output:** …
```

Run `typedown check docs/` and the checker verifies:

- every field of the declared shape has a `##` heading
- `## Instructions` body is actually an ordered list
- `## Examples` contains `### Example N` sub-sections
- each example has `Input:` and `Output:` markers
- no undeclared `##` sections slip in

## Example

![typedown CLI output](docs/typedown.png)

## Layout

```
crates/
  td-core/    diagnostics + spans
  td-ast/     markdown & td-DSL ASTs
  td-parse/   markdown parser + td-DSL parser
  td-check/   type environment + conformance rules
  td-stdlib/  built-in types (Section, Prose, Prompt, Tool, Runbook, …)
  td-cli/     `typedown` binary with miette-rendered diagnostics
```

## Usage

```sh
cargo run -p td-cli -- check examples/
cargo run -p td-cli -- types        # print stdlib modules
```

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

## Stdlib

Two modules ship out of the box:

- **`typedown/agents`** — `Prompt<I, O>`, `Tool<A, R>`, `Runbook`, `Example<I, O>`
- **`typedown/docs`** — `Readme`, `AgentsMd`

Plus implicit content-shape primitives usable without import:
`Section<T>`, `Prose`, `OrderedList`, `UnorderedList`, `TaskList`,
`CodeBlock<Lang>`, `Heading<Level>`.

## Status

Vertical slice is shipping today:
- Markdown + td-DSL parsing
- Generic instantiation & intersection flattening
- Full conformance check for the `Prompt<I, O>` shape
- CLI with miette diagnostics

Roadmap:
- LSP server (`td-lsp`) for in-editor diagnostics
- JSON value validation inside `Example<I, O>` code fences
- User-authored `.td` modules via import paths
- Executable code-fence checking (tsc / rustc / shellcheck on blocks)
- Watch mode, incremental parsing, formatter

## License

MIT OR Apache-2.0
