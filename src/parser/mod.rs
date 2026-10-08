//! Port of `phases/1-parse/index.js` and `read/{expression,context}.js`.

mod element;
pub mod entities;
mod options;
mod style;
mod tag;
pub mod utils;

use std::collections::HashSet;
use std::rc::Rc;
use std::sync::LazyLock;

use oxc_allocator::Allocator;
use oxc_ast::ast::{Expression, IdentifierName};
use oxc_ast::builder::AstBuilder;
use oxc_span::{GetSpan, Span};
use serde_json::Value;

use crate::ast::{Ast, Expr, FragId, IdentLoc, Node, NodeId, Pattern, Root, TypeAnn};
use crate::error::Result;
use crate::errors as e;
use crate::js::{JsExpr, JsParser, Src};
use crate::locator::Locator;
pub use utils::is_whitespace_char;
use utils::*;

/// An entry of the parser's stack: the root, or a template node
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Open {
    Root,
    Node(NodeId),
}

pub struct LastAutoClosedTag {
    pub tag: String,
    pub reason: String,
    pub depth: usize,
}

pub struct Parser<'a> {
    /// The (trimmed) template
    pub full: &'a str,
    /// Usually `full`; temporarily truncated while reading `{#each}` expressions
    pub template: &'a str,
    pub index: usize,
    pub loose: bool,
    pub ts: bool,
    pub ast: Ast<'a>,
    pub root: Root<'a>,
    pub stack: Vec<Open>,
    pub fragments: Vec<FragId>,
    pub meta_tags: HashSet<String>,
    pub last_auto_closed_tag: Option<LastAutoClosedTag>,
    pub css_comments: Vec<crate::css::CssComment>,
    pub loc: Rc<Locator<'a>>,
    pub js: JsParser<'a>,
    pub builder: AstBuilder<'a>,
    /// Warnings the parser emits (`w.*` calls in `phases/1-parse`)
    pub warnings: Vec<crate::analyze::Warning>,
}

impl Parser<'_> {
    pub fn warn(&mut self, start: usize, end: usize, w: crate::analyze::warnings::W) {
        self.warnings.push(crate::analyze::Warning { code: w.code, message: w.message, position: Some((start, end)) });
    }
}

/// An identifier read by `read_identifier` (the name may be empty)
#[derive(Debug, Clone, Copy)]
pub struct Ident<'a> {
    pub name: &'a str,
    pub start: usize,
    pub end: usize,
}

impl Ident<'_> {
    /// As an expression, with Svelte's `loc` (with `character`)
    pub fn expr<'x>(&self) -> Expr<'x> {
        Expr::Ident { name: self.name.to_string(), start: self.start, end: self.end, loc: IdentLoc::Svelte }
    }
}

/// What `regex_lang_attribute` finds: the `lang` of the first `<script ...>` tag outside an
/// HTML comment, if it has one
fn script_lang(source: &str) -> Option<&str> {
    let bytes = source.as_bytes();
    let mut i = 0;
    while let Some(p) = memchr_lt(bytes, i) {
        i = p;
        if bytes[i..].starts_with(b"<!--") {
            if let Some(end) = source[i + 4..].find("-->") {
                i += 4 + end + 3;
                continue;
            }
        } else if bytes[i..].starts_with(b"<script") && bytes.get(i + 7).is_some_and(|b| is_js_space(*b)) {
            if let Some(lang) = lang_in_tag(&source[i + 7..]).or_else(|| lang_after_attributes(&source[i + 7..])) {
                return Some(lang);
            }
        }
        i += 1;
    }
    None
}

fn memchr_lt(bytes: &[u8], from: usize) -> Option<usize> {
    bytes.get(from..)?.iter().position(|&b| b == b'<').map(|p| p + from)
}

fn is_js_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}

