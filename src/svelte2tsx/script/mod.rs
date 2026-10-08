//! The `<script>` half of svelte2tsx (`src/svelte2tsx/*`), on oxc's AST.

pub mod events;
pub mod exported;
pub mod generics;
pub mod hoistable;
pub mod instance;
pub mod module;
pub mod render;
pub mod stores;
pub mod ts;

use std::collections::HashMap;

use oxc_ast::ast::*;
use oxc_span::GetSpan;

use crate::magic_string::{MagicString, MagicStringError};

pub type Result<T> = std::result::Result<T, MagicStringError>;

/// MagicString plus the `__prepends__` bookkeeping of `utils/magic-string.ts`
pub struct Out<'s> {
    pub ms: MagicString<'s>,
    prepends: HashMap<usize, Vec<String>>,
}

impl<'s> Out<'s> {
    pub fn new(ms: MagicString<'s>) -> Self {
        Out { ms, prepends: HashMap::new() }
    }

    pub fn original(&self) -> &'s str {
        self.ms.original
    }

    fn update_prepends(&mut self, index: usize, s: &str, remove_existing: bool) -> String {
        let list = self.prepends.entry(index).or_default();
        if remove_existing {
            list.clear();
        }
        list.push(s.to_string());
        list.concat()
    }

    /// `preprendStr`: prepend so the source map maps the text to the char at `index`
    pub fn prepend_str(&mut self, index: usize, s: &str) -> Result<()> {
        let to_append = self.update_prepends(index, s, false);
        let original = self.ms.original;
        let next = super::transform::next_char(original, index);
        let c = super::transform::char_at(original, index);
        self.ms.overwrite(index, next, &format!("{to_append}{c}"), true)?;
        Ok(())
    }

    /// `overwriteStr`
    pub fn overwrite_str(&mut self, start: usize, end: usize, s: &str, remove_existing: bool) -> Result<()> {
        let text = self.update_prepends(start, s, remove_existing);
        self.ms.overwrite(start, end, &text, true)?;
        Ok(())
    }

    pub fn has_prepends(&self, index: usize) -> bool {
        self.prepends.get(&index).is_some_and(|l| !l.is_empty())
    }
}

/// A TS `VariableStatement`: the declaration, and the `export` keyword's start if exported
pub struct VarStatement<'r, 'a> {
    pub decl: &'r VariableDeclaration<'a>,
    pub export_start: Option<u32>,
    /// start of the whole statement (the `export` keyword, or the declaration)
    pub start: u32,
}

/// `ts.isIdentifier(node.name)`
pub fn binding_ident<'r, 'a>(p: &'r BindingPattern<'a>) -> Option<&'r BindingIdentifier<'a>> {
    match p {
        BindingPattern::BindingIdentifier(id) => Some(id),
        _ => None,
    }
}

/// Visit the identifiers a pattern binds (periscopic/TS `extractIdentifiers` on binding names)
pub fn binding_names<'r, 'a>(p: &'r BindingPattern<'a>, out: &mut Vec<&'r BindingIdentifier<'a>>) {
    match p {
        BindingPattern::BindingIdentifier(id) => out.push(id),
        BindingPattern::ObjectPattern(o) => {
            for prop in &o.properties {
                binding_names(&prop.value, out);
            }
            if let Some(rest) = &o.rest {
                binding_names(&rest.argument, out);
            }
        }
        BindingPattern::ArrayPattern(a) => {
            for el in a.elements.iter().flatten() {
                binding_names(el, out);
            }
            if let Some(rest) = &a.rest {
                binding_names(&rest.argument, out);
            }
        }
        BindingPattern::AssignmentPattern(a) => binding_names(&a.left, out),
    }
}

/// TS `BindingElement`s of a binding pattern: `(propertyName, name, initializer, is_rest)`,
/// with `AssignmentPattern`s split into name and initializer
pub fn binding_elements<'r, 'a>(p: &'r BindingPattern<'a>) -> Vec<BindingElement<'r, 'a>> {
    let mut out = Vec::new();
    let split = |p: &'r BindingPattern<'a>| match p {
        BindingPattern::AssignmentPattern(a) => (&a.left, Some(&a.right)),
        other => (other, None),
    };
    match p {
        BindingPattern::ObjectPattern(o) => {
            for prop in &o.properties {
                let (name, init) = split(&prop.value);
                out.push(BindingElement { property_name: if prop.shorthand { None } else { Some(&prop.key) }, name, initializer: init, rest: false });
            }
            if let Some(rest) = &o.rest {
                out.push(BindingElement { property_name: None, name: &rest.argument, initializer: None, rest: true });
            }
        }
        BindingPattern::ArrayPattern(a) => {
            for el in a.elements.iter().flatten() {
                let (name, init) = split(el);
                out.push(BindingElement { property_name: None, name, initializer: init, rest: false });
            }
            if let Some(rest) = &a.rest {
                out.push(BindingElement { property_name: None, name: &rest.argument, initializer: None, rest: true });
            }
        }
        _ => {}
    }
    out
}

