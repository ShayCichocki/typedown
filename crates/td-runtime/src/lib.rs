//! Runtime enforcement of typedown contracts.
//!
//! Effect rows declared on a typed markdown document are useless unless
//! *something* honors them at runtime. This crate is that something. Load
//! a typed document and you get an [`EnforcedPrompt`] that:
//!
//! * Exposes the parsed effect policy (tools, reads, writes, model, token
//!   ceiling) as a structured value.
//! * Authorizes or rejects individual tool calls / file reads / writes
//!   before they reach the model or the filesystem.
//! * Validates concrete JSON input & output payloads against the `I` and
//!   `O` types declared on `Prompt<I, O>` (or `Tool<A, R>`).
//!
//! # Design tenets
//!
//! * **Deny-by-default for capabilities.** `Uses<[]>` means zero tools are
//!   permitted — not "use the global default." Declaration is opt-in; once
//!   you opt in, the set is authoritative.
//! * **Sync, cheap, allocation-light.** No async or I/O in the core API;
//!   this lets the runtime sit anywhere in a call stack — including inside
//!   an async agent loop — without imposing a runtime.
//! * **JSON Schema is the truth.** The runtime re-uses `td-check`'s schema
//!   emitter and value typer rather than defining its own; what you test
//!   with `typedown check` is what fails at runtime.
//! * **No provider coupling.** We don't know about Anthropic, OpenAI, or
//!   any specific SDK. Callers wrap the runtime around *their* client.
//!
//! # Minimum viable demo
//!
//! ```no_run
//! use td_runtime::EnforcedPrompt;
//! # fn demo() -> Result<(), Box<dyn std::error::Error>> {
//! let prompt = EnforcedPrompt::load("examples/code_reviewer_prompt.md")?;
//!
//! // Before invoking a tool, ask the runtime.
//! prompt.authorize_tool("read_file")?;       // Ok if declared in Uses<>
//! assert!(prompt.authorize_tool("shell_exec").is_err());
//!
//! // Before sending input to the model, validate it.
//! prompt.validate_input(&serde_json::json!({
//!     "diff": "…", "context": "src/x.rs",
//! }))?;
//! # Ok(()) }
//! ```

use std::path::{Path, PathBuf};

use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::Serialize;
use serde_json::Value;
use td_ast::td::TdType;
use td_check::{
    check_source, check_value, resolve_doc_type, to_json_schema, to_subschema, Composition,
    Effects, LookupResult, TypeEnv,
};
use td_core::{Diagnostics, SourceFile, Span};
use thiserror::Error;

/// A typed prompt loaded with its policy and I/O schemas resolved.
///
/// Construction runs the full typedown check pipeline and bakes the
/// results into sync lookup structures (a compiled `GlobSet` for each
/// path-based capability). After construction, every `authorize_*` and
/// `validate_*` call is a pure, allocation-free check.
#[derive(Debug)]
pub struct EnforcedPrompt {
    path: PathBuf,
    title: String,
    effects: Effects,
    schema: Value,
    /// Pre-compiled globs for [`Effects::reads`], in declaration order.
    reads_globs: GlobSet,
    /// Pre-compiled globs for [`Effects::writes`], in declaration order.
    writes_globs: GlobSet,
    /// The resolved (stripped) doc type. Kept so value validation can
    /// re-use the existing env without another markdown parse.
    env: TypeEnv,
    /// Input type extracted from `Prompt<I, O>`, if present.
    input_ty: Option<TdType>,
    /// Output type extracted from `Prompt<I, O>`, if present.
    output_ty: Option<TdType>,
    /// Pipeline structure, if the doc was declared via `Compose<[…]>`.
    /// Exposes the ordered step list so host runtimes can orchestrate
    /// each step with its own effect ceiling.
    composition: Option<Composition>,
}

