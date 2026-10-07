//! Port of `phases/1-parse/read/options.js`.

use std::sync::LazyLock;

use serde_json::{Map, Value};

use super::{node_type, Parser};
use crate::ast::{Ast, Attr, AttrValue, Chunk, Node, NodeId, SvelteOptions};
use crate::error::{CompileError, Result};
use crate::errors as e;

const NAMESPACE_SVG: &str = "http://www.w3.org/2000/svg";
const NAMESPACE_MATHML: &str = "http://www.w3.org/1998/Math/MathML";

static REGEX_VALID_TAG_NAME: LazyLock<regex::Regex> = LazyLock::new(|| {
    let c = r"[a-z0-9_.\u{B7}\u{C0}-\u{D6}\u{D8}-\u{F6}\u{F8}-\u{37D}\u{37F}-\u{1FFF}\u{200C}-\u{200D}\u{203F}-\u{2040}\u{2070}-\u{218F}\u{2C00}-\u{2FEF}\u{3001}-\u{D7FF}\u{F900}-\u{FDCF}\u{FDF0}-\u{FFFD}\u{10000}-\u{EFFFF}-]";
    regex::Regex::new(&format!("^[a-z]{c}*-{c}*$")).unwrap()
});

const RESERVED_TAG_NAMES: &[&str] = &[
    "annotation-xml",
    "color-profile",
    "font-face",
    "font-face-src",
    "font-face-uri",
    "font-face-format",
    "font-face-name",
    "missing-glyph",
];

fn span(attr: &Attr) -> (usize, usize) {
    (attr.start(), attr.end())
}

/// What `get_static_value` returns
#[derive(Debug, Clone, PartialEq)]
enum Static {
    /// `true` (no value)
    True,
    /// `null` (not static)
    Null,
    Value(Value),
}

fn get_static_value(value: &AttrValue) -> Static {
    let chunk = match value {
        AttrValue::True => return Static::True,
        AttrValue::Expression(c) => &**c,
        AttrValue::Sequence(chunks) => {
            let Some(first) = chunks.first() else { return Static::True };
            if chunks.len() > 1 {
                return Static::Null;
            }
            first
        }
    };
    match chunk {
        Chunk::Text { data, .. } => Static::Value(Value::String(data.clone())),
        Chunk::Expression { expression, .. } => {
            if node_type(expression) != "Literal" {
                return Static::Null;
            }
            Static::Value(expression.get("value").cloned().unwrap_or(Value::Null))
        }
    }
}

fn get_boolean_value(attr: &Attr, value: &AttrValue) -> Result<Value> {
    match get_static_value(value) {
        Static::True => Ok(Value::Bool(true)),
        Static::Value(v @ Value::Bool(_)) => Ok(v),
        _ => Err(e::svelte_options_invalid_attribute_value(span(attr), "true or false")),
    }
}

fn validate_tag(loc: (usize, usize), tag: &Static) -> Result<String> {
    let Static::Value(Value::String(tag)) = tag else {
        return Err(e::svelte_options_invalid_tagname(loc));
    };
    if !tag.is_empty() {
        if !REGEX_VALID_TAG_NAME.is_match(tag) {
            return Err(e::svelte_options_invalid_tagname(loc));
        } else if RESERVED_TAG_NAMES.contains(&tag.as_str()) {
            return Err(e::svelte_options_reserved_tagname(loc));
        }
    }
    Ok(tag.clone())
}

fn props_error(attr: &Attr) -> CompileError {
    e::svelte_options_invalid_customelement_props(span(attr))
}

