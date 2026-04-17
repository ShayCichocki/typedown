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
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum ExportFormat {
    /// JSON Schema Draft 2020-12.
    JsonSchema,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Cmd::Check { paths } => run_check(paths),
        Cmd::Types => run_types(),
        Cmd::Export { path, format, out } => run_export(path, format, out),
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
    let content = match fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("failed to read {}: {e}", path.display());
            return ExitCode::from(2);
        }
    };
    let file = SourceFile::new(&path, content);
    let (_doc, env, ty, diagnostics) = resolve_doc_type(&file);

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

    let rendered = match format {
        ExportFormat::JsonSchema => {
            let title = path.file_stem().and_then(|s| s.to_str()).map(str::to_string);
            let schema = to_json_schema(&ty, &env, title.as_deref());
            match serde_json::to_string_pretty(&schema) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("failed to render schema: {e}");
                    return ExitCode::from(2);
                }
            }
        }
    };

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