/// `lang=(["'])?([^"' >]+)\1[^>]*>` somewhere inside the tag. With a greedy `[^>]*` before it,
/// the regex prefers the last `lang=` in the tag that fits.
/// The regex's second alternative, `(?:[^=>'"/]+=(?:"[^"]*"|'[^']*'|[^>\s]+)\s+)*lang=...`:
/// attributes whose quoted values may contain `>` before `lang`
fn lang_after_attributes(rest: &str) -> Option<&str> {
    let bytes = rest.as_bytes();
    let mut i = 0;
    while i < bytes.len() && is_js_space(bytes[i]) {
        i += 1;
    }
    loop {
        if rest[i..].starts_with("lang=") {
            let after = &rest[i + 5..];
            let quote = after.chars().next().filter(|c| *c == '"' || *c == '\'');
            let value_start = quote.map_or(0, |_| 1);
            let value_len = after[value_start..].find(|c| matches!(c, '"' | '\'' | ' ' | '>')).unwrap_or(after.len() - value_start);
            if value_len == 0 {
                return None;
            }
            let value_end = value_start + value_len;
            let closes = match quote {
                Some(q) => after[value_end..].starts_with(q),
                None => true,
            };
            // `[^>]*>`: the tag must end
            if closes && after[value_end..].contains('>') {
                return Some(&after[value_start..value_end]);
            }
            return None;
        }
        // name
        let name_start = i;
        while i < bytes.len() && !matches!(bytes[i], b'=' | b'>' | b'\'' | b'"' | b'/') {
            i += 1;
        }
        if i == name_start || bytes.get(i) != Some(&b'=') {
            return None;
        }
        i += 1;
        // value
        match bytes.get(i) {
            Some(&q @ (b'"' | b'\'')) => {
                let end = rest[i + 1..].find(q as char)?;
                i += 1 + end + 1;
            }
            _ => {
                let start = i;
                while i < bytes.len() && bytes[i] != b'>' && !is_js_space(bytes[i]) {
                    i += 1;
                }
                if i == start {
                    return None;
                }
            }
        }
        // whitespace
        let ws_start = i;
        while i < bytes.len() && is_js_space(bytes[i]) {
            i += 1;
        }
        if i == ws_start {
            return None;
        }
    }
}

fn lang_in_tag(rest: &str) -> Option<&str> {
    let tag_end = rest.find('>')?;
    let tag = &rest[..tag_end];
    let mut search_end = tag.len();
    while let Some(p) = tag[..search_end].rfind("lang=") {
        let after = &tag[p + 5..];
        let quote = after.chars().next().filter(|c| *c == '"' || *c == '\'');
        let value_start = quote.map_or(0, |_| 1);
        let value_len = after[value_start..].find(|c| matches!(c, '"' | '\'' | ' ' | '>')).unwrap_or(after.len() - value_start);
        if value_len > 0 {
            let value_end = value_start + value_len;
            let closes = match quote {
                Some(q) => after[value_end..].starts_with(q),
                None => true,
            };
            if closes {
                return Some(&after[value_start..value_end]);
            }
        }
        search_end = p;
    }
    None
}

/// Parse a component. `source` must already have its BOM removed.
pub fn parse<'a>(alloc: &'a Allocator, source: &'a str, loc: Rc<Locator<'a>>, loose: bool) -> Result<(Ast<'a>, Root<'a>)> {
    parse_collecting(alloc, source, loc, loose, None)
}

/// [`parse`], appending the parser's warnings to `warnings`
pub fn parse_collecting<'a>(
    alloc: &'a Allocator,
    source: &'a str,
    loc: Rc<Locator<'a>>,
    loose: bool,
    warnings: Option<&mut Vec<crate::analyze::Warning>>,
) -> Result<(Ast<'a>, Root<'a>)> {
    let result = parse_inner(alloc, source, loc, loose);
    match result {
        Ok((ast, root, w)) => {
            if let Some(warnings) = warnings {
                warnings.extend(w);
            }
            Ok((ast, root))
        }
        Err(e) => Err(e),
    }
}

