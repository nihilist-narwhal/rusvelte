//! Port of `phases/1-parse/state/tag.js`.

use serde_json::{json, Value};

use super::utils::*;
use super::{node_end, node_start, node_type, Open, Parser};
use crate::ast::Node;
use crate::error::Result;
use crate::errors as e;
use crate::js::remove_parens;

fn is_word_boundary_after(template: &str, i: usize) -> bool {
    // `\b` after a word char: the next char is not a word char
    !matches!(template.as_bytes().get(i), Some(b) if b.is_ascii_alphanumeric() || *b == b'_')
}

pub fn tag(parser: &mut Parser) -> Result<()> {
    let start = parser.index;
    parser.index += 1;
    parser.allow_whitespace();

    if parser.eat("#") {
        return open(parser);
    }
    if parser.eat(":") {
        return next(parser);
    }
    if parser.eat("@") {
        return special(parser);
    }
    if parser.match_str("/") && !parser.match_str("/*") && !parser.match_str("//") {
        parser.eat("/");
        return close(parser);
    }

    if let Some(declaration) = read_declaration(parser)? {
        parser.append(Node::DeclarationTag { start, end: parser.index, declaration });
        return Ok(());
    }

    let expression = parser.read_expression()?;
    parser.allow_whitespace();
    parser.expect("}")?;
    parser.append(Node::ExpressionTag { start, end: parser.index, expression });
    Ok(())
}

fn read_declaration(parser: &mut Parser) -> Result<Option<Value>> {
    let start = parser.index;
    let t = parser.template;

    for kw in ["var", "interface", "enum"] {
        if t[start..].starts_with(kw) && is_word_boundary_after(t, start + kw.len()) {
            return Err(e::declaration_tag_invalid_type((start, start + kw.len())));
        }
    }

    let supported = ["let", "const"].iter().any(|kw| t[start..].starts_with(kw) && is_word_boundary_after(t, start + kw.len()));
    let maybe_type = t[start..].starts_with("type") && is_word_boundary_after(t, start + 4);
    if !supported && !maybe_type {
        return Ok(None);
    }

    let initial_comment_count = parser.root.comments.len();

    let declaration = match parser.js.parse_statement_at(t, start, &mut parser.root.comments) {
        Ok(d) => d,
        Err(error) => {
            if !parser.loose {
                return Err(error);
            }
            let Some(end) = find_matching_bracket(t, start, b'{') else {
                return Err(error);
            };
            parser.index = end;
            let kind = if t[start..].starts_with("const") { "const" } else { "let" };
            json!({
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
            })
        }
    };

    if node_type(&declaration) != "VariableDeclaration" {
        if node_type(&declaration) == "ExpressionStatement" {
            parser.root.comments.truncate(initial_comment_count);
            return Ok(None);
        }
        // a TSTypeAliasDeclaration
        let s = declaration.get("start").and_then(Value::as_u64).map_or(start, |v| v as usize);
        let e_ = declaration.get("end").and_then(Value::as_u64).map_or(parser.index, |v| v as usize);
        return Err(e::declaration_tag_invalid_type((s, e_)));
    }

    let kind = declaration["kind"].as_str().unwrap_or("");
    if kind != "let" && kind != "const" {
        return Err(e::declaration_tag_invalid_type((node_start(&declaration), node_end(&declaration))));
    }

    parser.index = node_end(&declaration);
    parser.allow_whitespace();
    parser.expect("}")?;
    Ok(Some(declaration))
}

