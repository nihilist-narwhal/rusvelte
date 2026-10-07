//! The Svelte template AST (the "modern" AST of `svelte/compiler`'s `parse`).
//!
//! Template nodes live in an arena (`Ast::nodes`) and fragments in another
//! (`Ast::fragments`), so the parser can keep a stack of open nodes while also
//! appending them to their parent's fragment, as the JS parser does with shared references.
//! JS subtrees are oxc AST nodes (see [`crate::js`]); [`Ast::fragment_json`] and
//! [`Root::to_json`] produce the JSON shape of `svelte/compiler`.

use std::borrow::Cow;

use serde_json::{json, Map, Value};

use crate::js::{JsComment, JsExpr, JsProgram, JsStatement, ToJson};

pub type NodeId = usize;
pub type FragId = usize;

#[derive(Debug, Default)]
pub struct Fragment {
    pub nodes: Vec<NodeId>,
    pub transparent: bool,
}

/// `name_loc`: the name's range, written with Svelte's locator (with `character`)
#[derive(Debug, Clone, Copy)]
pub struct NameLoc {
    pub start: usize,
    pub end: usize,
}

impl NameLoc {
    pub fn to_json(self, cx: &ToJson) -> Value {
        json!({ "start": cx.loc.locate(self.start), "end": cx.loc.locate(self.end) })
    }
}

/// How a synthetic identifier's `loc` is written
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum IdentLoc {
    /// no `loc`
    None,
    /// `loc` from Svelte's locator, with `character` (`parser.read_identifier`)
    Svelte,
}

/// An expression slot in the template AST
#[derive(Debug)]
pub enum Expr<'a> {
    Js(JsExpr<'a>),
    /// An Identifier Svelte builds itself
    Ident { name: String, start: usize, end: usize, loc: IdentLoc },
    /// `<svelte:element this="div">`
    Literal { value: String, raw: String, start: usize, end: usize },
}

impl<'a> Expr<'a> {
    pub fn start(&self) -> usize {
        match self {
            Expr::Js(e) => e.inner_start(),
            Expr::Ident { start, .. } | Expr::Literal { start, .. } => *start,
        }
    }

    pub fn end(&self) -> usize {
        match self {
            Expr::Js(e) => e.inner_end(),
            Expr::Ident { end, .. } | Expr::Literal { end, .. } => *end,
        }
    }

    pub fn to_json(&self, cx: &ToJson) -> Value {
        match self {
            Expr::Js(e) => cx.expr(e),
            Expr::Ident { name, start, end, loc } => {
                let mut m = Map::new();
                m.insert("type".into(), "Identifier".into());
                m.insert("name".into(), name.clone().into());
                m.insert("start".into(), (*start).into());
                m.insert("end".into(), (*end).into());
                if *loc == IdentLoc::Svelte {
                    m.insert("loc".into(), json!({ "start": cx.loc.locate(*start), "end": cx.loc.locate(*end) }));
                }
                Value::Object(m)
            }
            Expr::Literal { value, raw, start, end } => json!({
                "type": "Literal",
                "value": value,
                "raw": raw,
                "start": start,
                "end": end
            }),
        }
    }
}

impl<'a> JsExpr<'a> {
    /// `start` after parentheses removal
    pub fn inner_start(&self) -> usize {
        oxc_span::GetSpan::span(self.inner()).start as usize
    }
    /// `end` after parentheses removal and Svelte's rewrites
    pub fn inner_end(&self) -> usize {
        match self.fix.as_ref().and_then(|f| f.set_end) {
            Some(end) => end as usize,
            None => oxc_span::GetSpan::span(self.inner()).end as usize,
        }
    }
}

/// The type annotation `read_type_annotation` builds: a TSTypeAnnotation whose
/// `typeAnnotation` is taken from parsing `_ as <type>`
#[derive(Debug)]
pub struct TypeAnn<'a> {
    pub start: usize,
    pub end: usize,
    pub expr: JsExpr<'a>,
}

impl TypeAnn<'_> {
    fn to_json(&self, cx: &ToJson) -> Value {
        let mut value = cx.expr(&self.expr);
        let mut m = Map::new();
        m.insert("type".into(), "TSTypeAnnotation".into());
        m.insert("start".into(), self.start.into());
        m.insert("end".into(), self.end.into());
        if let Some(t) = value.get_mut("typeAnnotation") {
            m.insert("typeAnnotation".into(), t.take());
        }
        Value::Object(m)
    }
}