fn parse_inner<'a>(
    alloc: &'a Allocator,
    source: &'a str,
    loc: Rc<Locator<'a>>,
    loose: bool,
) -> Result<(Ast<'a>, Root<'a>, Vec<crate::analyze::Warning>)> {
    let template = js_trim_end(source);

    let ts = script_lang(source) == Some("ts");

    let mut ast = Ast::default();
    let fragment = ast.new_fragment(false);
    let root = Root {
        start: 0,
        end: 0,
        fragment,
        css: None,
        instance: None,
        module: None,
        options: None,
        comments: Vec::new(),
        ts,
    };

    let mut parser = Parser {
        full: template,
        template,
        index: 0,
        loose,
        ts,
        ast,
        root,
        stack: vec![Open::Root],
        fragments: vec![fragment],
        meta_tags: HashSet::new(),
        last_auto_closed_tag: None,
        css_comments: Vec::new(),
        js: JsParser::new(ts, loc.clone(), alloc),
        loc,
        builder: AstBuilder::new(alloc),
        warnings: Vec::new(),
    };

    while parser.index < parser.template.len() {
        if parser.match_str("<") {
            element::element(&mut parser)?;
        } else if parser.match_str("{") {
            tag::tag(&mut parser)?;
        } else {
            parser.text();
        }
    }

    if parser.stack.len() > 1 {
        let Open::Node(current) = parser.current() else { unreachable!() };
        let len = parser.template.len();
        let node = &mut parser.ast.nodes[current];
        if parser.loose {
            node.set_end(len);
        } else {
            let start = node.start();
            node.set_end(start + 1);
            if let Node::Element(el) = node {
                if el.kind == "RegularElement" {
                    let name = el.name;
                    return Err(e::element_unclosed((start, start + 1), &name));
                }
            }
            return Err(e::block_unclosed((start, start + 1)));
        }
    }

    parser.root.start = 0;
    parser.root.end = source.len();

    let root_fragment = parser.root.fragment;
    let options_index = parser.ast.fragments[root_fragment]
        .nodes
        .iter()
        .position(|&n| parser.ast.nodes[n].type_name() == "SvelteOptions");
    if let Some(i) = options_index {
        let id = parser.ast.fragments[root_fragment].nodes.remove(i);
        let options = options::read_options(&mut parser, id)?;
        parser.root.options = Some(options);

        // disallow_children
        let Node::Element(el) = &parser.ast.nodes[id] else { unreachable!() };
        let nodes = &parser.ast.fragments[el.fragment].nodes;
        if let (Some(&first), Some(&last)) = (nodes.first(), nodes.last()) {
            let start = parser.ast.nodes[first].start();
            let end = parser.ast.nodes[last].end().unwrap_or(0);
            return Err(e::svelte_meta_invalid_content((start, end), &el.name));
        }
    }

    Ok((parser.ast, parser.root, parser.warnings))
}

impl<'a> Parser<'a> {
    pub fn current(&self) -> Open {
        *self.stack.last().unwrap()
    }

    #[inline]
    pub fn byte(&self, i: usize) -> Option<u8> {
        self.template.as_bytes().get(i).copied()
    }

    #[inline]
    pub fn match_str(&self, s: &str) -> bool {
        self.template.as_bytes()[self.index.min(self.template.len())..].starts_with(s.as_bytes())
    }

    /// `eat(str, required, required_in_loose)`
    pub fn eat_req(&mut self, s: &str, required: bool, required_in_loose: bool) -> Result<bool> {
        if self.match_str(s) {
            self.index += s.len();
            return Ok(true);
        }
        if required && (!self.loose || required_in_loose) {
            return Err(e::expected_token(self.index, s));
        }
        Ok(false)
    }

    pub fn eat(&mut self, s: &str) -> bool {
        if self.match_str(s) {
            self.index += s.len();
            true
        } else {
            false
        }
    }

    pub fn expect(&mut self, s: &str) -> Result<()> {
        self.eat_req(s, true, true).map(|_| ())
    }

    pub fn allow_whitespace(&mut self) {
        while let Some(c) = char_at(self.template, self.index) {
            if !is_whitespace_char(c) {
                break;
            }
            self.index += c.len_utf8();
        }
    }

    pub fn require_whitespace(&mut self) -> Result<()> {
        match char_at(self.template, self.index) {
            Some(c) if is_whitespace_char(c) => {
                self.index += c.len_utf8();
                self.allow_whitespace();
                Ok(())
            }
            _ => Err(e::expected_whitespace(self.index)),
        }
    }

