//! The legacy (Svelte 4 shaped) AST, a port of `svelte/src/compiler/legacy.js`.
//!
//! `parse(source)` without `modern: true` returns this shape, and it's what `svelte2tsx`
//! walks. Only the template part is converted (`html` and `_comments`): `svelte2tsx` blanks
//! out `<script>` and `<style>` before parsing, so it never looks at the CSS AST.
//! Nodes borrow expressions from the modern AST.

use std::borrow::Cow;

use serde_json::{json, Map, Value};

use crate::ast::*;
use crate::js::ToJson;

#[derive(Debug)]
pub struct LegacyRoot<'m, 'a> {
    /// `html.start` / `html.end`: `None` when there are no nodes
    pub start: Option<usize>,
    pub end: Option<usize>,
    pub children: Vec<LNode<'m, 'a>>,
}

/// What `remove_surrounding_whitespace_nodes` leaves of a text node
#[derive(Debug)]
pub struct LText<'a> {
    pub start: usize,
    pub end: usize,
    /// `None` for text inside a non-top-level `<style>` (the legacy AST drops `raw` there)
    pub raw: Option<&'a str>,
    pub data: Cow<'a, str>,
}

#[derive(Debug)]
pub enum LNode<'m, 'a> {
    Text(LText<'a>),
    Comment { start: usize, end: usize, data: &'a str, ignores: Vec<String> },
    MustacheTag { start: usize, end: usize, expression: &'m Expr<'a> },
    RawMustacheTag { start: usize, end: usize, expression: &'m Expr<'a> },
    /// kept in its modern shape
    DebugTag { start: usize, end: usize, id: NodeId, identifiers: &'m DebugArgs<'a> },
    /// `{@const id = init}`: legacy wraps it as an AssignmentExpression
    ConstTag { start: usize, end: usize, id: &'m Pattern<'a>, init: &'m Expr<'a> },
    RenderTag { start: usize, end: usize, expression: &'m Expr<'a> },
    /// kept in its modern shape
    DeclarationTag { id: NodeId, start: usize, end: usize },
    IfBlock {
        start: usize,
        end: Option<usize>,
        expression: &'m Expr<'a>,
        children: Vec<LNode<'m, 'a>>,
        else_block: Option<Box<LElseBlock<'m, 'a>>>,
        elseif: bool,
    },
    EachBlock {
        start: usize,
        end: Option<usize>,
        children: Vec<LNode<'m, 'a>>,
        context: Option<&'m Pattern<'a>>,
        expression: &'m Expr<'a>,
        index: Option<&'m str>,
        key: Option<&'m Expr<'a>>,
        else_block: Option<Box<LElseBlock<'m, 'a>>>,
    },
    AwaitBlock {
        start: usize,
        end: Option<usize>,
        expression: &'m Expr<'a>,
        value: Option<&'m Pattern<'a>>,
        error: Option<&'m Pattern<'a>>,
        pending: LSubBlock<'m, 'a>,
        then: LSubBlock<'m, 'a>,
        catch: LSubBlock<'m, 'a>,
    },
    KeyBlock { start: usize, end: Option<usize>, expression: &'m Expr<'a>, children: Vec<LNode<'m, 'a>> },
    SnippetBlock {
        start: usize,
        end: Option<usize>,
        expression: &'m Expr<'a>,
        /// the parsed arrow function the parameters come from
        parameters: Option<&'m crate::js::JsExpr<'a>>,
        type_params: Option<&'m str>,
        children: Vec<LNode<'m, 'a>>,
    },
    Element(LElement<'m, 'a>),
}

impl LNode<'_, '_> {
    pub fn start(&self) -> Option<usize> {
        Some(match self {
            LNode::Text(t) => t.start,
            LNode::Comment { start, .. }
            | LNode::MustacheTag { start, .. }
            | LNode::RawMustacheTag { start, .. }
            | LNode::DebugTag { start, .. }
            | LNode::ConstTag { start, .. }
            | LNode::RenderTag { start, .. }
            | LNode::IfBlock { start, .. }
            | LNode::EachBlock { start, .. }
            | LNode::AwaitBlock { start, .. }
            | LNode::KeyBlock { start, .. }
            | LNode::SnippetBlock { start, .. } => *start,
            LNode::Element(el) => el.start,
            LNode::DeclarationTag { start, .. } => *start,
        })
    }
}

#[derive(Debug)]
pub struct LElseBlock<'m, 'a> {
    pub start: usize,
    pub end: usize,
    pub children: Vec<LNode<'m, 'a>>,
}

/// `PendingBlock`, `ThenBlock` or `CatchBlock`
#[derive(Debug)]
pub struct LSubBlock<'m, 'a> {
    pub start: Option<usize>,
    pub end: Option<usize>,
    pub children: Vec<LNode<'m, 'a>>,
    pub skip: bool,
}

/// `svelte:element`'s tag: a string for `this="div"`, else the expression
#[derive(Debug)]
pub enum LTag<'m, 'a> {
    Static(&'m str),
    Expr(&'m Expr<'a>),
}

#[derive(Debug)]
pub struct LElement<'m, 'a> {
    /// `Element`, `InlineComponent`, `Slot`, `Head`, `Title`, `Window`, `Body`, `Document`,
    /// `Options`, `SlotTemplate` or `SvelteBoundary`
    pub kind: &'static str,
    pub name: &'a str,
    pub start: usize,
    pub end: Option<usize>,
    pub attributes: Vec<LAttr<'m, 'a>>,
    /// `None` for `svelte:options`, which has no `children` in the legacy AST
    pub children: Option<Vec<LNode<'m, 'a>>>,
    /// svelte:element
    pub tag: Option<LTag<'m, 'a>>,
    /// svelte:component
    pub expression: Option<&'m Expr<'a>>,
    /// The modern node, for `name_loc` etc.
    pub modern: NodeId,
}

/// An attribute value chunk
#[derive(Debug)]
pub enum LChunk<'m, 'a> {
    Text(&'m Chunk<'a>),
    MustacheTag { start: usize, end: usize, expression: &'m Expr<'a> },
    AttributeShorthand { start: usize, end: usize, expression: &'m Expr<'a> },
}

#[derive(Debug)]
pub enum LAttrValue<'m, 'a> {
    True,
    Chunks(Vec<LChunk<'m, 'a>>),
}

#[derive(Debug)]
pub enum LAttr<'m, 'a> {
    /// `Attribute` / `StyleDirective`
    Attribute { attr: &'m Attr<'a>, value: LAttrValue<'m, 'a> },
    /// `Spread`, `AttachTag` and directives under their legacy names
    Other { attr: &'m Attr<'a>, legacy_type: &'static str },
}

impl LAttr<'_, '_> {
    pub fn legacy_type(&self) -> &'static str {
        match self {
            LAttr::Attribute { attr, .. } => attr.type_name(),
            LAttr::Other { legacy_type, .. } => legacy_type,
        }
    }
}

// ---------------------------------------------------------------------------------------
// Conversion

fn is_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\r' | b'\n')
}

/// `/\s/` (JS whitespace) at byte `i`
fn js_space_at(source: &str, i: usize) -> bool {
    crate::parser::utils::char_at(source, i).is_some_and(crate::parser::utils::is_whitespace_char)
}

/// `/\s/` for the char ending at byte `i` (exclusive)
fn js_space_before(source: &str, i: usize) -> bool {
    source[..i].chars().next_back().is_some_and(crate::parser::utils::is_whitespace_char)
}

pub struct Converter<'m, 'a> {
    ast: &'m Ast<'a>,
    source: &'a str,
    /// svelte:options' attributes, which the parser moved to `root.options`
    options_attrs: &'m [Attr<'a>],
}

/// Convert a parsed component to the legacy shape
pub fn convert<'m, 'a>(ast: &'m Ast<'a>, root: &'m Root<'a>, source: &'a str) -> LegacyRoot<'m, 'a> {
    let cx = Converter { ast, source, options_attrs: root.options.as_ref().map_or(&[][..], |o| &o.attributes[..]) };
    let mut nodes: Vec<Option<NodeId>> = ast.fragments[root.fragment].nodes.iter().map(|&n| Some(n)).collect();

    // Insert svelte:options back into the root nodes (as `None`, converted separately)
    let options_node = root.options.as_ref().and_then(|_| {
        ast.nodes.iter().enumerate().find(|(_, n)| n.type_name() == "SvelteOptions").map(|(i, _)| i)
    });
    if let (Some(options), Some(id)) = (&root.options, options_node) {
        let idx = nodes
            .iter()
            .position(|n| n.is_some_and(|n| options.end <= ast.nodes[n].start()))
            .unwrap_or(nodes.len());
        nodes.insert(idx, Some(id));
    }

    let (mut start, mut end) = (None, None);
    if let (Some(Some(first)), Some(Some(last))) = (nodes.first(), nodes.last()) {
        let mut s = ast.nodes[*first].start();
        let mut e = ast.nodes[*last].end().unwrap_or(0);
        while js_space_at(source, s) {
            s += crate::parser::utils::char_at(source, s).map_or(1, char::len_utf8);
        }
        while e > 0 && js_space_before(source, e) {
            e -= source[..e].chars().next_back().map_or(1, char::len_utf8);
        }
        start = Some(s);
        end = Some(e);
    }

    let children = nodes.into_iter().flatten().map(|n| cx.node(n, None)).collect();
    LegacyRoot { start, end, children }
}

/// The parent of a node, for the few visitors that look at it
#[derive(Clone, Copy)]
struct Parent<'s> {
    style_element: bool,
    _name: &'s str,
}

