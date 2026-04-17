//! The typedown type-system AST.
//!
//! A `.td` block (or `typedown:` frontmatter ref) parses into a [`TdModule`]
//! of [`TdDecl`]s. Types compose via the [`TdType`] enum, which is
//! deliberately a small, explicit superset of "JSON-ish types + a few
//! content-shaped primitives (Section, Prose, OrderedList, …)".
//!
//! Why not reuse TypeScript's AST directly? Because we only need a tiny
//! subset, our error messages want domain-specific wording ("missing
//! Section"), and we'll extend this with shapes that don't exist in TS
//! (e.g. `Section<T>` is inherently a *document* construct).

use serde::Serialize;
use td_core::Span;

/// A parsed `td` module: the collection of type/interface/import declarations
/// from a single `td` code fence (or stitched across multiple fences in one
/// document — we concat them before parsing).
#[derive(Debug, Clone, Default, Serialize)]
pub struct TdModule {
    pub span: Span,
    pub imports: Vec<TdImport>,
    pub decls: Vec<TdDecl>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TdImport {
    pub span: Span,
    pub specifiers: Vec<String>,
    /// Module path — either a stdlib ref (`typedown/agents`) or a relative
    /// path (`./shared.td`). We don't resolve here; that's the checker's job.
    pub source: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TdDecl {
    pub span: Span,
    pub name: String,
    pub exported: bool,
    pub generics: Vec<String>,
    pub kind: TdDeclKind,
}

#[derive(Debug, Clone, Serialize)]
pub enum TdDeclKind {
    /// `type X<T> = ...`
    TypeAlias(TdType),
    /// `interface X<T> { ... }` — sugar for a type alias over an object type.
    Interface(TdObjectType),
}

/// A typedown type expression.
///
/// Note the blend of "value" types (String, Number, Object) and "document"
/// types (Section, Prose, OrderedList). This blend is the whole point —
/// `Prompt<I, O>` can constrain both its JSON-ish examples AND its markdown
/// body shape inside one type graph.
#[derive(Debug, Clone, Serialize)]
pub enum TdType {
    /// Primitive `string`, `number`, `boolean`, `null`.
    Primitive { span: Span, kind: TdPrim },
    /// String literal type: `"foo"`.
    StringLit { span: Span, value: String },
    /// Number literal type: `42`.
    NumberLit { span: Span, value: f64 },
    /// `string[]` sugar == `Array<string>`.
    Array { span: Span, elem: Box<TdType> },
    /// `[T1, T2, ...]` — positional tuple. Zero-width (`[]`) is legal and
    /// encodes the empty-tuple type, used by effect rows like `Writes<[]>`.
    Tuple { span: Span, elems: Vec<TdType> },
    /// `{ foo: string; bar?: number }`.
    Object(TdObjectType),
    /// `A | B | C`.
    Union { span: Span, variants: Vec<TdType> },
    /// `A & B` — intersection (used by shapes like `Prompt<I,O> & { extra }`).
    Intersection { span: Span, parts: Vec<TdType> },
    /// Reference to a named type (possibly generic), e.g. `Section<Prose>`.
    ///
    /// The `ref_target` field is filled in by the checker's resolver phase —
    /// at parse time we only know the name.
    NamedRef {
        span: Span,
        name: String,
        type_args: Vec<TdType>,
    },
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub enum TdPrim {
    String,
    Number,
    Boolean,
    Null,
    /// The top type — anything goes. Prefer not to use this in agent schemas.
    Any,
}

#[derive(Debug, Clone, Serialize)]
pub struct TdObjectType {
    pub span: Span,
    pub fields: Vec<TdField>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TdField {
    pub span: Span,
    pub name: String,
    pub optional: bool,
    pub ty: TdType,
    /// Doc comment directly above this field, if any. Surfaced in diagnostics.
    pub doc: Option<String>,
}

impl TdType {
    pub fn span(&self) -> Span {
        match self {
            TdType::Primitive { span, .. }
            | TdType::StringLit { span, .. }
            | TdType::NumberLit { span, .. }
            | TdType::Array { span, .. }
            | TdType::Tuple { span, .. }
            | TdType::Union { span, .. }
            | TdType::Intersection { span, .. }
            | TdType::NamedRef { span, .. } => *span,
            TdType::Object(o) => o.span,
        }
    }

    /// Helper: is this a reference to the given built-in by bare name?
    pub fn is_named(&self, n: &str) -> bool {
        matches!(self, TdType::NamedRef { name, .. } if name == n)
    }
}
