//! Vercel AI SDK backend.
//!
//! Compiles a typed markdown document to a TypeScript module that
//! imports from [`"ai"`](https://ai-sdk.dev) and `"zod"`, and exports:
//!
//! * A **Zod schema** and inferred TypeScript type for every locally
//!   declared type in the document's `td` fences.
//! * A **policy constant** derived from the effect rows
//!   (`model`, `maxOutputTokens`, `allowedTools`, `allowedModels`,
//!   `reads`, `writes`). Mirrors [`td-runtime::EnforcedPrompt`]'s
//!   in-process enforcement at the edge of your TypeScript codebase.
//! * The rendered **system prompt** (markdown body minus the `td`
//!   fences) as a string constant.
//! * An async invocation function wrapping
//!   [`generateText`](https://ai-sdk.dev/docs/ai-sdk-core/generating-text)
//!   with structured output
//!   ([`Output.object({ schema })`](https://ai-sdk.dev/docs/reference/ai-sdk-core/generate-text#outputobject))
//!   and tool-allowlist filtering.
//!
//! ## Scope
//!
//! Two document shapes compile today:
//!
//! * **Single-prompt** docs declared with `Prompt<I, O>` (optionally
//!   intersected with effect rows) → one Zod schema per local type,
//!   a policy const, system prompt, and an async invocation function.
//!
//! * **Pipeline** docs declared via `Compose<[A, B, C]>` → per-step
//!   policy / system / invocation functions, a pipeline-level policy
//!   ceiling, and an orchestrator function that sequences the steps
//!   with the type-checker's I/O flow statically proven. Each step's
//!   system prompt is bucketed from the markdown body by matching
//!   level-2 headings against the step type-alias names.
//!
//! Unsupported doc shapes (e.g. `Readme`, `Tool<A, R>`) produce a
//! clean [`CodegenError::UnsupportedShape`] with a descriptive reason.

use std::fmt::Write as _;

use td_ast::td::TdType;
use td_check::{ComposedStep, Composition, Effects, EntryOrigin, LookupResult, TypeEnv};
use td_core::SourceFile;

use crate::{
    naming::{camel_case_from_path, pascal_case, pascal_case_from_path},
    prompt::{pipeline_step_prompts, system_prompt},
    zod::{emit_zod, js_string},
    CodegenError, CompileUnit,
};

const BACKEND: &str = "ai-sdk";

/// Compile a document to a Vercel AI SDK TypeScript module.
///
/// Returns the full source of a `.ts` file. Callers write it to disk or
/// pipe it to stdout — this function never touches the filesystem.
///
/// Dispatches based on whether the document is a typed pipeline
/// (`Compose<[…]>`) or a single prompt.
pub fn emit(unit: &CompileUnit<'_>) -> Result<String, CodegenError> {
    match unit.composition {
        Some(comp) => emit_pipeline(unit, comp),
        None => emit_single_prompt(unit),
    }
}

fn emit_single_prompt(unit: &CompileUnit<'_>) -> Result<String, CodegenError> {
    let doc_type = unit.doc_type.ok_or(CodegenError::MissingDocType)?;

    // Find the Prompt<I, O> inside the doc type. Unlike td-runtime we
    // refuse to compile anything that isn't a Prompt — generating
    // `generateText` code for a Readme would be incoherent.
    let (input_ty, output_ty) = match find_prompt_io(doc_type, unit.env) {
        Some(io) => io,
        None => {
            return Err(CodegenError::UnsupportedShape {
                backend: BACKEND,
                shape: type_shape_name(doc_type),
                reason: "the AI SDK backend requires a `Prompt<I, O>` document type"
                    .to_string(),
            });
        }
    };

    let mut out = String::new();
    let title = pascal_case_from_path(&unit.file.path);
    let fn_name = camel_case_from_path(&unit.file.path);

    write_header(&mut out, unit.file);
    writeln!(&mut out).unwrap();
    write_imports(&mut out);
    writeln!(&mut out).unwrap();
    write_local_schemas(&mut out, unit.env)?;
    write_io_alias_if_needed(&mut out, unit.env, &input_ty, &format!("{title}Input"))?;
    write_io_alias_if_needed(&mut out, unit.env, &output_ty, &format!("{title}Output"))?;
    write_policy(&mut out, unit.effects, &title);
    writeln!(&mut out).unwrap();
    write_system_prompt_const(&mut out, &title, &system_prompt(unit.file, unit.doc));
    writeln!(&mut out).unwrap();
    write_invoke_function(
        &mut out,
        &fn_name,
        &title,
        unit.effects,
        &input_ty,
        &output_ty,
    )?;

    Ok(out)
}