impl EnforcedPrompt {
    /// Load a typed markdown file from disk.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, LoadError> {
        let path = path.as_ref().to_path_buf();
        let content = std::fs::read_to_string(&path).map_err(|e| LoadError::Io {
            path: path.clone(),
            source: e,
        })?;
        let file = SourceFile::new(&path, content);
        Self::from_source(&file)
    }

    /// Load from an already-read [`SourceFile`]. Useful for tests and for
    /// hosts that have their own virtual filesystem.
    ///
    /// The runtime refuses to construct an [`EnforcedPrompt`] for a
    /// document that would fail `typedown check`. Silently running on a
    /// broken contract would be worse than failing loud — enforcing
    /// policy against a type graph the author didn't intend is a
    /// security anti-pattern.
    pub fn from_source(file: &SourceFile) -> Result<Self, LoadError> {
        // Run the full check pipeline (parse + resolve + conformance).
        // This catches td403 (unknown doc type), td401 (missing
        // sections), td502 (value mismatch in examples), td601
        // (malformed effect row), etc. — every error worth surfacing.
        let (_doc, check_diags) = check_source(file);
        let fatal: Vec<String> = check_diags
            .iter()
            .filter(|d| matches!(d.severity, td_core::Severity::Error))
            .map(|d| format!("[{}] {}", d.code, d.message))
            .collect();
        if !fatal.is_empty() {
            return Err(LoadError::Check(fatal));
        }

        // Now it's safe to derive the policy & schemas from the document.
        let (_doc, env, ty_opt, effects, composition, _diags) = resolve_doc_type(file);

        let ty = ty_opt.ok_or(LoadError::NoDocType)?;

        // Extract I / O from a `Prompt<I, O>` node in the stripped type.
        // For docs declared as `Tool<A, R>` we surface A/R the same way.
        let (input_ty, output_ty) = extract_io(&ty, &env);

        // Compile globs for path-based policy. A malformed glob is a
        // spec-author error we surface immediately.
        let reads_globs =
            compile_globs(&effects.reads).map_err(LoadError::GlobBuild)?;
        let writes_globs =
            compile_globs(&effects.writes).map_err(LoadError::GlobBuild)?;

        let title = file
            .path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Doc")
            .to_string();
        let schema = to_json_schema(
            &ty,
            &env,
            Some(&title),
            Some(&effects),
            composition.as_ref(),
        );

        Ok(Self {
            path: file.path.clone(),
            title,
            effects,
            schema,
            reads_globs,
            writes_globs,
            env,
            input_ty,
            output_ty,
            composition,
        })
    }

    /// The pipeline structure declared on this prompt, if any.
    ///
    /// Returns `Some` for documents declaring `Compose<[…]>`; their
    /// stepwise I/O + per-step effects are exposed so host runtimes can
    /// orchestrate (authorize each step against its own ceiling, route
    /// output between steps, etc.). Single-prompt docs return `None`.
    pub fn composition(&self) -> Option<&Composition> {
        self.composition.as_ref()
    }

    /// Is this a pipeline declaration?
    pub fn is_pipeline(&self) -> bool {
        self.composition.is_some()
    }

    // -------------------------------------------------------------- getters

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn effects(&self) -> &Effects {
        &self.effects
    }

    pub fn schema(&self) -> &Value {
        &self.schema
    }

    // ------------------------------------------------------- authorization

    /// Authorize a tool invocation.
    ///
    /// Returns `Err(UnauthorizedTool)` when the tool isn't in [`Effects::uses`].
    /// A prompt that didn't declare `Uses<…>` at all is treated as deny-all
    /// when [`Effects::declared`] is set — the safe interpretation of
    /// "effects were declared but `uses` was omitted." An undeclared
    /// effects block (no rows at all) is permissive: callers that want
    /// deny-by-default should check [`Effects::declared`] themselves.
    pub fn authorize_tool(&self, tool: &str) -> Result<(), EnforcementError> {
        if !self.effects.declared {
            return Ok(());
        }
        if self.effects.uses.iter().any(|t| t == tool) {
            Ok(())
        } else {
            Err(EnforcementError::UnauthorizedTool {
                tool: tool.to_string(),
                allowed: self.effects.uses.clone(),
            })
        }
    }

    /// Authorize a read against the [`Effects::reads`] glob set. Paths are
    /// compared as literal strings (no filesystem canonicalization); the
    /// caller is expected to normalize before calling.
    pub fn authorize_read(&self, path: &str) -> Result<(), EnforcementError> {
        if !self.effects.declared {
            return Ok(());
        }
        if self.reads_globs.is_match(path) {
            Ok(())
        } else {
            Err(EnforcementError::UnauthorizedRead {
                path: path.to_string(),
                allowed: self.effects.reads.clone(),
            })
        }
    }

    /// Authorize a write against [`Effects::writes`]. See [`authorize_read`].
    pub fn authorize_write(&self, path: &str) -> Result<(), EnforcementError> {
        if !self.effects.declared {
            return Ok(());
        }
        if self.writes_globs.is_match(path) {
            Ok(())
        } else {
            Err(EnforcementError::UnauthorizedWrite {
                path: path.to_string(),
                allowed: self.effects.writes.clone(),
            })
        }
    }

    /// Check that a model identifier is one the prompt was validated
    /// against. If no `Model<>` was declared, any model is permitted.
    pub fn check_model(&self, model: &str) -> Result<(), EnforcementError> {
        if self.effects.models.is_empty() {
            return Ok(());
        }
        if self.effects.models.iter().any(|m| m == model) {
            Ok(())
        } else {
            Err(EnforcementError::UnknownModel {
                model: model.to_string(),
                allowed: self.effects.models.clone(),
            })
        }
    }

    /// Enforce the token ceiling declared by `MaxTokens<>`. Returns Ok if
    /// none was declared or the request is within budget.
    pub fn check_token_limit(&self, tokens: u64) -> Result<(), EnforcementError> {
        match self.effects.max_tokens {
            None => Ok(()),
            Some(limit) if tokens <= limit => Ok(()),
            Some(limit) => Err(EnforcementError::TokenLimitExceeded { limit, requested: tokens }),
        }
    }

    // ------------------------------------------------------- I/O validation

    /// Validate an input payload against the declared `I` type.
    ///
    /// Returns `Err(InputInvalid)` with a list of type-mismatch messages
    /// (same format as `typedown check`). If the document's type didn't
    /// expose an input slot (e.g. it's not a `Prompt<I, O>`), the check
    /// passes — runtimes that require an input schema should test with
    /// [`has_input_schema`] first.
    pub fn validate_input(&self, value: &Value) -> Result<(), EnforcementError> {
        let Some(ty) = &self.input_ty else {
            return Ok(());
        };
        let diags = run_value_check(value, ty, &self.env, &self.path);
        if diags.is_empty() {
            Ok(())
        } else {
            Err(EnforcementError::InputInvalid { diagnostics: diags })
        }
    }

    /// Validate an output payload against the declared `O` type.
    pub fn validate_output(&self, value: &Value) -> Result<(), EnforcementError> {
        let Some(ty) = &self.output_ty else {
            return Ok(());
        };
        let diags = run_value_check(value, ty, &self.env, &self.path);
        if diags.is_empty() {
            Ok(())
        } else {
            Err(EnforcementError::OutputInvalid { diagnostics: diags })
        }
    }

    pub fn has_input_schema(&self) -> bool {
        self.input_ty.is_some()
    }

    pub fn has_output_schema(&self) -> bool {
        self.output_ty.is_some()
    }

    /// Render just the input's JSON Schema fragment — useful for
    /// forwarding as a provider tool-call spec.
    pub fn input_schema(&self) -> Option<Value> {
        self.input_ty.as_ref().map(|t| to_subschema(t, &self.env))
    }

    /// Render just the output's JSON Schema fragment.
    pub fn output_schema(&self) -> Option<Value> {
        self.output_ty.as_ref().map(|t| to_subschema(t, &self.env))
    }
}

