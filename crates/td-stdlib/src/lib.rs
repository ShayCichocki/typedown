//! The typedown standard library.
//!
//! The stdlib has two layers:
//!
//! * **Built-ins** ([`Builtin`]) — primitive *content-shaped* types that map
//!   directly onto markdown nodes. The checker has hand-written semantics for
//!   each one because they describe document shapes, not values.
//! * **Module sources** ([`module_source`]) — plain `.td` text for composite
//!   types like `Prompt<I, O>`, `Runbook`, `Tool<A, R>`. These are parsed like
//!   user modules so they automatically compose with user-defined helpers.
//!
//! The split keeps the checker small: it only needs per-built-in logic for
//! the ~8 primitive content types, and everything else (Prompt, Runbook,
//! AgentsMd, …) reduces to combinations of them.

use std::collections::HashMap;

/// A built-in content-shape type.
///
/// Each variant corresponds to a concrete markdown construct the checker
/// can match against directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Builtin {
    /// `Section<T>` — a heading plus a body that must match `T`.
    Section,
    /// `Prose` — one or more paragraph nodes.
    Prose,
    /// `OrderedList` — an ordered list node.
    OrderedList,
    /// `UnorderedList` — an unordered list node.
    UnorderedList,
    /// `TaskList` — a task-list node.
    TaskList,
    /// `CodeBlock<Lang>` — a fenced code block, optionally with a required lang.
    CodeBlock,
    /// `Heading<Level>` — a heading of a specific level.
    Heading,
    /// `Example<I, O>` — a `### Example N` subsection with Input / Output.
    Example,
}

impl Builtin {
    /// Resolve a named type reference to a built-in, if any.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "Section" => Some(Self::Section),
            "Prose" => Some(Self::Prose),
            "OrderedList" => Some(Self::OrderedList),
            "UnorderedList" => Some(Self::UnorderedList),
            "TaskList" => Some(Self::TaskList),
            "CodeBlock" => Some(Self::CodeBlock),
            "Heading" => Some(Self::Heading),
            "Example" => Some(Self::Example),
            _ => None,
        }
    }

    pub fn display(&self) -> &'static str {
        match self {
            Self::Section => "Section",
            Self::Prose => "Prose",
            Self::OrderedList => "OrderedList",
            Self::UnorderedList => "UnorderedList",
            Self::TaskList => "TaskList",
            Self::CodeBlock => "CodeBlock",
            Self::Heading => "Heading",
            Self::Example => "Example",
        }
    }
}

/// Return the `.td` source for a stdlib module, if known.
///
/// These get parsed by the checker just like user modules. Keeping them as
/// textual sources means IDEs can jump-to-definition and hover can render
/// the declaration the user expects to see.
pub fn module_source(path: &str) -> Option<&'static str> {
    match path {
        "typedown/agents" => Some(AGENTS_SRC),
        "typedown/docs" => Some(DOCS_SRC),
        "typedown/workflows" => Some(WORKFLOWS_SRC),
        _ => None,
    }
}

/// List every stdlib module path.
pub fn module_paths() -> &'static [&'static str] {
    &["typedown/agents", "typedown/docs", "typedown/workflows"]
}

/// Every symbol exported by built-in module sources, keyed by symbol name.
///
/// Computed lazily because we don't want to pay HashMap construction at
/// startup for tools that never touch stdlib resolution.
pub fn builtin_index() -> HashMap<&'static str, &'static str> {
    let mut m = HashMap::new();
    for &path in module_paths() {
        let src = module_source(path).expect("known path");
        for line in src.lines() {
            let l = line.trim_start();
            let rest = l
                .strip_prefix("export type ")
                .or_else(|| l.strip_prefix("export interface "));
            if let Some(rest) = rest {
                if let Some(name_end) = rest.find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                {
                    let name = &rest[..name_end];
                    m.insert(name, path);
                }
            }
        }
    }
    m
}

const AGENTS_SRC: &str = r#"
// Agent-native document types.
//
// These types describe the shape of markdown documents consumed by LLM
// agents: prompts, tool specifications, runbooks. A document declaring
// `Prompt<In, Out>` has a stable, validated structure the agent runtime
// can rely on.

/// A `##` section whose body must match `T`.
export type Section<T> = any

/// One or more paragraph blocks of free text.
export type Prose = any

/// A numbered list (`1. 2. 3.`).
export type OrderedList = any

/// A bullet list (`- foo`).
export type UnorderedList = any

/// A task list (`- [ ] foo` / `- [x] bar`).
export type TaskList = any

/// A fenced code block. `Lang` narrows the required info string.
export type CodeBlock<Lang> = any

/// A heading at a specific level.
export type Heading<Level> = any

/// A sub-section with paired `Input` / `Output` snippets.
export type Example<I, O> = {
  input: I
  output: O
}

/// A typed prompt document. Authors declaring `Prompt<In, Out>` promise:
/// a role blurb, an ordered list of instructions, and one or more examples.
export type Prompt<Input, Output> = {
  role: Section<Prose>
  instructions: Section<OrderedList | UnorderedList | Prose>
  examples: Section<Example<Input, Output>[]>
}

