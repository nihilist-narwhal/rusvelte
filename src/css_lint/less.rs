//! Port of vscode-css-languageservice's `parser/lessParser.ts` (the methods it adds; the
//! overrides of base methods dispatch from `parser.rs`).

use super::nodes::{Class, Field, NodeId, NodeType};
use super::parser::{DeclFn, ParseError, Parser, is_word};
use super::scanner::TT;

/// `/^[\w-]+/` (only the first character matters for `test`)
fn property_regex(t: &[u16]) -> bool {
    t.first().is_some_and(|&c| is_word(c) || c == b'-' as u16)
}

impl<'a> Parser<'a> {
    pub fn less_parse_stylesheet_statement(&mut self, is_nested: bool) -> Option<NodeId> {
        if self.peek(TT::AtKeyword) {
            if let Some(n) = self.less_parse_variable_declaration(Some(&[])) {
                return Some(n);
            }
            if let Some(n) = self.less_parse_plugin() {
                return Some(n);
            }
            return self.css_parse_stylesheet_at_statement(is_nested);
        }
        if let Some(n) = self.less_try_parse_mixin_declaration() {
            return Some(n);
        }
        if let Some(n) = self.less_try_parse_mixin_reference(true) {
            return Some(n);
        }
        if let Some(n) = self.parse_function() {
            return Some(n);
        }
        self.parse_ruleset(true)
    }

    pub fn less_parse_import(&mut self) -> Option<NodeId> {
        if !self.peek_keyword("@import") && !self.peek_keyword("@import-once") {
            return None;
        }
        let node = self.create(Class::Import);
        self.consume_token();
        if self.accept(TT::ParenthesisL) {
            if !self.accept(TT::Ident) {
                return Some(self.finish_resync(node, ParseError::IdentifierExpected, Some(&[TT::SemiColon]), None));
            }
            loop {
                if !self.accept(TT::Comma) {
                    break;
                }
                if !self.accept(TT::Ident) {
                    break;
                }
            }
            if !self.accept(TT::ParenthesisR) {
                return Some(self.finish_resync(node, ParseError::RightParenthesisExpected, Some(&[TT::SemiColon]), None));
            }
        }
        if !self.add_uri_or_string(node) {
            return Some(self.finish_resync(node, ParseError::URIOrStringExpected, Some(&[TT::SemiColon]), None));
        }
        if !self.peek(TT::SemiColon) && !self.peek(TT::EOF) {
            let m = self.parse_media_query_list();
            self.ast.adopt_child(node, m, -1);
        }
        Some(self.complete_parse_import(node))
    }

    fn less_parse_plugin(&mut self) -> Option<NodeId> {
        if !self.peek_keyword("@plugin") {
            return None;
        }
        let node = self.create_node(NodeType::Plugin);
        self.consume_token();
        let s = self.parse_string_literal();
        if !self.add_child(node, s) {
            return Some(self.finish_err(node, ParseError::StringLiteralExpected));
        }
        if !self.accept(TT::SemiColon) {
            return Some(self.finish_err(node, ParseError::SemiColonExpected));
        }
        Some(self.finish(node))
    }

    pub fn less_parse_media_declaration(&mut self, is_nested: bool) -> Option<NodeId> {
        if let Some(n) = self.try_parse_ruleset(is_nested) {
            return Some(n);
        }
        if let Some(n) = self.try_to_parse_declaration(None) {
            return Some(n);
        }
        if let Some(n) = self.less_try_parse_mixin_declaration() {
            return Some(n);
        }
        if let Some(n) = self.less_try_parse_mixin_reference(true) {
            return Some(n);
        }
        if let Some(n) = self.less_parse_detached_rule_set_mixin() {
            return Some(n);
        }
        self.parse_stylesheet_statement(is_nested)
    }

