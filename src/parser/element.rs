//! Port of `phases/1-parse/state/element.js` and `read/script.js`.

use std::sync::LazyLock;

use regex::Regex;

use super::utils::*;
use super::{style, LastAutoClosedTag, Open, Parser};
use crate::ast::{Attr, AttrValue, Chunk, Element, Expr, IdentLoc, NameLoc, Node, Script, StyleSheet};
use crate::error::Result;
use crate::errors as e;
use crate::js::JsComment;

const WS: &str = r"\t\n\x0B\x0C\r \u{a0}\u{1680}\u{2000}-\u{200a}\u{2028}\u{2029}\u{202f}\u{205f}\u{3000}\u{feff}";

static REGEX_VALID_TAG_NAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[a-zA-Z][a-zA-Z0-9]*(-[a-zA-Z0-9.\-_\u{00B7}\u{00C0}-\u{00D6}\u{00D8}-\u{00F6}\u{00F8}-\u{037D}\u{037F}-\u{1FFF}\u{200C}-\u{200D}\u{203F}-\u{2040}\u{2070}-\u{218F}\u{2C00}-\u{2FEF}\u{3001}-\u{D7FF}\u{F900}-\u{FDCF}\u{FDF0}-\u{FFFD}\u{10000}-\u{EFFFF}]*)?$").unwrap()
});
static REGEX_VALID_COMPONENT_NAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:\p{Lu}[$\u{200c}\u{200d}\p{ID_Continue}.]*|\p{ID_Start}[$\u{200c}\u{200d}\p{ID_Continue}]*(?:\.[$\u{200c}\u{200d}\p{ID_Continue}]+)+)$").unwrap()
});
static REGEX_NAMESPACED_NAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-zA-Z][a-zA-Z0-9]*:[a-zA-Z][a-zA-Z0-9-]*[a-zA-Z0-9]$").unwrap());
static REGEX_CLOSING_TEXTAREA_TAG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!(r"(?i)^</textarea([{WS}][^>]*)?>")).unwrap());
static REGEX_CLOSING_SCRIPT_TAG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!(r"</script[{WS}]*>")).unwrap());
static REGEX_INVALID_UNQUOTED_ATTRIBUTE_VALUE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!(r#"^(/>|[{WS}"'=<>`])"#)).unwrap());
static REGEX_ATTRIBUTE_VALUE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!(r#"^(?:"([^"]*)"|'([^'])*'|([^>{WS}]+))"#)).unwrap());

const ROOT_ONLY_META_TAGS: &[(&str, &str)] = &[
    ("svelte:head", "SvelteHead"),
    ("svelte:options", "SvelteOptions"),
    ("svelte:window", "SvelteWindow"),
    ("svelte:document", "SvelteDocument"),
    ("svelte:body", "SvelteBody"),
];
const META_TAGS: &[(&str, &str)] = &[
    ("svelte:head", "SvelteHead"),
    ("svelte:options", "SvelteOptions"),
    ("svelte:window", "SvelteWindow"),
    ("svelte:document", "SvelteDocument"),
    ("svelte:body", "SvelteBody"),
    ("svelte:element", "SvelteElement"),
    ("svelte:component", "SvelteComponent"),
    ("svelte:self", "SvelteSelf"),
    ("svelte:fragment", "SvelteFragment"),
    ("svelte:boundary", "SvelteBoundary"),
];

fn meta_tag(name: &str) -> Option<&'static str> {
    META_TAGS.iter().find(|(n, _)| *n == name).map(|(_, t)| *t)
}

fn is_valid_element_name(name: &str) -> bool {
    // fast path: a plain tag name like `div` or `h1` matches REGEX_VALID_TAG_NAME
    let b = name.as_bytes();
    if !b.is_empty() && b[0].is_ascii_alphabetic() && b.iter().all(u8::is_ascii_alphanumeric) {
        return true;
    }
    // !DOCTYPE
    if name.len() > 1 && name.starts_with('!') && name[1..].bytes().all(|b| b.is_ascii_alphabetic()) {
        return true;
    }
    REGEX_NAMESPACED_NAME.is_match(name) || REGEX_VALID_TAG_NAME.is_match(name)
}

pub fn is_valid_component_name(name: &str) -> bool {
    // fast paths for ASCII names: `Foo` / `Foo_2` are components, plain lowercase names aren't
    let b = name.as_bytes();
    if !b.is_empty() && b.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'_' || *c == b'$') {
        return b[0].is_ascii_uppercase();
    }
    REGEX_VALID_COMPONENT_NAME.is_match(name)
}

