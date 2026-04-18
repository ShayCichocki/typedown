//! `typedown` — the CLI entrypoint.
//!
//! Subcommands:
//!   - `check <paths>...`  Type-check one or more `.md` files (or directories).
//!   - `types`             Print the stdlib module index.
//!   - `export <path>`     Compile a typed document to a JSON Schema.
//!
//! Errors are rendered via miette's fancy handler so spans land directly on
//! the offending byte range.

use std::{fs, path::PathBuf, process::ExitCode};

use clap::{Parser, Subcommand, ValueEnum};
use ignore::WalkBuilder;
use miette::{GraphicalReportHandler, GraphicalTheme};
use td_check::{check_source, resolve_doc_type, to_json_schema};
use td_core::{SourceFile, TdDiagnostic};

#[derive(Parser, Debug)]
#[command(name = "typedown", version, about = "Statically typed markdown")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Type-check markdown files against their declared typedown types.
    Check {
        /// Files or directories to check. Directories are walked recursively
        /// honoring `.gitignore`.
        #[arg(required = true)]
        paths: Vec<PathBuf>,
    },
    /// List stdlib modules available to typedown docs.
    Types,
    /// Compile a typed document's declared type to a machine-readable schema.
    ///
    /// Use this to hand the doc's type to systems outside typedown — JSON
    /// Schema validators, OpenAPI bundlers, Zod generators, LLM tool-call
    /// specs. One source of truth, many sinks.
    Export {
        /// A single markdown file to export.
        path: PathBuf,
        /// Output format. Currently only `json-schema` is supported;
        /// `typescript` and others will follow.
        #[arg(long, value_enum, default_value_t = ExportFormat::JsonSchema)]
        format: ExportFormat,
        /// Optional output path. Defaults to stdout.
        #[arg(long, short)]
        out: Option<PathBuf>,
    },
    /// Print the effect-row policy declared on a typed document.
    ///
    /// Shows the tools, reads, writes, allowed models, and token ceiling
    /// — the same data td-runtime uses to enforce behavior. Useful for
    /// policy audits and shell-based pipelines that want to pre-check
    /// capability sets before wiring up the runtime.
    Effects {
        /// Markdown file to inspect.
        path: PathBuf,
        /// Emit JSON instead of the human-readable table.
        #[arg(long)]
        json: bool,
    },
    /// Print the typed pipeline structure declared by `Compose<[…]>`.
    ///
    /// For a doc that declares `Compose<[A, B, C]>`, dumps the ordered
    /// step list with each step's I/O types and per-step effect ceiling.
    /// Use this to audit pipeline shape, pipe into orchestrators, or
    /// diff two pipelines during migration.
    Pipeline {
        /// Markdown file to inspect.
        path: PathBuf,
        /// Emit JSON instead of the human-readable table.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum ExportFormat {
    /// JSON Schema Draft 2020-12.
    JsonSchema,
    /// Vercel AI SDK TypeScript module (`generateText` with structured
    /// output, Zod schemas for every declared type, policy constants).
    AiSdk,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Cmd::Check { paths } => run_check(paths),
        Cmd::Types => run_types(),
        Cmd::Export { path, format, out } => run_export(path, format, out),
        Cmd::Effects { path, json } => run_effects(path, json),
        Cmd::Pipeline { path, json } => run_pipeline(path, json),
    }
}

