//! The Svelte template AST (the "modern" AST of `svelte/compiler`'s `parse`).
//!
//! Template nodes live in an arena (`Ast::nodes`) and fragments in another
//! (`Ast::fragments`), so the parser can keep a stack of open nodes while also
//! appending them to their parent's fragment, as the JS parser does with shared references.
//! JS subtrees are ESTree JSON values.

use serde_json::{Map, Value};

use crate::js::JsComment;

pub type NodeId = usize;
pub type FragId = usize;
pub type Js = Value;

#[derive(Debug, Default)]
pub struct Fragment {
    pub nodes: Vec<NodeId>,
    pub transparent: bool,
}

/// `name_loc`: Svelte locator positions (with `character`)
pub type NameLoc = Value;

#[derive(Debug, Clone)]
pub enum AttrValue {
    True,
    /// A lone `{expression}`
    Expression(Box<Chunk>),
    Sequence(Vec<Chunk>),
}

/// A `Text` or `ExpressionTag` inside an attribute value
#[derive(Debug, Clone)]
pub enum Chunk {
    Text { start: usize, end: usize, raw: String, data: String },
    Expression { start: usize, end: usize, expression: Js },
}

impl Chunk {
    pub fn start(&self) -> usize {
        match self {
            Chunk::Text { start, .. } | Chunk::Expression { start, .. } => *start,
        }
    }
}

#[derive(Debug, Clone)]
pub enum Attr {
    Attribute {
        start: usize,
        end: usize,
        name: String,
        name_loc: Option<NameLoc>,
        value: AttrValue,
    },
    Spread {
        start: usize,
        end: usize,
        expression: Js,
    },
    Attach {
        start: usize,
        end: usize,
        expression: Js,
    },
    Directive {
        kind: &'static str,
        start: usize,
        end: usize,
        name: String,
        name_loc: NameLoc,
        modifiers: Vec<String>,
        expression: Option<Js>,
        /// TransitionDirective only
        intro_outro: Option<(bool, bool)>,
    },
    StyleDirective {
        start: usize,
        end: usize,
        name: String,
        name_loc: NameLoc,
        modifiers: Vec<String>,
        value: AttrValue,
    },
}

impl Attr {
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
    pub fn name(&self) -> Option<&str> {
        match self {
            Attr::Attribute { name, .. } | Attr::Directive { name, .. } | Attr::StyleDirective { name, .. } => {
                Some(name)
            }
            _ => None,
        }
    }
}

#[derive(Debug)]
pub struct Element {
    pub kind: &'static str,
    pub start: usize,
    pub end: Option<usize>,
    pub name: String,
    pub name_loc: NameLoc,
    pub attributes: Vec<Attr>,
    pub fragment: FragId,
    /// SvelteElement
    pub tag: Option<Js>,
    /// SvelteComponent
    pub expression: Option<Js>,
}

#[derive(Debug)]
pub enum Node {
    Text { start: usize, end: usize, raw: String, data: String },
    Comment { start: usize, end: usize, data: String },
    ExpressionTag { start: usize, end: usize, expression: Js },
    HtmlTag { start: usize, end: usize, expression: Js },
    DebugTag { start: usize, end: usize, identifiers: Vec<Js> },
    ConstTag { start: usize, end: usize, declaration: Js },
    RenderTag { start: usize, end: usize, expression: Js },
    DeclarationTag { start: usize, end: usize, declaration: Js },
    IfBlock { start: usize, end: Option<usize>, elseif: bool, test: Js, consequent: FragId, alternate: Option<FragId> },
    EachBlock {
        start: usize,
        end: Option<usize>,
        expression: Js,
        context: Option<Js>,
        body: FragId,
        fallback: Option<FragId>,
        index: Option<String>,
        key: Option<Js>,
    },
    AwaitBlock {
        start: usize,
        end: Option<usize>,
        expression: Js,
        value: Option<Js>,
        error: Option<Js>,
        pending: Option<FragId>,
        then: Option<FragId>,
        catch: Option<FragId>,
    },
    KeyBlock { start: usize, end: Option<usize>, expression: Js, fragment: FragId },
    SnippetBlock {
        start: usize,
        end: Option<usize>,
        expression: Js,
        type_params: Option<String>,
        parameters: Vec<Js>,
        body: FragId,
    },
    Element(Element),
}