    fn less_parse_variable_declaration(&mut self, panic: Option<&[TT]>) -> Option<NodeId> {
        let node = self.create(Class::VariableDeclaration);
        let mark = self.mark();
        let v = self.less_parse_variable(true, false)?;
        self.ast.adopt_child(node, v, -1);
        self.ast.set_field(node, Field::Variable, v);
        if self.accept(TT::Colon) {
            if let Some(prev) = self.prev_token {
                self.ast.get_mut(node).colon_position = Some(prev.offset);
            }
            if let Some(d) = self.less_parse_detached_rule_set() {
                self.ast.adopt_child(node, d, -1);
                self.ast.set_field(node, Field::Value, d);
                self.ast.get_mut(node).needs_semicolon = false;
            } else {
                let e = self.parse_expr(false);
                let Some(e) = e else {
                    return Some(self.finish_resync(node, ParseError::VariableValueExpected, Some(&[]), panic));
                };
                self.ast.adopt_child(node, e, -1);
                self.ast.set_field(node, Field::Value, e);
            }
            let p = self.parse_prio();
            self.add_child(node, p);
        } else {
            self.restore_at_mark(mark);
            return None;
        }
        Some(self.finish(node))
    }

    fn less_parse_detached_rule_set(&mut self) -> Option<NodeId> {
        let mark = self.mark();
        if self.peek_delim("#") || self.peek_delim(".") {
            self.consume_token();
            if !self.has_whitespace() && self.accept(TT::ParenthesisL) {
                let node = self.create(Class::MixinDeclaration);
                let params = self.nodelist(node, Field::Parameters);
                let p = self.less_parse_mixin_parameter();
                if self.add_child(params, p) {
                    while self.accept(TT::Comma) || self.accept(TT::SemiColon) {
                        if self.peek(TT::ParenthesisR) {
                            break;
                        }
                        let p = self.less_parse_mixin_parameter();
                        if !self.add_child(params, p) {
                            self.mark_error(node, ParseError::IdentifierExpected, Some(&[]), Some(&[TT::ParenthesisR]));
                        }
                    }
                }
                if !self.accept(TT::ParenthesisR) {
                    self.restore_at_mark(mark);
                    return None;
                }
            } else {
                self.restore_at_mark(mark);
                return None;
            }
        }
        if !self.peek(TT::CurlyL) {
            return None;
        }
        let content = self.create(Class::BodyDeclaration);
        self.parse_body(content, DeclFn::LessDetachedRuleSetBody);
        Some(self.finish(content))
    }

    pub fn less_parse_detached_rule_set_body(&mut self) -> Option<NodeId> {
        self.try_parse_keyframe_selector().or_else(|| self.parse_rule_set_declaration())
    }

    fn less_add_lookup_children(&mut self, node: NodeId) -> bool {
        let l = self.less_parse_lookup_value();
        if !self.add_child(node, l) {
            return false;
        }
        let mut expects_value = false;
        loop {
            if self.peek(TT::BracketL) {
                expects_value = true;
            }
            let l = self.less_parse_lookup_value();
            if !self.add_child(node, l) {
                break;
            }
            expects_value = false;
        }
        !expects_value
    }

    fn less_parse_lookup_value(&mut self) -> Option<NodeId> {
        let node = self.create(Class::Node);
        let mark = self.mark();
        if !self.accept(TT::BracketL) {
            self.restore_at_mark(mark);
            return None;
        }
        let inner = {
            let v = self.less_parse_variable(false, true);
            self.add_child(node, v) || {
                let p = self.less_parse_property_identifier(false);
                self.add_child(node, p)
            }
        };
        if (inner && self.accept(TT::BracketR)) || self.accept(TT::BracketR) {
            return Some(node);
        }
        self.restore_at_mark(mark);
        None
    }

    pub fn less_parse_variable(&mut self, declaration: bool, inside_lookup: bool) -> Option<NodeId> {
        let is_property_reference = !declaration && self.peek_delim("$");
        if !self.peek_delim("@") && !is_property_reference && !self.peek(TT::AtKeyword) {
            return None;
        }
        let node = self.create(Class::Variable);
        let mark = self.mark();
        while self.accept_delim("@") || (!declaration && self.accept_delim("$")) {
            if self.has_whitespace() {
                self.restore_at_mark(mark);
                return None;
            }
        }
        if !self.accept(TT::AtKeyword) && !self.accept(TT::Ident) {
            self.restore_at_mark(mark);
            return None;
        }
        if !inside_lookup && self.peek(TT::BracketL) && !self.less_add_lookup_children(node) {
            self.restore_at_mark(mark);
            return None;
        }
        Some(node)
    }