// ---------------------------------------------------------------------------
// Pipeline emitter
// ---------------------------------------------------------------------------

/// Compile a pipeline document (declared via `Compose<[…]>`) to a
/// TypeScript module that emits:
///
/// 1. Shared Zod schemas for every value-shape local type.
/// 2. For each pipeline step: a `<Step>Policy` const, `<Step>System`
///    template string (bucketed from the markdown body by heading),
///    and a `<step>()` invocation function typed `In → Out` for that
///    step.
/// 3. A `<Pipeline>Policy` const documenting the declared ceiling.
/// 4. An async orchestrator `<pipeline>(input, options?): Promise<Out>`
///    that chains the step calls with type-checked I/O flow.
///
/// The orchestrator is a straight sequence — one `await` per step —
/// because `typedown check` already verified adjacent-step I/O match
/// (td702). No runtime shape check is needed between steps.
fn emit_pipeline(
    unit: &CompileUnit<'_>,
    comp: &Composition,
) -> Result<String, CodegenError> {
    if comp.steps.is_empty() {
        return Err(CodegenError::UnsupportedShape {
            backend: BACKEND,
            shape: "Compose<[]> (empty)".to_string(),
            reason: "pipeline has no steps to compile".to_string(),
        });
    }

    let mut out = String::new();
    let pipeline_title = pascal_case_from_path(&unit.file.path);
    let pipeline_fn_name = camel_case_from_path(&unit.file.path);

    write_header(&mut out, unit.file);
    writeln!(&mut out).unwrap();
    write_imports(&mut out);
    writeln!(&mut out).unwrap();
    write_local_schemas(&mut out, unit.env)?;

    // Bucket the markdown body into per-step system prompts. Step names
    // are the `ComposedStep::name` values (the type-alias identifiers
    // referenced from `Compose<[Classify, Answer]>`).
    let step_names: Vec<String> = comp.steps.iter().map(|s| s.name.clone()).collect();
    let step_system_prompts = pipeline_step_prompts(unit.file, unit.doc, &step_names);

    // Emit each step's policy + system + invoke function.
    for step in &comp.steps {
        write_step_block(&mut out, unit.env, step, &step_system_prompts)?;
    }

    // Pipeline-level policy (the ceiling — documented here for
    // auditability; the per-step consts are what the runtime actually
    // threads through `generateText`).
    write_pipeline_policy(&mut out, unit.effects, &pipeline_title);
    writeln!(&mut out).unwrap();

    // Orchestrator function: sequences the step invocations with
    // statically-typed I/O flow.
    write_pipeline_orchestrator(&mut out, &pipeline_fn_name, &pipeline_title, &comp.steps);

    Ok(out)
}