pub fn read_options(ast: &Ast, id: NodeId) -> Result<SvelteOptions> {
    let Node::Element(node) = &ast.nodes[id] else { unreachable!() };
    let mut values = Map::new();

    for attribute in &node.attributes {
        let Attr::Attribute { name, value, .. } = attribute else {
            return Err(e::svelte_options_invalid_attribute(span(attribute)));
        };

        match name.as_str() {
            "runes" => {
                values.insert("runes".into(), get_boolean_value(attribute, value)?);
            }
            "tag" => return Err(e::svelte_options_deprecated_tag(span(attribute))),
            "customElement" => {
                let mut ce = Map::new();
                let first = match value {
                    AttrValue::True => return Err(e::svelte_options_invalid_customelement(span(attribute))),
                    AttrValue::Expression(c) => (**c).clone(),
                    AttrValue::Sequence(chunks) => chunks[0].clone(),
                };
                let expression = match first {
                    Chunk::Text { .. } => {
                        let tag = validate_tag(span(attribute), &get_static_value(value))?;
                        ce.insert("tag".into(), tag.into());
                        values.insert("customElement".into(), Value::Object(ce));
                        continue;
                    }
                    Chunk::Expression { expression, .. } => expression,
                };
                if node_type(&expression) != "ObjectExpression" {
                    // `customElement={null}` is allowed for backwards compatibility
                    if node_type(&expression) == "Literal" && expression.get("value") == Some(&Value::Null) {
                        continue;
                    }
                    return Err(e::svelte_options_invalid_customelement(span(attribute)));
                }

                let mut properties: Vec<(String, Value)> = Vec::new();
                for property in expression["properties"].as_array().unwrap() {
                    if node_type(property) != "Property"
                        || property["computed"] == true
                        || node_type(&property["key"]) != "Identifier"
                    {
                        return Err(e::svelte_options_invalid_customelement(span(attribute)));
                    }
                    properties.push((property["key"]["name"].as_str().unwrap().to_string(), property["value"].clone()));
                }
                let find = |n: &str| properties.iter().find(|(k, _)| k == n).map(|(_, v)| v.clone());

                if let Some(tag) = find("tag") {
                    let tag_value = match tag.get("value") {
                        Some(v) => Static::Value(v.clone()),
                        None => Static::Null,
                    };
                    // the error position is the [name, value] pair, which has no start/end
                    let tag = validate_tag_at_none(&tag_value)?;
                    ce.insert("tag".into(), tag.into());
                }

                if let Some(props) = find("props") {
                    if node_type(&props) != "ObjectExpression" {
                        return Err(props_error(attribute));
                    }
                    let mut out = Map::new();
                    for property in props["properties"].as_array().unwrap() {
                        if node_type(property) != "Property"
                            || property["computed"] == true
                            || node_type(&property["key"]) != "Identifier"
                            || node_type(&property["value"]) != "ObjectExpression"
                        {
                            return Err(props_error(attribute));
                        }
                        let key = property["key"]["name"].as_str().unwrap().to_string();
                        let mut def = Map::new();
                        for prop in property["value"]["properties"].as_array().unwrap() {
                            if node_type(prop) != "Property"
                                || prop["computed"] == true
                                || node_type(&prop["key"]) != "Identifier"
                                || node_type(&prop["value"]) != "Literal"
                            {
                                return Err(props_error(attribute));
                            }
                            let v = prop["value"]["value"].clone();
                            match prop["key"]["name"].as_str().unwrap() {
                                "type" => {
                                    let ok = matches!(v.as_str(), Some("String" | "Number" | "Boolean" | "Array" | "Object"));
                                    if !ok {
                                        return Err(props_error(attribute));
                                    }
                                    def.insert("type".into(), v);
                                }
                                "reflect" => {
                                    if !v.is_boolean() {
                                        return Err(props_error(attribute));
                                    }
                                    def.insert("reflect".into(), v);
                                }
                                "attribute" => {
                                    if !v.is_string() {
                                        return Err(props_error(attribute));
                                    }
                                    def.insert("attribute".into(), v);
                                }
                                _ => return Err(props_error(attribute)),
                            }
                        }
                        out.insert(key, Value::Object(def));
                    }
                    ce.insert("props".into(), Value::Object(out));
                }

                if let Some(shadow) = find("shadow") {
                    let literal = node_type(&shadow) == "Literal"
                        && matches!(shadow["value"].as_str(), Some("open" | "none"));
                    if literal {
                        ce.insert("shadow".into(), shadow["value"].clone());
                    } else if node_type(&shadow) == "ObjectExpression" {
                        ce.insert("shadow".into(), shadow);
                    } else {
                        return Err(e::svelte_options_invalid_customelement_shadow(span(attribute)));
                    }
                }

                if let Some(extend) = find("extend") {
                    ce.insert("extend".into(), extend);
                }

                values.insert("customElement".into(), Value::Object(ce));
            }
            "namespace" => {
                let v = get_static_value(value);
                let ns = match &v {
                    Static::Value(Value::String(s)) if s == NAMESPACE_SVG => "svg",
                    Static::Value(Value::String(s)) if s == NAMESPACE_MATHML => "mathml",
                    Static::Value(Value::String(s)) if s == "html" || s == "mathml" || s == "svg" => s.as_str(),
                    _ => {
                        return Err(e::svelte_options_invalid_attribute_value(
                            span(attribute),
                            "\"html\", \"mathml\" or \"svg\"",
                        ))
                    }
                };
                values.insert("namespace".into(), ns.into());
            }
            "css" => {
                if get_static_value(value) == Static::Value("injected".into()) {
                    values.insert("css".into(), "injected".into());
                } else {
                    return Err(e::svelte_options_invalid_attribute_value(span(attribute), "\"injected\""));
                }
            }
            "immutable" | "preserveWhitespace" | "accessors" => {
                values.insert(name.clone(), get_boolean_value(attribute, value)?);
            }
            _ => return Err(e::svelte_options_unknown_attribute(span(attribute), name)),
        }
    }

    Ok(SvelteOptions {
        start: node.start,
        end: node.end.unwrap_or(0),
        attributes: node.attributes.clone(),
        values,
    })
}

/// `validate_tag(tag, tag_value)` where `tag` is a `[name, value]` tuple: errors have no position
fn validate_tag_at_none(tag: &Static) -> Result<String> {
    validate_tag((0, 0), tag).map_err(|mut err| {
        err.position = None;
        err
    })
}

#[allow(dead_code)]
fn _unused(_: &Parser) {}