pub fn element(parser: &mut Parser) -> Result<()> {
    let start = parser.index;
    parser.index += 1;

    let mut parent = parser.current();

    if parser.eat("!--") {
        let data = parser.read_until("-->")?;
        parser.expect("-->")?;
        parser.append(Node::Comment { start, end: parser.index, data });
        return Ok(());
    }

    if parser.eat("/") {
        let name = read_tag_name(parser, false)?;
        parser.allow_whitespace();
        parser.expect(">")?;

        if is_void(&name) {
            return Err(e::void_element_invalid_content(start));
        }

        // close any elements that don't have their own closing tags, e.g. <div><p></div>
        loop {
            let parent_name = match parent {
                Open::Node(id) => match &parser.ast.nodes[id] {
                    Node::Element(el) => Some(el.name),
                    _ => None,
                },
                Open::Root => None,
            };
            if parent_name.as_deref() == Some(name) {
                break;
            }

            if parser.loose {
                // If the previous element did interpret the next opening tag as an attribute, backtrack
                if let Open::Node(id) = parent {
                    if let Node::Element(el) = &mut parser.ast.nodes[id] {
                        if let Some(Attr::Attribute { name: attr_name, start: attr_start, .. }) = el.attributes.last() {
                            if *attr_name == format!("<{name}") {
                                parser.index = *attr_start;
                                el.attributes.pop();
                                break;
                            }
                        }
                    }
                }
            }

            let parent_type = match parent {
                Open::Root => "Root",
                Open::Node(id) => parser.ast.nodes[id].type_name(),
            };
            if parent_type != "RegularElement" && !parser.loose {
                if let Some(last) = &parser.last_auto_closed_tag {
                    if last.tag == name {
                        return Err(e::element_invalid_closing_tag_autoclosed(start, &name, &last.reason));
                    }
                }
                return Err(e::element_invalid_closing_tag(start, &name));
            }

            match parent {
                Open::Node(id) => parser.ast.nodes[id].set_end(start),
                // only reachable in loose mode: the root has no end to set
                Open::Root => {}
            }
            if parent == Open::Root {
                // the JS parser would pop the root here and crash on the next iteration;
                // in loose mode we stop instead
                break;
            }
            parser.pop();
            parent = parser.current();
        }

        if let Open::Node(id) = parent {
            parser.ast.nodes[id].set_end(parser.index);
            parser.pop();
        }

        if let Some(last) = &parser.last_auto_closed_tag {
            if parser.stack.len() < last.depth {
                parser.last_auto_closed_tag = None;
            }
        }

        return Ok(());
    }

    let (tag_name, tag_loc) = read_tag(parser, false)?;

    if tag_name.starts_with("svelte:") && meta_tag(&tag_name).is_none() {
        let list = META_TAGS.iter().map(|(n, _)| *n).collect::<Vec<_>>();
        return Err(e::svelte_meta_invalid_tag((start + 1, start + 1 + tag_name.len()), &list_str(&list)));
    }

    if !is_valid_element_name(&tag_name) && !is_valid_component_name(&tag_name) {
        // <div. -> in the middle of typing -> allow in loose mode
        if !parser.loose || !tag_name.ends_with('.') {
            return Err(e::tag_invalid_name((start + 1, start + 1 + tag_name.len())));
        }
    }

    if ROOT_ONLY_META_TAGS.iter().any(|(n, _)| *n == tag_name) {
        if parser.meta_tags.contains(tag_name) {
            return Err(e::svelte_meta_duplicate(start, &tag_name));
        }
        if parent != Open::Root {
            return Err(e::svelte_meta_invalid_placement(start, &tag_name));
        }
        parser.meta_tags.insert(tag_name.to_string());
    }

    let kind: &'static str = if let Some(t) = meta_tag(&tag_name) {
        t
    } else if is_valid_component_name(&tag_name) || (parser.loose && tag_name.ends_with('.')) {
        "Component"
    } else if tag_name == "title" && parent_is_head(parser) {
        "TitleElement"
    } else if tag_name == "slot" && !parent_is_shadowroot_template(parser) {
        "SlotElement"
    } else {
        "RegularElement"
    };

    let fragment = parser.ast.new_fragment(true);
    let mut element = Element {
        kind,
        start,
        end: None,
        name: tag_name,
        name_loc: tag_loc,
        attributes: Vec::new(),
        fragment,
        tag: None,
        expression: None,
    };

    parser.allow_whitespace();

    if let Open::Node(parent_id) = parent {
        let parent_info = match &parser.ast.nodes[parent_id] {
            Node::Element(el) if el.kind == "RegularElement" => Some(el.name),
            _ => None,
        };
        if let Some(parent_name) = parent_info {
            if closing_tag_omitted(&parent_name, &tag_name) {
                parser.ast.nodes[parent_id].set_end(start);
                parser.pop();
                parser.last_auto_closed_tag = Some(LastAutoClosedTag {
                    tag: parent_name.to_string(),
                    reason: tag_name.to_string(),
                    depth: parser.stack.len(),
                });
            }
        }
    }

    let mut unique_names: Vec<String> = Vec::new();

    let current = parser.current();
    let is_top_level_script_or_style = (tag_name == "script" || tag_name == "style") && current == Open::Root;

    loop {
        let attribute = if is_top_level_script_or_style {
            read_static_attribute(parser)?
        } else {
            read_attribute(parser)?
        };
        let Some(attribute) = attribute else { break };

        let ty = match &attribute {
            Attr::Attribute { .. } => Some("Attribute"),
            Attr::Directive { kind: "BindDirective", .. } => Some("Attribute"),
            Attr::StyleDirective { .. } => Some("StyleDirective"),
            Attr::Directive { kind: "ClassDirective", .. } => Some("ClassDirective"),
            _ => None,
        };
        if let Some(ty) = ty {
            let name = attribute.name().unwrap_or("");
            let key = format!("{ty}{name}");
            if unique_names.contains(&key) {
                return Err(e::attribute_duplicate((attribute.start(), attribute.end())));
            } else if name != "this" {
                unique_names.push(key);
            }
        }

        element.attributes.push(attribute);
        parser.allow_whitespace();
    }

    if kind == "SvelteComponent" {
        let index = element
            .attributes
            .iter()
            .position(|a| matches!(a, Attr::Attribute { name, .. } if *name == "this"));
        let Some(index) = index else {
            return Err(e::svelte_component_missing_this(start));
        };
        let definition = element.attributes.remove(index);
        if !is_expression_attribute(&definition) {
            return Err(e::svelte_component_invalid_this(definition.start()));
        }
        element.expression = get_attribute_expression(definition);
    }

    if kind == "SvelteElement" {
        let index = element
            .attributes
            .iter()
            .position(|a| matches!(a, Attr::Attribute { name, .. } if *name == "this"));
        let Some(index) = index else {
            return Err(e::svelte_element_missing_this(start));
        };
        let definition = element.attributes.remove(index);
        let (def_start, def_end) = (definition.start(), definition.end());
        let Attr::Attribute { value, .. } = &definition else { unreachable!() };

        if matches!(value, AttrValue::True) {
            return Err(e::svelte_element_missing_this((def_start, def_end)));
        }

        if !is_expression_attribute(&definition) {
            // note that this is wrong, in the case of e.g. `this="h{n}"` — it will result in `<h>`.
            // Svelte preserves the buggy Svelte 4 behaviour; TODO in 6.0, error
            let Attr::Attribute { value, .. } = definition else { unreachable!() };
            let chunk = match value {
                AttrValue::Sequence(chunks) => chunks.into_iter().next().unwrap(),
                AttrValue::Expression(c) => *c,
                AttrValue::True => unreachable!(),
            };
            element.tag = Some(match chunk {
                Chunk::Text { start, end, raw, data } => Expr::Literal { value: data.into_owned(), raw: format!("'{raw}'"), start, end },
                Chunk::Expression { expression, .. } => expression,
            });
        } else {
            element.tag = get_attribute_expression(definition);
        }
    }

    if is_top_level_script_or_style {
        parser.expect(">")?;

        let mut prev_comment: Option<(usize, usize, String)> = None;
        let root_nodes = &parser.ast.fragments[parser.root.fragment].nodes;
        for i in (0..root_nodes.len()).rev() {
            let node = &parser.ast.nodes[root_nodes[i]];
            if i == root_nodes.len() - 1 && node.end() != Some(start) {
                break;
            }
            match node {
                Node::Comment { start, end, data } => {
                    prev_comment = Some((*start, *end, data.to_string()));
                    break;
                }
                Node::Text { data, .. } if js_trim(data).is_empty() => {}
                _ => break,
            }
        }

        if tag_name == "script" {
            let mut script = read_script(parser, start, element.attributes)?;
            script.leading_comment = prev_comment.map(|(_, _, data)| data);
            if script.context == "module" {
                if parser.root.module.is_some() {
                    return Err(e::script_duplicate(start));
                }
                parser.root.module = Some(script);
            } else {
                if parser.root.instance.is_some() {
                    return Err(e::script_duplicate(start));
                }
                parser.root.instance = Some(script);
            }
        } else {
            let mut content = style::read_style(parser, start)?;
            content.comment = prev_comment;
            if parser.root.css.is_some() {
                return Err(e::style_duplicate(start));
            }
            parser.root.css = Some(StyleSheet { attributes: element.attributes, css: content });
        }
        return Ok(());
    }

    let element_id = parser.append(Node::Element(element));

    let self_closing = parser.eat("/") || is_void(&tag_name);
    let closed = parser.eat_req(">", true, false)?;

    // Loose parsing mode
    if !closed {
        let Node::Element(el) = &mut parser.ast.nodes[element_id] else { unreachable!() };
        // We may have eaten an opening `<` of the next element and treated it as an attribute...
        let last_is_lt = matches!(el.attributes.last(), Some(Attr::Attribute { name, .. }) if *name == "<");
        if last_is_lt {
            parser.index = el.attributes.last().unwrap().start();
            el.attributes.pop();
        } else {
            // ... or we may have eaten part of a following block ...
            let i = parser.index;
            let prev_1 = if i >= 1 { parser.byte(i - 1) } else { None };
            let prev_2 = if i >= 2 { parser.byte(i - 2) } else { None };
            let cur = parser.byte(i);
            if prev_2 == Some(b'{') && prev_1 == Some(b'/') {
                parser.index -= 2;
            } else if prev_1 == Some(b'{') && matches!(cur, Some(b'#' | b'@' | b':')) {
                parser.index -= 1;
            } else {
                // ... or we're followed by whitespace, for example near the end of the template
                parser.allow_whitespace();
            }
        }
    }

    if self_closing || !closed {
        parser.ast.nodes[element_id].set_end(parser.index);
    } else if tag_name == "textarea" {
        let nodes = read_sequence(
            parser,
            |p| REGEX_CLOSING_TEXTAREA_TAG.is_match(&p.template[p.index..]),
            "inside <textarea>",
        )?;
        if let Some(m) = REGEX_CLOSING_TEXTAREA_TAG.find(&parser.template[parser.index..]) {
            parser.index += m.end();
        }
        let ids: Vec<_> = nodes.into_iter().map(|c| parser.ast.add(chunk_to_node(c))).collect();
        let Node::Element(el) = &parser.ast.nodes[element_id] else { unreachable!() };
        let frag = el.fragment;
        parser.ast.fragments[frag].nodes = ids;
        parser.ast.nodes[element_id].set_end(parser.index);
    } else if tag_name == "script" || tag_name == "style" {
        let start = parser.index;
        let close_tag = format!("</{tag_name}>");
        let close_index = parser.template[parser.index..].find(&close_tag).map(|p| p + parser.index);
        let end = close_index.unwrap_or(parser.template.len());
        let data = &parser.template[start..end];
        parser.index = end;
        let text = parser.ast.add(Node::Text { start, end, raw: data, data: data.into() });
        let Node::Element(el) = &parser.ast.nodes[element_id] else { unreachable!() };
        let frag = el.fragment;
        parser.ast.fragments[frag].nodes.push(text);
        parser.expect(&close_tag)?;
        parser.ast.nodes[element_id].set_end(parser.index);
    } else {
        let Node::Element(el) = &parser.ast.nodes[element_id] else { unreachable!() };
        let frag = el.fragment;
        parser.stack.push(Open::Node(element_id));
        parser.push_fragment(frag);
    }

    Ok(())
}