impl<'m, 'a> Converter<'m, 'a> {
    fn children(&self, ids: &[NodeId], trim: bool, parent: Option<Parent>) -> Vec<LNode<'m, 'a>> {
        let mut nodes: Vec<LNode<'m, 'a>> = ids.iter().map(|&id| self.node(id, parent)).collect();
        if trim {
            remove_surrounding_whitespace_nodes(&mut nodes);
        }
        nodes
    }

    fn fragment(&self, id: FragId, trim: bool, parent: Option<Parent>) -> Vec<LNode<'m, 'a>> {
        self.children(&self.ast.fragments[id].nodes, trim, parent)
    }

    fn node(&self, id: NodeId, parent: Option<Parent>) -> LNode<'m, 'a> {
        let source = self.source;
        let node = &self.ast.nodes[id];
        match node {
            Node::Text { start, end, raw, data } => LText {
                start: *start,
                end: *end,
                raw: if parent.is_some_and(|p| p.style_element) { None } else { Some(raw) },
                data: data.clone(),
            }
            .into(),
            Node::Comment { start, end, data } => {
                LNode::Comment { start: *start, end: *end, data, ignores: extract_svelte_ignore(data) }
            }
            Node::ExpressionTag { start, end, expression } => {
                LNode::MustacheTag { start: *start, end: *end, expression }
            }
            Node::HtmlTag { start, end, expression } => LNode::RawMustacheTag { start: *start, end: *end, expression },
            Node::DebugTag { start, end, identifiers } => LNode::DebugTag { start: *start, end: *end, id, identifiers },
            Node::ConstTag { start, end, id, init, .. } => LNode::ConstTag { start: *start, end: *end, id, init },
            Node::RenderTag { start, end, expression } => LNode::RenderTag { start: *start, end: *end, expression },
            Node::DeclarationTag { start, end, .. } => LNode::DeclarationTag { id, start: *start, end: *end },
            Node::KeyBlock { start, end, expression, fragment } => LNode::KeyBlock {
                start: *start,
                end: *end,
                expression,
                children: self.fragment(*fragment, true, None),
            },
            Node::EachBlock { start, end, expression, context, body, fallback, index, key } => {
                let else_block = fallback.map(|f| {
                    let nodes = &self.ast.fragments[f].nodes;
                    let block_end = last_index_of(source, b'{', end.unwrap_or(0).saturating_sub(1));
                    let block_start = nodes.first().map_or(block_end, |&n| self.ast.nodes[n].start());
                    Box::new(LElseBlock { start: block_start, end: block_end, children: self.fragment(f, true, None) })
                });
                LNode::EachBlock {
                    start: *start,
                    end: *end,
                    children: self.fragment(*body, true, None),
                    context: context.as_ref(),
                    expression,
                    index: index.as_deref(),
                    key: key.as_ref(),
                    else_block,
                }
            }
            Node::IfBlock { start, end, elseif, test, consequent, alternate } => {
                let else_block = alternate.map(|alt| {
                    let mut nodes = &self.ast.fragments[alt].nodes;
                    if nodes.len() == 1 {
                        if let Node::IfBlock { elseif: true, consequent: inner, .. } = &self.ast.nodes[nodes[0]] {
                            nodes = &self.ast.fragments[*inner].nodes;
                        }
                    }
                    let block_end = last_index_of(source, b'{', end.unwrap_or(0).saturating_sub(1));
                    let block_start = nodes.first().map_or(block_end, |&n| self.ast.nodes[n].start());
                    Box::new(LElseBlock { start: block_start, end: block_end, children: self.fragment(alt, true, None) })
                });
                let start = if *elseif {
                    match self.ast.fragments[*consequent].nodes.first() {
                        Some(&n) => self.ast.nodes[n].start(),
                        None => last_index_of(source, b'{', end.unwrap_or(0).saturating_sub(1)),
                    }
                } else {
                    *start
                };
                LNode::IfBlock {
                    start,
                    end: *end,
                    expression: test,
                    children: self.fragment(*consequent, true, None),
                    else_block,
                    elseif: *elseif,
                }
            }
            Node::AwaitBlock { start, end, expression, value, error, pending, then, catch } => {
                let expr_end = expression.end();
                let after_expr = || source[expr_end..].find('}').map_or(0, |p| expr_end + p + 1);
                let block = |f: &Option<FragId>| LSubBlock {
                    start: None,
                    end: None,
                    children: f.map_or_else(Vec::new, |f| self.fragment(f, false, None)),
                    skip: true,
                };
                let (mut p, mut t, mut c) = (block(pending), block(then), block(catch));
                let first_last = |f: FragId| {
                    let nodes = &self.ast.fragments[f].nodes;
                    (
                        nodes.first().map(|&n| self.ast.nodes[n].start()),
                        nodes.last().and_then(|&n| self.ast.nodes[n].end()),
                    )
                };
                if let Some(f) = pending {
                    let (first, last) = first_last(*f);
                    let s = first.unwrap_or_else(after_expr);
                    p.start = Some(s);
                    p.end = Some(last.unwrap_or(s));
                    p.skip = false;
                }
                if let Some(f) = then {
                    let (first, last) = first_last(*f);
                    t.start = Some(p.end.or(first).unwrap_or_else(after_expr));
                    t.end = Some(last.unwrap_or_else(|| last_index_of(source, b'}', p.end.unwrap_or(expr_end)) + 1));
                    t.skip = false;
                }
                if let Some(f) = catch {
                    let (first, last) = first_last(*f);
                    c.start = Some(t.end.or(p.end).or(first).unwrap_or_else(after_expr));
                    c.end = Some(last.unwrap_or_else(|| {
                        last_index_of(source, b'}', t.end.or(p.end).unwrap_or(expr_end)) + 1
                    }));
                    c.skip = false;
                }
                LNode::AwaitBlock {
                    start: *start,
                    end: *end,
                    expression,
                    value: value.as_ref(),
                    error: error.as_ref(),
                    pending: p,
                    then: t,
                    catch: c,
                }
            }
            Node::SnippetBlock { start, end, expression, type_params, parameters, body } => LNode::SnippetBlock {
                start: *start,
                end: *end,
                expression,
                parameters: parameters.as_ref(),
                type_params: type_params.as_deref(),
                children: self.fragment(*body, true, None),
            },
            Node::Element(el) => LNode::Element(self.element(id, el)),
        }
    }

