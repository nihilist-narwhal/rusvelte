//! Port of `phases/1-parse/read/style.js`. CSS nodes are built directly as JSON.

use std::sync::LazyLock;

use serde_json::{json, Map, Value};

use super::utils::*;
use super::Parser;
use crate::error::Result;
use crate::errors as e;

static REGEX_NTH_OF: LazyLock<fancy_regex::Regex> = LazyLock::new(|| {
    fancy_regex::Regex::new(r"^(even|odd|\+?(\d+|\d*n(\s*[+-]\s*\d+)?)|-\d*n(\s*\+\s*\d+))((?=\s*[,)])|\s+of(\s+|(?=[.#\[*:&])))").unwrap()
});
static REGEX_PERCENTAGE: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"^\d+(\.\d+)?%").unwrap());
static REGEX_UNICODE_SEQUENCE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^\\[0-9a-fA-F]{1,6}(\r\n|\s)?").unwrap());
static REGEX_CLOSING_STYLE: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"^\s*>").unwrap());

fn obj(pairs: Vec<(&str, Value)>) -> Value {
    let mut m = Map::with_capacity(pairs.len());
    for (k, v) in pairs {
        m.insert(k.to_string(), v);
    }
    Value::Object(m)
}

pub fn read_style(parser: &mut Parser, start: usize, attributes: Vec<Value>) -> Result<Value> {
    let content_start = parser.index;
    parser.css_comments.clear();
    let children = read_body(parser, |p| p.match_str("</style") || p.index >= p.template.len())?;
    let content_end = parser.index;

    parser.expect("</style")?;
    if let Some(m) = REGEX_CLOSING_STYLE.find(&parser.template[parser.index..]) {
        parser.index += m.end();
    }

    Ok(obj(vec![
        ("type", "StyleSheet".into()),
        ("start", start.into()),
        ("end", parser.index.into()),
        ("attributes", Value::Array(attributes)),
        ("children", Value::Array(children)),
        ("comments", Value::Array(std::mem::take(&mut parser.css_comments))),
        (
            "content",
            obj(vec![
                ("start", content_start.into()),
                ("end", content_end.into()),
                ("styles", parser.template[content_start..content_end].into()),
                ("comment", Value::Null),
            ]),
        ),
    ]))
}

fn read_body(parser: &mut Parser, finished: impl Fn(&Parser) -> bool) -> Result<Vec<Value>> {
    let mut children = Vec::new();
    loop {
        allow_comment_or_whitespace(parser, true)?;
        if finished(parser) {
            break;
        }
        if parser.match_str("@") {
            children.push(read_at_rule(parser)?);
        } else {
            children.push(read_rule(parser)?);
        }
    }
    Ok(children)
}

fn read_at_rule(parser: &mut Parser) -> Result<Value> {
    let start = parser.index;
    parser.expect("@")?;
    let name = read_identifier(parser)?;
    let prelude = read_value(parser, true)?;

    let block = if parser.match_str("{") {
        read_block(parser)?
    } else {
        parser.expect(";")?;
        Value::Null
    };

    Ok(obj(vec![
        ("type", "Atrule".into()),
        ("start", start.into()),
        ("end", parser.index.into()),
        ("name", name.into()),
        ("prelude", prelude.into()),
        ("block", block),
    ]))
}

fn read_rule(parser: &mut Parser) -> Result<Value> {
    let start = parser.index;
    let prelude = read_selector_list(parser, false)?;
    let block = read_block(parser)?;
    Ok(obj(vec![
        ("type", "Rule".into()),
        ("prelude", prelude),
        ("block", block),
        ("start", start.into()),
        ("end", parser.index.into()),
    ]))
}