/// `list` from utils/string.js
fn list_str(strings: &[&str]) -> String {
    match strings.len() {
        1 => strings[0].to_string(),
        2 => format!("{} or {}", strings[0], strings[1]),
        n => format!("{} or {}", strings[..n - 1].join(", "), strings[n - 1]),
    }
}

fn chunk_to_node(chunk: Chunk) -> Node {
    match chunk {
        Chunk::Text { start, end, raw, data } => Node::Text { start, end, raw, data },
        Chunk::Expression { start, end, expression } => Node::ExpressionTag { start, end, expression },
    }
}

fn is_expression_attribute(attr: &Attr) -> bool {
    match attr {
        Attr::Attribute { value: AttrValue::Expression(_), .. } => true,
        Attr::Attribute { value: AttrValue::Sequence(chunks), .. } => {
            chunks.len() == 1 && matches!(chunks[0], Chunk::Expression { .. })
        }
        _ => false,
    }
}

fn get_attribute_expression(attr: Attr) -> Option<Expr> {
    match attr {
        Attr::Attribute { value: AttrValue::Expression(chunk), .. } => match *chunk {
            Chunk::Expression { expression, .. } => Some(expression),
            Chunk::Text { .. } => None,
        },
        Attr::Attribute { value: AttrValue::Sequence(chunks), .. } => match chunks.into_iter().next() {
            Some(Chunk::Expression { expression, .. }) => Some(expression),
            _ => None,
        },
        _ => None,
    }
}