    fn element(&self, id: NodeId, el: &'m Element<'a>) -> LElement<'m, 'a> {
        let (kind, trim) = match el.kind {
            "RegularElement" => ("Element", false),
            "Component" | "SvelteComponent" | "SvelteSelf" => ("InlineComponent", false),
            "SlotElement" => ("Slot", false),
            "TitleElement" => ("Title", false),
            "SvelteHead" => ("Head", false),
            "SvelteWindow" => ("Window", false),
            "SvelteBody" => ("Body", false),
            "SvelteDocument" => ("Document", false),
            "SvelteOptions" => ("Options", false),
            "SvelteFragment" => ("SlotTemplate", false),
            "SvelteElement" => ("Element", false),
            "SvelteBoundary" => ("SvelteBoundary", true),
            other => unreachable!("element kind {other}"),
        };
        let parent = Parent { style_element: el.kind == "RegularElement" && el.name == "style", _name: el.name };
        let attributes = if el.kind == "SvelteOptions" { self.options_attrs } else { &el.attributes[..] };
        let attributes = attributes.iter().map(|a| self.attr(a)).collect();
        let tag = el.tag.as_ref().map(|tag| match tag {
            Expr::Literal { value, start, .. } if self.source.as_bytes().get(start.wrapping_sub(1)) != Some(&b'{') => {
                LTag::Static(value)
            }
            other => LTag::Expr(other),
        });
        LElement {
            kind,
            name: el.name,
            start: el.start,
            end: el.end,
            attributes,
            children: if el.kind == "SvelteOptions" {
                None
            } else {
                Some(self.fragment(el.fragment, trim, Some(parent)))
            },
            tag,
            expression: el.expression.as_ref(),
            modern: id,
        }
    }