/// Emit the policy + system + invocation function trio for a single
/// pipeline step. The step's system prompt comes from the per-heading
/// bucketing done by [`pipeline_step_prompts`]; if no heading matched
/// the step's type-alias name, we emit a comment noting the gap so
/// the author can fix the doc.
fn write_step_block(
    out: &mut String,
    env: &TypeEnv,
    step: &ComposedStep,
    step_system_prompts: &std::collections::HashMap<String, String>,
) -> Result<(), CodegenError> {
    let step_title = pascal_case(&step.name);
    let step_fn_name = lower_first(&step_title);

    let system_body = step_system_prompts
        .get(&step.name)
        .cloned()
        .unwrap_or_default();

    writeln!(out, "// ── Step: {} ──", step.name).unwrap();
    writeln!(out).unwrap();

    // Synthesize I/O schema names *per step* for inline types. When the
    // step's I/O is a NamedRef (the common case), these calls are no-ops
    // and the existing local schema is reused.
    write_io_alias_if_needed(
        out,
        env,
        &step.input,
        &format!("{step_title}Input"),
    )?;
    write_io_alias_if_needed(
        out,
        env,
        &step.output,
        &format!("{step_title}Output"),
    )?;

    write_policy(out, &step.effects, &step_title);
    writeln!(out).unwrap();

    if system_body.is_empty() {
        writeln!(
            out,
            "/// No `##` heading matched step `{}`; the model will run\n\
             /// with an empty system prompt. Add a heading whose text\n\
             /// contains `{}` to supply its instructions.",
            step.name, step.name
        )
        .unwrap();
    }
    write_system_prompt_const(out, &step_title, &system_body);
    writeln!(out).unwrap();

    write_invoke_function(
        out,
        &step_fn_name,
        &step_title,
        &step.effects,
        &step.input,
        &step.output,
    )?;
    writeln!(out).unwrap();

    Ok(())
}

/// Render the pipeline-level policy as a commented `<Pipeline>Policy`
/// constant. This mirrors `write_policy` but tags the comment so
/// consumers know this is the *ceiling* rather than a per-step record.
fn write_pipeline_policy(out: &mut String, effects: &Effects, title: &str) {
    writeln!(
        out,
        "/// Pipeline-level capability ceiling — every step's declared\n\
         /// policy is verified at `typedown check` time to fit inside\n\
         /// this envelope. Emitted here for auditability; the per-step\n\
         /// policy constants are what `generateText` actually receives."
    )
    .unwrap();
    if !effects.declared {
        writeln!(out, "export const {title}Policy = {{}} as const;").unwrap();
        return;
    }
    writeln!(out, "export const {title}Policy = {{").unwrap();
    if !effects.models.is_empty() {
        writeln!(
            out,
            "  allowedModels: {} as const,",
            render_string_array(&effects.models)
        )
        .unwrap();
    }
    if let Some(max) = effects.max_tokens {
        writeln!(out, "  maxOutputTokens: {max},").unwrap();
    }
    writeln!(
        out,
        "  allowedTools: {} as const,",
        render_string_array(&effects.uses)
    )
    .unwrap();
    writeln!(
        out,
        "  reads: {} as const,",
        render_string_array(&effects.reads)
    )
    .unwrap();
    writeln!(
        out,
        "  writes: {} as const,",
        render_string_array(&effects.writes)
    )
    .unwrap();
    writeln!(out, "}} as const;").unwrap();
}

/// Emit the pipeline's orchestrator function. Each step reads from the
/// previous step's output variable; the final step's output is
/// returned. All seams are statically typed because each step's
/// function signature (emitted above) carries explicit `In`/`Out`
/// types that typedown already verified flow cleanly from one to the
/// next.
fn write_pipeline_orchestrator(
    out: &mut String,
    pipeline_fn_name: &str,
    pipeline_title: &str,
    steps: &[ComposedStep],
) {
    // Safe because emit_pipeline rejected empty pipelines upstream.
    let first = steps.first().expect("non-empty steps");
    let last = steps.last().expect("non-empty steps");

    let input_ts = ts_ref_for(&first.input, &format!("{pipeline_title}Input"));
    let output_ts = ts_ref_for(&last.output, &format!("{pipeline_title}Output"));

    writeln!(
        out,
        "/// Orchestrate the `{pipeline_title}` pipeline end-to-end.\n\
         ///\n\
         /// Each step's I/O match was verified at `typedown check` time\n\
         /// (td702), so the chain is statically sound. The `options`\n\
         /// argument is forwarded to every step; per-step tool allowlists\n\
         /// still apply, so out-of-policy tools are filtered independently\n\
         /// at each hop."
    )
    .unwrap();
    writeln!(
        out,
        "export async function {pipeline_fn_name}(\n  \
         input: {input_ts},\n  \
         options?: {{ tools?: Record<string, Tool>; abortSignal?: AbortSignal }},\n\
         ): Promise<{output_ts}> {{"
    )
    .unwrap();

    // Chain the steps. First step reads `input`; every subsequent step
    // reads the previous step's output variable.
    for (i, step) in steps.iter().enumerate() {
        let step_fn = lower_first(&pascal_case(&step.name));
        let input_var = if i == 0 {
            "input".to_string()
        } else {
            format!("step{}Out", i)
        };
        let output_var = format!("step{}Out", i + 1);
        writeln!(
            out,
            "  const {output_var} = await {step_fn}({input_var}, options);"
        )
        .unwrap();
    }
    writeln!(out, "  return step{}Out;", steps.len()).unwrap();
    writeln!(out, "}}").unwrap();
}

