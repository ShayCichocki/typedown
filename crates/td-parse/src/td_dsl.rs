//! Parser for the typedown DSL (`td` fences).
//!
//! Grammar (TS-flavored subset):
//!
//! ```text
//! module      := (import | decl)*
//! import      := 'import' '{' ident (',' ident)* '}' 'from' string ';'?
//! decl        := 'export'? ('type' ident generics? '=' type ';'?
//!                | 'interface' ident generics? object)
//! generics    := '<' ident (',' ident)* '>'
//! type        := union
//! union       := intersection ('|' intersection)*
//! intersection:= postfix ('&' postfix)*
//! postfix     := atom ('[' ']')*
//! atom        := primitive | string_lit | number_lit | named_ref | object
//!              | '(' type ')'
//! primitive   := 'string' | 'number' | 'boolean' | 'null' | 'any'
//! named_ref   := ident ('<' type (',' type)* '>')?
//! object      := '{' (field (';'|',')?)* '}'
//! field       := (doc_comment)? ident '?'? ':' type
//! ```
//!
//! Deliberately tiny. We get: unions, intersections, generics, arrays, object
//! types, and imports. Everything the checker needs for `Prompt<I, O> & {...}`
//! style declarations.

use td_ast::td::{
    TdDecl, TdDeclKind, TdField, TdImport, TdModule, TdObjectType, TdPrim, TdType,
};
use td_core::{Diagnostics, Severity, SourceFile, Span, TdDiagnostic};

pub fn parse_td_module(src: &str, file: &SourceFile, source_offset: usize) -> (TdModule, Diagnostics) {
    let mut diagnostics = Diagnostics::new();
    let tokens = tokenize(src, source_offset);
    let mut parser = Parser {
        tokens: &tokens,
        pos: 0,
        file,
        diagnostics: &mut diagnostics,
    };
    let module = parser.parse_module();
    (module, diagnostics)
}

// ---------------------------------------------------------------------------
// Lexer
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Ident(String),
    StringLit(String),
    NumberLit(f64),
    Punct(char),
    DocComment(String),
    Eof,
}

#[derive(Debug, Clone)]
struct Spanned {
    tok: Tok,
    span: Span,
}

/// Tokenize a `td` snippet. `base` is added to all spans so diagnostics point
/// back into the outer markdown file.
fn tokenize(input: &str, base: usize) -> Vec<Spanned> {
    let bytes = input.as_bytes();
    let mut i = 0;
    let mut out = Vec::new();
    let mut pending_doc: Option<String> = None;

    while i < bytes.len() {
        let c = bytes[i];
        // Skip ASCII whitespace.
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        // Line comment `// ...` — capture `///` as doc.
        if c == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            let is_doc = i + 2 < bytes.len() && bytes[i + 2] == b'/';
            let start = i;
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            if is_doc {
                let content = &input[start + 3..i];
                let entry = pending_doc.get_or_insert_with(String::new);
                if !entry.is_empty() {
                    entry.push('\n');
                }
                entry.push_str(content.trim());
            }
            continue;
        }
        // Block comment `/* ... */`.
        if c == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            let is_doc = i + 2 < bytes.len() && bytes[i + 2] == b'*';
            i += 2;
            let start_content = i;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            let end_content = i;
            if is_doc {
                let content = &input[start_content..end_content];
                pending_doc = Some(content.trim().to_string());
            }
            i = (i + 2).min(bytes.len());
            continue;
        }

        // Punctuation we care about.
        if matches!(
            c,
            b'{' | b'}' | b'(' | b')' | b'<' | b'>' | b'[' | b']'
            | b',' | b';' | b':' | b'=' | b'|' | b'&' | b'?'
        ) {
            out.push(Spanned {
                tok: Tok::Punct(c as char),
                span: Span::new(base + i, base + i + 1),
            });
            i += 1;
            continue;
        }

        // String literal: `"..."` or `'...'`. No escapes beyond `\\`/`\"`.
        if c == b'"' || c == b'\'' {
            let quote = c;
            let start = i;
            i += 1;
            let mut s = String::new();
            while i < bytes.len() && bytes[i] != quote {
                if bytes[i] == b'\\' && i + 1 < bytes.len() {
                    s.push(bytes[i + 1] as char);
                    i += 2;
                } else {
                    s.push(bytes[i] as char);
                    i += 1;
                }
            }
            let end = (i + 1).min(bytes.len());
            i = end;
            out.push(Spanned {
                tok: Tok::StringLit(s),
                span: Span::new(base + start, base + end),
            });
            continue;
        }

        // Number literal (simple int/float).
        if c.is_ascii_digit()
            || (c == b'-' && i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit())
        {
            let start = i;
            if c == b'-' {
                i += 1;
            }
            while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
                i += 1;
            }
            let text = &input[start..i];
            let value: f64 = text.parse().unwrap_or(0.0);
            out.push(Spanned {
                tok: Tok::NumberLit(value),
                span: Span::new(base + start, base + i),
            });
            continue;
        }

        // Identifier (ASCII + underscore).
        if c == b'_' || c.is_ascii_alphabetic() {
            let start = i;
            while i < bytes.len()
                && (bytes[i] == b'_' || bytes[i].is_ascii_alphanumeric())
            {
                i += 1;
            }
            let ident = input[start..i].to_string();
            // If we had pending doc, attach it to next *identifier* emission
            // by emitting a DocComment token just before.
            if let Some(doc) = pending_doc.take() {
                out.push(Spanned {
                    tok: Tok::DocComment(doc),
                    span: Span::new(base + start, base + start),
                });
            }
            out.push(Spanned {
                tok: Tok::Ident(ident),
                span: Span::new(base + start, base + i),
            });
            continue;
        }

        // Unknown byte — skip with a silent drop. We don't produce a
        // diagnostic here because we'll naturally fail at the parser layer
        // with a span-rich error.
        i += 1;
    }

    out.push(Spanned {
        tok: Tok::Eof,
        span: Span::new(base + input.len(), base + input.len()),
    });
    out
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