    fn attr(&self, attr: &'m Attr<'a>) -> LAttr<'m, 'a> {
        match attr {
            Attr::Attribute { start, value, .. } => {
                let shorthand = self.source.as_bytes().get(*start) == Some(&b'{');
                LAttr::Attribute { attr, value: self.attr_value(value, shorthand) }
            }
            Attr::StyleDirective { value, .. } => LAttr::Attribute { attr, value: self.attr_value(value, false) },
            Attr::Spread { .. } => LAttr::Other { attr, legacy_type: "Spread" },
            Attr::Attach { .. } => LAttr::Other { attr, legacy_type: "AttachTag" },
            Attr::Directive { kind, .. } => LAttr::Other {
                attr,
                legacy_type: match *kind {
                    "AnimateDirective" => "Animation",
                    "BindDirective" => "Binding",
                    "ClassDirective" => "Class",
                    "OnDirective" => "EventHandler",
                    "TransitionDirective" => "Transition",
                    "UseDirective" => "Action",
                    "LetDirective" => "Let",
                    other => other,
                },
            },
        }
    }

    /// `shorthand`: the attribute starts with `{` (and is in an `Attribute`, not a
    /// `StyleDirective`), so expression tags become `AttributeShorthand`
    fn attr_value(&self, value: &'m AttrValue<'a>, shorthand: bool) -> LAttrValue<'m, 'a> {
        let chunk = |c: &'m Chunk<'a>| match c {
            Chunk::Text { .. } => LChunk::Text(c),
            Chunk::Expression { start, end, expression } => {
                if shorthand {
                    LChunk::AttributeShorthand { start: *start, end: *end, expression }
                } else {
                    LChunk::MustacheTag { start: *start, end: *end, expression }
                }
            }
        };
        match value {
            AttrValue::True => LAttrValue::True,
            AttrValue::Expression(c) => LAttrValue::Chunks(vec![chunk(c)]),
            AttrValue::Sequence(chunks) => LAttrValue::Chunks(chunks.iter().map(chunk).collect()),
        }
    }
}