pub struct BindingElement<'r, 'a> {
    pub property_name: Option<&'r PropertyKey<'a>>,
    pub name: &'r BindingPattern<'a>,
    pub initializer: Option<&'r Expression<'a>>,
    pub rest: bool,
}

/// `getText()` of a node, from the script source
pub fn text<'a>(src: &'a str, span: oxc_span::Span) -> &'a str {
    &src[span.start as usize..span.end as usize]
}

/// The TS type annotation of a declarator (on the declarator in oxc)
pub fn declarator_type<'r, 'a>(d: &'r VariableDeclarator<'a>) -> Option<&'r TSType<'a>> {
    d.type_annotation.as_ref().map(|t| &t.type_annotation)
}

pub fn span_of<T: GetSpan>(n: &T) -> oxc_span::Span {
    n.span()
}

/// `String.prototype.trim`
pub fn js_trim(s: &str) -> &str {
    s.trim_matches(|c: char| crate::parser::utils::is_whitespace_char(c) || c == '\u{feff}')
}

/// `rewriteExternalImportsInNode`'s edit: the specifier's contents, rewritten
pub fn rewrite_specifier(out: &mut Out, off: usize, span: oxc_span::Span, value: &str, o: Option<&crate::svelte2tsx::rewrite_imports::RewriteExternalImports>) -> Result<()> {
    let Some(o) = o else { return Ok(()) };
    if let Some(r) = crate::svelte2tsx::rewrite_imports::external_import_rewrite(value, o) {
        out.ms.overwrite(span.start as usize + off + 1, span.end as usize + off - 1, &r.rewritten, false)?;
    }
    Ok(())
}

/// The module specifier a node imports, the way `rewriteExternalImportsInNode` finds them:
/// import/export declarations, `import()`/`require()` with a literal, import types
pub fn import_specifier<'r>(kind: &oxc_ast::AstKind<'r>) -> Option<(oxc_span::Span, &'r str)> {
    use oxc_ast::AstKind;
    let lit = |e: &'r Expression| match e {
        Expression::StringLiteral(s) => Some((s.span, s.value.as_str())),
        Expression::TemplateLiteral(t) if t.expressions.is_empty() && t.quasis.len() == 1 => {
            t.quasis[0].value.cooked.as_ref().map(|c| (t.span, c.as_str()))
        }
        _ => None,
    };
    match kind {
        AstKind::ImportDeclaration(i) => Some((i.source.span, i.source.value.as_str())),
        AstKind::ExportFromDeclaration(e) => Some((e.source.span, e.source.value.as_str())),
        AstKind::ExportAllDeclaration(e) => Some((e.source.span, e.source.value.as_str())),
        AstKind::ImportExpression(i) => lit(&i.source),
        AstKind::CallExpression(c) => match &c.callee {
            Expression::Identifier(id) if id.name == "require" => c.arguments.first().and_then(|a| a.as_expression()).and_then(lit),
            _ => None,
        },
        AstKind::TSImportType(t) => Some((t.source.span, t.source.value.as_str())),
        _ => None,
    }
}

/// Import types in JSDoc comments (`@type {import('../x').T}`)
pub fn rewrite_jsdoc_imports(out: &mut Out, ast: &ts::ScriptAst, o: Option<&crate::svelte2tsx::rewrite_imports::RewriteExternalImports>) -> Result<()> {
    let Some(o) = o else { return Ok(()) };
    static IMPORT: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| regex::Regex::new(r#"import\(\s*(?:'([^'\n]*)'|"([^"\n]*)")"#).unwrap());
    for c in &ast.program.comments {
        let (s, e) = (c.span.start as usize, c.span.end as usize);
        let text = &ast.text[s..e];
        if !text.starts_with("/**") || text.starts_with("/**/") {
            continue;
        }
        for m in IMPORT.captures_iter(text) {
            let v = m.get(1).or(m.get(2)).unwrap();
            if let Some(r) = crate::svelte2tsx::rewrite_imports::external_import_rewrite(v.as_str(), o) {
                let start = s + v.start() + ast.offset;
                out.ms.overwrite(start, start + v.len(), &r.rewritten, false)?;
            }
        }
    }
    Ok(())
}