fn parent_is_head(parser: &Parser) -> bool {
    for open in parser.stack.iter().rev() {
        let ty = match open {
            Open::Root => "Root",
            Open::Node(id) => parser.ast.nodes[*id].type_name(),
        };
        if ty == "SvelteHead" {
            return true;
        }
        if ty == "RegularElement" || ty == "Component" {
            return false;
        }
    }
    false
}

fn parent_is_shadowroot_template(parser: &Parser) -> bool {
    parser.stack.iter().rev().any(|open| match open {
        Open::Node(id) => match &parser.ast.nodes[*id] {
            Node::Element(el) if el.kind == "RegularElement" => el
                .attributes
                .iter()
                .any(|a| matches!(a, Attr::Attribute { name, .. } if *name == "shadowrootmode")),
            _ => false,
        },
        Open::Root => false,
    })
}

fn read_static_attribute<'a>(parser: &mut Parser<'a>) -> Result<Option<Attr<'a>>> {
    let start = parser.index;
    let (name, name_loc) = read_tag(parser, true)?;
    if name.is_empty() {
        return Ok(None);
    }

    let mut value = AttrValue::True;

    if parser.eat("=") {
        parser.allow_whitespace();
        let Some(m) = REGEX_ATTRIBUTE_VALUE.find(&parser.template[parser.index..]) else {
            return Err(e::expected_attribute_value(parser.index));
        };
        let mut raw = m.as_str();
        parser.index += raw.len();

        let quoted = raw.starts_with('"') || raw.starts_with('\'');
        if quoted {
            raw = &raw[1..raw.len() - 1];
        }

        value = AttrValue::Sequence(vec![Chunk::Text {
            start: parser.index - raw.len() - usize::from(quoted),
            end: if quoted { parser.index - 1 } else { parser.index },
            raw,
            data: decode_character_references(raw, true),
        }]);
    }

    if parser.match_str("\"") || parser.match_str("'") {
        return Err(e::expected_token(parser.index, "="));
    }

    Ok(Some(Attr::Attribute { start, end: parser.index, name, name_loc: Some(name_loc), value }))
}