/// Errors that can occur during [`EnforcedPrompt::load`].
#[derive(Debug, Error)]
pub enum LoadError {
    #[error("failed to read `{path}`: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// The document's type machinery produced one or more errors
    /// (malformed effect row, unknown type, missing required section, …).
    /// Each entry is a stable `[tdXXX] message` line.
    #[error("typedown check failed: {}", .0.join("; "))]
    Check(Vec<String>),
    /// The frontmatter didn't declare `typedown: SomeType`.
    #[error("document has no `typedown:` frontmatter declaration")]
    NoDocType,
    /// A glob pattern in the effect policy failed to compile.
    #[error("malformed glob in effect policy: {0}")]
    GlobBuild(globset::Error),
}

/// Errors raised by runtime enforcement of a loaded contract.
///
/// Every variant includes the declared policy on the error so the caller
/// can surface a precise message (e.g. "tool `shell_exec` denied; allowed:
/// read_file, run_tests") without having to re-query the runtime.
#[derive(Debug, Clone, Error, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EnforcementError {
    #[error("tool `{tool}` is not authorized (allowed: {})", .allowed.join(", "))]
    UnauthorizedTool { tool: String, allowed: Vec<String> },
    #[error("read of `{path}` is not authorized (allowed: {})", .allowed.join(", "))]
    UnauthorizedRead { path: String, allowed: Vec<String> },
    #[error("write to `{path}` is not authorized (allowed: {})", .allowed.join(", "))]
    UnauthorizedWrite { path: String, allowed: Vec<String> },
    #[error("model `{model}` is not in the declared set (allowed: {})", .allowed.join(", "))]
    UnknownModel { model: String, allowed: Vec<String> },
    #[error("requested {requested} tokens exceeds declared ceiling of {limit}")]
    TokenLimitExceeded { limit: u64, requested: u64 },
    #[error("input value does not match declared type: {}", .diagnostics.join("; "))]
    InputInvalid { diagnostics: Vec<String> },
    #[error("output value does not match declared type: {}", .diagnostics.join("; "))]
    OutputInvalid { diagnostics: Vec<String> },
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn compile_globs(patterns: &[String]) -> Result<GlobSet, globset::Error> {
    let mut b = GlobSetBuilder::new();
    for p in patterns {
        b.add(Glob::new(p)?);
    }
    b.build()
}

