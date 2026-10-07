//! Port of `phases/1-parse/index.js` and `read/{expression,context}.js`.

mod element;
pub mod entities;
mod options;
mod style;
mod tag;
pub mod utils;

use std::collections::HashSet;
use std::sync::LazyLock;

use serde_json::{json, Map, Value};

use crate::ast::{Ast, FragId, Js, Node, NodeId, Root};
use crate::error::{CompileError, Result};
use crate::errors as e;
use crate::js::{remove_parens, Js as JsParser};
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

pub struct Parser<'s> {
    /// The (trimmed) template
    pub full: &'s str,
    /// Usually `full`; temporarily truncated while reading `{#each}` expressions
    pub template: &'s str,
    pub index: usize,
    pub loose: bool,
    pub ts: bool,
    pub ast: Ast,
    pub root: Root,
    pub stack: Vec<Open>,
    pub fragments: Vec<FragId>,
    pub meta_tags: HashSet<String>,
    pub last_auto_closed_tag: Option<LastAutoClosedTag>,
    pub css_comments: Vec<Value>,
    pub loc: &'s Locator<'s>,
    pub js: JsParser<'s>,
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
            if let Some(lang) = lang_in_tag(&source[i + 7..]) {
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
pub fn parse<'s>(source: &'s str, loc: &'s Locator<'s>, loose: bool) -> Result<(Ast, Root)> {
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
        loc,
        js: JsParser::new(ts, loc),
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
                    let name = el.name.clone();
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
        let options = options::read_options(&parser.ast, id)?;
        parser.root.options = Some(options);

        // disallow_children
        let Node::Element(el) = &parser.ast.nodes[id] else { unreachable!() };
        let nodes = &parser.ast.fragments[el.fragment].nodes;
        if let (Some(&first), Some(&last)) = (nodes.first(), nodes.last()) {
            let start = parser.ast.nodes[first].start();
            let end = parser.ast.nodes[last].end().map_or(-1i64, |e| e as i64);
            return Err(e::svelte_meta_invalid_content((start, end.max(0) as usize), &el.name));
        }
    }

    Ok((parser.ast, parser.root))
}

impl<'s> Parser<'s> {
    pub fn current(&self) -> Open {
        *self.stack.last().unwrap()
    }

    pub fn current_type(&self) -> &'static str {
        match self.current() {
            Open::Root => "Root",
            Open::Node(id) => self.ast.nodes[id].type_name(),
        }
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

    /// `read_identifier`: an Identifier node (name may be empty)
    pub fn read_identifier(&mut self) -> Result<Js> {
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
        Ok(json!({
            "type": "Identifier",
            "name": &self.template[start..end],
            "start": start,
            "end": end,
            "loc": { "start": self.loc.locate(start), "end": self.loc.locate(end) }
        }))
    }

