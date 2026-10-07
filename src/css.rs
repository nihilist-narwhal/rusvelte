//! The CSS AST (`AST.CSS.*`), produced by `parser/style.rs`.

use serde_json::{json, Map, Value};

#[derive(Debug)]
pub struct StyleSheet {
    pub start: usize,
    pub end: usize,
    pub children: Vec<BlockChild>,
    pub comments: Vec<CssComment>,
    pub content_start: usize,
    pub content_end: usize,
    /// The HTML comment right before `<style>`, as `(start, end, data)`
    pub comment: Option<(usize, usize, String)>,
}

/// Something inside a block (or at the top level, where it's only rules and at-rules)
#[derive(Debug)]
pub enum BlockChild {
    Rule(Rule),
    Atrule(Atrule),
    Declaration(Declaration),
}

#[derive(Debug)]
pub struct Rule {
    pub start: usize,
    pub end: usize,
    pub prelude: SelectorList,
    pub block: Block,
}

#[derive(Debug)]
pub struct Atrule {
    pub start: usize,
    pub end: usize,
    pub name: String,
    pub prelude: String,
    pub block: Option<Block>,
}

#[derive(Debug)]
pub struct Block {
    pub start: usize,
    pub end: usize,
    pub children: Vec<BlockChild>,
}

#[derive(Debug)]
pub struct Declaration {
    pub start: usize,
    pub end: usize,
    pub property: String,
    pub value: String,
}

#[derive(Debug)]
pub struct SelectorList {
    pub start: usize,
    pub end: usize,
    pub children: Vec<ComplexSelector>,
}

#[derive(Debug)]
pub struct ComplexSelector {
    pub start: usize,
    pub end: usize,
    pub children: Vec<RelativeSelector>,
}

#[derive(Debug)]
pub struct RelativeSelector {
    pub start: usize,
    pub end: usize,
    pub combinator: Option<Combinator>,
    pub selectors: Vec<SimpleSelector>,
}

#[derive(Debug)]
pub struct Combinator {
    pub start: usize,
    pub end: usize,
    pub name: &'static str,
}

#[derive(Debug)]
pub enum SimpleSelector {
    Nesting { start: usize, end: usize },
    Type { start: usize, end: usize, name: String, namespace: Option<String> },
    Id { start: usize, end: usize, name: String },
    Class { start: usize, end: usize, name: String },
    PseudoElement { start: usize, end: usize, name: String, args: Option<SelectorList> },
    PseudoClass { start: usize, end: usize, name: String, args: Option<SelectorList> },
    Attribute { start: usize, end: usize, name: String, matcher: Option<String>, value: Option<String>, flags: Option<String> },
    Nth { start: usize, end: usize, value: String },
    Percentage { start: usize, end: usize, value: String },
}

#[derive(Debug)]
pub struct CssComment {
    pub start: usize,
    pub end: usize,
    pub value: String,
    /// Where in a declaration's value the comment sat (UTF-16 offset), for comments inside values
    pub position: Option<i64>,
}

// ---------------------------------------------------------------------------------------

fn obj(pairs: Vec<(&str, Value)>) -> Value {
    let mut m = Map::with_capacity(pairs.len());
    for (k, v) in pairs {
        m.insert(k.to_string(), v);
    }
    Value::Object(m)
}

impl StyleSheet {
    /// The `StyleSheet` node, without `attributes`
    pub fn to_json(&self, styles: &str) -> Value {
        obj(vec![
            ("type", "StyleSheet".into()),
            ("start", self.start.into()),
            ("end", self.end.into()),
            ("children", Value::Array(self.children.iter().map(BlockChild::to_json).collect())),
            ("comments", Value::Array(self.comments.iter().map(CssComment::to_json).collect())),
            (
                "content",
                obj(vec![
                    ("start", self.content_start.into()),
                    ("end", self.content_end.into()),
                    ("styles", styles.into()),
                    (
                        "comment",
                        match &self.comment {
                            Some((start, end, data)) => json!({ "type": "Comment", "start": start, "end": end, "data": data }),
                            None => Value::Null,
                        },
                    ),
                ]),
            ),
        ])
    }
}

impl BlockChild {
    fn to_json(&self) -> Value {
        match self {
            BlockChild::Rule(r) => r.to_json(),
            BlockChild::Atrule(a) => a.to_json(),
            BlockChild::Declaration(d) => obj(vec![
                ("type", "Declaration".into()),
                ("start", d.start.into()),
                ("end", d.end.into()),
                ("property", d.property.clone().into()),
                ("value", d.value.clone().into()),
            ]),
        }
    }
}