/// Walk the stripped document type and pull out `(I, O)` from a
/// `Prompt<I, O>` or `Tool<A, R>` reference. Returns `(None, None)` for
/// doc types that don't expose I/O (e.g. a plain `Readme`).
fn extract_io(ty: &TdType, env: &TypeEnv) -> (Option<TdType>, Option<TdType>) {
    fn find<'a>(ty: &'a TdType, env: &'a TypeEnv) -> Option<&'a [TdType]> {
        match ty {
            TdType::NamedRef { name, type_args, .. } => {
                if matches!(name.as_str(), "Prompt" | "Tool") && type_args.len() >= 2 {
                    return Some(type_args);
                }
                // Resolve one step and recurse.
                match env.lookup(name) {
                    LookupResult::Decl(_entry) => {
                        // We can't directly recurse on the instantiated body
                        // without owning it; defer to owned search below.
                        None
                    }
                    _ => None,
                }
            }
            TdType::Intersection { parts, .. } => parts.iter().find_map(|p| find(p, env)),
            _ => None,
        }
    }

    // First try the cheap path (named ref already at hand).
    if let Some(args) = find(ty, env) {
        return (Some(args[0].clone()), Some(args[1].clone()));
    }
    // Fall back: one-step resolve aliases owning their instantiation.
    if let TdType::NamedRef { name, type_args, .. } = ty {
        if let LookupResult::Decl(entry) = env.lookup(name) {
            let expanded = env.instantiate(&entry.decl, type_args);
            return extract_io(&expanded, env);
        }
    }
    if let TdType::Intersection { parts, .. } = ty {
        for p in parts {
            let (i, o) = extract_io(p, env);
            if i.is_some() {
                return (i, o);
            }
        }
    }
    (None, None)
}