fn read_selector_list(parser: &mut Parser, inside_pseudo_class: bool) -> Result<Value> {
    let mut children = Vec::new();
    allow_comment_or_whitespace(parser, true)?;
    let start = parser.index;

    while parser.index < parser.template.len() {
        children.push(read_selector(parser, inside_pseudo_class)?);
        let end = parser.index;
        allow_comment_or_whitespace(parser, true)?;

        if if inside_pseudo_class { parser.match_str(")") } else { parser.match_str("{") } {
            return Ok(obj(vec![
                ("type", "SelectorList".into()),
                ("start", start.into()),
                ("end", end.into()),
                ("children", Value::Array(children)),
            ]));
        } else {
            parser.expect(",")?;
            allow_comment_or_whitespace(parser, true)?;
        }
    }

    Err(e::unexpected_eof(parser.template.len()))
}

fn relative_selector(combinator: Value, start: usize) -> (Value, usize, Vec<Value>) {
    (combinator, start, Vec::new())
}

fn finish_relative((combinator, start, selectors): (Value, usize, Vec<Value>), end: usize) -> Value {
    obj(vec![
        ("type", "RelativeSelector".into()),
        ("combinator", combinator),
        ("selectors", Value::Array(selectors)),
        ("start", start.into()),
        ("end", end.into()),
    ])
}

fn read_selector(parser: &mut Parser, inside_pseudo_class: bool) -> Result<Value> {
    let list_start = parser.index;
    let mut children = Vec::new();
    let mut relative = relative_selector(Value::Null, parser.index);

    while parser.index < parser.template.len() {
        let start = parser.index;

        if parser.eat("&") {
            relative.2.push(json!({ "type": "NestingSelector", "name": "&", "start": start, "end": parser.index }));
        } else if parser.eat("*") {
            let mut name = "*".to_string();
            let mut namespace = None;
            if parser.eat("|") {
                namespace = Some(name);
                name = if parser.eat("*") { "*".into() } else { read_identifier(parser)? };
            }
            relative.2.push(type_selector(name, namespace, start, parser.index));
        } else if parser.eat("#") {
            let name = read_identifier(parser)?;
            relative.2.push(json!({ "type": "IdSelector", "name": name, "start": start, "end": parser.index }));
        } else if parser.eat(".") {
            let name = read_identifier(parser)?;
            relative.2.push(json!({ "type": "ClassSelector", "name": name, "start": start, "end": parser.index }));
        } else if parser.eat("::") {
            let name = read_identifier(parser)?;
            let mut args = None;
            if parser.eat("(") {
                args = Some(read_selector_list(parser, true)?);
                parser.expect(")")?;
            }
            let mut pairs = vec![
                ("type", "PseudoElementSelector".into()),
                ("name", name.into()),
                ("start", start.into()),
                ("end", parser.index.into()),
            ];
            if let Some(args) = args {
                pairs.push(("args", args));
            }
            relative.2.push(obj(pairs));
        } else if parser.eat(":") {
            let name = read_identifier(parser)?;
            let mut args = Value::Null;
            if parser.eat("(") {
                args = read_selector_list(parser, true)?;
                parser.expect(")")?;
            }
            relative.2.push(obj(vec![
                ("type", "PseudoClassSelector".into()),
                ("name", name.into()),
                ("args", args),
                ("start", start.into()),
                ("end", parser.index.into()),
            ]));
        } else if parser.eat("[") {
            parser.allow_whitespace();
            let name = read_identifier(parser)?;
            parser.allow_whitespace();

            let mut value = Value::Null;
            let matcher = read_matcher(parser);
            if matcher.is_some() {
                parser.allow_whitespace();
                value = read_attribute_value(parser)?.into();
            }
            parser.allow_whitespace();

            let flags_start = parser.index;
            while matches!(parser.byte(parser.index), Some(b) if b.is_ascii_alphabetic()) {
                parser.index += 1;
            }
            let flags = if parser.index > flags_start {
                Value::String(parser.template[flags_start..parser.index].to_string())
            } else {
                Value::Null
            };

            parser.allow_whitespace();
            parser.expect("]")?;

            relative.2.push(obj(vec![
                ("type", "AttributeSelector".into()),
                ("start", start.into()),
                ("end", parser.index.into()),
                ("name", name.into()),
                ("matcher", matcher.map_or(Value::Null, Value::String)),
                ("value", value),
                ("flags", flags),
            ]));
        } else if inside_pseudo_class && nth_of(parser).is_some() {
            // must come before the combinator check, else the '+' in '+2n-1' would be a combinator
            let len = nth_of(parser).unwrap();
            let value = parser.template[parser.index..parser.index + len].to_string();
            parser.index += len;
            relative.2.push(json!({ "type": "Nth", "value": value, "start": start, "end": parser.index }));
        } else if let Some(m) = REGEX_PERCENTAGE.find(&parser.template[parser.index..]) {
            let value = m.as_str().to_string();
            parser.index += m.end();
            relative.2.push(json!({ "type": "Percentage", "value": value, "start": start, "end": parser.index }));
        } else if !matches_combinator(parser) {
            let mut name = read_identifier(parser)?;
            let mut namespace = None;
            if parser.eat("|") {
                namespace = Some(name);
                name = if parser.eat("*") { "*".into() } else { read_identifier(parser)? };
            }
            relative.2.push(type_selector(name, namespace, start, parser.index));
        }

        let index = parser.index;
        allow_comment_or_whitespace(parser, false)?;

        if parser.match_str(",") || (if inside_pseudo_class { parser.match_str(")") } else { parser.match_str("{") }) {
            // rewind, so we know whether to continue building the selector list
            parser.index = index;
            children.push(finish_relative(relative, index));
            return Ok(obj(vec![
                ("type", "ComplexSelector".into()),
                ("start", list_start.into()),
                ("end", index.into()),
                ("children", Value::Array(children)),
            ]));
        }

        parser.index = index;
        if let Some(combinator) = read_combinator(parser) {
            let combinator_start = combinator["start"].as_u64().unwrap() as usize;
            let prev = std::mem::replace(&mut relative, relative_selector(combinator, combinator_start));
            if !prev.2.is_empty() {
                children.push(finish_relative(prev, index));
            }

            parser.allow_whitespace();
            if parser.match_str(",") || (if inside_pseudo_class { parser.match_str(")") } else { parser.match_str("{") }) {
                return Err(e::css_selector_invalid(parser.index));
            }
        }
    }

    Err(e::unexpected_eof(parser.template.len()))
}