fn read_attribute<'a>(parser: &mut Parser<'a>) -> Result<Option<Attr<'a>>> {
    while let Some(comment) = read_comment(parser) {
        parser.root.comments.push(comment);
        parser.allow_whitespace();
    }

    let start = parser.index;

    if parser.eat("{") {
        parser.allow_whitespace();

        if parser.eat("@attach") {
            parser.require_whitespace()?;
            let expression = parser.read_expression()?;
            parser.allow_whitespace();
            parser.expect("}")?;
            return Ok(Some(Attr::Attach { start, end: parser.index, expression }));
        }

        if parser.eat("...") {
            let expression = parser.read_expression()?;
            parser.allow_whitespace();
            parser.expect("}")?;
            return Ok(Some(Attr::Spread { start, end: parser.index, expression }));
        }

        let id = parser.read_identifier()?;
        let name = id.name;
        if name.is_empty() {
            if parser.loose && (parser.match_str("#") || parser.match_str("/") || parser.match_str("@") || parser.match_str(":")) {
                return Ok(None);
            } else if parser.loose && parser.match_str("}") {
                // Likely in the middle of typing, just created the shorthand
            } else {
                return Err(e::attribute_empty_shorthand(start));
            }
        }

        parser.allow_whitespace();
        parser.expect("}")?;

        let name_loc = NameLoc { start: id.start, end: id.end };
        let chunk = Chunk::Expression { start: id.start, end: id.end, expression: id.expr() };
        return Ok(Some(Attr::Attribute {
            start,
            end: parser.index,
            name,
            name_loc: Some(name_loc),
            value: AttrValue::Expression(Box::new(chunk)),
        }));
    }

    let (name, name_loc) = read_tag(parser, true)?;
    if name.is_empty() {
        return Ok(None);
    }

    let mut end = parser.index;
    parser.allow_whitespace();

    let colon_index = name.find(':');
    let directive_type = colon_index.and_then(|i| get_directive_type(&name[..i]));

    let mut value = AttrValue::True;
    if parser.eat("=") {
        parser.allow_whitespace();
        if parser.match_str("/>") {
            let char_start = parser.index;
            parser.index += 1;
            value = AttrValue::Sequence(vec![Chunk::Text {
                start: char_start,
                end: char_start + 1,
                raw: "/".into(),
                data: "/".into(),
            }]);
        } else {
            value = read_attribute_value(parser)?;
        }
        end = parser.index;
    } else if parser.match_str("\"") || parser.match_str("'") {
        return Err(e::expected_token(parser.index, "="));
    }

    if let (Some(kind), Some(colon_index)) = (directive_type, colon_index) {
        let mut parts = name[colon_index + 1..].split('|');
        let directive_name = parts.next().unwrap_or("");
        let modifiers: Vec<&str> = parts.collect();

        if directive_name.is_empty() {
            return Err(e::directive_missing_name((start, start + colon_index + 1), &name));
        }

        if kind == "StyleDirective" {
            return Ok(Some(Attr::StyleDirective { start, end, name: directive_name, name_loc, modifiers, value }));
        }

        let first = match value {
            AttrValue::True => None,
            AttrValue::Expression(c) => Some(*c),
            AttrValue::Sequence(chunks) => {
                if chunks.len() > 1 {
                    return Err(e::directive_invalid_value(chunks[0].start()));
                }
                chunks.into_iter().next()
            }
        };

        let mut expression = match first {
            None => None,
            Some(Chunk::Text { start, .. }) => return Err(e::directive_invalid_value(start)),
            // TODO Svelte: throw a parser error in a future version if this is `[ExpressionTag]`
            // instead of `ExpressionTag`, which means stringified value
            Some(Chunk::Expression { expression, .. }) => Some(expression),
        };

        let intro_outro = if kind == "TransitionDirective" {
            let direction = &name[..colon_index];
            Some((direction == "in" || direction == "transition", direction == "out" || direction == "transition"))
        } else {
            None
        };

        // Directive name is expression, e.g. <p class:isRed />
        if (kind == "BindDirective" || kind == "ClassDirective") && expression.is_none() {
            expression = Some(Expr::Ident {
                name: directive_name.to_string(),
                start: start + colon_index + 1,
                end,
                loc: IdentLoc::None,
            });
        }

        return Ok(Some(Attr::Directive {
            kind,
            start,
            end,
            name: directive_name,
            name_loc,
            modifiers,
            expression,
            intro_outro,
        }));
    }

    Ok(Some(Attr::Attribute { start, end, name, name_loc: Some(name_loc), value }))
}