struct Parser<'a> {
    tokens: &'a [Spanned],
    pos: usize,
    file: &'a SourceFile,
    diagnostics: &'a mut Diagnostics,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> &Spanned {
        &self.tokens[self.pos]
    }

    fn peek_kind(&self) -> &Tok {
        &self.tokens[self.pos].tok
    }

    fn bump(&mut self) -> &Spanned {
        let t = &self.tokens[self.pos];
        if self.pos + 1 < self.tokens.len() {
            self.pos += 1;
        }
        t
    }

    fn eat_punct(&mut self, p: char) -> bool {
        if matches!(self.peek_kind(), Tok::Punct(c) if *c == p) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn expect_punct(&mut self, p: char) -> Option<Span> {
        let sp = self.peek().span;
        if self.eat_punct(p) {
            Some(sp)
        } else {
            self.error(format!("expected `{p}`"), format!("got `{}`", self.peek_display()), sp);
            None
        }
    }

    fn expect_ident(&mut self) -> Option<(String, Span)> {
        let Spanned { tok, span } = self.peek().clone();
        if let Tok::Ident(name) = tok {
            self.bump();
            Some((name, span))
        } else {
            self.error(
                "expected identifier".to_string(),
                format!("got `{}`", self.peek_display()),
                span,
            );
            None
        }
    }

    fn peek_display(&self) -> String {
        match self.peek_kind() {
            Tok::Ident(s) => s.clone(),
            Tok::StringLit(s) => format!("\"{s}\""),
            Tok::NumberLit(n) => n.to_string(),
            Tok::Punct(c) => c.to_string(),
            Tok::DocComment(_) => "/** */".to_string(),
            Tok::Eof => "<eof>".to_string(),
        }
    }

    fn error(&mut self, message: String, label: String, span: Span) {
        self.diagnostics.push(
            TdDiagnostic::error("td101", message, self.file, span, label)
                .with_severity(Severity::Error)
                .with_help("see `td` DSL grammar in typedown docs"),
        );
    }

    // --- module / top level ------------------------------------------------

    fn parse_module(&mut self) -> TdModule {
        let start = self.peek().span;
        let mut imports = Vec::new();
        let mut decls = Vec::new();

        while !matches!(self.peek_kind(), Tok::Eof) {
            // Skip stray doc comments at the top (they'll attach to the next
            // thing). In practice parse_decl pulls them in where relevant.
            if matches!(self.peek_kind(), Tok::DocComment(_)) {
                self.bump();
                continue;
            }
            match self.peek_kind() {
                Tok::Ident(kw) if kw == "import" => {
                    if let Some(im) = self.parse_import() {
                        imports.push(im);
                    }
                }
                Tok::Ident(kw) if kw == "export" || kw == "type" || kw == "interface" => {
                    if let Some(d) = self.parse_decl() {
                        decls.push(d);
                    }
                }
                _ => {
                    // Skip unknown token to recover — we've already emitted a
                    // diagnostic upstream if warranted.
                    self.bump();
                }
            }
        }

        let end = self.peek().span;
        TdModule {
            span: start.join(end),
            imports,
            decls,
        }
    }

    fn parse_import(&mut self) -> Option<TdImport> {
        let start = self.bump().span; // `import`
        self.expect_punct('{')?;
        let mut specifiers = Vec::new();
        loop {
            if self.eat_punct('}') {
                break;
            }
            let (name, _) = self.expect_ident()?;
            specifiers.push(name);
            if !self.eat_punct(',') && !matches!(self.peek_kind(), Tok::Punct('}')) {
                self.expect_punct('}')?;
                break;
            }
        }
        // expect `from`
        match self.peek_kind() {
            Tok::Ident(kw) if kw == "from" => {
                self.bump();
            }
            _ => {
                let sp = self.peek().span;
                self.error(
                    "expected `from` in import".into(),
                    format!("got `{}`", self.peek_display()),
                    sp,
                );
                return None;
            }
        }
        // string literal
        let (source, end) = match self.peek_kind() {
            Tok::StringLit(s) => {
                let s = s.clone();
                let sp = self.bump().span;
                (s, sp)
            }
            _ => {
                let sp = self.peek().span;
                self.error(
                    "expected module path string".into(),
                    format!("got `{}`", self.peek_display()),
                    sp,
                );
                return None;
            }
        };
        self.eat_punct(';');
        Some(TdImport {
            span: start.join(end),
            specifiers,
            source,
        })
    }

    fn parse_decl(&mut self) -> Option<TdDecl> {
        let start = self.peek().span;
        let exported = matches!(self.peek_kind(), Tok::Ident(kw) if kw == "export");
        if exported {
            self.bump();
        }
        let keyword = match self.peek_kind() {
            Tok::Ident(k) if k == "type" || k == "interface" => k.clone(),
            _ => {
                let sp = self.peek().span;
                self.error(
                    "expected `type` or `interface`".into(),
                    format!("got `{}`", self.peek_display()),
                    sp,
                );
                self.bump();
                return None;
            }
        };
        self.bump();
        let (name, _) = self.expect_ident()?;
        let generics = self.parse_generics();

        let kind = if keyword == "type" {
            self.expect_punct('=')?;
            let ty = self.parse_type();
            self.eat_punct(';');
            TdDeclKind::TypeAlias(ty)
        } else {
            // interface → object body
            let obj = self.parse_object()?;
            TdDeclKind::Interface(obj)
        };

        let end_span = match &kind {
            TdDeclKind::TypeAlias(t) => t.span(),
            TdDeclKind::Interface(o) => o.span,
        };

        Some(TdDecl {
            span: start.join(end_span),
            name,
            exported,
            generics,
            kind,
        })
    }

    fn parse_generics(&mut self) -> Vec<String> {
        if !matches!(self.peek_kind(), Tok::Punct('<')) {
            return Vec::new();
        }
        self.bump();
        let mut params = Vec::new();
        loop {
            if self.eat_punct('>') {
                break;
            }
            if let Some((name, _)) = self.expect_ident() {
                params.push(name);
            } else {
                break;
            }
            if !self.eat_punct(',') && !matches!(self.peek_kind(), Tok::Punct('>')) {
                self.expect_punct('>');
                break;
            }
        }
        params
    }

    // --- type expressions --------------------------------------------------

    fn parse_type(&mut self) -> TdType {
        self.parse_union()
    }

    fn parse_union(&mut self) -> TdType {
        let first = self.parse_intersection();
        if !matches!(self.peek_kind(), Tok::Punct('|')) {
            return first;
        }
        let mut variants = vec![first];
        while self.eat_punct('|') {
            variants.push(self.parse_intersection());
        }
        let span = variants
            .iter()
            .map(|v| v.span())
            .reduce(|a, b| a.join(b))
            .unwrap_or(Span::DUMMY);
        TdType::Union { span, variants }
    }

    fn parse_intersection(&mut self) -> TdType {
        let first = self.parse_postfix();
        if !matches!(self.peek_kind(), Tok::Punct('&')) {
            return first;
        }
        let mut parts = vec![first];
        while self.eat_punct('&') {
            parts.push(self.parse_postfix());
        }
        let span = parts
            .iter()
            .map(|v| v.span())
            .reduce(|a, b| a.join(b))
            .unwrap_or(Span::DUMMY);
        TdType::Intersection { span, parts }
    }

    fn parse_postfix(&mut self) -> TdType {
        let mut ty = self.parse_atom();
        loop {
            if self.eat_punct('[') {
                let end = self.expect_punct(']').unwrap_or(ty.span());
                let span = ty.span().join(end);
                ty = TdType::Array {
                    span,
                    elem: Box::new(ty),
                };
            } else {
                break;
            }
        }
        ty
    }

    fn parse_atom(&mut self) -> TdType {
        let Spanned { tok, span } = self.peek().clone();
        match tok {
            Tok::Punct('{') => TdType::Object(self.parse_object().unwrap_or(TdObjectType {
                span,
                fields: vec![],
            })),
            Tok::Punct('(') => {
                self.bump();
                let inner = self.parse_type();
                self.expect_punct(')');
                inner
            }
            Tok::StringLit(value) => {
                self.bump();
                TdType::StringLit { span, value }
            }
            Tok::NumberLit(value) => {
                self.bump();
                TdType::NumberLit { span, value }
            }
            Tok::Ident(name) => {
                self.bump();
                match name.as_str() {
                    "string" => TdType::Primitive {
                        span,
                        kind: TdPrim::String,
                    },
                    "number" => TdType::Primitive {
                        span,
                        kind: TdPrim::Number,
                    },
                    "boolean" => TdType::Primitive {
                        span,
                        kind: TdPrim::Boolean,
                    },
                    "null" => TdType::Primitive {
                        span,
                        kind: TdPrim::Null,
                    },
                    "any" => TdType::Primitive {
                        span,
                        kind: TdPrim::Any,
                    },
                    _ => {
                        // possibly generic reference
                        let mut type_args = Vec::new();
                        let mut end = span;
                        if self.eat_punct('<') {
                            loop {
                                if self.eat_punct('>') {
                                    break;
                                }
                                let arg = self.parse_type();
                                end = arg.span();
                                type_args.push(arg);
                                if !self.eat_punct(',') && !matches!(self.peek_kind(), Tok::Punct('>')) {
                                    if let Some(sp) = self.expect_punct('>') {
                                        end = sp;
                                    }
                                    break;
                                }
                            }
                        }
                        TdType::NamedRef {
                            span: span.join(end),
                            name,
                            type_args,
                        }
                    }
                }
            }
            _ => {
                self.error(
                    "expected a type".into(),
                    format!("got `{}`", self.peek_display()),
                    span,
                );
                self.bump();
                TdType::Primitive {
                    span,
                    kind: TdPrim::Any,
                }
            }
        }
    }

    fn parse_object(&mut self) -> Option<TdObjectType> {
        let start = self.expect_punct('{')?;
        let mut fields = Vec::new();
        loop {
            // Skip separators.
            while self.eat_punct(';') || self.eat_punct(',') {}
            if self.eat_punct('}') {
                let end = self.tokens[self.pos.saturating_sub(1)].span;
                return Some(TdObjectType {
                    span: start.join(end),
                    fields,
                });
            }
            // Optional doc comment.
            let doc = if let Tok::DocComment(d) = self.peek_kind() {
                let d = d.clone();
                self.bump();
                Some(d)
            } else {
                None
            };
            let Some((name, name_span)) = self.expect_ident() else {
                self.bump();
                continue;
            };
            let optional = self.eat_punct('?');
            self.expect_punct(':')?;
            let ty = self.parse_type();
            let span = name_span.join(ty.span());
            fields.push(TdField {
                span,
                name,
                optional,
                ty,
                doc,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> (TdModule, Diagnostics) {
        let file = SourceFile::new("t.td", src.to_string());
        parse_td_module(src, &file, 0)
    }

    #[test]
    fn parses_simple_type_alias() {
        let (m, d) = parse("type Foo = { name: string; age?: number }");
        assert!(d.is_empty(), "diags: {:#?}", d.into_vec());
        assert_eq!(m.decls.len(), 1);
        assert_eq!(m.decls[0].name, "Foo");
        match &m.decls[0].kind {
            TdDeclKind::TypeAlias(TdType::Object(o)) => {
                assert_eq!(o.fields.len(), 2);
                assert!(o.fields[1].optional);
            }
            other => panic!("wrong kind: {other:?}"),
        }
    }

    #[test]
    fn parses_generic_named_ref() {
        let (m, _) = parse("type Doc = Section<Prose>");
        match &m.decls[0].kind {
            TdDeclKind::TypeAlias(TdType::NamedRef { name, type_args, .. }) => {
                assert_eq!(name, "Section");
                assert_eq!(type_args.len(), 1);
            }
            other => panic!("wrong kind: {other:?}"),
        }
    }

    #[test]
    fn parses_intersection_and_union() {
        let (m, _) = parse("type Doc = A & (B | C) & { x: string[] }");
        match &m.decls[0].kind {
            TdDeclKind::TypeAlias(TdType::Intersection { parts, .. }) => {
                assert_eq!(parts.len(), 3);
            }
            other => panic!("wrong kind: {other:?}"),
        }
    }

    #[test]
    fn parses_import() {
        let (m, _) = parse("import { Section, Prose } from \"typedown/agents\"");
        assert_eq!(m.imports.len(), 1);
        assert_eq!(m.imports[0].specifiers, vec!["Section", "Prose"]);
        assert_eq!(m.imports[0].source, "typedown/agents");
    }

    #[test]
    fn parses_interface() {
        let (m, _) = parse("interface Comment { file: string; line: number }");
        match &m.decls[0].kind {
            TdDeclKind::Interface(o) => assert_eq!(o.fields.len(), 2),
            other => panic!("wrong kind: {other:?}"),
        }
    }
}