fn type_selector(name: String, namespace: Option<String>, start: usize, end: usize) -> Value {
    let mut pairs = vec![("type", "TypeSelector".into()), ("name", name.into())];
    if let Some(ns) = namespace {
        pairs.push(("namespace", ns.into()));
    }
    pairs.push(("start", start.into()));
    pairs.push(("end", end.into()));
    obj(pairs)
}

/// `[~^$*|]?=`
fn read_matcher(parser: &mut Parser) -> Option<String> {
    let s = parser.template.as_bytes();
    let i = parser.index;
    let len = match (s.get(i), s.get(i + 1)) {
        (Some(b'~' | b'^' | b'$' | b'*' | b'|'), Some(b'=')) => 2,
        (Some(b'='), _) => 1,
        _ => return None,
    };
    parser.index += len;
    Some(parser.template[i..i + len].to_string())
}

fn nth_of(parser: &Parser) -> Option<usize> {
    REGEX_NTH_OF.find(&parser.template[parser.index..]).ok().flatten().map(|m| m.end())
}

/// `(\+|~|>|\|\|)` at the current index
fn combinator_len(parser: &Parser) -> Option<usize> {
    let s = parser.template.as_bytes();
    match s.get(parser.index) {
        Some(b'+' | b'~' | b'>') => Some(1),
        Some(b'|') if s.get(parser.index + 1) == Some(&b'|') => Some(2),
        _ => None,
    }
}

fn matches_combinator(parser: &Parser) -> bool {
    combinator_len(parser).is_some()
}