fn open(parser: &mut Parser) -> Result<()> {
    let mut start = parser.index - 2;
    while parser.byte(start) != Some(b'{') {
        start -= 1;
    }

    if parser.eat("if") {
        parser.require_whitespace()?;
        let test = parser.read_expression()?;
        let consequent = parser.ast.new_fragment(false);
        let id = parser.append(Node::IfBlock { start, end: None, elseif: false, test, consequent, alternate: None });
        parser.allow_whitespace();
        parser.expect("}")?;
        parser.stack.push(Open::Node(id));
        parser.push_fragment(consequent);
        return Ok(());
    }

    if parser.eat("each") {
        return open_each(parser, start);
    }

    if parser.eat("await") {
        parser.require_whitespace()?;
        let expression = parser.read_expression()?;
        parser.allow_whitespace();

        let mut value = None;
        let mut error = None;
        let (mut pending, mut then, mut catch) = (None, None, None);

        if parser.eat("then") {
            if matches_ws_closing_brace(parser) {
                parser.allow_whitespace();
            } else {
                parser.require_whitespace()?;
                value = Some(parser.read_pattern()?);
                parser.allow_whitespace();
            }
            then = Some(parser.ast.new_fragment(false));
        } else if parser.eat("catch") {
            if matches_ws_closing_brace(parser) {
                parser.allow_whitespace();
            } else {
                parser.require_whitespace()?;
                error = Some(parser.read_pattern()?);
                parser.allow_whitespace();
            }
            catch = Some(parser.ast.new_fragment(false));
        } else {
            pending = Some(parser.ast.new_fragment(false));
        }
        let pushed = then.or(catch).or(pending).unwrap();

        let id = parser.append(Node::AwaitBlock { start, end: None, expression, value, error, pending, then, catch });
        parser.push_fragment(pushed);

        let matches = parser.eat_req("}", true, false)?;

        // Parser may have read the `then/catch` as part of the expression (e.g. in `{#await foo. then x}`)
        if !matches {
            let i = parser.index;
            if i >= 6 && parser.template.get(i - 6..i) == Some(" then ") {
                let pattern = parser.read_pattern()?;
                parser.expect("}")?;
                if let Node::AwaitBlock { expression, value, then, pending, .. } = &mut parser.ast.nodes[id] {
                    let expr_start = node_start(expression);
                    *expression = json!({ "type": "Identifier", "name": "", "start": expr_start, "end": i - 6 });
                    *value = Some(pattern);
                    *then = pending.take();
                }
            } else if i >= 7 && parser.template.get(i - 7..i) == Some(" catch ") {
                let pattern = parser.read_pattern()?;
                parser.expect("}")?;
                if let Node::AwaitBlock { expression, error, catch, pending, .. } = &mut parser.ast.nodes[id] {
                    let expr_start = node_start(expression);
                    *expression = json!({ "type": "Identifier", "name": "", "start": expr_start, "end": i - 7 });
                    *error = Some(pattern);
                    *catch = pending.take();
                }
            } else {
                parser.expect("}")?;
            }
        }

        parser.stack.push(Open::Node(id));
        return Ok(());
    }

    if parser.eat("key") {
        parser.require_whitespace()?;
        let expression = parser.read_expression()?;
        parser.allow_whitespace();
        parser.expect("}")?;
        let fragment = parser.ast.new_fragment(false);
        let id = parser.append(Node::KeyBlock { start, end: None, expression, fragment });
        parser.stack.push(Open::Node(id));
        parser.push_fragment(fragment);
        return Ok(());
    }

    if parser.eat("snippet") {
        parser.require_whitespace()?;
        let id = parser.read_identifier()?;
        if id["name"] == "" && !parser.loose {
            return Err(e::expected_identifier(parser.index));
        }
        parser.allow_whitespace();

        let params_start = parser.index;
        let mut type_params = None;
        if parser.ts && parser.match_str("<") {
            let s = parser.index;
            let end = match_bracket(parser.template, s, &[(b'<', b'>')])?;
            type_params = Some(parser.template[s + 1..end - 1].to_string());
            parser.index = end;
        }

        parser.allow_whitespace();
        let matched = parser.eat_req("(", true, false)?;

        if matched {
            let mut parentheses = 1;
            while parser.index < parser.template.len() && (!parser.match_str(")") || parentheses != 1) {
                if parser.match_str("(") {
                    parentheses += 1;
                }
                if parser.match_str(")") {
                    parentheses -= 1;
                }
                parser.index += char_at(parser.template, parser.index).map_or(1, char::len_utf8);
            }
            parser.expect(")")?;
        }

        let parameters = if matched {
            let source = format!("{} => {{}}", &parser.template[..parser.index]);
            let function = parser.parse_expression_at(&source, params_start)?;
            match function.get("params") {
                Some(Value::Array(params)) => params.clone(),
                _ => Vec::new(),
            }
        } else {
            Vec::new()
        };

        parser.allow_whitespace();
        parser.expect("}")?;

        let body = parser.ast.new_fragment(false);
        let id = parser.append(Node::SnippetBlock { start, end: None, expression: id, type_params, parameters, body });
        parser.stack.push(Open::Node(id));
        parser.push_fragment(body);
        return Ok(());
    }

    Err(e::expected_block_type(parser.index))
}