/// What `read_pattern` returns
#[derive(Debug)]
pub enum Pattern<'a> {
    Ident { name: String, start: usize, end: usize, type_ann: Option<TypeAnn<'a>> },
    /// `{ a, b }` / `[a, b]`: the `left` of parsing `<pattern> = 1`
    Destructure { assign: JsExpr<'a>, type_ann: Option<TypeAnn<'a>> },
}

impl Pattern<'_> {
    pub fn start(&self) -> usize {
        match self {
            Pattern::Ident { start, .. } => *start,
            Pattern::Destructure { assign, .. } => match assign.inner() {
                oxc_ast::ast::Expression::AssignmentExpression(a) => oxc_span::GetSpan::span(&a.left).start as usize,
                other => oxc_span::GetSpan::span(other).start as usize,
            },
        }
    }

    pub fn to_json(&self, cx: &ToJson) -> Value {
        match self {
            Pattern::Ident { name, start, end, type_ann } => {
                let mut m = Map::new();
                m.insert("type".into(), "Identifier".into());
                m.insert("name".into(), name.clone().into());
                m.insert("start".into(), (*start).into());
                m.insert("end".into(), (*end).into());
                m.insert("loc".into(), json!({ "start": cx.loc.locate(*start), "end": cx.loc.locate(*end) }));
                if let Some(t) = type_ann {
                    m.insert("typeAnnotation".into(), t.to_json(cx));
                }
                Value::Object(m)
            }
            Pattern::Destructure { assign, type_ann } => {
                let mut value = cx.expr(assign);
                let mut left = value.get_mut("left").map(Value::take).unwrap_or(Value::Null);
                if let (Some(t), Value::Object(m)) = (type_ann, &mut left) {
                    m.insert("typeAnnotation".into(), t.to_json(cx));
                    m.insert("end".into(), t.end.into());
                }
                left
            }
        }
    }
}

#[derive(Debug)]
pub enum AttrValue<'a> {
    True,
    /// A lone `{expression}`
    Expression(Box<Chunk<'a>>),
    Sequence(Vec<Chunk<'a>>),
}

/// A `Text` or `ExpressionTag` inside an attribute value
#[derive(Debug)]
pub enum Chunk<'a> {
    Text { start: usize, end: usize, raw: &'a str, data: Cow<'a, str> },
    Expression { start: usize, end: usize, expression: Expr<'a> },
}

impl Chunk<'_> {
    pub fn start(&self) -> usize {
        match self {
            Chunk::Text { start, .. } | Chunk::Expression { start, .. } => *start,
        }
    }
}

#[derive(Debug)]
pub enum Attr<'a> {
    Attribute {
        start: usize,
        end: usize,
        name: &'a str,
        name_loc: Option<NameLoc>,
        value: AttrValue<'a>,
    },
    Spread {
        start: usize,
        end: usize,
        expression: Expr<'a>,
    },
    Attach {
        start: usize,
        end: usize,
        expression: Expr<'a>,
    },
    Directive {
        kind: &'static str,
        start: usize,
        end: usize,
        name: &'a str,
        name_loc: NameLoc,
        modifiers: Vec<&'a str>,
        expression: Option<Expr<'a>>,
        /// TransitionDirective only
        intro_outro: Option<(bool, bool)>,
    },
    StyleDirective {
        start: usize,
        end: usize,
        name: &'a str,
        name_loc: NameLoc,
        modifiers: Vec<&'a str>,
        value: AttrValue<'a>,
    },
}