fn read_combinator(parser: &mut Parser) -> Option<Value> {
    let start = parser.index;
    parser.allow_whitespace();

    let index = parser.index;
    if let Some(len) = combinator_len(parser) {
        let name = parser.template[index..index + len].to_string();
        parser.index += len;
        let end = parser.index;
        parser.allow_whitespace();
        return Some(json!({ "type": "Combinator", "name": name, "start": index, "end": end }));
    }

    if parser.index != start {
        return Some(json!({ "type": "Combinator", "name": " ", "start": start, "end": parser.index }));
    }

    None
}

fn read_block(parser: &mut Parser) -> Result<Value> {
    let start = parser.index;
    parser.expect("{")?;
    let mut children = Vec::new();

    while parser.index < parser.template.len() {
        allow_comment_or_whitespace(parser, true)?;
        if parser.match_str("}") {
            break;
        }
        children.push(read_block_item(parser)?);
    }

    parser.expect("}")?;
    Ok(obj(vec![
        ("type", "Block".into()),
        ("start", start.into()),
        ("end", parser.index.into()),
        ("children", Value::Array(children)),
    ]))
}

fn read_block_item(parser: &mut Parser) -> Result<Value> {
    if parser.match_str("@") {
        return read_at_rule(parser);
    }
    // read ahead to understand whether we're dealing with a declaration or a nested rule
    let start = parser.index;
    read_value(parser, false)?;
    let ch = parser.byte(parser.index);
    parser.index = start;

    if ch == Some(b'{') {
        read_rule(parser)
    } else {
        read_declaration(parser)
    }
}

fn read_declaration(parser: &mut Parser) -> Result<Value> {
    let start = parser.index;

    // read_until_regex(/[\s:]/)
    if parser.index >= parser.template.len() && !parser.loose {
        return Err(e::unexpected_eof(parser.template.len()));
    }
    let template = parser.template;
    let mut i = parser.index;
    while let Some(c) = char_at(template, i) {
        if c == ':' || c.is_whitespace() || c == '\u{feff}' {
            break;
        }
        i += c.len_utf8();
    }
    let property = &template[parser.index..i];
    parser.index = i;

    parser.allow_whitespace();
    parser.eat(":");
    let index = parser.index;
    parser.allow_whitespace();

    let value = read_value(parser, true)?;

    if value.is_empty() && !property.starts_with("--") {
        return Err(e::css_empty_declaration((start, index)));
    }

    let end = parser.index;
    if !parser.match_str("}") {
        parser.expect(";")?;
    }

    Ok(obj(vec![
        ("type", "Declaration".into()),
        ("start", start.into()),
        ("end", end.into()),
        ("property", property.into()),
        ("value", value.into()),
    ]))
}

fn read_value(parser: &mut Parser, capture_comments: bool) -> Result<String> {
    let mut value = String::new();
    // indices into parser.css_comments of comments in this value
    let mut value_comments: Vec<usize> = Vec::new();
    let mut escaped = false;
    let mut in_url = false;
    let mut quote_mark: Option<char> = None;

    let template = parser.template;
    while let Some(c) = char_at(template, parser.index) {
        if escaped {
            value.push('\\');
            value.push(c);
            escaped = false;
            parser.index += c.len_utf8();
            continue;
        } else if c == '\\' {
            escaped = true;
            parser.index += 1;
            continue;
        } else if Some(c) == quote_mark {
            quote_mark = None;
        } else if c == ')' {
            in_url = false;
        } else if quote_mark.is_none() && (c == '"' || c == '\'') {
            quote_mark = Some(c);
        } else if c == '(' && value.ends_with("url") {
            in_url = true;
        } else if (c == ';' || c == '{' || c == '}') && !in_url && quote_mark.is_none() {
            let trimmed_start = value.trim_start_matches(is_whitespace_char);
            let leading_whitespace = js_len(&value[..value.len() - trimmed_start.len()]) as i64;
            for &i in &value_comments {
                let comment = &mut parser.css_comments[i];
                let position = comment["position"].as_i64().unwrap_or(0);
                comment["position"] = (position - leading_whitespace).max(0).into();
            }
            return Ok(js_trim(&value).to_string());
        } else if c == '/' && !in_url && quote_mark.is_none() && parser.byte(parser.index + 1) == Some(b'*') {
            let mut comment = read_comment(parser)?;
            if capture_comments {
                comment["position"] = js_len(&value).into();
                parser.css_comments.push(comment);
                value_comments.push(parser.css_comments.len() - 1);
            }
            continue;
        }

        value.push(c);
        parser.index += c.len_utf8();
    }

    Err(e::unexpected_eof(parser.template.len()))
}