impl<'a> From<LText<'a>> for LNode<'_, 'a> {
    fn from(t: LText<'a>) -> Self {
        LNode::Text(t)
    }
}

fn last_index_of(source: &str, byte: u8, from: usize) -> usize {
    let from = from.min(source.len().saturating_sub(1));
    source.as_bytes()[..=from].iter().rposition(|&b| b == byte).unwrap_or(usize::MAX)
}

fn remove_surrounding_whitespace_nodes(nodes: &mut Vec<LNode>) {
    if let Some(LNode::Text(first)) = nodes.first_mut() {
        if !first.data.bytes().any(|b| !is_ws(b)) {
            nodes.remove(0);
        } else {
            let trimmed = first.data.trim_start_matches([' ', '\t', '\r', '\n']);
            if trimmed.len() != first.data.len() {
                first.data = Cow::Owned(trimmed.to_string());
            }
        }
    }
    if let Some(LNode::Text(last)) = nodes.last_mut() {
        if !last.data.bytes().any(|b| !is_ws(b)) {
            nodes.pop();
        } else {
            let trimmed = last.data.trim_end_matches([' ', '\t', '\r', '\n']);
            if trimmed.len() != last.data.len() {
                last.data = Cow::Owned(trimmed.to_string());
            }
        }
    }
}