/// `Foo` → `foo`. Minor helper used to derive step function names from
/// their PascalCase type-alias identifiers.
fn lower_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => {
            let mut out = String::with_capacity(s.len());
            for lc in c.to_lowercase() {
                out.push(lc);
            }
            out.push_str(chars.as_str());
            out
        }
        None => String::new(),
    }
}

// ---------------------------------------------------------------------------
// File layout
// ---------------------------------------------------------------------------

fn write_header(out: &mut String, file: &SourceFile) {
    writeln!(
        out,
        "// Generated by typedown — DO NOT EDIT.\n\
         // Source: {}\n\
         // Regenerate: `typedown export --format ai-sdk <source>`",
        file.path.display()
    )
    .unwrap();
}

fn write_imports(out: &mut String) {
    writeln!(
        out,
        "import {{ generateText, Output, type Tool }} from \"ai\";\n\
         import {{ z }} from \"zod\";"
    )
    .unwrap();
}

// ---------------------------------------------------------------------------
// Local type declarations
// ---------------------------------------------------------------------------

/// Names of stdlib types that describe *markdown structure*, not
/// JSON values. Local decls that reference any of these are skipped
/// from Zod emission — a schema for "a document with a Role section"
/// is incoherent in value-land.
const CONTENT_SHAPE_TYPES: &[&str] = &[
    "Prompt",
    "Tool",
    "Runbook",
    "Readme",
    "AgentsMd",
    "Section",
    "Prose",
    "OrderedList",
    "UnorderedList",
    "TaskList",
    "CodeBlock",
    "Heading",
    "Example",
];

/// Emit a Zod schema and `z.infer` TypeScript alias for every
/// value-shaped user-declared type in the environment. Effect-row
/// markers, content-shape aliases (anything transitively referencing
/// `Prompt`, `Section`, etc.), and generic decls are skipped — they
/// don't have a meaningful JSON-value schema, and emitting one would
/// produce garbage like `DocSchema = ....and(z.any()).and(z.any())`.
///
/// Emission order is a **topological sort** on the type-reference
/// graph so `Comment` lands before `ReviewOutput` that uses it.
/// Without this, Zod consts can't be hoisted and we'd compile to
/// broken TypeScript.
fn write_local_schemas(out: &mut String, env: &TypeEnv) -> Result<(), CodegenError> {
    // Collect only value-shape locals. We drop generic decls (they'd need
    // parameterized Zod emission, which v1 punts on), anything that
    // references a content-shape stdlib type (Prompt, Section, …), and
    // effect-row markers.
    let mut locals: Vec<(String, TdType, usize)> = env
        .entries
        .iter()
        .filter(|(_, e)| matches!(e.origin, EntryOrigin::Local))
        .filter(|(_, e)| e.decl.generics.is_empty())
        .map(|(name, entry)| {
            let body = env.instantiate(&entry.decl, &[]);
            (name.clone(), body, entry.decl.span.start)
        })
        .filter(|(_, body, _)| is_value_shape(body))
        .collect();
    locals.sort_by_key(|(_, _, span_start)| *span_start);

    // Topo-sort so referenced schemas come first. Zod consts aren't
    // hoisted in JavaScript, so forward references to
    // `CommentSchema` from `ReviewOutputSchema` would be a TDZ bug.
    let local_names: std::collections::HashSet<String> =
        locals.iter().map(|(n, _, _)| n.clone()).collect();
    let ordered = topo_sort(&locals, &local_names);

    let mut any_written = false;
    for (name, body) in ordered {
        let zod = emit_zod(&body, env, &name)?;
        writeln!(out, "export const {name}Schema = {zod};").unwrap();
        writeln!(out, "export type {name} = z.infer<typeof {name}Schema>;").unwrap();
        writeln!(out).unwrap();
        any_written = true;
    }

    if !any_written {
        writeln!(out).unwrap();
    }

    Ok(())
}

