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
cargo run -p td-cli -- types                         # print stdlib modules
cargo run -p td-cli -- export examples/foo.md        # JSON Schema → stdout
cargo run -p td-cli -- export examples/foo.md -o out.json
```

## Typed example values

`Example<I, O>` is now load-bearing. Write your examples with `json` or
`yaml` value fences and typedown type-checks the payloads against `I` / `O`:

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

## Stdlib

Two modules ship out of the box:

- **`typedown/agents`** — `Prompt<I, O>`, `Tool<A, R>`, `Runbook`, `Example<I, O>`
- **`typedown/docs`** — `Readme`, `AgentsMd`

Plus implicit content-shape primitives usable without import:
`Section<T>`, `Prose`, `OrderedList`, `UnorderedList`, `TaskList`,
`CodeBlock<Lang>`, `Heading<Level>`.

## Status

Shipping today:
- Markdown + td-DSL parsing
- Generic instantiation & intersection flattening
- Full conformance check for `Prompt<I, O>` and `Readme` / `AgentsMd`
- **Value typing**: JSON / YAML fences inside `Example<I, O>` are parsed
  and checked against `I` / `O` — generic parameters are no longer phantom
- **Schema export**: `typedown export` emits JSON Schema (Draft 2020-12)
  with every local type declaration under `$defs`
- CLI with miette diagnostics

Roadmap:
- Effect / capability types (`Uses<>`, `Reads<>`, `Writes<>`) for agent
  prompt policy
- `td diff` for semver-style compatibility checks between doc versions
- Additional export targets (`.d.ts`, Zod, OpenAI / Anthropic tool JSON)
- LSP server (`td-lsp`) for in-editor diagnostics
- User-authored `.td` modules via import paths
- Executable code-fence checking (tsc / rustc / shellcheck on blocks)
- Watch mode, incremental parsing, formatter

## License

MIT OR Apache-2.0