/// `extract_svelte_ignore(offset, text, false)` (non-runes mode, which never warns)
pub fn extract_svelte_ignore(text: &str) -> Vec<String> {
    let trimmed = text.trim_start_matches(|c: char| crate::parser::utils::is_whitespace_char(c));
    let Some(rest) = trimmed.strip_prefix("svelte-ignore") else { return Vec::new() };
    if !rest.starts_with(|c: char| crate::parser::utils::is_whitespace_char(c)) {
        return Vec::new();
    }
    let rest = &rest[rest.chars().next().unwrap().len_utf8()..];
    let codes = crate::warning_codes::CODES;
    let mut ignores = Vec::new();
    let is_code_char = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '$' || c == '-';
    for code in rest.split(|c: char| !is_code_char(c)).filter(|s| !s.is_empty()) {
        ignores.push(code.to_string());
        if !codes.contains(&code) {
            let replacement = match code {
                "non-top-level-reactive-declaration" => "reactive_declaration_invalid_placement".to_string(),
                "module-script-reactive-declaration" => "reactive_declaration_module_script".to_string(),
                "empty-block" => "block_empty".to_string(),
                "avoid-is" => "attribute_avoid_is".to_string(),
                "invalid-html-attribute" => "attribute_invalid_property_name".to_string(),
                "a11y-structure" => "a11y_figcaption_parent".to_string(),
                "illegal-attribute-character" => "attribute_illegal_colon".to_string(),
                "invalid-rest-eachblock-binding" => "bind_invalid_each_rest".to_string(),
                "unused-export-let" => "export_let_unused".to_string(),
                other => other.replace('-', "_"),
            };
            if codes.contains(&replacement.as_str()) {
                ignores.push(replacement);
            }
        }
    }
    ignores
}

// ---------------------------------------------------------------------------------------
// JSON, in the shape `parse(source)` returns, for checking against svelte/compiler

fn obj(pairs: Vec<(&str, Value)>) -> Value {
    let mut m = Map::with_capacity(pairs.len());
    for (k, v) in pairs {
        if !v.is_null() || !matches!(k, "else" | "elseif" | "key" | "index" | "typeParams") {
            m.insert(k.to_string(), v);
        }
    }
    Value::Object(m)
}

fn opt_num(v: Option<usize>) -> Value {
    v.map_or(Value::Null, Value::from)
}

fn end_num(v: Option<usize>) -> Value {
    v.map_or(Value::from(-1), Value::from)
}