/// Kahn's algorithm over the reference graph. Emits dependencies
/// before dependents so Zod consts resolve in declaration order.
/// Cycles (pathological in value types) fall through to source order
/// on the last pass rather than panicking.
fn topo_sort(
    locals: &[(String, TdType, usize)],
    local_names: &std::collections::HashSet<String>,
) -> Vec<(String, TdType)> {
    use std::collections::HashMap;

    let mut deps: HashMap<&str, std::collections::HashSet<String>> = HashMap::new();
    for (name, body, _) in locals {
        let mut refs = std::collections::HashSet::new();
        collect_referenced_names(body, &mut refs);
        let d: std::collections::HashSet<String> = refs
            .into_iter()
            .filter(|n| local_names.contains(n) && n != name)
            .collect();
        deps.insert(name.as_str(), d);
    }

    let mut done: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(locals.len());
    for _ in 0..locals.len() + 1 {
        let before = out.len();
        for (name, body, _) in locals {
            if done.contains(name) {
                continue;
            }
            let ready = deps
                .get(name.as_str())
                .map(|d| d.iter().all(|d| done.contains(d)))
                .unwrap_or(true);
            if ready {
                out.push((name.clone(), body.clone()));
                done.insert(name.clone());
            }
        }
        if out.len() == before {
            // Cycle detected — emit the remainder in source order and stop.
            for (name, body, _) in locals {
                if !done.contains(name) {
                    out.push((name.clone(), body.clone()));
                    done.insert(name.clone());
                }
            }
            break;
        }
    }
    out
}

/// True iff the type describes JSON-valued data (not markdown
/// structure and not effect-row metadata). False for anything that
/// references `Prompt`, `Section`, `Uses`, etc.
fn is_value_shape(ty: &TdType) -> bool {
    let mut refs = std::collections::HashSet::new();
    collect_referenced_names(ty, &mut refs);
    !refs.iter().any(|n| {
        CONTENT_SHAPE_TYPES.contains(&n.as_str())
            || matches!(n.as_str(), "Uses" | "Reads" | "Writes" | "Model" | "MaxTokens")
            || matches!(n.as_str(), "Compose" | "Sequential")
    })
}

fn collect_referenced_names(ty: &TdType, out: &mut std::collections::HashSet<String>) {
    match ty {
        TdType::NamedRef {
            name, type_args, ..
        } => {
            out.insert(name.clone());
            for a in type_args {
                collect_referenced_names(a, out);
            }
        }
        TdType::Array { elem, .. } => collect_referenced_names(elem, out),
        TdType::Tuple { elems, .. } => {
            for e in elems {
                collect_referenced_names(e, out);
            }
        }
        TdType::Object(obj) => {
            for f in &obj.fields {
                collect_referenced_names(&f.ty, out);
            }
        }
        TdType::Union { variants, .. } => {
            for v in variants {
                collect_referenced_names(v, out);
            }
        }
        TdType::Intersection { parts, .. } => {
            for p in parts {
                collect_referenced_names(p, out);
            }
        }
        _ => {}
    }
}

/// If `ty` is a bare object / tuple / union (not a `NamedRef`),
/// synthesize a `<synthesized_name>Schema` and matching
/// `<synthesized_name>` TS type. Named types are already handled by
/// `write_local_schemas`. Used for single-prompt I/O and per-pipeline-
/// step I/O in the same shape.
fn write_io_alias_if_needed(
    out: &mut String,
    env: &TypeEnv,
    ty: &TdType,
    synthesized_name: &str,
) -> Result<(), CodegenError> {
    if is_named_ref(ty) {
        return Ok(());
    }
    let zod = emit_zod(ty, env, synthesized_name)?;
    writeln!(out, "export const {synthesized_name}Schema = {zod};").unwrap();
    writeln!(
        out,
        "export type {synthesized_name} = z.infer<typeof {synthesized_name}Schema>;"
    )
    .unwrap();
    writeln!(out).unwrap();
    Ok(())
}