fn read_comment(parser: &mut Parser) -> Option<JsComment> {
    let start = parser.index;
    let block = if parser.eat("//") {
        false
    } else if parser.eat("/*") {
        true
    } else {
        return None;
    };
    // read_until doesn't fail here: index < len after eating
    let value = if block {
        let v = parser.read_until("*/").unwrap_or("").to_string();
        parser.eat("*/");
        v
    } else {
        parser.read_until("\n").unwrap_or("").to_string()
    };
    let end = parser.index;
    Some(JsComment {
        block,
        value,
        start,
        end,
        svelte_loc: true,
    })
}

fn get_directive_type(name: &str) -> Option<&'static str> {
    Some(match name {
        "use" => "UseDirective",
        "animate" => "AnimateDirective",
        "bind" => "BindDirective",
        "class" => "ClassDirective",
        "style" => "StyleDirective",
        "on" => "OnDirective",
        "let" => "LetDirective",
        "in" | "out" | "transition" => "TransitionDirective",
        _ => return None,
    })
}

fn read_attribute_value<'a>(parser: &mut Parser<'a>) -> Result<AttrValue<'a>> {
    let quote_mark = if parser.eat("'") {
        Some("'")
    } else if parser.eat("\"") {
        Some("\"")
    } else {
        None
    };
    if let Some(q) = quote_mark {
        if parser.eat(q) {
            return Ok(AttrValue::Sequence(vec![Chunk::Text {
                start: parser.index - 1,
                end: parser.index - 1,
                raw: "",
                data: "".into(),
            }]));
        }
    }

    let result = read_sequence(
        parser,
        |p| match quote_mark {
            Some(q) => p.match_str(q),
            None => REGEX_INVALID_UNQUOTED_ATTRIBUTE_VALUE.is_match(&p.template[p.index..]),
        },
        "in attribute value",
    );

    let value = match result {
        Ok(v) => v,
        Err(error) => {
            if error.code == "js_parse_error" {
                // e.g. `<Component test={{a:1} />`: the JS parser trips over `/>`
                if let Some((mut pos, _)) = error.position {
                    if parser.template.get(pos..pos + 2) == Some("/>") {
                        pos += 1;
                    }
                    if pos >= 1 && parser.template.get(pos - 1..pos + 1) == Some("/>") {
                        parser.index = pos;
                        return Err(e::expected_token(pos, quote_mark.unwrap_or("}")));
                    }
                }
            }
            return Err(error);
        }
    };

    if value.is_empty() && quote_mark.is_none() {
        return Err(e::expected_attribute_value(parser.index));
    }

    if quote_mark.is_some() {
        parser.index += 1;
    }

    if quote_mark.is_some() || value.len() > 1 || matches!(value[0], Chunk::Text { .. }) {
        Ok(AttrValue::Sequence(value))
    } else {
        Ok(AttrValue::Expression(Box::new(value.into_iter().next().unwrap())))
    }
}