impl LegacyRoot<'_, '_> {
    /// `{ html, _comments }` of the legacy AST
    pub fn to_json(&self, ast: &Ast, root: &Root, cx: &ToJson) -> Value {
        let mut m = Map::new();
        m.insert(
            "html".into(),
            json!({
                "type": "Fragment",
                "start": opt_num(self.start),
                "end": opt_num(self.end),
                "children": self.children.iter().map(|c| c.to_json(ast, root, cx)).collect::<Vec<_>>()
            }),
        );
        if !root.comments.is_empty() {
            m.insert("_comments".into(), Value::Array(root.comments.iter().map(|c| c.to_json(cx.loc)).collect()));
        }
        Value::Object(m)
    }
}

impl LNode<'_, '_> {
    pub fn to_json(&self, ast: &Ast, root: &Root, cx: &ToJson) -> Value {
        let kids = |c: &[LNode]| Value::Array(c.iter().map(|n| n.to_json(ast, root, cx)).collect());
        match self {
            LNode::Text(t) => {
                let mut pairs = vec![("type", "Text".into()), ("start", t.start.into()), ("end", t.end.into())];
                if let Some(raw) = t.raw {
                    pairs.push(("raw", raw.into()));
                }
                pairs.push(("data", t.data.as_ref().into()));
                obj(pairs)
            }
            LNode::Comment { start, end, data, ignores } => obj(vec![
                ("type", "Comment".into()),
                ("start", (*start).into()),
                ("end", (*end).into()),
                ("data", (*data).into()),
                ("ignores", Value::Array(ignores.iter().map(|s| s.clone().into()).collect())),
            ]),
            LNode::MustacheTag { start, end, expression } | LNode::RawMustacheTag { start, end, expression } => obj(vec![
                ("type", (if matches!(self, LNode::MustacheTag { .. }) { "MustacheTag" } else { "RawMustacheTag" }).into()),
                ("start", (*start).into()),
                ("end", (*end).into()),
                ("expression", expression.to_json(cx)),
            ]),
            LNode::RenderTag { start, end, expression } => obj(vec![
                ("type", "RenderTag".into()),
                ("start", (*start).into()),
                ("end", (*end).into()),
                ("expression", expression.to_json(cx)),
            ]),
            LNode::DeclarationTag { id, .. } | LNode::DebugTag { id, .. } => ast.node_json(*id, cx),
            LNode::ConstTag { start, end, id: pattern, init } => {
                let mut left = pattern.to_json(cx);
                if let Value::Object(m) = &mut left {
                    m.shift_remove("typeAnnotation");
                }
                // the modern declaration spans `const ...` (start + 2 .. end - 1)
                obj(vec![
                    ("type", "ConstTag".into()),
                    ("start", (*start).into()),
                    ("end", (*end).into()),
                    (
                        "expression",
                        json!({
                            "type": "AssignmentExpression",
                            "start": start + 2 + "const ".len(),
                            "end": end - 1,
                            "operator": "=",
                            "left": left,
                            "right": init.to_json(cx)
                        }),
                    ),
                ])
            }
            LNode::IfBlock { start, end, expression, children, else_block, elseif } => obj(vec![
                ("type", "IfBlock".into()),
                ("start", (*start).into()),
                ("end", end_num(*end)),
                ("expression", expression.to_json(cx)),
                ("children", kids(children)),
                ("else", else_block.as_ref().map_or(Value::Null, |e| e.to_json(ast, root, cx))),
                ("elseif", if *elseif { Value::Bool(true) } else { Value::Null }),
            ]),
            LNode::EachBlock { start, end, children, context, expression, index, key, else_block } => obj(vec![
                ("type", "EachBlock".into()),
                ("start", (*start).into()),
                ("end", end_num(*end)),
                ("children", kids(children)),
                ("context", context.map_or(Value::Null, |c| c.to_json(cx))),
                ("expression", expression.to_json(cx)),
                ("index", index.map_or(Value::Null, Value::from)),
                ("key", key.map_or(Value::Null, |k| k.to_json(cx))),
                ("else", else_block.as_ref().map_or(Value::Null, |e| e.to_json(ast, root, cx))),
            ]),
            LNode::AwaitBlock { start, end, expression, value, error, pending, then, catch } => {
                let sub = |ty: &str, b: &LSubBlock| {
                    json!({
                        "type": ty,
                        "start": opt_num(b.start),
                        "end": opt_num(b.end),
                        "children": kids(&b.children),
                        "skip": b.skip
                    })
                };
                obj(vec![
                    ("type", "AwaitBlock".into()),
                    ("start", (*start).into()),
                    ("end", end_num(*end)),
                    ("expression", expression.to_json(cx)),
                    ("value", value.map_or(Value::Null, |p| p.to_json(cx))),
                    ("error", error.map_or(Value::Null, |p| p.to_json(cx))),
                    ("pending", sub("PendingBlock", pending)),
                    ("then", sub("ThenBlock", then)),
                    ("catch", sub("CatchBlock", catch)),
                ])
            }
            LNode::KeyBlock { start, end, expression, children } => obj(vec![
                ("type", "KeyBlock".into()),
                ("start", (*start).into()),
                ("end", end_num(*end)),
                ("expression", expression.to_json(cx)),
                ("children", kids(children)),
            ]),
            LNode::SnippetBlock { start, end, expression, parameters, type_params, children } => {
                let params = match parameters {
                    Some(arrow) => match cx.expr(arrow) {
                        Value::Object(mut m) => m.shift_remove("params").unwrap_or(Value::Array(vec![])),
                        _ => Value::Array(vec![]),
                    },
                    None => Value::Array(vec![]),
                };
                obj(vec![
                    ("type", "SnippetBlock".into()),
                    ("start", (*start).into()),
                    ("end", end_num(*end)),
                    ("expression", expression.to_json(cx)),
                    ("parameters", params),
                    ("children", kids(children)),
                    ("typeParams", type_params.map_or(Value::Null, Value::from)),
                ])
            }
            LNode::Element(el) => {
                let mut pairs = vec![
                    ("type", el.kind.into()),
                    ("start", el.start.into()),
                    ("end", end_num(el.end)),
                    ("name", el.name.into()),
                ];
                let attributes: Vec<Value> = el.attributes.iter().map(|a| legacy_attr_json(a, cx)).collect();
                pairs.push(("attributes", Value::Array(attributes)));
                if let Some(children) = &el.children {
                    pairs.push(("children", kids(children)));
                }
                if let Some(tag) = &el.tag {
                    pairs.push((
                        "tag",
                        match tag {
                            LTag::Static(s) => (*s).into(),
                            LTag::Expr(e) => e.to_json(cx),
                        },
                    ));
                }
                if let Some(e) = el.expression {
                    pairs.push(("expression", e.to_json(cx)));
                }
                obj(pairs)
            }
        }
    }
}