impl<'a> Attr<'a> {
    pub fn start(&self) -> usize {
        match self {
            Attr::Attribute { start, .. }
            | Attr::Spread { start, .. }
            | Attr::Attach { start, .. }
            | Attr::Directive { start, .. }
            | Attr::StyleDirective { start, .. } => *start,
        }
    }
    pub fn end(&self) -> usize {
        match self {
            Attr::Attribute { end, .. }
            | Attr::Spread { end, .. }
            | Attr::Attach { end, .. }
            | Attr::Directive { end, .. }
            | Attr::StyleDirective { end, .. } => *end,
        }
    }
    pub fn type_name(&self) -> &'static str {
        match self {
            Attr::Attribute { .. } => "Attribute",
            Attr::Spread { .. } => "SpreadAttribute",
            Attr::Attach { .. } => "AttachTag",
            Attr::Directive { kind, .. } => kind,
            Attr::StyleDirective { .. } => "StyleDirective",
        }
    }
    /// `name` for attributes and directives
    pub fn name(&self) -> Option<&'a str> {
        match self {
            Attr::Attribute { name, .. } | Attr::Directive { name, .. } | Attr::StyleDirective { name, .. } => {
                Some(name)
            }
            _ => None,
        }
    }
}

#[derive(Debug)]
pub struct Element<'a> {
    pub kind: &'static str,
    pub start: usize,
    pub end: Option<usize>,
    pub name: &'a str,
    pub name_loc: NameLoc,
    pub attributes: Vec<Attr<'a>>,
    pub fragment: FragId,
    /// SvelteElement
    pub tag: Option<Expr<'a>>,
    /// SvelteComponent
    pub expression: Option<Expr<'a>>,
}