fn read_sequence<'a>(parser: &mut Parser<'a>, done: impl Fn(&Parser) -> bool, location: &str) -> Result<Vec<Chunk<'a>>> {
    let mut chunks = Vec::new();
    let mut chunk_start = parser.index;

    let template = parser.template;
    let flush = |chunks: &mut Vec<Chunk<'a>>, chunk_start: usize, end: usize| {
        if end > chunk_start {
            let raw = &template[chunk_start..end];
            chunks.push(Chunk::Text {
                start: chunk_start,
                end,
                raw,
                data: decode_character_references(raw, true),
            });
        }
    };

    while parser.index < parser.template.len() {
        let index = parser.index;

        if done(parser) {
            flush(&mut chunks, chunk_start, parser.index);
            return Ok(chunks);
        } else if parser.eat("{") {
            if parser.match_str("#") {
                let index = parser.index - 1;
                parser.eat("#");
                let name = read_lowercase_name(parser);
                return Err(e::block_invalid_placement(index, name, location));
            } else if parser.match_str("@") {
                let index = parser.index - 1;
                parser.eat("@");
                let name = read_lowercase_name(parser);
                return Err(e::tag_invalid_placement(index, name, location));
            }

            flush(&mut chunks, chunk_start, parser.index - 1);

            parser.allow_whitespace();
            let expression = parser.read_expression()?;
            parser.allow_whitespace();
            parser.expect("}")?;

            chunks.push(Chunk::Expression { start: index, end: parser.index, expression });
            chunk_start = parser.index;
        } else {
            // step over a whole char
            let c = char_at(parser.template, parser.index).map_or(1, char::len_utf8);
            parser.index += c;
        }
    }

    if parser.loose {
        Ok(chunks)
    } else {
        Err(e::unexpected_eof(parser.template.len()))
    }
}