impl LElseBlock<'_, '_> {
    fn to_json(&self, ast: &Ast, root: &Root, cx: &ToJson) -> Value {
        json!({
            "type": "ElseBlock",
            "start": self.start,
            "end": self.end,
            "children": self.children.iter().map(|c| c.to_json(ast, root, cx)).collect::<Vec<_>>()
        })
    }
}

fn legacy_attr_json(attr: &LAttr, cx: &ToJson) -> Value {
    match attr {
        LAttr::Attribute { attr, value } => {
            let mut json = attr_json(attr, cx);
            json["value"] = match value {
                LAttrValue::True => Value::Bool(true),
                LAttrValue::Chunks(chunks) => Value::Array(
                    chunks
                        .iter()
                        .map(|c| match c {
                            LChunk::Text(t) => chunk_json(t, cx),
                            LChunk::MustacheTag { start, end, expression } => json!({
                                "type": "MustacheTag", "start": start, "end": end, "expression": expression.to_json(cx)
                            }),
                            LChunk::AttributeShorthand { start, end, expression } => json!({
                                "type": "AttributeShorthand", "start": start, "end": end, "expression": expression.to_json(cx)
                            }),
                        })
                        .collect(),
                ),
            };
            json
        }
        LAttr::Other { attr, legacy_type } => {
            let mut json = attr_json(attr, cx);
            json["type"] = (*legacy_type).into();
            json
        }
    }
}