    pub fn less_parse_escaped(&mut self) -> Option<NodeId> {
        if self.peek(TT::EscapedJavaScript) || self.peek(TT::BadEscapedJavaScript) {
            let node = self.create_node(NodeType::EscapedValue);
            self.consume_token();
            return Some(self.finish(node));
        }
        if self.peek_delim("~") {
            let node = self.create_node(NodeType::EscapedValue);
            self.consume_token();
            if self.accept(TT::String) || self.accept(TT::EscapedJavaScript) {
                return Some(self.finish(node));
            } else {
                return Some(self.finish_err(node, ParseError::TermExpected));
            }
        }
        None
    }

    pub fn less_parse_guard_operator(&mut self) -> Option<NodeId> {
        if self.peek_delim(">") {
            let node = self.create_node(NodeType::Operator);
            self.consume_token();
            self.accept_delim("=");
            return Some(node);
        } else if self.peek_delim("=") {
            let node = self.create_node(NodeType::Operator);
            self.consume_token();
            self.accept_delim("<");
            return Some(node);
        } else if self.peek_delim("<") {
            let node = self.create_node(NodeType::Operator);
            self.consume_token();
            self.accept_delim("=");
            return Some(node);
        }
        None
    }

    pub fn less_parse_rule_set_declaration(&mut self) -> Option<NodeId> {
        if self.peek(TT::AtKeyword) {
            if let Some(n) = self.parse_keyframe() {
                return Some(n);
            }
            if let Some(n) = self.parse_media(true) {
                return Some(n);
            }
            if let Some(n) = self.parse_import() {
                return Some(n);
            }
            if let Some(n) = self.parse_supports(true) {
                return Some(n);
            }
            if let Some(n) = self.parse_layer(false) {
                return Some(n);
            }
            if let Some(n) = self.parse_property_at_rule() {
                return Some(n);
            }
            if let Some(n) = self.parse_container(true) {
                return Some(n);
            }
            if let Some(n) = self.less_parse_detached_rule_set_mixin() {
                return Some(n);
            }
            if let Some(n) = self.less_parse_variable_declaration(Some(&[])) {
                return Some(n);
            }
            return self.parse_rule_set_declaration_at_statement();
        }
        if let Some(n) = self.less_try_parse_mixin_declaration() {
            return Some(n);
        }
        if let Some(n) = self.try_parse_ruleset(true) {
            return Some(n);
        }
        if let Some(n) = self.less_try_parse_mixin_reference(true) {
            return Some(n);
        }
        if let Some(n) = self.parse_function() {
            return Some(n);
        }
        if let Some(n) = self.less_parse_extend() {
            return Some(n);
        }
        self.parse_declaration(None)
    }

    pub fn less_parse_selector(&mut self, is_nested: bool) -> Option<NodeId> {
        let node = self.create(Class::Selector);
        let mut has_content = false;
        if is_nested {
            let c = self.parse_combinator();
            has_content = self.add_child(node, c);
        }
        loop {
            let s = self.parse_simple_selector();
            if !self.add_child(node, s) {
                break;
            }
            has_content = true;
            let mark = self.mark();
            let g = self.less_parse_guard();
            if self.add_child(node, g) && self.peek(TT::CurlyL) {
                break;
            }
            self.restore_at_mark(mark);
            let c = self.parse_combinator();
            self.add_child(node, c);
        }
        if has_content { Some(self.finish(node)) } else { None }
    }

    pub fn less_parse_selector_ident(&mut self) -> Option<NodeId> {
        if !self.less_peek_interpolated_ident() {
            return None;
        }
        let node = self.create_node(NodeType::SelectorInterpolation);
        let has_content = self.less_accept_interpolated_ident(node, None);
        if has_content { Some(self.finish(node)) } else { None }
    }

    pub fn less_parse_property_identifier(&mut self, in_lookup: bool) -> Option<NodeId> {
        if !self.less_peek_interpolated_ident() && !property_regex(self.text()) {
            return None;
        }
        let mark = self.mark();
        let node = self.create(Class::Identifier);
        let is_custom_property = self.accept_delim("-") && self.accept_delim("-");
        let child_added = if !in_lookup {
            if is_custom_property {
                self.less_accept_interpolated_ident(node, None)
            } else {
                self.less_accept_interpolated_ident(node, Some(property_regex))
            }
        } else if is_custom_property {
            let i = self.parse_ident();
            self.add_child(node, i)
        } else {
            let r = self.parse_regexp(property_regex);
            self.add_child(node, Some(r))
        };
        if !child_added {
            self.restore_at_mark(mark);
            return None;
        }
        if !in_lookup && !self.has_whitespace() {
            self.accept_delim("+");
            if !self.has_whitespace() {
                self.accept_ident("_");
            }
        }
        Some(self.finish(node))
    }