// ---------------------------------------------------------------------------
// Policy constant
// ---------------------------------------------------------------------------

fn write_policy(out: &mut String, effects: &Effects, title: &str) {
    writeln!(
        out,
        "/// Declared capability policy extracted from the document's effect rows.\n\
         /// Mirrors the constraints `td-runtime::EnforcedPrompt` enforces server-side.\n\
         export const {title}Policy = {{"
    )
    .unwrap();

    // `model` is a single string (AI SDK expects one). If multiple are
    // declared we still pick the first as the default and emit the full
    // allowlist for apps that want to switch at runtime.
    if let Some(first) = effects.models.first() {
        writeln!(out, "  model: {} as const,", js_string(first)).unwrap();
    }
    if effects.models.len() > 1 {
        writeln!(
            out,
            "  allowedModels: {} as const,",
            render_string_array(&effects.models)
        )
        .unwrap();
    }
    if let Some(max) = effects.max_tokens {
        writeln!(out, "  maxOutputTokens: {max},").unwrap();
    }
    // Always emit the three capability tuples so runtime filtering has
    // a stable shape regardless of what was declared.
    writeln!(
        out,
        "  allowedTools: {} as const,",
        render_string_array(&effects.uses)
    )
    .unwrap();
    writeln!(
        out,
        "  reads: {} as const,",
        render_string_array(&effects.reads)
    )
    .unwrap();
    writeln!(
        out,
        "  writes: {} as const,",
        render_string_array(&effects.writes)
    )
    .unwrap();
    writeln!(out, "}} as const;").unwrap();
}

fn render_string_array(xs: &[String]) -> String {
    if xs.is_empty() {
        "[]".to_string()
    } else {
        let parts: Vec<String> = xs.iter().map(|s| js_string(s)).collect();
        format!("[{}]", parts.join(", "))
    }
}

// ---------------------------------------------------------------------------
// System prompt
// ---------------------------------------------------------------------------

fn write_system_prompt_const(out: &mut String, title: &str, body: &str) {
    writeln!(out, "/// Rendered system prompt (markdown body minus the `td` fences).").unwrap();
    writeln!(out, "export const {title}System = {};", js_template_string(body)).unwrap();
}

/// Emit a multi-line JS string using a backtick template literal to
/// preserve newlines without escape-soup. Backticks and `${` inside
/// the body are escaped.
fn js_template_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('`');
    let chars: Vec<char> = s.chars().collect();
    for (i, c) in chars.iter().enumerate() {
        match *c {
            '\\' => out.push_str("\\\\"),
            '`' => out.push_str("\\`"),
            '$' if chars.get(i + 1) == Some(&'{') => out.push_str("\\$"),
            other => out.push(other),
        }
    }
    out.push('`');
    out
}

// ---------------------------------------------------------------------------
// Invocation function
// ---------------------------------------------------------------------------

