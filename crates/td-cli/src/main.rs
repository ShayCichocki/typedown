//! `typedown` — the CLI entrypoint.
//!
//! Subcommands:
//!   - `check <paths>...`  Type-check one or more `.md` files (or directories).
//!   - `types`             Print the stdlib module index.
//!
//! Errors are rendered via miette's fancy handler so spans land directly on
//! the offending byte range.

use std::{fs, path::PathBuf, process::ExitCode};

use clap::{Parser, Subcommand};
use ignore::WalkBuilder;
use miette::{GraphicalReportHandler, GraphicalTheme};
use td_check::check_source;
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
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Cmd::Check { paths } => run_check(paths),
        Cmd::Types => run_types(),
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
