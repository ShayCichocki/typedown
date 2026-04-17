//! Type environment: the set of names the checker can resolve.
//!
//! Three tiers, resolved in order:
//!
//! 1. **Local decls** — types declared in the doc's ```td fences.
//! 2. **Imports** — names pulled in from stdlib modules.
//! 3. **Built-ins** — `Section`, `Prose`, etc. — available without import.
//!
//! The resolver intentionally doesn't understand cross-file user modules
//! yet; path-based imports beyond the stdlib raise a diagnostic. That
//! feature is a clean follow-up once the core semantics settle.

use std::collections::HashMap;

use td_ast::td::{TdDecl, TdDeclKind, TdModule, TdType};
use td_core::{Diagnostics, Severity, SourceFile, TdDiagnostic};
use td_parse::parse_td_module;
use td_stdlib::{module_source, Builtin};

/// An entry in the type environment: either a user-defined decl or a
/// stdlib decl that was pulled in via import.
#[derive(Debug, Clone)]
pub struct EnvEntry {
    pub decl: TdDecl,
    pub origin: EntryOrigin,
}

#[derive(Debug, Clone)]
pub enum EntryOrigin {
    Local,
    Stdlib(&'static str),
}

#[derive(Debug)]
pub struct TypeEnv {
    pub entries: HashMap<String, EnvEntry>,
}

impl TypeEnv {
    pub fn build(module: &TdModule, file: &SourceFile) -> (Self, Diagnostics) {
        let mut diagnostics = Diagnostics::new();
        let mut entries: HashMap<String, EnvEntry> = HashMap::new();

        // 1. Local decls.
        for decl in &module.decls {
            if let Some(prev) = entries.insert(
                decl.name.clone(),
                EnvEntry {
                    decl: decl.clone(),
                    origin: EntryOrigin::Local,
                },
            ) {
                // Duplicate local name.
                diagnostics.push(
                    TdDiagnostic::error(
                        "td201",
                        format!("duplicate type `{}`", decl.name),
                        file,
                        decl.span,
                        "redeclared here",
                    )
                    .with_help(format!("previous declaration at span {:?}", prev.decl.span)),
                );
            }
        }

        // 2. Imports.
        for import in &module.imports {
            let Some(source) = module_source(&import.source) else {
                diagnostics.push(
                    TdDiagnostic::error(
                        "td202",
                        format!("unknown module `{}`", import.source),
                        file,
                        import.span,
                        "module not found",
                    )
                    .with_help(
                        "typedown currently only resolves `typedown/*` stdlib modules; \
                         user-authored modules will land in a future release."
                            .to_string(),
                    ),
                );
                continue;
            };

            // Parse the stdlib source. Any diagnostics from stdlib code are
            // internal bugs, so they're elevated to errors with a clear code.
            let (stdlib_mod, stdlib_diags) = parse_td_module(source, file, 0);
            for d in stdlib_diags.into_vec() {
                let d = TdDiagnostic {
                    code: "td299".into(),
                    message: format!("internal: stdlib module `{}` failed to parse: {}", import.source, d.message),
                    severity: Severity::Error,
                    ..d
                };
                diagnostics.push(d);
            }

            let stdlib_decls: HashMap<String, TdDecl> = stdlib_mod
                .decls
                .into_iter()
                .map(|d| (d.name.clone(), d))
                .collect();

            for name in &import.specifiers {
                let Some(decl) = stdlib_decls.get(name) else {
                    diagnostics.push(TdDiagnostic::error(
                        "td203",
                        format!("`{}` is not exported from `{}`", name, import.source),
                        file,
                        import.span,
                        "unresolved import",
                    ));
                    continue;
                };
                // Built-in content-shape types (Section, Prose, …) are
                // recognized without an entry: letting them fall through to
                // `Builtin::from_name` preserves the hand-written semantics.
                // Users may still `import` them for readability.
                if Builtin::from_name(name).is_some() {
                    continue;
                }
                entries
                    .entry(name.clone())
                    .or_insert_with(|| EnvEntry {
                        decl: decl.clone(),
                        origin: EntryOrigin::Stdlib(stdlib_path_intern(&import.source)),
                    });
            }
        }

        (TypeEnv { entries }, diagnostics)
    }

    /// Look up a name; returns `None` if neither a decl nor a built-in.
    pub fn lookup(&self, name: &str) -> LookupResult<'_> {
        if let Some(entry) = self.entries.get(name) {
            LookupResult::Decl(entry)
        } else if let Some(b) = Builtin::from_name(name) {
            LookupResult::Builtin(b)
        } else {
            LookupResult::Missing
        }
    }

    /// Substitute type arguments into a decl's body. Returns a freshly
    /// rewritten [`TdType`].
    ///
    /// If `decl.generics.len() != type_args.len()` we still substitute what
    /// we can and let the caller emit a diagnostic — failing fast here
    /// would obscure downstream errors.
    pub fn instantiate(&self, decl: &TdDecl, type_args: &[TdType]) -> TdType {
        let body = match &decl.kind {
            TdDeclKind::TypeAlias(t) => t.clone(),
            TdDeclKind::Interface(o) => TdType::Object(o.clone()),
        };
        if decl.generics.is_empty() {
            return body;
        }
        let mapping: HashMap<&str, &TdType> = decl
            .generics
            .iter()
            .zip(type_args.iter())
            .map(|(p, a)| (p.as_str(), a))
            .collect();
        substitute(&body, &mapping)
    }
}

pub enum LookupResult<'a> {
    Decl(&'a EnvEntry),
    Builtin(Builtin),
    Missing,
}

/// Recursively substitute generic parameters with their concrete args.
fn substitute(ty: &TdType, map: &HashMap<&str, &TdType>) -> TdType {
    match ty {
        TdType::Primitive { .. } | TdType::StringLit { .. } | TdType::NumberLit { .. } => {
            ty.clone()
        }
        TdType::Array { span, elem } => TdType::Array {
            span: *span,
            elem: Box::new(substitute(elem, map)),
        },
        TdType::Tuple { span, elems } => TdType::Tuple {
            span: *span,
            elems: elems.iter().map(|e| substitute(e, map)).collect(),
        },
        TdType::Object(o) => {
            let fields = o
                .fields
                .iter()
                .map(|f| td_ast::td::TdField {
                    span: f.span,
                    name: f.name.clone(),
                    optional: f.optional,
                    ty: substitute(&f.ty, map),
                    doc: f.doc.clone(),
                })
                .collect();
            TdType::Object(td_ast::td::TdObjectType {
                span: o.span,
                fields,
            })
        }
        TdType::Union { span, variants } => TdType::Union {
            span: *span,
            variants: variants.iter().map(|v| substitute(v, map)).collect(),
        },
        TdType::Intersection { span, parts } => TdType::Intersection {
            span: *span,
            parts: parts.iter().map(|p| substitute(p, map)).collect(),
        },
        TdType::NamedRef {
            span,
            name,
            type_args,
        } => {
            if type_args.is_empty() {
                if let Some(&sub) = map.get(name.as_str()) {
                    return sub.clone();
                }
            }
            TdType::NamedRef {
                span: *span,
                name: name.clone(),
                type_args: type_args.iter().map(|a| substitute(a, map)).collect(),
            }
        }
    }
}

fn stdlib_path_intern(s: &str) -> &'static str {
    // The list of known stdlib paths is closed, so interning is trivial.
    match s {
        "typedown/agents" => "typedown/agents",
        "typedown/docs" => "typedown/docs",
        _ => "unknown",
    }
}