    fn less_peek_interpolated_ident(&self) -> bool {
        self.peek(TT::Ident) || self.peek_delim("@") || self.peek_delim("$") || self.peek_delim("-")
    }

    fn less_accept_interpolated_ident(&mut self, node: NodeId, ident_regex: Option<fn(&[u16]) -> bool>) -> bool {
        let mut has_content = false;
        loop {
            let accepted = match ident_regex {
                Some(r) => self.accept_regexp(r),
                None => self.accept(TT::Ident),
            };
            let ok = accepted || {
                let i = match self.less_parse_interpolation() {
                    Some(i) => Some(i),
                    None => self.less_try_ident_interpolation(),
                };
                self.add_child(node, i)
            };
            if !ok {
                break;
            }
            has_content = true;
            if self.has_whitespace() {
                break;
            }
        }
        has_content
    }

    /// `this.try(indentInterpolation)` in `_acceptInterpolatedIdent`
    fn less_try_ident_interpolation(&mut self) -> Option<NodeId> {
        let outer = self.mark();
        let result = (|| {
            let pos = self.mark();
            if self.accept_delim("-") {
                if !self.has_whitespace() {
                    self.accept_delim("-");
                }
                if self.has_whitespace() {
                    self.restore_at_mark(pos);
                    return None;
                }
            }
            self.less_parse_interpolation()
        })();
        if result.is_none() {
            self.restore_at_mark(outer);
        }
        result
    }

    fn less_parse_interpolation(&mut self) -> Option<NodeId> {
        let mark = self.mark();
        if self.peek_delim("@") || self.peek_delim("$") {
            let node = self.create_node(NodeType::Interpolation);
            self.consume_token();
            if self.has_whitespace() || !self.accept(TT::CurlyL) {
                self.restore_at_mark(mark);
                return None;
            }
            let i = self.parse_ident();
            if !self.add_child(node, i) {
                return Some(self.finish_err(node, ParseError::IdentifierExpected));
            }
            if !self.accept(TT::CurlyR) {
                return Some(self.finish_err(node, ParseError::RightCurlyExpected));
            }
            return Some(self.finish(node));
        }
        None
    }

    fn less_try_parse_mixin_declaration(&mut self) -> Option<NodeId> {
        let mark = self.mark();
        let node = self.create(Class::MixinDeclaration);
        let id = self.less_parse_mixin_declaration_identifier();
        if !self.set_node_at(node, Field::Identifier, id, 0) || !self.accept(TT::ParenthesisL) {
            self.restore_at_mark(mark);
            return None;
        }
        let params = self.nodelist(node, Field::Parameters);
        let p = self.less_parse_mixin_parameter();
        if self.add_child(params, p) {
            while self.accept(TT::Comma) || self.accept(TT::SemiColon) {
                if self.peek(TT::ParenthesisR) {
                    break;
                }
                let p = self.less_parse_mixin_parameter();
                if !self.add_child(params, p) {
                    self.mark_error(node, ParseError::IdentifierExpected, Some(&[]), Some(&[TT::ParenthesisR]));
                }
            }
        }
        if !self.accept(TT::ParenthesisR) {
            self.restore_at_mark(mark);
            return None;
        }
        if let Some(g) = self.less_parse_guard() {
            self.ast.adopt_child(node, g, -1);
            self.ast.set_field(node, Field::Guard, g);
        }
        if !self.peek(TT::CurlyL) {
            self.restore_at_mark(mark);
            return None;
        }
        Some(self.parse_body(node, DeclFn::LessMixInBodyDeclaration))
    }

    pub fn less_parse_mixin_body_declaration(&mut self) -> Option<NodeId> {
        self.parse_font_face().or_else(|| self.parse_rule_set_declaration())
    }

    fn less_parse_mixin_declaration_identifier(&mut self) -> Option<NodeId> {
        let identifier;
        if self.peek_delim("#") || self.peek_delim(".") {
            identifier = self.create(Class::Identifier);
            self.consume_token();
            if self.has_whitespace() {
                return None;
            }
            let i = self.parse_ident();
            if !self.add_child(identifier, i) {
                return None;
            }
        } else if self.peek(TT::Hash) {
            identifier = self.create(Class::Identifier);
            self.consume_token();
        } else {
            return None;
        }
        Some(self.finish(identifier))
    }