    /// `read_identifier`: the name may be empty
    pub fn read_identifier(&mut self) -> Result<Ident<'a>> {
        let start = self.index;
        let mut end = start;
        if let Some(c) = char_at(self.template, start) {
            if is_identifier_start(c) {
                end += c.len_utf8();
                while let Some(c) = char_at(self.template, end) {
                    if !is_identifier_char(c) {
                        break;
                    }
                    end += c.len_utf8();
                }
                self.index = end;
                let name = &self.template[start..end];
                if is_reserved(name) {
                    return Err(e::unexpected_reserved_word(start, name));
                }
            }
        }
        Ok(Ident { name: &self.template[start..end], start, end })
    }

    pub fn read_until(&mut self, delimiter: &str) -> Result<&'a str> {
        if self.index >= self.template.len() {
            if self.loose {
                return Ok("");
            }
            return Err(e::unexpected_eof(self.template.len()));
        }
        let start = self.index;
        let template = self.template;
        match template[start..].find(delimiter) {
            Some(p) => {
                self.index = start + p;
                Ok(&template[start..self.index])
            }
            None => {
                self.index = template.len();
                Ok(&template[start..])
            }
        }
    }

    fn text(&mut self) {
        let start = self.index;
        let bytes = self.template.as_bytes();
        let mut i = start;
        while i < bytes.len() && bytes[i] != b'<' && bytes[i] != b'{' {
            i += 1;
        }
        self.index = i;
        let raw = &self.template[start..i];
        let node = Node::Text {
            start,
            end: i,
            raw,
            data: decode_character_references(raw, false),
        };
        self.append(node);
    }

    pub fn append(&mut self, node: Node<'a>) -> NodeId {
        let id = self.ast.add(node);
        let frag = *self.fragments.last().unwrap();
        self.ast.fragments[frag].nodes.push(id);
        id
    }

    pub fn pop(&mut self) -> Option<Open> {
        if let Some(f) = self.fragments.pop() {
            // a fragment with declaration tags gets its own scope
            let fragment = &self.ast.fragments[f];
            if fragment.transparent && fragment.nodes.iter().any(|&n| matches!(self.ast.nodes[n], crate::ast::Node::DeclarationTag { .. })) {
                self.ast.fragments[f].transparent = false;
            }
        }
        self.stack.pop()
    }

    pub fn push_fragment(&mut self, id: FragId) {
        self.fragments.push(id);
    }

    // --- JS ------------------------------------------------------------------------------

    pub fn parse_expression_at(&mut self, source: Src<'a>, index: usize) -> Result<JsExpr<'a>> {
        self.js.parse_expression_at(source, index, &mut self.root.comments)
    }

    /// `get_loose_identifier`
    pub fn get_loose_identifier(&mut self, opening_token: u8) -> Option<Expr<'a>> {
        let end = find_matching_bracket(self.template, self.index, opening_token)?;
        let start = self.index;
        self.index = end;
        Some(Expr::Ident { name: String::new(), start, end, loc: IdentLoc::None })
    }

    /// `read_expression(parser, opening_token, disallow_loose)`
    pub fn read_expression_with(&mut self, opening_token: u8, disallow_loose: bool) -> Result<Expr<'a>> {
        if let Some(simple) = self.read_simple_expression() {
            return Ok(simple);
        }

        let template = Src::new(self.template, 0);
        match self.parse_expression_at(template, self.index) {
            Ok(node) => {
                // the end including any parentheses around the expression
                let mut index = node.end();
                if let Some(last) = self.root.comments.last() {
                    if last.end > index {
                        index = last.end;
                    }
                }
                self.index = index;
                Ok(Expr::Js(node))
            }
            Err(err) => {
                if self.loose && !disallow_loose {
                    if let Some(expression) = self.get_loose_identifier(opening_token) {
                        return Ok(expression);
                    }
                }
                Err(err)
            }
        }
    }

    pub fn read_expression(&mut self) -> Result<Expr<'a>> {
        self.read_expression_with(b'{', false)
    }

    /// Most template expressions are an identifier or an `a.b.c` member chain followed by `}`;
    /// build those directly
    fn read_simple_expression(&mut self) -> Option<Expr<'a>> {
        if !self.loc.lf_only() {
            return None;
        }
        let template = self.template;
        let index = self.index;
        self.allow_whitespace();
        let start = self.index;

        let Some(mut end) = read_word(template, start) else {
            self.index = index;
            return None;
        };
        if is_reserved(&template[start..end]) {
            self.index = index;
            return None;
        }

        // check the shape before building anything
        let mut chain_end = end;
        while template.as_bytes().get(chain_end) == Some(&b'.') {
            let Some(property_end) = read_word(template, chain_end + 1) else {
                self.index = index;
                return None;
            };
            chain_end = property_end;
        }
        self.index = chain_end;
        self.allow_whitespace();
        if !self.match_str("}") {
            self.index = index;
            return None;
        }
        self.index = chain_end;

        let span = |s: usize, e: usize| Span::new(s as u32, e as u32);
        let mut node = Expression::new_identifier(span(start, end), &template[start..end], &self.builder);
        while end < chain_end {
            let property_end = read_word(template, end + 1).unwrap();
            let property = IdentifierName::new(span(end + 1, property_end), &template[end + 1..property_end], &self.builder);
            node = Expression::new_static_member_expression(span(start, property_end), node, property, false, &self.builder);
            end = property_end;
        }
        Some(Expr::Js(JsExpr { expr: node, source: Src::new(template, 0), comments: None, lenient: false, remove_parens: true, fix: None }))
    }

    /// `read_pattern` (read/context.js)
    pub fn read_pattern(&mut self) -> Result<Pattern<'a>> {
        let start = self.index;
        let id = self.read_identifier()?;

        if !id.name.is_empty() {
            let type_ann = self.read_type_annotation()?;
            return Ok(Pattern::Ident { name: id.name.to_string(), start: id.start, end: id.end, type_ann });
        }

        match self.byte(start) {
            Some(b'{' | b'[') => {}
            _ => return Err(e::expected_pattern(start)),
        }

        let i = match_bracket(self.template, start, DEFAULT_BRACKETS)?;
        self.index = i;

        let source = self.js.alloc_str(&format!("{} = 1", &self.template[start..i]));
        let assign = self.parse_expression_at(Src::new(source, start), start)?;
        let type_ann = self.read_type_annotation()?;
        Ok(Pattern::Destructure { assign, type_ann })
    }

    fn read_type_annotation(&mut self) -> Result<Option<TypeAnn<'a>>> {
        let start = self.index;
        self.allow_whitespace();

        if !self.eat(":") {
            self.index = start;
            return Ok(None);
        }

        // trick the JS parser into parsing the type annotation
        let mut a = self.index - "_ as ".len();
        while !self.template.is_char_boundary(a) {
            a -= 1;
        }
        let padding = " ".repeat(self.index - "_ as ".len() - a);
        let rest = REGEX_OPTIONAL_COLON.replace_all(&self.template[self.index..], ":");
        // only the text from `_ as` on is needed: the JS parser never reads before it
        let template = Src::new(self.js.alloc_str(&format!("{padding}_ as {rest}")), a);
        let a = a + padding.len();

        let mut expr = self.parse_expression_at(template, a)?;

        // `foo: bar = baz` gets mangled — fix it
        if let Expression::AssignmentExpression(assign) = expr.inner() {
            let mut b = assign.right.without_parentheses().span().start as usize;
            while template.byte(b) != Some(b'=') {
                b -= 1;
            }
            expr = self.parse_expression_at(Src::new(template.slice(template.base, b), template.base), a)?;
        }

        // `array as item: string, index` becomes `string, index` — fix that
        let (end, seq_first) = match expr.inner() {
            Expression::SequenceExpression(seq) => (seq.expressions[0].without_parentheses().span().end as usize, true),
            other => (other.span().end as usize, false),
        };
        if seq_first {
            expr.fix = Some(Box::new(crate::js::ExprFix { seq_first: true, ..Default::default() }));
        }

        self.index = end;
        Ok(Some(TypeAnn { start, end, expr }))
    }
}

static REGEX_OPTIONAL_COLON: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"\?[\t\n\x0B\x0C\r \u{a0}\u{1680}\u{2000}-\u{200a}\u{2028}\u{2029}\u{202f}\u{205f}\u{3000}\u{feff}]*:").unwrap());

/// End of the identifier starting at `start`
fn read_word(template: &str, start: usize) -> Option<usize> {
    let c = char_at(template, start)?;
    if !is_identifier_start(c) {
        return None;
    }
    let mut end = start + c.len_utf8();
    while let Some(c) = char_at(template, end) {
        if !is_identifier_char(c) {
            break;
        }
        end += c.len_utf8();
    }
    Some(end)
}

pub fn node_type(node: &Value) -> &str {
    node.get("type").and_then(Value::as_str).unwrap_or("")
}