fn read_attribute_value(parser: &mut Parser) -> Result<String> {
    let mut value = String::new();
    let mut escaped = false;
    let quote_mark = if parser.eat("\"") {
        Some('"')
    } else if parser.eat("'") {
        Some('\'')
    } else {
        None
    };

    let template = parser.template;
    while let Some(c) = char_at(template, parser.index) {
        if escaped {
            value.push('\\');
            value.push(c);
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if match quote_mark {
            Some(q) => c == q,
            None => c == ']' || c.is_whitespace() || c == '\u{feff}',
        } {
            if let Some(q) = quote_mark {
                parser.expect(&q.to_string())?;
            }
            return Ok(js_trim(&value).to_string());
        } else {
            value.push(c);
        }
        parser.index += c.len_utf8();
    }

    Err(e::unexpected_eof(parser.template.len()))
}

fn read_identifier(parser: &mut Parser) -> Result<String> {
    let start = parser.index;
    let mut identifier = String::new();

    // /-?\d/y
    let s = parser.template.as_bytes();
    let digit_at = |i: usize| s.get(i).is_some_and(u8::is_ascii_digit);
    if digit_at(start) || (s.get(start) == Some(&b'-') && digit_at(start + 1)) {
        return Err(e::css_expected_identifier(start));
    }

    let template = parser.template;
    while let Some(c) = char_at(template, parser.index) {
        if c == '\\' {
            if let Some(m) = REGEX_UNICODE_SEQUENCE.find(&template[parser.index..]) {
                let seq = m.as_str();
                let hex: String = seq[1..].chars().take_while(char::is_ascii_hexdigit).collect();
                let code = u32::from_str_radix(&hex, 16).unwrap_or(0xFFFD);
                let character = char::from_u32(code).unwrap_or('\u{FFFD}');
                if character == '\\' {
                    identifier.push_str("\\\\");
                } else {
                    identifier.push(character);
                }
                parser.index += m.end();
            } else {
                identifier.push('\\');
                if let Some(next) = char_at(template, parser.index + 1) {
                    identifier.push(next);
                    parser.index += 1 + next.len_utf8();
                } else {
                    parser.index += 2;
                }
            }
        } else if c as u32 >= 160 || c.is_ascii_alphanumeric() || c == '_' || c == '-' {
            identifier.push(c);
            parser.index += c.len_utf8();
        } else {
            break;
        }
    }

    if identifier.is_empty() {
        return Err(e::css_expected_identifier(start));
    }

    Ok(identifier)
}

fn allow_comment_or_whitespace(parser: &mut Parser, capture_comments: bool) -> Result<()> {
    parser.allow_whitespace();
    while parser.match_str("/*") || parser.match_str("<!--") {
        if parser.match_str("/*") {
            let comment = read_comment(parser)?;
            if capture_comments {
                parser.css_comments.push(comment);
            }
        }
        if parser.eat("<!--") {
            parser.read_until("-->")?;
            parser.expect("-->")?;
        }
        parser.allow_whitespace();
    }
    Ok(())
}

fn read_comment(parser: &mut Parser) -> Result<Value> {
    let start = parser.index;
    parser.expect("/*")?;
    let value = parser.read_until("*/")?.to_string();
    parser.expect("*/")?;
    Ok(obj(vec![
        ("type", "CSSComment".into()),
        ("value", value.into()),
        ("start", start.into()),
        ("end", parser.index.into()),
    ]))
}