/// A typed tool spec.
export type Tool<Args, Return> = {
  description: Section<Prose>
  arguments: Section<Args>
  returns: Section<Return>
}

/// A typed runbook — prerequisites plus step-by-step procedure.
export type Runbook = {
  prerequisites: Section<UnorderedList | TaskList>
  steps: Section<OrderedList>
}

// ---------------------------------------------------------------------------
// Effect rows.
//
// These are intersected into a document's declared type to encode the
// *capabilities* the prompt is authorized to exercise at runtime. They
// are structurally `any` — the type checker extracts them in a separate
// pass (`td-check::effects`) rather than treating them as content fields.
//
//   type Doc =
//     & Prompt<In, Out>
//     & Uses<["read_file", "run_tests"]>
//     & Reads<["./src/**"]>
//     & Writes<[]>
//     & Model<"claude-opus-4-5" | "claude-sonnet-4-5">
//     & MaxTokens<4096>
//
// The compiled JSON Schema carries them under `x-typedown-effects`, and
// the `td-runtime` crate turns the declared set into enforced behavior.
// ---------------------------------------------------------------------------

/// Tools this prompt is authorized to invoke. Pass a tuple of tool-name
/// string literals: `Uses<["read_file", "run_tests"]>`. The empty tuple
/// `Uses<[]>` declares "no tools permitted."
export type Uses<T> = any

/// Filesystem / resource patterns this prompt may read. Entries are glob
/// strings: `Reads<["./src/**", "./docs/*.md"]>`.
export type Reads<T> = any

/// Filesystem / resource patterns this prompt may write. `Writes<[]>`
/// declares an explicitly read-only prompt.
export type Writes<T> = any

/// Models this prompt has been validated against. Accepts a tuple of
/// string literals or a string-literal union for the variadic case.
export type Model<T> = any

/// Hard token ceiling honored by the runtime.
export type MaxTokens<N> = any
"#;

const DOCS_SRC: &str = r#"
// Common documentation shapes.

export type Readme = {
  overview: Section<Prose>
  installation: Section<OrderedList | CodeBlock>
  usage: Section<Prose | CodeBlock>
}

export type AgentsMd = {
  conventions: Section<Prose | UnorderedList>
  tools: Section<UnorderedList | Prose>
  examples: Section<Prose>
}
"#;

const WORKFLOWS_SRC: &str = r#"
// Composition combinators for multi-step agent workflows.
//
// These types describe how individual `Prompt<I, O>` declarations compose
// into a typed pipeline. The checker treats them as meta — they are
// harvested by a dedicated pass (`td-check::compose`) which verifies:
//
//   * Each step resolves to a `Prompt<I, O>` shape.
//   * Adjacent steps' I/O line up: output(step N) = input(step N+1).
//   * Effect rows compose by subset:
//       union(child.Uses)   ⊆ parent.Uses
//       union(child.Reads)  ⊆ parent.Reads
//       union(child.Writes) ⊆ parent.Writes
//       child.Model         ⊆ parent.Model     (for each step)
//       child.MaxTokens     ≤ parent.MaxTokens (for each step)
//
// The parent's effect rows are *authoritative* — they're the ceiling a
// pipeline is permitted to operate within. Children must fit. This gives
// you type-level policy composition: you cannot accidentally widen an
// agent's capabilities by pulling in a subagent that uses a tool the
// parent didn't authorize.
//
// Example:
//
//   type Classify = Prompt<Query, Class>
//     & Uses<[]>
//     & Model<"openai/gpt-4o-mini">
//
//   type Answer = Prompt<Class, Response>
//     & Uses<["retrieve"]>
//     & Model<"anthropic/claude-sonnet-4.5">
//
//   export type Pipeline =
//     & Compose<[Classify, Answer]>
//     & Uses<["retrieve"]>
//     & Model<"openai/gpt-4o-mini" | "anthropic/claude-sonnet-4.5">
//     & MaxTokens<4096>

/// Sequential composition of typed prompts.
///
/// `Compose<[A, B, C]>` declares that a pipeline runs A, then B, then C,
/// with the output of each step feeding the input of the next. The
/// pipeline's implicit I/O is `Prompt<A.Input, C.Output>`. Steps are
/// named type references (`Prompt<…> & …` declarations) rather than
/// inline Prompt literals so they can be type-checked and reused.
export type Compose<Steps> = any

/// Alias for `Compose` matching the Anthropic / Vercel AI SDK naming.
export type Sequential<Steps> = any
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_lookup() {
        assert_eq!(Builtin::from_name("Section"), Some(Builtin::Section));
        assert_eq!(Builtin::from_name("Nope"), None);
    }

    #[test]
    fn index_contains_prompt() {
        let idx = builtin_index();
        assert_eq!(idx.get("Prompt"), Some(&"typedown/agents"));
        assert_eq!(idx.get("Runbook"), Some(&"typedown/agents"));
        assert_eq!(idx.get("Readme"), Some(&"typedown/docs"));
    }
}