fn read_tag_name<'s>(parser: &mut Parser<'s>, attribute: bool) -> Result<&'s str> {
    let start = parser.index;
    if start >= parser.template.len() && !parser.loose {
        return Err(e::unexpected_eof(parser.template.len()));
    }
    let template = parser.template;
    let bytes = template.as_bytes();
    while parser.index < bytes.len() {
        let b = bytes[parser.index];
        if b < 0x80 {
            if matches!(b, b' ' | b'\t'..=b'\r' | b'/' | b'>') || (attribute && matches!(b, b'"' | b'\'' | b'=')) {
                break;
            }
            parser.index += 1;
        } else {
            let c = char_at(template, parser.index).unwrap();
            if is_whitespace_char(c) {
                break;
            }
            parser.index += c.len_utf8();
        }
    }
    Ok(&template[start..parser.index])
}

fn read_tag<'a>(parser: &mut Parser<'a>, attribute: bool) -> Result<(&'a str, NameLoc)> {
    let start = parser.index;
    let name = read_tag_name(parser, attribute)?;
    let end = parser.index;
    Ok((name, NameLoc { start, end }))
}

fn read_lowercase_name<'s>(parser: &mut Parser<'s>) -> &'s str {
    let start = parser.index;
    while matches!(parser.byte(parser.index), Some(b'a'..=b'z')) {
        parser.index += 1;
    }
    &parser.template[start..parser.index]
}

// --- read/script.js ----------------------------------------------------------------------

const RESERVED_ATTRIBUTES: &[&str] = &["server", "client", "worker", "test", "default"];

fn read_script<'a>(parser: &mut Parser<'a>, start: usize, attributes: Vec<Attr<'a>>) -> Result<Script<'a>> {
    let script_start = parser.index;
    let template = parser.template;
    if script_start >= template.len() && !parser.loose {
        return Err(e::unexpected_eof(template.len()));
    }
    let (data_end, close_len) = match REGEX_CLOSING_SCRIPT_TAG.find(&template[script_start..]) {
        Some(m) => (script_start + m.start(), m.len()),
        None => (template.len(), 0),
    };
    parser.index = data_end;
    if parser.index >= template.len() {
        return Err(e::element_unclosed(template.len(), "script"));
    }
    let data = &template[script_start..data_end];
    parser.index += close_len;

    let content = parser.js.parse_program(data, script_start, &mut parser.root.comments)?;

    let mut context = "default";
    for attribute in &attributes {
        let Attr::Attribute { name, value, .. } = attribute else { continue };
        if RESERVED_ATTRIBUTES.contains(name) {
            return Err(e::script_reserved_attribute((attribute.start(), attribute.end()), name));
        }
        if *name == "module" {
            if !matches!(value, AttrValue::True) {
                return Err(e::script_invalid_attribute_value((attribute.start(), attribute.end()), name));
            }
            context = "module";
        }
        if *name == "context" {
            let text = match value {
                AttrValue::Sequence(chunks) if chunks.len() == 1 => match &chunks[0] {
                    Chunk::Text { data, .. } => Some(data.as_ref()),
                    _ => None,
                },
                _ => None,
            };
            if text != Some("module") {
                return Err(e::script_invalid_context((attribute.start(), attribute.end())));
            }
            context = "module";
        }
    }

    Ok(Script { start, end: parser.index, context, content, attributes, leading_comment: None })
}