fn run_check(roots: Vec<PathBuf>) -> ExitCode {
    let files = collect_markdown_files(&roots);
    if files.is_empty() {
        eprintln!("no markdown files found");
        return ExitCode::from(2);
    }

    let handler = GraphicalReportHandler::new_themed(GraphicalTheme::unicode());

    let mut total_errors = 0usize;
    let mut total_warnings = 0usize;
    let mut files_checked = 0usize;

    for path in files {
        let content = match fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("failed to read {}: {e}", path.display());
                total_errors += 1;
                continue;
            }
        };
        let file = SourceFile::new(&path, content);
        let (_doc, diagnostics) = check_source(&file);
        files_checked += 1;

        for d in diagnostics.iter() {
            render(&handler, d);
            match d.severity {
                td_core::Severity::Error => total_errors += 1,
                td_core::Severity::Warning => total_warnings += 1,
                _ => {}
            }
        }
    }

    println!(
        "\ntypedown: {files_checked} file(s) checked, {total_errors} error(s), {total_warnings} warning(s)"
    );

    if total_errors > 0 {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

fn run_export(path: PathBuf, format: ExportFormat, out: Option<PathBuf>) -> ExitCode {
    // AI SDK codegen goes through its own strict-check path: we refuse
    // to emit runtime code for any document that wouldn't pass
    // `typedown check`, because shipping generated code against a
    // broken contract just moves the blast radius downstream. The
    // JSON-schema path is more permissive — users sometimes want a
    // schema out of a work-in-progress doc.
    match format {
        ExportFormat::JsonSchema => run_export_json_schema(path, out),
        ExportFormat::AiSdk => run_export_ai_sdk(path, out),
    }
}

fn run_export_json_schema(path: PathBuf, out: Option<PathBuf>) -> ExitCode {
    let content = match fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("failed to read {}: {e}", path.display());
            return ExitCode::from(2);
        }
    };
    let file = SourceFile::new(&path, content);
    let (_doc, env, ty, effects, composition, diagnostics) = resolve_doc_type(&file);

    // Hard-fail on diagnostics originating from the type machinery itself
    // (unknown imports, syntax errors in td fences). Rendering the schema
    // of a broken type graph would be worse than silence.
    let handler = GraphicalReportHandler::new_themed(GraphicalTheme::unicode());
    let has_fatal = diagnostics.iter().any(|d| {
        matches!(d.severity, td_core::Severity::Error)
            && (d.code.starts_with("td1") || d.code.starts_with("td2"))
    });
    if has_fatal {
        for d in diagnostics.iter() {
            render(&handler, d);
        }
        return ExitCode::from(1);
    }

    let Some(ty) = ty else {
        eprintln!(
            "{}: no `typedown:` field in frontmatter — nothing to export",
            path.display()
        );
        return ExitCode::from(2);
    };

    let title = path.file_stem().and_then(|s| s.to_str()).map(str::to_string);
    let schema = to_json_schema(
        &ty,
        &env,
        title.as_deref(),
        Some(&effects),
        composition.as_ref(),
    );
    let rendered = match serde_json::to_string_pretty(&schema) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("failed to render schema: {e}");
            return ExitCode::from(2);
        }
    };
    write_or_print(out, rendered)
}

fn run_export_ai_sdk(path: PathBuf, out: Option<PathBuf>) -> ExitCode {
    let loaded = match td_codegen::LoadedDoc::from_path(path.clone()) {
        Ok(d) => d,
        Err(td_codegen::LoadError::Io { source, .. }) => {
            eprintln!("failed to read {}: {source}", path.display());
            return ExitCode::from(2);
        }
        Err(td_codegen::LoadError::Check(diags)) => {
            eprintln!("{}: typedown check failed; refusing to emit AI SDK code", path.display());
            for d in diags {
                eprintln!("  {d}");
            }
            return ExitCode::from(1);
        }
    };
    let rendered = match td_codegen::ai_sdk::emit(&loaded.as_unit()) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("ai-sdk codegen failed: {e}");
            return ExitCode::from(2);
        }
    };
    write_or_print(out, rendered)
}

fn write_or_print(out: Option<PathBuf>, rendered: String) -> ExitCode {
    match out {
        Some(out_path) => {
            if let Err(e) = fs::write(&out_path, rendered) {
                eprintln!("failed to write {}: {e}", out_path.display());
                return ExitCode::from(2);
            }
        }
        None => println!("{rendered}"),
    }
    ExitCode::SUCCESS
}

fn run_effects(path: PathBuf, json: bool) -> ExitCode {
    let content = match fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("failed to read {}: {e}", path.display());
            return ExitCode::from(2);
        }
    };
    let file = SourceFile::new(&path, content);
    let (_doc, _env, _ty, effects, _composition, _diags) = resolve_doc_type(&file);

    if json {
        let obj = serde_json::json!({
            "declared": effects.declared,
            "uses": effects.uses,
            "reads": effects.reads,
            "writes": effects.writes,
            "model": effects.models,
            "maxTokens": effects.max_tokens,
        });
        match serde_json::to_string_pretty(&obj) {
            Ok(s) => println!("{s}"),
            Err(e) => {
                eprintln!("failed to render: {e}");
                return ExitCode::from(2);
            }
        }
        return ExitCode::SUCCESS;
    }

    if !effects.declared {
        println!("{}: no effect rows declared", path.display());
        return ExitCode::SUCCESS;
    }

    println!("{}", path.display());
    println!("  uses:       {}", fmt_list(&effects.uses));
    println!("  reads:      {}", fmt_list(&effects.reads));
    println!("  writes:     {}", fmt_list(&effects.writes));
    println!("  model:      {}", fmt_list(&effects.models));
    println!(
        "  max tokens: {}",
        effects
            .max_tokens
            .map(|n| n.to_string())
            .unwrap_or_else(|| "—".into())
    );
    ExitCode::SUCCESS
}