fn run_value_check(
    value: &Value,
    ty: &TdType,
    env: &TypeEnv,
    source_path: &Path,
) -> Vec<String> {
    // Use a throwaway SourceFile so `check_value` can anchor diagnostics
    // even though the value didn't come from any markdown file.
    let file = SourceFile::new(source_path, String::new());
    let mut diagnostics = Diagnostics::new();
    check_value(value, ty, env, &file, Span::DUMMY, "", &mut diagnostics);
    diagnostics
        .iter()
        .map(|d| format!("[{}] {}", d.code, d.message))
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = r#"---
typedown: Doc
---

# Reviewer

```td
import { Prompt, Uses, Reads, Writes, Model, MaxTokens } from "typedown/agents"

type In  = { diff: string, context: string }
type Out = { approved: boolean, severity: "nit" | "suggestion" | "blocking" }

export type Doc =
  & Prompt<In, Out>
  & Uses<["read_file", "run_tests"]>
  & Reads<["./src/**", "./tests/**"]>
  & Writes<[]>
  & Model<"claude-opus-4-5">
  & MaxTokens<4096>
```

## Role
r.

## Instructions
1. x

## Examples

### Example 1

**Input:** hi

**Output:** hi
"#;

    fn loaded() -> EnforcedPrompt {
        let file = SourceFile::new("reviewer.md", DOC.to_string());
        EnforcedPrompt::from_source(&file).expect("loads")
    }

    #[test]
    fn authorizes_declared_tool() {
        let p = loaded();
        assert!(p.authorize_tool("read_file").is_ok());
        assert!(p.authorize_tool("run_tests").is_ok());
    }

    #[test]
    fn rejects_undeclared_tool() {
        let p = loaded();
        let err = p.authorize_tool("shell_exec").unwrap_err();
        assert!(matches!(err, EnforcementError::UnauthorizedTool { .. }));
        // Error surfaces the allowed set for debuggable error messages.
        let msg = err.to_string();
        assert!(msg.contains("read_file"), "msg: {msg}");
        assert!(msg.contains("run_tests"), "msg: {msg}");
    }

    #[test]
    fn read_glob_matches() {
        let p = loaded();
        assert!(p.authorize_read("./src/main.rs").is_ok());
        assert!(p.authorize_read("./src/nested/module.rs").is_ok());
        assert!(p.authorize_read("./tests/it.rs").is_ok());
        assert!(p.authorize_read("/etc/passwd").is_err());
    }

    #[test]
    fn writes_are_deny_all_when_declared_empty() {
        let p = loaded();
        // `Writes<[]>` compiled to an empty GlobSet → nothing matches.
        let err = p.authorize_write("./src/main.rs").unwrap_err();
        assert!(matches!(err, EnforcementError::UnauthorizedWrite { .. }));
    }

    #[test]
    fn model_enforcement() {
        let p = loaded();
        assert!(p.check_model("claude-opus-4-5").is_ok());
        assert!(p.check_model("gpt-5").is_err());
    }

    #[test]
    fn token_ceiling() {
        let p = loaded();
        assert!(p.check_token_limit(1024).is_ok());
        assert!(p.check_token_limit(4096).is_ok());
        assert!(p.check_token_limit(4097).is_err());
    }

    #[test]
    fn validates_input_ok() {
        let p = loaded();
        p.validate_input(&serde_json::json!({
            "diff": "a",
            "context": "src/x.rs"
        }))
        .expect("valid input");
    }

    #[test]
    fn rejects_input_with_wrong_type() {
        let p = loaded();
        let err = p
            .validate_input(&serde_json::json!({ "diff": 42, "context": "x" }))
            .unwrap_err();
        match err {
            EnforcementError::InputInvalid { diagnostics } => {
                assert!(
                    diagnostics.iter().any(|d| d.contains("td502")),
                    "diags: {diagnostics:?}"
                );
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn rejects_output_with_bad_enum() {
        let p = loaded();
        let err = p
            .validate_output(&serde_json::json!({
                "approved": true,
                "severity": "critical"
            }))
            .unwrap_err();
        assert!(matches!(err, EnforcementError::OutputInvalid { .. }));
    }

    #[test]
    fn schema_includes_vendor_effects() {
        let p = loaded();
        let schema = p.schema();
        let x = &schema["x-typedown-effects"];
        assert_eq!(x["uses"], serde_json::json!(["read_file", "run_tests"]));
        assert_eq!(x["writes"], serde_json::json!([]));
    }

    #[test]
    fn input_output_fragments_isolatable() {
        let p = loaded();
        let i = p.input_schema().expect("has input");
        assert_eq!(i["type"], serde_json::json!("object"));
        assert_eq!(i["required"], serde_json::json!(["diff", "context"]));
        let o = p.output_schema().expect("has output");
        assert_eq!(o["properties"]["severity"]["enum"],
            serde_json::json!(["nit", "suggestion", "blocking"]));
    }

    #[test]
    fn prompt_without_effects_is_permissive() {
        let src = r#"---
typedown: Prompt<In, Out>
---

# x

```td
import { Prompt } from "typedown/agents"
type In = { x: string }
type Out = { y: string }
```

## Role
r.

## Instructions
1. x

## Examples

### Example 1

**Input:** hi

**Output:** hi
"#;
        let file = SourceFile::new("p.md", src.to_string());
        let p = EnforcedPrompt::from_source(&file).expect("loads");
        // No effects declared → authorize_tool is permissive.
        assert!(p.authorize_tool("anything").is_ok());
        assert!(p.check_model("any-model").is_ok());
        assert!(p.check_token_limit(999_999).is_ok());
    }

    #[test]
    fn load_fails_on_broken_doc() {
        // Reference a type that was never declared or imported. The type
        // environment will fire td403 during resolution and the runtime
        // must refuse to construct rather than silently enforcing an
        // empty policy.
        let src = r#"---
typedown: CompletelyUndefinedType
---

# x

```td
```
"#;
        let file = SourceFile::new("broken.md", src.to_string());
        let err = EnforcedPrompt::from_source(&file).unwrap_err();
        match err {
            LoadError::Check(diags) => {
                assert!(
                    diags.iter().any(|d| d.contains("td403")),
                    "expected td403 in {diags:?}"
                );
            }
            other => panic!("expected Check error, got {other:?}"),
        }
    }

    #[test]
    fn load_fails_on_malformed_effect() {
        let src = r#"---
typedown: Doc
---

# x

```td
import { Prompt, MaxTokens } from "typedown/agents"
type In = { x: string }
type Out = { y: string }
export type Doc = Prompt<In, Out> & MaxTokens<"nope">
```

## Role
r.

## Instructions
1. x

## Examples

### Example 1

**Input:** hi

**Output:** hi
"#;
        let file = SourceFile::new("bad_effect.md", src.to_string());
        let err = EnforcedPrompt::from_source(&file).unwrap_err();
        match err {
            LoadError::Check(diags) => {
                assert!(
                    diags.iter().any(|d| d.contains("td601")),
                    "expected td601 in {diags:?}"
                );
            }
            other => panic!("expected Check error, got {other:?}"),
        }
    }
}