    pub fn less_parse_pseudo(&mut self) -> Option<NodeId> {
        if !self.peek(TT::Colon) {
            return None;
        }
        let mark = self.mark();
        let node = self.create(Class::ExtendsReference);
        self.consume_token();
        if self.accept_ident("extend") {
            return Some(self.less_complete_extends(node));
        }
        self.restore_at_mark(mark);
        self.css_parse_pseudo()
    }

    fn less_parse_extend(&mut self) -> Option<NodeId> {
        if !self.peek_delim("&") {
            return None;
        }
        let mark = self.mark();
        let node = self.create(Class::ExtendsReference);
        self.consume_token();
        if self.has_whitespace() || !self.accept(TT::Colon) || !self.accept_ident("extend") {
            self.restore_at_mark(mark);
            return None;
        }
        Some(self.less_complete_extends(node))
    }

    fn less_complete_extends(&mut self, node: NodeId) -> NodeId {
        if !self.accept(TT::ParenthesisL) {
            return self.finish_err(node, ParseError::LeftParenthesisExpected);
        }
        let selectors = self.nodelist(node, Field::Selectors);
        let s = self.parse_selector(true);
        if !self.add_child(selectors, s) {
            return self.finish_err(node, ParseError::SelectorExpected);
        }
        while self.accept(TT::Comma) {
            let s = self.parse_selector(true);
            if !self.add_child(selectors, s) {
                return self.finish_err(node, ParseError::SelectorExpected);
            }
        }
        if !self.accept(TT::ParenthesisR) {
            return self.finish_err(node, ParseError::RightParenthesisExpected);
        }
        self.finish(node)
    }

    pub fn less_parse_detached_rule_set_mixin(&mut self) -> Option<NodeId> {
        if !self.peek(TT::AtKeyword) {
            return None;
        }
        let mark = self.mark();
        let node = self.create(Class::MixinReference);
        let v = self.less_parse_variable(true, false);
        if self.add_child(node, v) && (self.has_whitespace() || !self.accept(TT::ParenthesisL)) {
            self.restore_at_mark(mark);
            return None;
        }
        if !self.accept(TT::ParenthesisR) {
            return Some(self.finish_err(node, ParseError::RightParenthesisExpected));
        }
        Some(self.finish(node))
    }

    pub fn less_try_parse_mixin_reference(&mut self, at_root: bool) -> Option<NodeId> {
        let mark = self.mark();
        let node = self.create(Class::MixinReference);
        let mut identifier = self.less_parse_mixin_declaration_identifier();
        while let Some(id) = identifier {
            self.accept_delim(">");
            let next_id = self.less_parse_mixin_declaration_identifier();
            if next_id.is_some() {
                let ns = self.nodelist(node, Field::Namespaces);
                self.add_child(ns, Some(id));
                identifier = next_id;
            } else {
                break;
            }
        }
        if !self.set_node_at(node, Field::Identifier, identifier, 0) {
            self.restore_at_mark(mark);
            return None;
        }
        let mut has_arguments = false;
        if self.accept(TT::ParenthesisL) {
            has_arguments = true;
            let args = self.nodelist(node, Field::Arguments);
            let a = self.less_parse_mixin_argument();
            if self.add_child(args, a) {
                while self.accept(TT::Comma) || self.accept(TT::SemiColon) {
                    if self.peek(TT::ParenthesisR) {
                        break;
                    }
                    let a = self.less_parse_mixin_argument();
                    if !self.add_child(args, a) {
                        return Some(self.finish_err(node, ParseError::ExpressionExpected));
                    }
                }
            }
            if !self.accept(TT::ParenthesisR) {
                return Some(self.finish_err(node, ParseError::RightParenthesisExpected));
            }
        }
        if self.peek(TT::BracketL) {
            if !at_root {
                self.less_add_lookup_children(node);
            }
        } else {
            let p = self.parse_prio();
            self.add_child(node, p);
        }
        if !has_arguments && !self.peek(TT::SemiColon) && !self.peek(TT::CurlyR) && !self.peek(TT::EOF) {
            self.restore_at_mark(mark);
            return None;
        }
        Some(self.finish(node))
    }