impl Node {
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
pub struct Script {
    pub start: usize,
    pub end: usize,
    pub context: &'static str,
    pub content: Js,
    pub attributes: Vec<Attr>,
}

#[derive(Debug, Default)]
pub struct SvelteOptions {
    pub start: usize,
    pub end: usize,
    pub attributes: Vec<Attr>,
    /// Option name → value, in insertion order, already in JSON form
    pub values: Map<String, Value>,
}

#[derive(Debug)]
pub struct Root {
    pub start: usize,
    pub end: usize,
    pub fragment: FragId,
    pub css: Option<Value>,
    pub instance: Option<Script>,
    pub module: Option<Script>,
    pub options: Option<SvelteOptions>,
    pub comments: Vec<JsComment>,
    pub ts: bool,
}

#[derive(Debug, Default)]
pub struct Ast {
    pub nodes: Vec<Node>,
    pub fragments: Vec<Fragment>,
}

impl Ast {
    pub fn new_fragment(&mut self, transparent: bool) -> FragId {
        self.fragments.push(Fragment { nodes: Vec::new(), transparent });
        self.fragments.len() - 1
    }

    pub fn add(&mut self, node: Node) -> NodeId {
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

impl Ast {
    pub fn fragment_json(&mut self, id: FragId) -> Value {
        let ids = std::mem::take(&mut self.fragments[id].nodes);
        let nodes = ids.into_iter().map(|n| self.node_json(n)).collect();
        obj(vec![("type", "Fragment".into()), ("nodes", Value::Array(nodes))])
    }

    fn opt_fragment(&mut self, id: Option<FragId>) -> Value {
        id.map_or(Value::Null, |f| self.fragment_json(f))
    }

    pub fn node_json(&mut self, id: NodeId) -> Value {
        let node = std::mem::replace(&mut self.nodes[id], Node::Comment { start: 0, end: 0, data: String::new() });
        let type_name = node.type_name();
        match node {
            Node::Text { start, end, raw, data } => text_json(start, end, &raw, &data),
            Node::Comment { start, end, data } => obj(vec![
                ("type", "Comment".into()),
                ("start", start.into()),
                ("end", end.into()),
                ("data", data.into()),
            ]),
            Node::ExpressionTag { start, end, expression } => expression_tag_json(start, end, expression),
            Node::HtmlTag { start, end, expression } | Node::RenderTag { start, end, expression } => obj(vec![
                ("type", type_name.into()),
                ("start", start.into()),
                ("end", end.into()),
                ("expression", expression),
            ]),
            Node::DebugTag { start, end, identifiers } => obj(vec![
                ("type", "DebugTag".into()),
                ("start", start.into()),
                ("end", end.into()),
                ("identifiers", Value::Array(identifiers)),
            ]),
            Node::ConstTag { start, end, declaration } | Node::DeclarationTag { start, end, declaration } => obj(vec![
                ("type", type_name.into()),
                ("start", start.into()),
                ("end", end.into()),
                ("declaration", declaration),
            ]),
            Node::IfBlock { start, end, elseif, test, consequent, alternate } => obj(vec![
                ("type", "IfBlock".into()),
                ("elseif", elseif.into()),
                ("start", start.into()),
                ("end", end_value(end)),
                ("test", test),
                ("consequent", self.fragment_json(consequent)),
                ("alternate", self.opt_fragment(alternate)),
            ]),
            Node::EachBlock { start, end, expression, context, body, fallback, index, key } => {
                let mut pairs = vec![
                    ("type", "EachBlock".into()),
                    ("start", start.into()),
                    ("end", end_value(end)),
                    ("expression", expression),
                    ("body", self.fragment_json(body)),
                    ("context", context.unwrap_or(Value::Null)),
                ];
                if let Some(index) = index {
                    pairs.push(("index", index.into()));
                }
                if let Some(key) = key {
                    pairs.push(("key", key));
                }
                if let Some(fallback) = fallback {
                    pairs.push(("fallback", self.fragment_json(fallback)));
                }
                obj(pairs)
            }
            Node::AwaitBlock { start, end, expression, value, error, pending, then, catch } => obj(vec![
                ("type", "AwaitBlock".into()),
                ("start", start.into()),
                ("end", end_value(end)),
                ("expression", expression),
                ("value", value.unwrap_or(Value::Null)),
                ("error", error.unwrap_or(Value::Null)),
                ("pending", self.opt_fragment(pending)),
                ("then", self.opt_fragment(then)),
                ("catch", self.opt_fragment(catch)),
            ]),
            Node::KeyBlock { start, end, expression, fragment } => obj(vec![
                ("type", "KeyBlock".into()),
                ("start", start.into()),
                ("end", end_value(end)),
                ("expression", expression),
                ("fragment", self.fragment_json(fragment)),
            ]),
            Node::SnippetBlock { start, end, expression, type_params, parameters, body } => {
                let mut pairs = vec![
                    ("type", "SnippetBlock".into()),
                    ("start", start.into()),
                    ("end", end_value(end)),
                    ("expression", expression),
                ];
                if let Some(tp) = type_params {
                    pairs.push(("typeParams", tp.into()));
                }
                pairs.push(("parameters", Value::Array(parameters)));
                pairs.push(("body", self.fragment_json(body)));
                obj(pairs)
            }
            Node::Element(el) => {
                let mut pairs = vec![
                    ("type", el.kind.into()),
                    ("start", el.start.into()),
                    ("end", end_value(el.end)),
                    ("name", el.name.into()),
                    ("name_loc", el.name_loc),
                    ("attributes", Value::Array(el.attributes.into_iter().map(attr_into_json).collect())),
                    ("fragment", self.fragment_json(el.fragment)),
                ];
                if let Some(tag) = el.tag {
                    pairs.push(("tag", tag));
                }
                if let Some(expression) = el.expression {
                    pairs.push(("expression", expression));
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

fn expression_tag_json(start: usize, end: usize, expression: Js) -> Value {
    obj(vec![
        ("type", "ExpressionTag".into()),
        ("start", start.into()),
        ("end", end.into()),
        ("expression", expression),
    ])
}

pub fn chunk_json(chunk: &Chunk) -> Value {
    match chunk {
        Chunk::Text { start, end, raw, data } => text_json(*start, *end, raw, data),
        Chunk::Expression { start, end, expression } => expression_tag_json(*start, *end, expression.clone()),
    }
}

fn chunk_into_json(chunk: Chunk) -> Value {
    match chunk {
        Chunk::Text { start, end, raw, data } => text_json(start, end, &raw, &data),
        Chunk::Expression { start, end, expression } => expression_tag_json(start, end, expression),
    }
}

fn attr_value_into_json(value: AttrValue) -> Value {
    match value {
        AttrValue::True => Value::Bool(true),
        AttrValue::Expression(chunk) => chunk_into_json(*chunk),
        AttrValue::Sequence(chunks) => Value::Array(chunks.into_iter().map(chunk_into_json).collect()),
    }
}

pub fn attr_into_json(attr: Attr) -> Value {
    let type_name = attr.type_name();
    match attr {
        Attr::Attribute { start, end, name, name_loc, value } => obj(vec![
            ("type", "Attribute".into()),
            ("start", start.into()),
            ("end", end.into()),
            ("name", name.into()),
            ("name_loc", name_loc.unwrap_or(Value::Null)),
            ("value", attr_value_into_json(value)),
        ]),
        Attr::Spread { start, end, expression } | Attr::Attach { start, end, expression } => obj(vec![
            ("type", type_name.into()),
            ("start", start.into()),
            ("end", end.into()),
            ("expression", expression),
        ]),
        Attr::Directive { kind, start, end, name, name_loc, modifiers, expression, intro_outro } => {
            let mut pairs = vec![
                ("start", start.into()),
                ("end", end.into()),
                ("type", kind.into()),
                ("name", name.into()),
                ("name_loc", name_loc),
                ("expression", expression.unwrap_or(Value::Null)),
                ("modifiers", Value::Array(modifiers.into_iter().map(Value::String).collect())),
            ];
            if let Some((intro, outro)) = intro_outro {
                pairs.push(("intro", intro.into()));
                pairs.push(("outro", outro.into()));
            }
            obj(pairs)
        }
        Attr::StyleDirective { start, end, name, name_loc, modifiers, value } => obj(vec![
            ("start", start.into()),
            ("end", end.into()),
            ("type", "StyleDirective".into()),
            ("name", name.into()),
            ("name_loc", name_loc),
            ("modifiers", Value::Array(modifiers.into_iter().map(Value::String).collect())),
            ("value", attr_value_into_json(value)),
        ]),
    }
}

pub fn attr_value_json(value: &AttrValue) -> Value {
    match value {
        AttrValue::True => Value::Bool(true),
        AttrValue::Expression(chunk) => chunk_json(chunk),
        AttrValue::Sequence(chunks) => Value::Array(chunks.iter().map(chunk_json).collect()),
    }
}

pub fn attr_json(attr: &Attr) -> Value {
    match attr {
        Attr::Attribute { start, end, name, name_loc, value } => obj(vec![
            ("type", "Attribute".into()),
            ("start", (*start).into()),
            ("end", (*end).into()),
            ("name", name.clone().into()),
            ("name_loc", name_loc.clone().unwrap_or(Value::Null)),
            ("value", attr_value_json(value)),
        ]),
        Attr::Spread { start, end, expression } | Attr::Attach { start, end, expression } => obj(vec![
            ("type", attr.type_name().into()),
            ("start", (*start).into()),
            ("end", (*end).into()),
            ("expression", expression.clone()),
        ]),
        Attr::Directive { kind, start, end, name, name_loc, modifiers, expression, intro_outro } => {
            let mut pairs = vec![
                ("start", (*start).into()),
                ("end", (*end).into()),
                ("type", (*kind).into()),
                ("name", name.clone().into()),
                ("name_loc", name_loc.clone()),
                ("expression", expression.clone().unwrap_or(Value::Null)),
                ("modifiers", Value::Array(modifiers.iter().map(|m| m.clone().into()).collect())),
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
            ("name", name.clone().into()),
            ("name_loc", name_loc.clone()),
            ("modifiers", Value::Array(modifiers.iter().map(|m| m.clone().into()).collect())),
            ("value", attr_value_json(value)),
        ]),
    }
}

impl Root {
    pub fn into_json(self, mut ast: Ast) -> Value {
        let mut pairs = vec![
            ("css", self.css.unwrap_or(Value::Null)),
            ("js", Value::Array(vec![])),
            ("start", self.start.into()),
            ("end", self.end.into()),
            ("type", "Root".into()),
            ("fragment", ast.fragment_json(self.fragment)),
            (
                "options",
                match self.options {
                    None => Value::Null,
                    Some(o) => {
                        let mut m = Map::new();
                        m.insert("start".into(), o.start.into());
                        m.insert("end".into(), o.end.into());
                        m.insert("attributes".into(), Value::Array(o.attributes.into_iter().map(attr_into_json).collect()));
                        for (k, v) in o.values {
                            m.insert(k, v);
                        }
                        Value::Object(m)
                    }
                },
            ),
            ("comments", Value::Array(self.comments.iter().map(JsComment::to_json).collect())),
        ];
        for (key, script) in [("instance", self.instance), ("module", self.module)] {
            if let Some(s) = script {
                pairs.push((
                    key,
                    obj(vec![
                        ("type", "Script".into()),
                        ("start", s.start.into()),
                        ("end", s.end.into()),
                        ("context", s.context.into()),
                        ("content", s.content),
                        ("attributes", Value::Array(s.attributes.into_iter().map(attr_into_json).collect())),
                    ]),
                ));
            }
        }
        obj(pairs)
    }
}