    pub fn read_until(&mut self, delimiter: &str) -> Result<&'s str> {
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
            raw: raw.to_string(),
            data: decode_character_references(raw, false),
        };
        self.append(node);
    }

    pub fn append(&mut self, node: Node) -> NodeId {
        let id = self.ast.add(node);
        let frag = *self.fragments.last().unwrap();
        self.ast.fragments[frag].nodes.push(id);
        id
    }

    pub fn pop(&mut self) -> Option<Open> {
        self.fragments.pop();
        self.stack.pop()
    }

    pub fn push_fragment(&mut self, id: FragId) {
        self.fragments.push(id);
    }

    // --- JS ------------------------------------------------------------------------------

    pub fn parse_expression_at(&mut self, source: &str, index: usize) -> Result<Js> {
        self.js.parse_expression_at(source, index, &mut self.root.comments)
    }

    /// `get_loose_identifier`
    pub fn get_loose_identifier(&mut self, opening_token: u8) -> Option<Js> {
        let end = find_matching_bracket(self.template, self.index, opening_token)?;
        let start = self.index;
        self.index = end;
        Some(json!({ "type": "Identifier", "start": start, "end": end, "name": "" }))
    }

    /// `read_expression(parser, opening_token, disallow_loose)`
    pub fn read_expression_with(&mut self, opening_token: u8, disallow_loose: bool) -> Result<Js> {
        if let Some(simple) = self.read_simple_expression() {
            return Ok(simple);
        }

        let template = self.template;
        match self.parse_expression_at(template, self.index) {
            Ok(mut node) => {
                let mut index = node_end(&node);
                if let Some(last) = self.root.comments.last() {
                    if last.end > index {
                        index = last.end;
                    }
                }
                self.index = index;
                remove_parens(&mut node);
                Ok(node)
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

    pub fn read_expression(&mut self) -> Result<Js> {
        self.read_expression_with(b'{', false)
    }

    fn read_simple_expression(&mut self) -> Option<Js> {
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

        let mut node = self.simple_identifier(start, end);
        while template.as_bytes().get(end) == Some(&b'.') {
            let Some(property_end) = read_word(template, end + 1) else {
                self.index = index;
                return None;
            };
            let property = self.simple_identifier(end + 1, property_end);
            node = json!({
                "type": "MemberExpression",
                "start": start,
                "end": property_end,
                "loc": { "start": self.loc.position(start), "end": self.loc.position(property_end) },
                "object": node,
                "property": property,
                "computed": false,
                "optional": false
            });
            end = property_end;
        }

        self.index = end;
        self.allow_whitespace();
        if !self.match_str("}") {
            self.index = index;
            return None;
        }
        self.index = end;
        Some(node)
    }

    fn simple_identifier(&self, start: usize, end: usize) -> Js {
        json!({
            "type": "Identifier",
            "start": start,
            "end": end,
            "loc": { "start": self.loc.position(start), "end": self.loc.position(end) },
            "name": &self.template[start..end]
        })
    }

    /// `read_pattern` (read/context.js)
    pub fn read_pattern(&mut self) -> Result<Js> {
        let start = self.index;
        let id = self.read_identifier()?;

        if id["name"].as_str() != Some("") {
            let annotation = self.read_type_annotation()?;
            let mut id = id;
            if let Some(a) = annotation {
                id.as_object_mut().unwrap().insert("typeAnnotation".into(), a);
            }
            return Ok(id);
        }

        match self.byte(start) {
            Some(b'{' | b'[') => {}
            _ => return Err(e::expected_pattern(start)),
        }

        let i = match_bracket(self.template, start, DEFAULT_BRACKETS)?;
        self.index = i;

        let source = format!("{} = 1", &self.template[..i]);
        let mut expression = self.parse_expression_at(&source, start)?;
        remove_parens(&mut expression);
        let mut expression = expression.get_mut("left").map(Value::take).unwrap_or(Value::Null);

        if let Some(annotation) = self.read_type_annotation()? {
            let end = annotation["end"].clone();
            let m = expression.as_object_mut().unwrap();
            m.insert("typeAnnotation".into(), annotation);
            m.insert("end".into(), end);
        }

        Ok(expression)
    }

    fn read_type_annotation(&mut self) -> Result<Option<Js>> {
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
        let template = format!("{}{}_ as {}", &self.template[..a], padding, rest);
        let a = a + padding.len();

        let mut expression = self.parse_expression_at(&template, a)?;
        remove_parens(&mut expression);

        // `foo: bar = baz` gets mangled — fix it
        if expression["type"] == "AssignmentExpression" {
            let mut b = node_start(&expression["right"]);
            while template.as_bytes()[b] != b'=' {
                b -= 1;
            }
            expression = self.parse_expression_at(&template[..b], a)?;
            remove_parens(&mut expression);
        }

        // `array as item: string, index` becomes `string, index` — fix that
        if expression["type"] == "SequenceExpression" {
            expression = expression["expressions"][0].take();
        }

        self.index = node_end(&expression);
        let mut m = Map::new();
        m.insert("type".into(), "TSTypeAnnotation".into());
        m.insert("start".into(), start.into());
        m.insert("end".into(), self.index.into());
        if let Some(t) = expression.get_mut("typeAnnotation") {
            m.insert("typeAnnotation".into(), t.take());
        }
        Ok(Some(Value::Object(m)))
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

pub fn node_start(node: &Value) -> usize {
    node.get("start").and_then(Value::as_u64).unwrap_or(0) as usize
}

pub fn node_end(node: &Value) -> usize {
    node.get("end").and_then(Value::as_u64).unwrap_or(0) as usize
}

pub fn node_type(node: &Value) -> &str {
    node.get("type").and_then(Value::as_str).unwrap_or("")
}

pub type ParseResult = std::result::Result<(Ast, Root), CompileError>;