impl Rule {
    fn to_json(&self) -> Value {
        obj(vec![
            ("type", "Rule".into()),
            ("prelude", self.prelude.to_json()),
            ("block", self.block.to_json()),
            ("start", self.start.into()),
            ("end", self.end.into()),
        ])
    }
}

impl Atrule {
    fn to_json(&self) -> Value {
        obj(vec![
            ("type", "Atrule".into()),
            ("start", self.start.into()),
            ("end", self.end.into()),
            ("name", self.name.clone().into()),
            ("prelude", self.prelude.clone().into()),
            ("block", self.block.as_ref().map_or(Value::Null, Block::to_json)),
        ])
    }
}

impl Block {
    fn to_json(&self) -> Value {
        obj(vec![
            ("type", "Block".into()),
            ("start", self.start.into()),
            ("end", self.end.into()),
            ("children", Value::Array(self.children.iter().map(BlockChild::to_json).collect())),
        ])
    }
}

impl SelectorList {
    fn to_json(&self) -> Value {
        obj(vec![
            ("type", "SelectorList".into()),
            ("start", self.start.into()),
            ("end", self.end.into()),
            ("children", Value::Array(self.children.iter().map(ComplexSelector::to_json).collect())),
        ])
    }
}

impl ComplexSelector {
    fn to_json(&self) -> Value {
        obj(vec![
            ("type", "ComplexSelector".into()),
            ("start", self.start.into()),
            ("end", self.end.into()),
            ("children", Value::Array(self.children.iter().map(RelativeSelector::to_json).collect())),
        ])
    }
}

impl RelativeSelector {
    fn to_json(&self) -> Value {
        obj(vec![
            ("type", "RelativeSelector".into()),
            (
                "combinator",
                match &self.combinator {
                    Some(c) => json!({ "type": "Combinator", "name": c.name, "start": c.start, "end": c.end }),
                    None => Value::Null,
                },
            ),
            ("selectors", Value::Array(self.selectors.iter().map(SimpleSelector::to_json).collect())),
            ("start", self.start.into()),
            ("end", self.end.into()),
        ])
    }
}

impl SimpleSelector {
    fn to_json(&self) -> Value {
        match self {
            SimpleSelector::Nesting { start, end } => {
                json!({ "type": "NestingSelector", "name": "&", "start": start, "end": end })
            }
            SimpleSelector::Type { start, end, name, namespace } => {
                let mut pairs = vec![("type", "TypeSelector".into()), ("name", name.clone().into())];
                if let Some(ns) = namespace {
                    pairs.push(("namespace", ns.clone().into()));
                }
                pairs.push(("start", (*start).into()));
                pairs.push(("end", (*end).into()));
                obj(pairs)
            }
            SimpleSelector::Id { start, end, name } => {
                json!({ "type": "IdSelector", "name": name, "start": start, "end": end })
            }
            SimpleSelector::Class { start, end, name } => {
                json!({ "type": "ClassSelector", "name": name, "start": start, "end": end })
            }
            SimpleSelector::PseudoElement { start, end, name, args } => {
                let mut pairs = vec![
                    ("type", "PseudoElementSelector".into()),
                    ("name", name.clone().into()),
                    ("start", (*start).into()),
                    ("end", (*end).into()),
                ];
                if let Some(args) = args {
                    pairs.push(("args", args.to_json()));
                }
                obj(pairs)
            }
            SimpleSelector::PseudoClass { start, end, name, args } => obj(vec![
                ("type", "PseudoClassSelector".into()),
                ("name", name.clone().into()),
                ("args", args.as_ref().map_or(Value::Null, SelectorList::to_json)),
                ("start", (*start).into()),
                ("end", (*end).into()),
            ]),
            SimpleSelector::Attribute { start, end, name, matcher, value, flags } => obj(vec![
                ("type", "AttributeSelector".into()),
                ("start", (*start).into()),
                ("end", (*end).into()),
                ("name", name.clone().into()),
                ("matcher", matcher.clone().map_or(Value::Null, Value::String)),
                ("value", value.clone().map_or(Value::Null, Value::String)),
                ("flags", flags.clone().map_or(Value::Null, Value::String)),
            ]),
            SimpleSelector::Nth { start, end, value } => {
                json!({ "type": "Nth", "value": value, "start": start, "end": end })
            }
            SimpleSelector::Percentage { start, end, value } => {
                json!({ "type": "Percentage", "value": value, "start": start, "end": end })
            }
        }
    }
}

impl CssComment {
    fn to_json(&self) -> Value {
        let mut pairs = vec![
            ("type", "CSSComment".into()),
            ("value", self.value.clone().into()),
            ("start", self.start.into()),
            ("end", self.end.into()),
        ];
        if let Some(p) = self.position {
            pairs.push(("position", p.into()));
        }
        obj(pairs)
    }
}