fn matches_ws_closing_brace(parser: &Parser) -> bool {
    let mut i = parser.index;
    while let Some(c) = char_at(parser.template, i) {
        if !is_whitespace_char(c) {
            break;
        }
        i += c.len_utf8();
    }
    parser.byte(i) == Some(b'}')
}

/// Remove a trailing `as T` that the TS parser read into the each expression
fn strip_trailing_as(node: &mut Value, target_end: usize, assertion: &mut Option<Value>) {
    if node_type(node) == "TSAsExpression" && node_end(node) == target_end {
        let inner = node["expression"].clone();
        *assertion = Some(std::mem::replace(node, inner));
        return;
    }
    match node {
        Value::Object(map) => {
            for (k, v) in map.iter_mut() {
                if k == "loc" {
                    continue;
                }
                match v {
                    Value::Object(_) if v.get("type").is_some_and(Value::is_string) => {
                        strip_trailing_as(v, target_end, assertion)
                    }
                    Value::Array(items) => {
                        for item in items {
                            if item.get("type").is_some_and(Value::is_string) {
                                strip_trailing_as(item, target_end, assertion);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
}

fn open_each(parser: &mut Parser, start: usize) -> Result<()> {
    parser.require_whitespace()?;

    let template = parser.template;

    // `{#each x as { y = z }}` fails to parse: the `as { y = z }` is read as part of the
    // expression. Backtrack and hide everything from the `as` onwards until it parses.
    let mut expression = loop {
        match parser.read_expression_with(b'{', true) {
            Ok(x) => break x,
            Err(err) => {
                let pos = err.position.map_or(0, |p| p.0);
                let mut end = pos.saturating_sub(2);
                while end > start && template.get(end..end + 2) != Some("as") {
                    end -= 1;
                }
                if end <= start {
                    if parser.loose {
                        if let Some(x) = parser.get_loose_identifier(b'{') {
                            break x;
                        }
                    }
                    parser.template = template;
                    return Err(err);
                }
                parser.template = &template[..end];
            }
        }
    };
    parser.template = template;

    parser.allow_whitespace();

    // {#each} blocks must declare a context – {#each list as item}
    if !parser.match_str("as") {
        // this could be a TypeScript assertion that was erroneously eaten.
        if node_type(&expression) == "SequenceExpression" {
            expression = expression["expressions"][0].take();
        }

        let mut assertion = None;
        let expression_end = node_end(&expression);
        let mut end = expression_end;
        strip_trailing_as(&mut expression, expression_end, &mut assertion);
        if let Some(a) = &assertion {
            end = node_end(&a["expression"]);
        }
        expression["end"] = end.into();

        if let Some(a) = &assertion {
            let mut end = node_start(&a["typeAnnotation"]).saturating_sub(2);
            while parser.template.get(end..end + 2) != Some("as") {
                end -= 1;
            }
            parser.index = end;
        }
    }

    let mut context = None;
    let mut index = None;
    let mut key = None;

    if parser.eat("as") {
        parser.require_whitespace()?;
        context = Some(parser.read_pattern()?);
    } else {
        // {#each Array.from({ length: 10 }), i} was read as a sequence expression
        parser.index = node_end(&expression);
    }

    parser.allow_whitespace();

    if parser.eat(",") {
        parser.allow_whitespace();
        let id = parser.read_identifier()?;
        let name = id["name"].as_str().unwrap_or("").to_string();
        if name.is_empty() {
            return Err(e::expected_identifier(parser.index));
        }
        index = Some(name);
        parser.allow_whitespace();
    }

    if parser.eat("(") {
        parser.allow_whitespace();
        key = Some(parser.read_expression_with(b'(', false)?);
        parser.allow_whitespace();
        parser.expect(")")?;
        parser.allow_whitespace();
    }

    let matches = parser.eat_req("}", true, false)?;
    if !matches {
        // Parser may have read the `as` as part of the expression (e.g. in `{#each foo. as x}`)
        let i = parser.index;
        if i >= 4 && parser.template.get(i - 4..i) == Some(" as ") {
            context = Some(parser.read_pattern()?);
            parser.expect("}")?;
            let expr_start = node_start(&expression);
            expression = json!({ "type": "Identifier", "name": "", "start": expr_start, "end": i - 4 });
        } else {
            parser.expect("}")?;
        }
    }

    let body = parser.ast.new_fragment(false);
    let id = parser.append(Node::EachBlock { start, end: None, expression, context, body, fallback: None, index, key });
    parser.stack.push(Open::Node(id));
    parser.push_fragment(body);
    Ok(())
}

fn next(parser: &mut Parser) -> Result<()> {
    let start = parser.index - 1;
    let Open::Node(block_id) = parser.current() else {
        return Err(e::block_invalid_continuation_placement(start));
    };

    match parser.ast.nodes[block_id].type_name() {
        "IfBlock" => {
            if !parser.eat("else") {
                return Err(e::expected_token(start, "{:else} or {:else if}"));
            }
            if parser.eat("if") {
                return Err(e::block_invalid_elseif(start));
            }
            if let Node::IfBlock { alternate: Some(_), .. } = parser.ast.nodes[block_id] {
                return Err(e::block_duplicate_clause(start, "{:else}"));
            }

            parser.allow_whitespace();
            parser.fragments.pop();
            let alternate = parser.ast.new_fragment(false);
            if let Node::IfBlock { alternate: a, .. } = &mut parser.ast.nodes[block_id] {
                *a = Some(alternate);
            }
            parser.push_fragment(alternate);

            // :else if
            if parser.eat("if") {
                parser.require_whitespace()?;
                let test = parser.read_expression()?;
                parser.allow_whitespace();
                parser.expect("}")?;

                let mut elseif_start = start - 1;
                while parser.byte(elseif_start) != Some(b'{') {
                    elseif_start -= 1;
                }

                let consequent = parser.ast.new_fragment(false);
                let child = parser.append(Node::IfBlock {
                    start: elseif_start,
                    end: None,
                    elseif: true,
                    test,
                    consequent,
                    alternate: None,
                });
                parser.stack.push(Open::Node(child));
                parser.fragments.pop();
                parser.push_fragment(consequent);
            } else {
                parser.allow_whitespace();
                parser.expect("}")?;
            }
            Ok(())
        }
        "EachBlock" => {
            if !parser.eat("else") {
                return Err(e::expected_token(start, "{:else}"));
            }
            if let Node::EachBlock { fallback: Some(_), .. } = parser.ast.nodes[block_id] {
                return Err(e::block_duplicate_clause(start, "{:else}"));
            }
            parser.allow_whitespace();
            parser.expect("}")?;
            let fallback = parser.ast.new_fragment(false);
            if let Node::EachBlock { fallback: f, .. } = &mut parser.ast.nodes[block_id] {
                *f = Some(fallback);
            }
            parser.fragments.pop();
            parser.push_fragment(fallback);
            Ok(())
        }
        "AwaitBlock" => {
            let is_then = parser.eat("then");
            if is_then || parser.eat("catch") {
                let Node::AwaitBlock { then, catch, .. } = &parser.ast.nodes[block_id] else { unreachable!() };
                if is_then && then.is_some() {
                    return Err(e::block_duplicate_clause(start, "{:then}"));
                }
                if !is_then && catch.is_some() {
                    return Err(e::block_duplicate_clause(start, "{:catch}"));
                }

                let mut pattern = None;
                if !parser.eat("}") {
                    parser.require_whitespace()?;
                    pattern = Some(parser.read_pattern()?);
                    parser.allow_whitespace();
                    parser.expect("}")?;
                }

                let fragment = parser.ast.new_fragment(false);
                if let Node::AwaitBlock { value, error, then, catch, .. } = &mut parser.ast.nodes[block_id] {
                    if is_then {
                        if pattern.is_some() {
                            *value = pattern;
                        }
                        *then = Some(fragment);
                    } else {
                        if pattern.is_some() {
                            *error = pattern;
                        }
                        *catch = Some(fragment);
                    }
                }
                parser.fragments.pop();
                parser.push_fragment(fragment);
                return Ok(());
            }
            Err(e::expected_token(start, "{:then ...} or {:catch ...}"))
        }
        _ => Err(e::block_invalid_continuation_placement(start)),
    }
}

fn close(parser: &mut Parser) -> Result<()> {
    let start = parser.index - 1;
    let open = parser.current();
    let Open::Node(mut block_id) = open else {
        return Err(e::block_unexpected_close(start));
    };

    let matched = match parser.ast.nodes[block_id].type_name() {
        "IfBlock" => {
            let matched = parser.eat_req("if", true, false)?;
            if !matched {
                parser.ast.nodes[block_id].set_end(start - 1);
                parser.pop();
                return close(parser);
            }
            parser.allow_whitespace();
            parser.expect("}")?;
            while matches!(parser.ast.nodes[block_id], Node::IfBlock { elseif: true, .. }) {
                parser.ast.nodes[block_id].set_end(parser.index);
                parser.stack.pop();
                let Open::Node(id) = parser.current() else { unreachable!() };
                block_id = id;
            }
            parser.ast.nodes[block_id].set_end(parser.index);
            parser.pop();
            return Ok(());
        }
        "EachBlock" => parser.eat_req("each", true, false)?,
        "KeyBlock" => parser.eat_req("key", true, false)?,
        "AwaitBlock" => parser.eat_req("await", true, false)?,
        "SnippetBlock" => parser.eat_req("snippet", true, false)?,
        "RegularElement" => {
            if parser.loose {
                false
            } else {
                return Err(e::block_unexpected_close(start));
            }
        }
        _ => return Err(e::block_unexpected_close(start)),
    };

    if !matched {
        parser.ast.nodes[block_id].set_end(start - 1);
        parser.pop();
        return close(parser);
    }

    parser.allow_whitespace();
    parser.expect("}")?;
    parser.ast.nodes[block_id].set_end(parser.index);
    parser.pop();
    Ok(())
}

fn special(parser: &mut Parser) -> Result<()> {
    let mut start = parser.index;
    while parser.byte(start) != Some(b'{') {
        start -= 1;
    }

    if parser.eat("html") {
        parser.require_whitespace()?;
        let expression = parser.read_expression()?;
        parser.allow_whitespace();
        parser.expect("}")?;
        parser.append(Node::HtmlTag { start, end: parser.index, expression });
        return Ok(());
    }

    if parser.eat("debug") {
        let identifiers;
        // `{@debug}` means "debug all"
        let save = parser.index;
        parser.allow_whitespace();
        if parser.eat("}") {
            identifiers = Vec::new();
        } else {
            parser.index = save;
            let expression = parser.read_expression()?;
            identifiers = if node_type(&expression) == "SequenceExpression" {
                expression["expressions"].as_array().cloned().unwrap_or_default()
            } else {
                vec![expression]
            };
            for node in &identifiers {
                if node_type(node) != "Identifier" {
                    return Err(e::debug_tag_invalid_arguments(node_start(node)));
                }
            }
            parser.allow_whitespace();
            parser.expect("}")?;
        }
        parser.append(Node::DebugTag { start, end: parser.index, identifiers });
        return Ok(());
    }

    if parser.eat("const") {
        parser.require_whitespace()?;
        let id = parser.read_pattern()?;
        parser.allow_whitespace();
        parser.expect("=")?;
        parser.allow_whitespace();

        let expression_start = parser.index;
        let init = parser.read_expression()?;
        // parser is past wrapping parens, but `init.end` is not — use the parser position
        let declarator_end = parser.index;
        if node_type(&init) == "SequenceExpression"
            && !parser.template[expression_start..node_start(&init).max(expression_start)].contains('(')
        {
            return Err(e::const_tag_invalid_expression((node_start(&init), node_end(&init))));
        }
        parser.allow_whitespace();
        parser.expect("}")?;

        let id_start = node_start(&id);
        let declaration = json!({
            "type": "VariableDeclaration",
            "kind": "const",
            "declarations": [{ "type": "VariableDeclarator", "id": id, "init": init, "start": id_start, "end": declarator_end }],
            "start": start + 2,
            "end": parser.index - 1
        });
        parser.append(Node::ConstTag { start, end: parser.index, declaration });
        return Ok(());
    }

    if parser.eat("render") {
        parser.require_whitespace()?;
        let expression = parser.read_expression()?;
        let ty = node_type(&expression);
        let is_call = ty == "CallExpression"
            || (ty == "ChainExpression" && node_type(&expression["expression"]) == "CallExpression");
        if !is_call {
            return Err(e::render_tag_invalid_expression((node_start(&expression), node_end(&expression))));
        }
        parser.allow_whitespace();
        parser.expect("}")?;
        parser.append(Node::RenderTag { start, end: parser.index, expression });
        return Ok(());
    }

    let _ = remove_parens;
    Err(e::expected_tag(parser.index))
}