fn write_invoke_function(
    out: &mut String,
    fn_name: &str,
    title: &str,
    effects: &Effects,
    input: &TdType,
    output: &TdType,
) -> Result<(), CodegenError> {
    // Identify input / output type names in TypeScript source. Either
    // the declared name (when the I/O is a `NamedRef`) or the
    // synthesized `<Title>Input`/`<Title>Output`.
    let input_ts = ts_ref_for(input, &format!("{title}Input"));
    let output_ts = ts_ref_for(output, &format!("{title}Output"));
    let input_schema = schema_ref_for(input, &format!("{title}Input"));
    let output_schema = schema_ref_for(output, &format!("{title}Output"));

    let has_uses = !effects.uses.is_empty() || effects.declared;

    writeln!(out, "/// Invoke the `{title}` prompt with structured I/O.").unwrap();
    writeln!(
        out,
        "///\n/// Provided tools are filtered against the declared `Uses<…>` allowlist\n\
         /// before being handed to the model — out-of-policy entries are silently\n\
         /// dropped. Use `td-runtime` on the server for deny-with-error semantics."
    )
    .unwrap();
    writeln!(
        out,
        "export async function {fn_name}(\n  \
         input: {input_ts},\n  \
         options?: {{ tools?: Record<string, Tool>; abortSignal?: AbortSignal }},\n\
         ): Promise<{output_ts}> {{"
    )
    .unwrap();

    // Tool filtering.
    if has_uses {
        writeln!(
            out,
            "  const tools = options?.tools\n    \
             ? Object.fromEntries(\n        \
             Object.entries(options.tools).filter(([name]) =>\n          \
             ({title}Policy.allowedTools as readonly string[]).includes(name),\n        \
             ),\n      )\n    \
             : undefined;"
        )
        .unwrap();
    }

    // The call itself. generateText with structured output.
    writeln!(out, "  const {{ output }} = await generateText({{").unwrap();
    writeln!(out, "    model: {title}Policy.model,").unwrap();
    writeln!(out, "    system: {title}System,").unwrap();
    writeln!(out, "    prompt: JSON.stringify({input_schema}.parse(input)),").unwrap();
    if has_uses {
        writeln!(out, "    tools,").unwrap();
    }
    if effects.max_tokens.is_some() {
        writeln!(out, "    maxOutputTokens: {title}Policy.maxOutputTokens,").unwrap();
    }
    writeln!(out, "    abortSignal: options?.abortSignal,").unwrap();
    writeln!(
        out,
        "    output: Output.object({{ schema: {output_schema} }}),"
    )
    .unwrap();
    writeln!(out, "  }});").unwrap();
    writeln!(out, "  return output;").unwrap();
    writeln!(out, "}}").unwrap();

    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn is_named_ref(ty: &TdType) -> bool {
    matches!(ty, TdType::NamedRef { .. })
}

fn ts_ref_for(ty: &TdType, fallback: &str) -> String {
    match ty {
        TdType::NamedRef { name, .. } => name.clone(),
        _ => fallback.to_string(),
    }
}

fn schema_ref_for(ty: &TdType, fallback: &str) -> String {
    match ty {
        TdType::NamedRef { name, .. } => format!("{name}Schema"),
        _ => format!("{fallback}Schema"),
    }
}

fn type_shape_name(ty: &TdType) -> String {
    match ty {
        TdType::Primitive { kind, .. } => format!("{kind:?}").to_lowercase(),
        TdType::StringLit { .. } => "string literal".into(),
        TdType::NumberLit { .. } => "number literal".into(),
        TdType::Array { .. } => "array".into(),
        TdType::Tuple { .. } => "tuple".into(),
        TdType::Object(_) => "object".into(),
        TdType::Union { .. } => "union".into(),
        TdType::Intersection { .. } => "intersection".into(),
        TdType::NamedRef { name, .. } => name.clone(),
    }
}

/// Locate a `Prompt<I, O>` named-ref inside a declared doc type. Walks
/// through intersections and user aliases (one step each) without
/// expanding `Prompt` itself — otherwise the NamedRef we're looking
/// for would be destroyed by its own object-body expansion.
fn find_prompt_io(ty: &TdType, env: &TypeEnv) -> Option<(TdType, TdType)> {
    fn go(ty: &TdType, env: &TypeEnv, depth: usize) -> Option<(TdType, TdType)> {
        if depth == 0 {
            return None;
        }
        match ty {
            TdType::NamedRef {
                name, type_args, ..
            } if name == "Prompt" && type_args.len() >= 2 => {
                Some((type_args[0].clone(), type_args[1].clone()))
            }
            TdType::NamedRef {
                name, type_args, ..
            } => match env.lookup(name) {
                LookupResult::Decl(entry) => {
                    let expanded = env.instantiate(&entry.decl, type_args);
                    go(&expanded, env, depth - 1)
                }
                _ => None,
            },
            TdType::Intersection { parts, .. } => parts.iter().find_map(|p| go(p, env, depth - 1)),
            _ => None,
        }
    }
    go(ty, env, 8)
}