    fn less_parse_mixin_argument(&mut self) -> Option<NodeId> {
        let node = self.create(Class::FunctionArgument);
        let pos = self.mark();
        let argument = self.less_parse_variable(false, false);
        if let Some(argument) = argument {
            if !self.accept(TT::Colon) {
                self.restore_at_mark(pos);
            } else {
                self.set_node_at(node, Field::Identifier, Some(argument), 0);
            }
        }
        let v = match self.less_parse_detached_rule_set() {
            Some(d) => Some(d),
            None => self.parse_expr(true),
        };
        if self.set_node_at(node, Field::Value, v, 0) {
            return Some(self.finish(node));
        }
        self.restore_at_mark(pos);
        None
    }

    fn less_parse_mixin_parameter(&mut self) -> Option<NodeId> {
        let node = self.create(Class::FunctionParameter);
        if self.peek_keyword("@rest") {
            let rest_node = self.create(Class::Node);
            self.consume_token();
            if !self.accept(TT::Ellipsis) {
                return Some(self.finish_resync(
                    node,
                    ParseError::DotExpected,
                    Some(&[]),
                    Some(&[TT::Comma, TT::ParenthesisR]),
                ));
            }
            let r = self.finish(rest_node);
            self.set_node_at(node, Field::Identifier, Some(r), 0);
            return Some(self.finish(node));
        }
        if self.peek(TT::Ellipsis) {
            let varargs = self.create(Class::Node);
            self.consume_token();
            let v = self.finish(varargs);
            self.set_node_at(node, Field::Identifier, Some(v), 0);
            return Some(self.finish(node));
        }
        let mut has_content = false;
        let v = self.less_parse_variable(false, false);
        if self.set_node_at(node, Field::Identifier, v, 0) {
            self.accept(TT::Colon);
            has_content = true;
        }
        let d = match self.less_parse_detached_rule_set() {
            Some(d) => Some(d),
            None => self.parse_expr(true),
        };
        if !self.set_node_at(node, Field::DefaultValue, d, 0) && !has_content {
            return None;
        }
        Some(self.finish(node))
    }

    fn less_parse_guard(&mut self) -> Option<NodeId> {
        if !self.peek_ident("when") {
            return None;
        }
        let node = self.create(Class::LessGuard);
        self.consume_token();
        let conditions = self.nodelist(node, Field::Conditions);
        let c = self.less_parse_guard_condition();
        if !self.add_child(conditions, c) {
            return Some(self.finish_err(node, ParseError::ConditionExpected));
        }
        while self.accept_ident("and") || self.accept(TT::Comma) {
            let c = self.less_parse_guard_condition();
            if !self.add_child(conditions, c) {
                return Some(self.finish_err(node, ParseError::ConditionExpected));
            }
        }
        Some(self.finish(node))
    }

    fn less_parse_guard_condition(&mut self) -> Option<NodeId> {
        let node = self.create(Class::GuardCondition);
        let is_negated = self.accept_ident("not");
        if !self.accept(TT::ParenthesisL) {
            if is_negated {
                return Some(self.finish_err(node, ParseError::LeftParenthesisExpected));
            }
            return None;
        }
        let e = self.parse_expr(false);
        self.add_child(node, e);
        if !self.accept(TT::ParenthesisR) {
            return Some(self.finish_err(node, ParseError::RightParenthesisExpected));
        }
        Some(self.finish(node))
    }

    pub fn less_parse_function(&mut self) -> Option<NodeId> {
        let pos = self.mark();
        let node = self.create(Class::Function);
        let id = self.parse_function_identifier();
        if !self.set_node_at(node, Field::Identifier, id, 0) {
            return None;
        }
        if self.has_whitespace() || !self.accept(TT::ParenthesisL) {
            self.restore_at_mark(pos);
            return None;
        }
        let args = self.nodelist(node, Field::Arguments);
        let a = self.less_parse_mixin_argument();
        if self.add_child(args, a) {
            while self.accept(TT::Comma) || self.accept(TT::SemiColon) {
                if self.peek(TT::ParenthesisR) {
                    break;
                }
                let a = self.less_parse_mixin_argument();
                if !self.add_child(args, a) {
                    return Some(self.finish_err(node, ParseError::ExpressionExpected));
                }
            }
        }
        if !self.accept(TT::ParenthesisR) {
            return Some(self.finish_err(node, ParseError::RightParenthesisExpected));
        }
        Some(self.finish(node))
    }
}