/// `{@debug ...}`
#[derive(Debug)]
pub enum DebugArgs<'a> {
    /// `{@debug}`
    All,
    /// A single identifier
    One(Expr<'a>),
    /// `{@debug a, b}`: the identifiers are the SequenceExpression's expressions
    Sequence(Expr<'a>),
}

/// The declaration of a `{let ...}`/`{const ...}` tag
#[derive(Debug)]
pub enum Declaration<'a> {
    Js(JsStatement<'a>),
    /// loose mode: a placeholder `let`/`const` with an empty identifier
    Loose { kind: &'static str, start: usize, end: usize },
}

#[derive(Debug)]
pub enum Node<'a> {
    Text { start: usize, end: usize, raw: &'a str, data: Cow<'a, str> },
    Comment { start: usize, end: usize, data: &'a str },
    ExpressionTag { start: usize, end: usize, expression: Expr<'a> },
    HtmlTag { start: usize, end: usize, expression: Expr<'a> },
    DebugTag { start: usize, end: usize, identifiers: DebugArgs<'a> },
    ConstTag { start: usize, end: usize, id: Pattern<'a>, init: Expr<'a>, declarator_end: usize },
    RenderTag { start: usize, end: usize, expression: Expr<'a> },
    DeclarationTag { start: usize, end: usize, declaration: Declaration<'a> },
    IfBlock { start: usize, end: Option<usize>, elseif: bool, test: Expr<'a>, consequent: FragId, alternate: Option<FragId> },
    EachBlock {
        start: usize,
        end: Option<usize>,
        expression: Expr<'a>,
        context: Option<Pattern<'a>>,
        body: FragId,
        fallback: Option<FragId>,
        index: Option<String>,
        key: Option<Expr<'a>>,
    },
    AwaitBlock {
        start: usize,
        end: Option<usize>,
        expression: Expr<'a>,
        value: Option<Pattern<'a>>,
        error: Option<Pattern<'a>>,
        pending: Option<FragId>,
        then: Option<FragId>,
        catch: Option<FragId>,
    },
    KeyBlock { start: usize, end: Option<usize>, expression: Expr<'a>, fragment: FragId },
    SnippetBlock {
        start: usize,
        end: Option<usize>,
        expression: Expr<'a>,
        type_params: Option<String>,
        /// The `(params) => {}` arrow function the parameters were parsed from
        parameters: Option<JsExpr<'a>>,
        body: FragId,
    },
    Element(Element<'a>),
}

impl Node<'_> {
    pub fn start(&self) -> usize {
        match self {
            Node::Text { start, .. }
            | Node::Comment { start, .. }
            | Node::ExpressionTag { start, .. }
            | Node::HtmlTag { start, .. }
            | Node::DebugTag { start, .. }
            | Node::ConstTag { start, .. }
            | Node::RenderTag { start, .. }
            | Node::DeclarationTag { start, .. }
            | Node::IfBlock { start, .. }
            | Node::EachBlock { start, .. }
            | Node::AwaitBlock { start, .. }
            | Node::KeyBlock { start, .. }
            | Node::SnippetBlock { start, .. } => *start,
            Node::Element(el) => el.start,
        }
    }

    /// `end`, or -1 (as `None`) for blocks/elements that haven't been closed
    pub fn end(&self) -> Option<usize> {
        match self {
            Node::Text { end, .. }
            | Node::Comment { end, .. }
            | Node::ExpressionTag { end, .. }
            | Node::HtmlTag { end, .. }
            | Node::DebugTag { end, .. }
            | Node::ConstTag { end, .. }
            | Node::RenderTag { end, .. }
            | Node::DeclarationTag { end, .. } => Some(*end),
            Node::IfBlock { end, .. }
            | Node::EachBlock { end, .. }
            | Node::AwaitBlock { end, .. }
            | Node::KeyBlock { end, .. }
            | Node::SnippetBlock { end, .. } => *end,
            Node::Element(el) => el.end,
        }
    }

    pub fn set_end(&mut self, value: usize) {
        match self {
            Node::IfBlock { end, .. }
            | Node::EachBlock { end, .. }
            | Node::AwaitBlock { end, .. }
            | Node::KeyBlock { end, .. }
            | Node::SnippetBlock { end, .. } => *end = Some(value),
            Node::Element(el) => el.end = Some(value),
            _ => unreachable!("set_end on a leaf node"),
        }
    }

    pub fn type_name(&self) -> &'static str {
        match self {
            Node::Text { .. } => "Text",
            Node::Comment { .. } => "Comment",
            Node::ExpressionTag { .. } => "ExpressionTag",
            Node::HtmlTag { .. } => "HtmlTag",
            Node::DebugTag { .. } => "DebugTag",
            Node::ConstTag { .. } => "ConstTag",
            Node::RenderTag { .. } => "RenderTag",
            Node::DeclarationTag { .. } => "DeclarationTag",
            Node::IfBlock { .. } => "IfBlock",
            Node::EachBlock { .. } => "EachBlock",
            Node::AwaitBlock { .. } => "AwaitBlock",
            Node::KeyBlock { .. } => "KeyBlock",
            Node::SnippetBlock { .. } => "SnippetBlock",
            Node::Element(el) => el.kind,
        }
    }
}

#[derive(Debug)]
pub struct Script<'a> {
    pub start: usize,
    pub end: usize,
    pub context: &'static str,
    pub content: JsProgram<'a>,
    pub attributes: Vec<Attr<'a>>,
    /// The HTML comment right before the `<script>`, which Svelte stores as the
    /// Program's `leadingComments`
    pub leading_comment: Option<String>,
}

#[derive(Debug)]
pub struct StyleSheet<'a> {
    pub attributes: Vec<Attr<'a>>,
    pub css: crate::css::StyleSheet,
}

#[derive(Debug, Default)]
pub struct SvelteOptions<'a> {
    pub start: usize,
    pub end: usize,
    pub attributes: Vec<Attr<'a>>,
    /// Option name → value, in insertion order, already in JSON form
    pub values: Map<String, Value>,
}

#[derive(Debug)]
pub struct Root<'a> {
    pub start: usize,
    pub end: usize,
    pub fragment: FragId,
    pub css: Option<StyleSheet<'a>>,
    pub instance: Option<Script<'a>>,
    pub module: Option<Script<'a>>,
    pub options: Option<SvelteOptions<'a>>,
    pub comments: Vec<JsComment>,
    pub ts: bool,
}

#[derive(Debug, Default)]
pub struct Ast<'a> {
    pub nodes: Vec<Node<'a>>,
    pub fragments: Vec<Fragment>,
}

impl<'a> Ast<'a> {
    pub fn new_fragment(&mut self, transparent: bool) -> FragId {
        self.fragments.push(Fragment { nodes: Vec::new(), transparent });
        self.fragments.len() - 1
    }

    pub fn add(&mut self, node: Node<'a>) -> NodeId {
        self.nodes.push(node);
        self.nodes.len() - 1
    }
}

// ---------------------------------------------------------------------------------------
// Serialization to the public (modern) JSON shape. Offsets are converted to UTF-16 by the
// caller in one final pass.

fn obj(pairs: Vec<(&str, Value)>) -> Value {
    let mut m = Map::with_capacity(pairs.len());
    for (k, v) in pairs {
        m.insert(k.to_string(), v);
    }
    Value::Object(m)
}

fn end_value(end: Option<usize>) -> Value {
    match end {
        Some(e) => e.into(),
        None => (-1).into(),
    }
}

fn opt<T>(value: &Option<T>, f: impl FnOnce(&T) -> Value) -> Value {
    value.as_ref().map_or(Value::Null, f)
}

impl Ast<'_> {
    pub fn fragment_json(&self, id: FragId, cx: &ToJson) -> Value {
        let nodes = self.fragments[id].nodes.iter().map(|&n| self.node_json(n, cx)).collect();
        obj(vec![("type", "Fragment".into()), ("nodes", Value::Array(nodes))])
    }

    fn opt_fragment(&self, id: Option<FragId>, cx: &ToJson) -> Value {
        id.map_or(Value::Null, |f| self.fragment_json(f, cx))
    }

    pub fn node_json(&self, id: NodeId, cx: &ToJson) -> Value {
        let node = &self.nodes[id];
        let type_name = node.type_name();
        match node {
            Node::Text { start, end, raw, data } => text_json(*start, *end, raw, data),
            Node::Comment { start, end, data } => obj(vec![
                ("type", "Comment".into()),
                ("start", (*start).into()),
                ("end", (*end).into()),
                ("data", (*data).into()),
            ]),
            Node::ExpressionTag { start, end, expression } => expression_tag_json(*start, *end, expression.to_json(cx)),
            Node::HtmlTag { start, end, expression } | Node::RenderTag { start, end, expression } => obj(vec![
                ("type", type_name.into()),
                ("start", (*start).into()),
                ("end", (*end).into()),
                ("expression", expression.to_json(cx)),
            ]),
            Node::DebugTag { start, end, identifiers } => {
                let identifiers = match identifiers {
                    DebugArgs::All => Vec::new(),
                    DebugArgs::One(e) => vec![e.to_json(cx)],
                    DebugArgs::Sequence(e) => match e.to_json(cx) {
                        Value::Object(mut m) => match m.shift_remove("expressions") {
                            Some(Value::Array(items)) => items,
                            _ => Vec::new(),
                        },
                        _ => Vec::new(),
                    },
                };
                obj(vec![
                    ("type", "DebugTag".into()),
                    ("start", (*start).into()),
                    ("end", (*end).into()),
                    ("identifiers", Value::Array(identifiers)),
                ])
            }
            Node::ConstTag { start, end, id, init, declarator_end } => obj(vec![
                ("type", "ConstTag".into()),
                ("start", (*start).into()),
                ("end", (*end).into()),
                (
                    "declaration",
                    json!({
                        "type": "VariableDeclaration",
                        "kind": "const",
                        "declarations": [{
                            "type": "VariableDeclarator",
                            "id": id.to_json(cx),
                            "init": init.to_json(cx),
                            "start": id.start(),
                            "end": declarator_end
                        }],
                        // start at const, not at @const
                        "start": start + 2,
                        "end": end - 1
                    }),
                ),
            ]),
            Node::DeclarationTag { start, end, declaration } => obj(vec![
                ("type", "DeclarationTag".into()),
                ("start", (*start).into()),
                ("end", (*end).into()),
                (
                    "declaration",
                    match declaration {
                        Declaration::Js(stmt) => cx.statement(stmt),
                        Declaration::Loose { kind, start, end } => json!({
                            "type": "VariableDeclaration",
                            "kind": kind,
                            "declarations": [{
                                "type": "VariableDeclarator",
                                "id": { "type": "Identifier", "name": "", "start": end, "end": end },
                                "init": null,
                                "start": end,
                                "end": end
                            }],
                            "start": start,
                            "end": end
                        }),
                    },
                ),
            ]),
            Node::IfBlock { start, end, elseif, test, consequent, alternate } => obj(vec![
                ("type", "IfBlock".into()),
                ("elseif", (*elseif).into()),
                ("start", (*start).into()),
                ("end", end_value(*end)),
                ("test", test.to_json(cx)),
                ("consequent", self.fragment_json(*consequent, cx)),
                ("alternate", self.opt_fragment(*alternate, cx)),
            ]),
            Node::EachBlock { start, end, expression, context, body, fallback, index, key } => {
                let mut pairs = vec![
                    ("type", "EachBlock".into()),
                    ("start", (*start).into()),
                    ("end", end_value(*end)),
                    ("expression", expression.to_json(cx)),
                    ("body", self.fragment_json(*body, cx)),
                    ("context", opt(context, |c| c.to_json(cx))),
                ];
                if let Some(index) = index {
                    pairs.push(("index", index.clone().into()));
                }
                if let Some(key) = key {
                    pairs.push(("key", key.to_json(cx)));
                }
                if let Some(fallback) = fallback {
                    pairs.push(("fallback", self.fragment_json(*fallback, cx)));
                }
                obj(pairs)
            }
            Node::AwaitBlock { start, end, expression, value, error, pending, then, catch } => obj(vec![
                ("type", "AwaitBlock".into()),
                ("start", (*start).into()),
                ("end", end_value(*end)),
                ("expression", expression.to_json(cx)),
                ("value", opt(value, |p| p.to_json(cx))),
                ("error", opt(error, |p| p.to_json(cx))),
                ("pending", self.opt_fragment(*pending, cx)),
                ("then", self.opt_fragment(*then, cx)),
                ("catch", self.opt_fragment(*catch, cx)),
            ]),
            Node::KeyBlock { start, end, expression, fragment } => obj(vec![
                ("type", "KeyBlock".into()),
                ("start", (*start).into()),
                ("end", end_value(*end)),
                ("expression", expression.to_json(cx)),
                ("fragment", self.fragment_json(*fragment, cx)),
            ]),
            Node::SnippetBlock { start, end, expression, type_params, parameters, body } => {
                let mut pairs = vec![
                    ("type", "SnippetBlock".into()),
                    ("start", (*start).into()),
                    ("end", end_value(*end)),
                    ("expression", expression.to_json(cx)),
                ];
                if let Some(tp) = type_params {
                    pairs.push(("typeParams", tp.clone().into()));
                }
                let params = match parameters {
                    Some(arrow) => match cx.expr(arrow) {
                        Value::Object(mut m) => m.shift_remove("params").unwrap_or(Value::Array(vec![])),
                        _ => Value::Array(vec![]),
                    },
                    None => Value::Array(vec![]),
                };
                pairs.push(("parameters", params));
                pairs.push(("body", self.fragment_json(*body, cx)));
                obj(pairs)
            }
            Node::Element(el) => {
                let mut pairs = vec![
                    ("type", el.kind.into()),
                    ("start", el.start.into()),
                    ("end", end_value(el.end)),
                    ("name", el.name.into()),
                    ("name_loc", el.name_loc.to_json(cx)),
                    ("attributes", Value::Array(el.attributes.iter().map(|a| attr_json(a, cx)).collect())),
                    ("fragment", self.fragment_json(el.fragment, cx)),
                ];
                if let Some(tag) = &el.tag {
                    pairs.push(("tag", tag.to_json(cx)));
                }
                if let Some(expression) = &el.expression {
                    pairs.push(("expression", expression.to_json(cx)));
                }
                obj(pairs)
            }
        }
    }
}

pub fn text_json(start: usize, end: usize, raw: &str, data: &str) -> Value {
    obj(vec![
        ("type", "Text".into()),
        ("start", start.into()),
        ("end", end.into()),
        ("raw", raw.into()),
        ("data", data.into()),
    ])
}

fn expression_tag_json(start: usize, end: usize, expression: Value) -> Value {
    obj(vec![
        ("type", "ExpressionTag".into()),
        ("start", start.into()),
        ("end", end.into()),
        ("expression", expression),
    ])
}

pub fn chunk_json(chunk: &Chunk, cx: &ToJson) -> Value {
    match chunk {
        Chunk::Text { start, end, raw, data } => text_json(*start, *end, raw, data),
        Chunk::Expression { start, end, expression } => expression_tag_json(*start, *end, expression.to_json(cx)),
    }
}

pub fn attr_value_json(value: &AttrValue, cx: &ToJson) -> Value {
    match value {
        AttrValue::True => Value::Bool(true),
        AttrValue::Expression(chunk) => chunk_json(chunk, cx),
        AttrValue::Sequence(chunks) => Value::Array(chunks.iter().map(|c| chunk_json(c, cx)).collect()),
    }
}

pub fn attr_json(attr: &Attr, cx: &ToJson) -> Value {
    match attr {
        Attr::Attribute { start, end, name, name_loc, value } => obj(vec![
            ("type", "Attribute".into()),
            ("start", (*start).into()),
            ("end", (*end).into()),
            ("name", (*name).into()),
            ("name_loc", opt(name_loc, |l| l.to_json(cx))),
            ("value", attr_value_json(value, cx)),
        ]),
        Attr::Spread { start, end, expression } | Attr::Attach { start, end, expression } => obj(vec![
            ("type", attr.type_name().into()),
            ("start", (*start).into()),
            ("end", (*end).into()),
            ("expression", expression.to_json(cx)),
        ]),
        Attr::Directive { kind, start, end, name, name_loc, modifiers, expression, intro_outro } => {
            let mut pairs = vec![
                ("start", (*start).into()),
                ("end", (*end).into()),
                ("type", (*kind).into()),
                ("name", (*name).into()),
                ("name_loc", name_loc.to_json(cx)),
                ("expression", opt(expression, |e| e.to_json(cx))),
                ("modifiers", Value::Array(modifiers.iter().map(|m| (*m).into()).collect())),
            ];
            if let Some((intro, outro)) = intro_outro {
                pairs.push(("intro", (*intro).into()));
                pairs.push(("outro", (*outro).into()));
            }
            obj(pairs)
        }
        Attr::StyleDirective { start, end, name, name_loc, modifiers, value } => obj(vec![
            ("start", (*start).into()),
            ("end", (*end).into()),
            ("type", "StyleDirective".into()),
            ("name", (*name).into()),
            ("name_loc", name_loc.to_json(cx)),
            ("modifiers", Value::Array(modifiers.iter().map(|m| (*m).into()).collect())),
            ("value", attr_value_json(value, cx)),
        ]),
    }
}

impl Script<'_> {
    fn to_json(&self, cx: &ToJson) -> Value {
        let mut content = cx.program(&self.content);
        if let Value::Object(m) = &mut content {
            m.insert("start".into(), self.content.start.into());
            m.insert(
                "loc".into(),
                json!({ "start": cx.loc.position(self.start), "end": cx.loc.position(self.end) }),
            );
            if let Some(data) = &self.leading_comment {
                m.insert("leadingComments".into(), json!([{ "type": "Line", "value": data }]));
            }
        }
        obj(vec![
            ("type", "Script".into()),
            ("start", self.start.into()),
            ("end", self.end.into()),
            ("context", self.context.into()),
            ("content", content),
            ("attributes", Value::Array(self.attributes.iter().map(|a| attr_json(a, cx)).collect())),
        ])
    }
}

impl Root<'_> {
    pub fn to_json(&self, ast: &Ast, cx: &ToJson) -> Value {
        let css = match &self.css {
            None => Value::Null,
            Some(sheet) => {
                let mut json = sheet.css.to_json(&cx.loc.source()[sheet.css.content_start..sheet.css.content_end]);
                json["attributes"] = Value::Array(sheet.attributes.iter().map(|a| attr_json(a, cx)).collect());
                json
            }
        };
        let options = match &self.options {
            None => Value::Null,
            Some(o) => {
                let mut m = Map::new();
                m.insert("start".into(), o.start.into());
                m.insert("end".into(), o.end.into());
                m.insert("attributes".into(), Value::Array(o.attributes.iter().map(|a| attr_json(a, cx)).collect()));
                for (k, v) in &o.values {
                    m.insert(k.clone(), v.clone());
                }
                Value::Object(m)
            }
        };
        let mut pairs = vec![
            ("css", css),
            ("js", Value::Array(vec![])),
            ("start", self.start.into()),
            ("end", self.end.into()),
            ("type", "Root".into()),
            ("fragment", ast.fragment_json(self.fragment, cx)),
            ("options", options),
            ("comments", Value::Array(self.comments.iter().map(|c| c.to_json(cx.loc)).collect())),
        ];
        if let Some(s) = &self.instance {
            pairs.push(("instance", s.to_json(cx)));
        }
        if let Some(s) = &self.module {
            pairs.push(("module", s.to_json(cx)));
        }
        obj(pairs)
    }
}