fn fmt_list(xs: &[String]) -> String {
    if xs.is_empty() {
        "∅ (deny-all)".to_string()
    } else {
        xs.join(", ")
    }
}

fn run_pipeline(path: PathBuf, json: bool) -> ExitCode {
    let content = match fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("failed to read {}: {e}", path.display());
            return ExitCode::from(2);
        }
    };
    let file = SourceFile::new(&path, content);
    let (_doc, env, _ty, _effects, composition, _diags) = resolve_doc_type(&file);

    let Some(comp) = composition else {
        println!("{}: not a pipeline (no `Compose<[…]>` declaration)", path.display());
        return ExitCode::SUCCESS;
    };

    if json {
        let schema = serde_json::to_string_pretty(&serde_json::json!({
            "steps": comp.steps.iter().map(|s| serde_json::json!({
                "name": s.name,
                "input": td_check::to_subschema(&s.input, &env),
                "output": td_check::to_subschema(&s.output, &env),
                "effects": {
                    "declared": s.effects.declared,
                    "uses": s.effects.uses,
                    "reads": s.effects.reads,
                    "writes": s.effects.writes,
                    "model": s.effects.models,
                    "maxTokens": s.effects.max_tokens,
                },
            })).collect::<Vec<_>>(),
        }))
        .unwrap();
        println!("{schema}");
        return ExitCode::SUCCESS;
    }

    println!("{} — pipeline ({} steps)", path.display(), comp.steps.len());
    for (i, step) in comp.steps.iter().enumerate() {
        println!("  [{}] {}", i + 1, step.name);
        println!(
            "      input:      {}",
            type_summary(&step.input, &env),
        );
        println!(
            "      output:     {}",
            type_summary(&step.output, &env),
        );
        if step.effects.declared {
            println!("      uses:       {}", fmt_list(&step.effects.uses));
            if !step.effects.reads.is_empty() {
                println!("      reads:      {}", fmt_list(&step.effects.reads));
            }
            if !step.effects.writes.is_empty() {
                println!("      writes:     {}", fmt_list(&step.effects.writes));
            }
            if !step.effects.models.is_empty() {
                println!("      model:      {}", fmt_list(&step.effects.models));
            }
            if let Some(n) = step.effects.max_tokens {
                println!("      max tokens: {n}");
            }
        }
    }
    ExitCode::SUCCESS
}

/// Render a single-line type preview for the pipeline table.
///
/// Consistent with the schema export (a subschema), just pretty-printed
/// to one line so it fits in a terminal. Tries named-ref → shape; falls
/// back to a compact JSON-ish rendering for anonymous types.
fn type_summary(ty: &td_ast::td::TdType, env: &td_check::TypeEnv) -> String {
    use td_ast::td::TdType;
    match ty {
        TdType::NamedRef { name, type_args, .. } if type_args.is_empty() => name.clone(),
        TdType::NamedRef { name, type_args, .. } => format!(
            "{name}<{}>",
            type_args
                .iter()
                .map(|a| type_summary(a, env))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        other => {
            let schema = td_check::to_subschema(other, env);
            serde_json::to_string(&schema).unwrap_or_else(|_| "…".into())
        }
    }
}

fn run_types() -> ExitCode {
    for path in td_stdlib::module_paths() {
        println!("# {path}");
        if let Some(src) = td_stdlib::module_source(path) {
            println!("{src}");
        }
    }
    ExitCode::SUCCESS
}

fn render(handler: &GraphicalReportHandler, diagnostic: &TdDiagnostic) {
    let mut buf = String::new();
    // Prefix every diagnostic with its stable code so grep-based CI works.
    println!("[{}]", diagnostic.code);
    let _ = handler.render_report(&mut buf, diagnostic);
    eprintln!("{buf}");
}

fn collect_markdown_files(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for root in roots {
        if root.is_file() {
            out.push(root.clone());
            continue;
        }
        for entry in WalkBuilder::new(root).hidden(false).build().flatten() {
            if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                continue;
            }
            let p = entry.path();
            if matches!(p.extension().and_then(|s| s.to_str()), Some("md") | Some("mdx") | Some("markdown"))
            {
                out.push(p.to_path_buf());
            }
        }
    }
    out
}
