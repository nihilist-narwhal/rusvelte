//! Port of `phases/1-parse/read/options.js`.
//!
//! `<svelte:options>` is rare and its values end up as JSON anyway, so this works on the
//! JSON form of the attributes.

use std::sync::LazyLock;

use serde_json::{Map, Value};

use super::{node_type, Parser};
use crate::ast::{attr_json, Node, NodeId, SvelteOptions};
use crate::error::{CompileError, Result};
use crate::errors as e;
use crate::js::ToJson;

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

fn span(attr: &Value) -> (usize, usize) {
    let n = |k: &str| attr.get(k).and_then(Value::as_u64).unwrap_or(0) as usize;
    (n("start"), n("end"))
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

fn get_static_value(value: &Value) -> Static {
    let chunk = match value {
        Value::Bool(true) => return Static::True,
        Value::Array(chunks) => {
            let Some(first) = chunks.first() else { return Static::True };
            if chunks.len() > 1 {
                return Static::Null;
            }
            first
        }
        other => other,
    };
    if node_type(chunk) == "Text" {
        return Static::Value(chunk["data"].clone());
    }
    let expression = &chunk["expression"];
    if node_type(expression) != "Literal" {
        return Static::Null;
    }
    Static::Value(expression.get("value").cloned().unwrap_or(Value::Null))
}

fn get_boolean_value(attr: &Value) -> Result<Value> {
    match get_static_value(&attr["value"]) {
        Static::True => Ok(Value::Bool(true)),
        Static::Value(v @ Value::Bool(_)) => Ok(v),
        _ => Err(e::svelte_options_invalid_attribute_value(span(attr), "true or false")),
    }
}

fn validate_tag(loc: Option<(usize, usize)>, tag: &Static) -> Result<String> {
    let err = |f: fn((usize, usize)) -> CompileError| {
        let mut err = f(loc.unwrap_or((0, 0)));
        if loc.is_none() {
            // the JS passes a `[name, value]` tuple as the node, which has no position
            err.position = None;
        }
        err
    };
    let Static::Value(Value::String(tag)) = tag else {
        return Err(err(e::svelte_options_invalid_tagname));
    };
    if !tag.is_empty() {
        if !REGEX_VALID_TAG_NAME.is_match(tag) {
            return Err(err(e::svelte_options_invalid_tagname));
        } else if RESERVED_TAG_NAMES.contains(&tag.as_str()) {
            return Err(err(e::svelte_options_reserved_tagname));
        }
    }
    Ok(tag.clone())
}

fn is_plain_property(property: &Value) -> bool {
    node_type(property) == "Property" && property["computed"] != true && node_type(&property["key"]) == "Identifier"
}

fn read_custom_element(attribute: &Value) -> Result<Option<Value>> {
    let mut ce = Map::new();
    let value = &attribute["value"];
    let first = match value {
        Value::Bool(true) => return Err(e::svelte_options_invalid_customelement(span(attribute))),
        Value::Array(chunks) => &chunks[0],
        other => other,
    };

    if node_type(first) == "Text" {
        let tag = validate_tag(Some(span(attribute)), &get_static_value(value))?;
        ce.insert("tag".into(), tag.into());
        return Ok(Some(Value::Object(ce)));
    }

    let expression = &first["expression"];
    if node_type(expression) != "ObjectExpression" {
        // `customElement={null}` is allowed for backwards compatibility
        if node_type(expression) == "Literal" && expression.get("value") == Some(&Value::Null) {
            return Ok(None);
        }
        return Err(e::svelte_options_invalid_customelement(span(attribute)));
    }

    let mut properties: Vec<(&str, &Value)> = Vec::new();
    for property in expression["properties"].as_array().into_iter().flatten() {
        if !is_plain_property(property) {
            return Err(e::svelte_options_invalid_customelement(span(attribute)));
        }
        properties.push((property["key"]["name"].as_str().unwrap_or(""), &property["value"]));
    }
    let find = |n: &str| properties.iter().find(|(k, _)| *k == n).map(|(_, v)| *v);
    let props_error = || e::svelte_options_invalid_customelement_props(span(attribute));

    if let Some(tag) = find("tag") {
        let tag_value = tag.get("value").map_or(Static::Null, |v| Static::Value(v.clone()));
        ce.insert("tag".into(), validate_tag(None, &tag_value)?.into());
    }

    if let Some(props) = find("props") {
        if node_type(props) != "ObjectExpression" {
            return Err(props_error());
        }
        let mut out = Map::new();
        for property in props["properties"].as_array().into_iter().flatten() {
            if !is_plain_property(property) || node_type(&property["value"]) != "ObjectExpression" {
                return Err(props_error());
            }
            let mut def = Map::new();
            for prop in property["value"]["properties"].as_array().into_iter().flatten() {
                if !is_plain_property(prop) || node_type(&prop["value"]) != "Literal" {
                    return Err(props_error());
                }
                let v = prop["value"]["value"].clone();
                match prop["key"]["name"].as_str().unwrap_or("") {
                    "type" if matches!(v.as_str(), Some("String" | "Number" | "Boolean" | "Array" | "Object")) => {
                        def.insert("type".into(), v);
                    }
                    "reflect" if v.is_boolean() => {
                        def.insert("reflect".into(), v);
                    }
                    "attribute" if v.is_string() => {
                        def.insert("attribute".into(), v);
                    }
                    _ => return Err(props_error()),
                }
            }
            out.insert(property["key"]["name"].as_str().unwrap_or("").to_string(), Value::Object(def));
        }
        ce.insert("props".into(), Value::Object(out));
    }

    if let Some(shadow) = find("shadow") {
        if node_type(shadow) == "Literal" && matches!(shadow["value"].as_str(), Some("open" | "none")) {
            ce.insert("shadow".into(), shadow["value"].clone());
        } else if node_type(shadow) == "ObjectExpression" {
            ce.insert("shadow".into(), shadow.clone());
        } else {
            return Err(e::svelte_options_invalid_customelement_shadow(span(attribute)));
        }
    }

    if let Some(extend) = find("extend") {
        ce.insert("extend".into(), extend.clone());
    }

    Ok(Some(Value::Object(ce)))
}

pub fn read_options<'a>(parser: &mut Parser<'a>, id: NodeId) -> Result<SvelteOptions<'a>> {
    let Node::Element(node) = &mut parser.ast.nodes[id] else { unreachable!() };
    let attributes = std::mem::take(&mut node.attributes);
    let (start, end) = (node.start, node.end.unwrap_or(0));

    let cx = ToJson { ts: parser.ts, loc: &parser.loc, comments: &parser.root.comments };
    let mut values = Map::new();

    for attribute in &attributes {
        let json = attr_json(attribute, &cx);
        if node_type(&json) != "Attribute" {
            return Err(e::svelte_options_invalid_attribute(span(&json)));
        }
        let name = json["name"].as_str().unwrap_or("").to_string();

        match name.as_str() {
            "runes" | "immutable" | "preserveWhitespace" | "accessors" => {
                values.insert(name, get_boolean_value(&json)?);
            }
            "tag" => return Err(e::svelte_options_deprecated_tag(span(&json))),
            "customElement" => {
                if let Some(ce) = read_custom_element(&json)? {
                    values.insert("customElement".into(), ce);
                }
            }
            "namespace" => {
                let ns = match get_static_value(&json["value"]) {
                    Static::Value(Value::String(s)) if s == NAMESPACE_SVG => "svg".to_string(),
                    Static::Value(Value::String(s)) if s == NAMESPACE_MATHML => "mathml".to_string(),
                    Static::Value(Value::String(s)) if s == "html" || s == "mathml" || s == "svg" => s,
                    _ => {
                        return Err(e::svelte_options_invalid_attribute_value(
                            span(&json),
                            "\"html\", \"mathml\" or \"svg\"",
                        ))
                    }
                };
                values.insert("namespace".into(), ns.into());
            }
            "css" => {
                if get_static_value(&json["value"]) == Static::Value("injected".into()) {
                    values.insert("css".into(), "injected".into());
                } else {
                    return Err(e::svelte_options_invalid_attribute_value(span(&json), "\"injected\""));
                }
            }
            _ => return Err(e::svelte_options_unknown_attribute(span(&json), &name)),
        }
    }

    Ok(SvelteOptions { start, end, attributes, values })
}
