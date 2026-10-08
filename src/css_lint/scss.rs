//! Port of vscode-css-languageservice's `parser/scssParser.ts` (the methods it adds; the
//! overrides of base methods dispatch from `parser.rs`).

use super::nodes::{Class, Field, NodeId, NodeType};
use super::parser::{DeclFn, ParseError, Parser, eq_str, is_word};
use super::scanner::TT;

/// `/^[\w-]/`
fn word_or_dash_start(t: &[u16]) -> bool {
    t.first().is_some_and(|&c| is_word(c) || c == b'-' as u16)
}

/// `/as|with/` (unanchored)
fn contains_as_or_with(t: &[u16]) -> bool {
    let has = |needle: &[u8]| t.windows(needle.len()).any(|w| w.iter().zip(needle).all(|(&a, &b)| a == b as u16));
    has(b"as") || has(b"with")
}

impl<'a> Parser<'a> {
    pub fn scss_parse_stylesheet_statement(&mut self, is_nested: bool) -> Option<NodeId> {
        if self.peek(TT::AtKeyword) {
            if let Some(n) = self.scss_parse_warn_and_debug() {
                return Some(n);
            }
            if let Some(n) = self.scss_parse_control_statement(DeclFn::RuleSetDeclaration) {
                return Some(n);
            }
            if let Some(n) = self.scss_parse_mixin_declaration() {
                return Some(n);
            }
            if let Some(n) = self.scss_parse_mixin_content() {
                return Some(n);
            }
            if let Some(n) = self.scss_parse_mixin_reference() {
                return Some(n);
            }
            if let Some(n) = self.scss_parse_function_declaration() {
                return Some(n);
            }
            if let Some(n) = self.scss_parse_forward() {
                return Some(n);
            }
            if let Some(n) = self.scss_parse_use() {
                return Some(n);
            }
            if let Some(n) = self.parse_ruleset(is_nested) {
                return Some(n);
            }
            return self.css_parse_stylesheet_at_statement(is_nested);
        }
        self.parse_ruleset(true).or_else(|| self.scss_parse_variable_declaration(Some(&[])))
    }

    pub fn scss_parse_import(&mut self) -> Option<NodeId> {
        if !self.peek_keyword("@import") {
            return None;
        }
        let node = self.create(Class::Import);
        self.consume_token();
        if !self.add_uri_or_string(node) {
            return Some(self.finish_err(node, ParseError::URIOrStringExpected));
        }
        while self.accept(TT::Comma) {
            if !self.add_uri_or_string(node) {
                return Some(self.finish_err(node, ParseError::URIOrStringExpected));
            }
        }
        Some(self.complete_parse_import(node))
    }

    /// `_parseVariableDeclaration(panic = [])`
    pub fn scss_parse_variable_declaration(&mut self, panic: Option<&[TT]>) -> Option<NodeId> {
        if !self.peek(TT::VariableName) {
            return None;
        }
        let node = self.create(Class::VariableDeclaration);
        let v = self.scss_parse_variable();
        let Some(v) = v else { return None };
        self.ast.adopt_child(node, v, -1);
        self.ast.set_field(node, Field::Variable, v);
        if !self.accept(TT::Colon) {
            return Some(self.finish_err(node, ParseError::ColonExpected));
        }
        if let Some(prev) = self.prev_token {
            self.ast.get_mut(node).colon_position = Some(prev.offset);
        }
        let e = self.parse_expr(false);
        let Some(e) = e else {
            return Some(self.finish_resync(node, ParseError::VariableValueExpected, Some(&[]), panic));
        };
        self.ast.adopt_child(node, e, -1);
        self.ast.set_field(node, Field::Value, e);
        while self.peek(TT::Exclamation) {
            let p = self.try_parse_prio();
            if self.add_child(node, p) {
                // `!important`
            } else {
                self.consume_token();
                if !(self.peek(TT::Ident) && (eq_str(self.text(), "default") || eq_str(self.text(), "global"))) {
                    return Some(self.finish_err(node, ParseError::UnknownKeyword));
                }
                self.consume_token();
            }
        }
        Some(self.finish(node))
    }

    pub fn scss_parse_keyframe_selector(&mut self) -> Option<NodeId> {
        if let Some(n) = self.try_parse_keyframe_selector() {
            return Some(n);
        }
        if let Some(n) = self.scss_parse_control_statement(DeclFn::KeyframeSelector) {
            return Some(n);
        }
        if let Some(n) = self.scss_parse_warn_and_debug() {
            return Some(n);
        }
        if let Some(n) = self.scss_parse_mixin_reference() {
            return Some(n);
        }
        if let Some(n) = self.scss_parse_function_declaration() {
            return Some(n);
        }
        if let Some(n) = self.scss_parse_variable_declaration(Some(&[])) {
            return Some(n);
        }
        self.scss_parse_mixin_content()
    }

    pub fn scss_parse_variable(&mut self) -> Option<NodeId> {
        if !self.peek(TT::VariableName) {
            return None;
        }
        let node = self.create(Class::Variable);
        self.consume_token();
        Some(node)
    }

    pub fn scss_parse_module_member(&mut self) -> Option<NodeId> {
        let pos = self.mark();
        let node = self.create(Class::Module);
        let id = self.parse_ident();
        if !self.set_node_at(node, Field::Identifier, id, 0) {
            return None;
        }
        if self.has_whitespace() || !self.accept_delim(".") || self.has_whitespace() {
            self.restore_at_mark(pos);
            return None;
        }
        let m = match self.scss_parse_variable() {
            Some(v) => Some(v),
            None => self.parse_function(),
        };
        if !self.add_child(node, m) {
            return Some(self.finish_err(node, ParseError::IdentifierOrVariableExpected));
        }
        Some(node)
    }

    pub fn scss_parse_ident(&mut self) -> Option<NodeId> {
        if !self.peek(TT::Ident) && !self.peek(TT::InterpolationFunction) && !self.peek_delim("-") {
            return None;
        }
        let node = self.create(Class::Identifier);
        let mut has_content = false;
        loop {
            let ok = self.accept(TT::Ident) || {
                let i = self.scss_ident_interpolation();
                self.add_child(node, i)
            } || (has_content && self.accept_regexp(word_or_dash_start));
            if !ok {
                break;
            }
            has_content = true;
            if self.has_whitespace() {
                break;
            }
        }
        if has_content { Some(self.finish(node)) } else { None }
    }

    /// `indentInterpolation` in `_parseIdent`
    fn scss_ident_interpolation(&mut self) -> Option<NodeId> {
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
        self.scss_parse_interpolation()
    }

    pub fn scss_parse_interpolation(&mut self) -> Option<NodeId> {
        if self.peek(TT::InterpolationFunction) {
            let node = self.create(Class::Interpolation);
            self.consume_token();
            let e = self.parse_expr(false);
            if !self.add_child(node, e) && self.parse_nesting_selector().is_none() {
                if self.accept(TT::CurlyR) {
                    return Some(self.finish(node));
                }
                return Some(self.finish_err(node, ParseError::ExpressionExpected));
            }
            if !self.accept(TT::CurlyR) {
                return Some(self.finish_err(node, ParseError::RightCurlyExpected));
            }
            return Some(self.finish(node));
        }
        None
    }

    pub fn scss_parse_rule_set_declaration(&mut self) -> Option<NodeId> {
        if self.peek(TT::AtKeyword) {
            if let Some(n) = self.parse_keyframe() {
                return Some(n);
            }
            if let Some(n) = self.parse_import() {
                return Some(n);
            }
            if let Some(n) = self.parse_media(true) {
                return Some(n);
            }
            if let Some(n) = self.parse_font_face() {
                return Some(n);
            }
            if let Some(n) = self.scss_parse_warn_and_debug() {
                return Some(n);
            }
            if let Some(n) = self.scss_parse_control_statement(DeclFn::RuleSetDeclaration) {
                return Some(n);
            }
            if let Some(n) = self.scss_parse_function_declaration() {
                return Some(n);
            }
            if let Some(n) = self.scss_parse_extends() {
                return Some(n);
            }
            if let Some(n) = self.scss_parse_mixin_reference() {
                return Some(n);
            }
            if let Some(n) = self.scss_parse_mixin_content() {
                return Some(n);
            }
            if let Some(n) = self.scss_parse_mixin_declaration() {
                return Some(n);
            }
            if let Some(n) = self.parse_ruleset(true) {
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
            return self.parse_rule_set_declaration_at_statement();
        }
        if let Some(n) = self.scss_parse_variable_declaration(Some(&[])) {
            return Some(n);
        }
        if let Some(n) = self.try_parse_ruleset(true) {
            return Some(n);
        }
        self.parse_declaration(None)
    }

    pub fn scss_parse_declaration(&mut self, stop_tokens: Option<&[TT]>) -> Option<NodeId> {
        if let Some(c) = self.try_parse_custom_property_declaration(stop_tokens) {
            return Some(c);
        }
        let node = self.create(Class::Declaration);
        let p = self.parse_property();
        if !self.set_node(node, Field::Property, p) {
            return None;
        }
        if !self.accept(TT::Colon) {
            return Some(self.finish_resync(
                node,
                ParseError::ColonExpected,
                Some(&[TT::Colon]),
                Some(stop_tokens.unwrap_or(&[TT::SemiColon])),
            ));
        }
        if let Some(prev) = self.prev_token {
            self.ast.get_mut(node).colon_position = Some(prev.offset);
        }
        let mut has_content = false;
        let e = self.parse_expr(false);
        if self.set_node(node, Field::Value, e) {
            has_content = true;
            let p = self.parse_prio();
            self.add_child(node, p);
        }
        if self.peek(TT::CurlyL) {
            let n = self.scss_parse_nested_properties();
            self.set_node(node, Field::NestedProperties, Some(n));
        } else if !has_content {
            return Some(self.finish_err(node, ParseError::PropertyValueExpected));
        }
        Some(self.finish(node))
    }

    fn scss_parse_nested_properties(&mut self) -> NodeId {
        let node = self.create(Class::NestedProperties);
        self.parse_body(node, DeclFn::Declaration)
    }

    fn scss_parse_extends(&mut self) -> Option<NodeId> {
        if self.peek_keyword("@extend") {
            let node = self.create(Class::ExtendsReference);
            self.consume_token();
            let s = self.parse_simple_selector();
            let sels = self.nodelist(node, Field::Selectors);
            if !self.add_child(sels, s) {
                return Some(self.finish_err(node, ParseError::SelectorExpected));
            }
            while self.accept(TT::Comma) {
                let s = self.parse_simple_selector();
                let sels = self.nodelist(node, Field::Selectors);
                self.add_child(sels, s);
            }
            if self.accept(TT::Exclamation) && !self.accept_ident("optional") {
                return Some(self.finish_err(node, ParseError::UnknownKeyword));
            }
            return Some(self.finish(node));
        }
        None
    }

    pub fn scss_parse_selector_placeholder(&mut self) -> Option<NodeId> {
        if self.peek_delim("%") {
            let node = self.create_node(NodeType::SelectorPlaceholder);
            self.consume_token();
            self.parse_ident();
            return Some(self.finish(node));
        } else if self.peek_keyword("@at-root") {
            let node = self.create_node(NodeType::SelectorPlaceholder);
            self.consume_token();
            if self.accept(TT::ParenthesisL) {
                if !self.accept_ident("with") && !self.accept_ident("without") {
                    return Some(self.finish_err(node, ParseError::IdentifierExpected));
                }
                if !self.accept(TT::Colon) {
                    return Some(self.finish_err(node, ParseError::ColonExpected));
                }
                let i = self.parse_ident();
                if !self.add_child(node, i) {
                    return Some(self.finish_err(node, ParseError::IdentifierExpected));
                }
                if !self.accept(TT::ParenthesisR) {
                    return Some(self.finish_resync(node, ParseError::RightParenthesisExpected, Some(&[TT::CurlyR]), None));
                }
            }
            return Some(self.finish(node));
        }
        None
    }

    fn scss_parse_warn_and_debug(&mut self) -> Option<NodeId> {
        if !self.peek_keyword("@debug") && !self.peek_keyword("@warn") && !self.peek_keyword("@error") {
            return None;
        }
        let node = self.create_node(NodeType::Debug);
        self.consume_token();
        let e = self.parse_expr(false);
        self.add_child(node, e);
        Some(self.finish(node))
    }

    fn scss_parse_control_statement(&mut self, parse_statement: DeclFn) -> Option<NodeId> {
        if !self.peek(TT::AtKeyword) {
            return None;
        }
        if let Some(n) = self.scss_parse_if_statement(parse_statement) {
            return Some(n);
        }
        if let Some(n) = self.scss_parse_for_statement(parse_statement) {
            return Some(n);
        }
        if let Some(n) = self.scss_parse_each_statement(parse_statement) {
            return Some(n);
        }
        self.scss_parse_while_statement(parse_statement)
    }

    fn scss_parse_if_statement(&mut self, parse_statement: DeclFn) -> Option<NodeId> {
        if !self.peek_keyword("@if") {
            return None;
        }
        Some(self.scss_internal_parse_if_statement(parse_statement))
    }

    pub(super) fn scss_internal_parse_if_statement_impl(&mut self, parse_statement: DeclFn) -> NodeId {
        let node = self.create(Class::IfStatement);
        self.consume_token();
        let e = self.parse_expr(true);
        if !self.set_node_at(node, Field::Expression, e, 0) {
            return self.finish_err(node, ParseError::ExpressionExpected);
        }
        self.parse_body(node, parse_statement);
        if self.accept_keyword("@else") {
            if self.peek_ident("if") {
                let e = self.scss_internal_parse_if_statement(parse_statement);
                self.set_node(node, Field::ElseClause, Some(e));
            } else if self.peek(TT::CurlyL) {
                let else_node = self.create(Class::ElseStatement);
                self.parse_body(else_node, parse_statement);
                self.set_node(node, Field::ElseClause, Some(else_node));
            }
        }
        self.finish(node)
    }

    fn scss_parse_for_statement(&mut self, parse_statement: DeclFn) -> Option<NodeId> {
        if !self.peek_keyword("@for") {
            return None;
        }
        let node = self.create(Class::ForStatement);
        self.consume_token();
        let v = self.scss_parse_variable();
        if !self.set_node_at(node, Field::Variable, v, 0) {
            return Some(self.finish_resync(node, ParseError::VariableNameExpected, Some(&[TT::CurlyR]), None));
        }
        if !self.accept_ident("from") {
            return Some(self.finish_resync(node, ParseError::ScssFromExpected, Some(&[TT::CurlyR]), None));
        }
        let b = self.parse_binary_expr(None, None);
        if !self.add_child(node, b) {
            return Some(self.finish_resync(node, ParseError::ExpressionExpected, Some(&[TT::CurlyR]), None));
        }
        if !self.accept_ident("to") && !self.accept_ident("through") {
            return Some(self.finish_resync(node, ParseError::ScssThroughOrToExpected, Some(&[TT::CurlyR]), None));
        }
        let b = self.parse_binary_expr(None, None);
        if !self.add_child(node, b) {
            return Some(self.finish_resync(node, ParseError::ExpressionExpected, Some(&[TT::CurlyR]), None));
        }
        Some(self.parse_body(node, parse_statement))
    }

    fn scss_parse_each_statement(&mut self, parse_statement: DeclFn) -> Option<NodeId> {
        if !self.peek_keyword("@each") {
            return None;
        }
        let node = self.create(Class::EachStatement);
        self.consume_token();
        let variables = self.nodelist(node, Field::Variables);
        let v = self.scss_parse_variable();
        if !self.add_child(variables, v) {
            return Some(self.finish_resync(node, ParseError::VariableNameExpected, Some(&[TT::CurlyR]), None));
        }
        while self.accept(TT::Comma) {
            let v = self.scss_parse_variable();
            if !self.add_child(variables, v) {
                return Some(self.finish_resync(node, ParseError::VariableNameExpected, Some(&[TT::CurlyR]), None));
            }
        }
        self.finish(variables);
        if !self.accept_ident("in") {
            return Some(self.finish_resync(node, ParseError::ScssInExpected, Some(&[TT::CurlyR]), None));
        }
        let e = self.parse_expr(false);
        if !self.add_child(node, e) {
            return Some(self.finish_resync(node, ParseError::ExpressionExpected, Some(&[TT::CurlyR]), None));
        }
        Some(self.parse_body(node, parse_statement))
    }

    fn scss_parse_while_statement(&mut self, parse_statement: DeclFn) -> Option<NodeId> {
        if !self.peek_keyword("@while") {
            return None;
        }
        let node = self.create(Class::WhileStatement);
        self.consume_token();
        let b = self.parse_binary_expr(None, None);
        if !self.add_child(node, b) {
            return Some(self.finish_resync(node, ParseError::ExpressionExpected, Some(&[TT::CurlyR]), None));
        }
        Some(self.parse_body(node, parse_statement))
    }

    pub fn scss_parse_function_body_declaration(&mut self) -> Option<NodeId> {
        if let Some(n) = self.scss_parse_variable_declaration(Some(&[])) {
            return Some(n);
        }
        if let Some(n) = self.scss_parse_return_statement() {
            return Some(n);
        }
        if let Some(n) = self.scss_parse_warn_and_debug() {
            return Some(n);
        }
        self.scss_parse_control_statement(DeclFn::ScssFunctionBodyDeclaration)
    }

    fn scss_parse_function_declaration(&mut self) -> Option<NodeId> {
        if !self.peek_keyword("@function") {
            return None;
        }
        let node = self.create(Class::FunctionDeclaration);
        self.consume_token();
        let id = self.parse_ident();
        if !self.set_node_at(node, Field::Identifier, id, 0) {
            return Some(self.finish_resync(node, ParseError::IdentifierExpected, Some(&[TT::CurlyR]), None));
        }
        if !self.accept(TT::ParenthesisL) {
            return Some(self.finish_resync(node, ParseError::LeftParenthesisExpected, Some(&[TT::CurlyR]), None));
        }
        let params = self.nodelist(node, Field::Parameters);
        let p = self.scss_parse_parameter_declaration();
        if self.add_child(params, p) {
            while self.accept(TT::Comma) {
                if self.peek(TT::ParenthesisR) {
                    break;
                }
                let p = self.scss_parse_parameter_declaration();
                if !self.add_child(params, p) {
                    return Some(self.finish_err(node, ParseError::VariableNameExpected));
                }
            }
        }
        if !self.accept(TT::ParenthesisR) {
            return Some(self.finish_resync(node, ParseError::RightParenthesisExpected, Some(&[TT::CurlyR]), None));
        }
        Some(self.parse_body(node, DeclFn::ScssFunctionBodyDeclaration))
    }

    fn scss_parse_return_statement(&mut self) -> Option<NodeId> {
        if !self.peek_keyword("@return") {
            return None;
        }
        let node = self.create_node(NodeType::ReturnStatement);
        self.consume_token();
        let e = self.parse_expr(false);
        if !self.add_child(node, e) {
            return Some(self.finish_err(node, ParseError::ExpressionExpected));
        }
        Some(self.finish(node))
    }

    fn scss_parse_mixin_declaration(&mut self) -> Option<NodeId> {
        if !self.peek_keyword("@mixin") {
            return None;
        }
        let node = self.create(Class::MixinDeclaration);
        self.consume_token();
        let id = self.parse_ident();
        if !self.set_node_at(node, Field::Identifier, id, 0) {
            return Some(self.finish_resync(node, ParseError::IdentifierExpected, Some(&[TT::CurlyR]), None));
        }
        if self.accept(TT::ParenthesisL) {
            let params = self.nodelist(node, Field::Parameters);
            let p = self.scss_parse_parameter_declaration();
            if self.add_child(params, p) {
                while self.accept(TT::Comma) {
                    if self.peek(TT::ParenthesisR) {
                        break;
                    }
                    let p = self.scss_parse_parameter_declaration();
                    if !self.add_child(params, p) {
                        return Some(self.finish_err(node, ParseError::VariableNameExpected));
                    }
                }
            }
            if !self.accept(TT::ParenthesisR) {
                return Some(self.finish_resync(node, ParseError::RightParenthesisExpected, Some(&[TT::CurlyR]), None));
            }
        }
        Some(self.parse_body(node, DeclFn::RuleSetDeclaration))
    }

    fn scss_parse_parameter_declaration(&mut self) -> Option<NodeId> {
        let node = self.create(Class::FunctionParameter);
        let v = self.scss_parse_variable();
        if !self.set_node_at(node, Field::Identifier, v, 0) {
            return None;
        }
        self.accept(TT::Ellipsis);
        if self.accept(TT::Colon) {
            let e = self.parse_expr(true);
            if !self.set_node_at(node, Field::DefaultValue, e, 0) {
                return Some(self.finish_resync(
                    node,
                    ParseError::VariableValueExpected,
                    Some(&[]),
                    Some(&[TT::Comma, TT::ParenthesisR]),
                ));
            }
        }
        Some(self.finish(node))
    }

    fn scss_parse_mixin_content(&mut self) -> Option<NodeId> {
        if !self.peek_keyword("@content") {
            return None;
        }
        let node = self.create(Class::MixinContentReference);
        self.consume_token();
        if self.accept(TT::ParenthesisL) {
            let args = self.nodelist(node, Field::Arguments);
            let a = self.scss_parse_function_argument();
            if self.add_child(args, a) {
                while self.accept(TT::Comma) {
                    if self.peek(TT::ParenthesisR) {
                        break;
                    }
                    let a = self.scss_parse_function_argument();
                    if !self.add_child(args, a) {
                        return Some(self.finish_err(node, ParseError::ExpressionExpected));
                    }
                }
            }
            if !self.accept(TT::ParenthesisR) {
                return Some(self.finish_err(node, ParseError::RightParenthesisExpected));
            }
        }
        Some(self.finish(node))
    }

    fn scss_parse_mixin_reference(&mut self) -> Option<NodeId> {
        if !self.peek_keyword("@include") {
            return None;
        }
        let node = self.create(Class::MixinReference);
        self.consume_token();
        let first_ident = self.parse_ident();
        if !self.set_node_at(node, Field::Identifier, first_ident, 0) {
            return Some(self.finish_resync(node, ParseError::IdentifierExpected, Some(&[TT::CurlyR]), None));
        }
        if !self.has_whitespace() && self.accept_delim(".") && !self.has_whitespace() {
            let second_ident = self.parse_ident();
            let Some(second_ident) = second_ident else {
                return Some(self.finish_resync(node, ParseError::IdentifierExpected, Some(&[TT::CurlyR]), None));
            };
            let module_token = self.create(Class::Module);
            self.set_node_at(module_token, Field::Identifier, first_ident, 0);
            self.set_node_at(node, Field::Identifier, Some(second_ident), 0);
            self.add_child(node, Some(module_token));
        }
        if self.accept(TT::ParenthesisL) {
            let args = self.nodelist(node, Field::Arguments);
            let a = self.scss_parse_function_argument();
            if self.add_child(args, a) {
                while self.accept(TT::Comma) {
                    if self.peek(TT::ParenthesisR) {
                        break;
                    }
                    let a = self.scss_parse_function_argument();
                    if !self.add_child(args, a) {
                        return Some(self.finish_err(node, ParseError::ExpressionExpected));
                    }
                }
            }
            if !self.accept(TT::ParenthesisR) {
                return Some(self.finish_err(node, ParseError::RightParenthesisExpected));
            }
        }
        if self.peek_ident("using") || self.peek(TT::CurlyL) {
            let c = self.scss_parse_mixin_content_declaration();
            self.set_node(node, Field::Content, Some(c));
        }
        Some(self.finish(node))
    }

    fn scss_parse_mixin_content_declaration(&mut self) -> NodeId {
        let node = self.create(Class::MixinContentDeclaration);
        if self.accept_ident("using") {
            if !self.accept(TT::ParenthesisL) {
                return self.finish_resync(node, ParseError::LeftParenthesisExpected, Some(&[TT::CurlyL]), None);
            }
            let params = self.nodelist(node, Field::Parameters);
            let p = self.scss_parse_parameter_declaration();
            if self.add_child(params, p) {
                while self.accept(TT::Comma) {
                    if self.peek(TT::ParenthesisR) {
                        break;
                    }
                    let p = self.scss_parse_parameter_declaration();
                    if !self.add_child(params, p) {
                        return self.finish_err(node, ParseError::VariableNameExpected);
                    }
                }
            }
            if !self.accept(TT::ParenthesisR) {
                return self.finish_resync(node, ParseError::RightParenthesisExpected, Some(&[TT::CurlyL]), None);
            }
        }
        if self.peek(TT::CurlyL) {
            self.parse_body(node, DeclFn::ScssMixinReferenceBodyStatement);
        }
        self.finish(node)
    }

    pub fn scss_parse_mixin_reference_body_statement(&mut self) -> Option<NodeId> {
        self.try_parse_keyframe_selector().or_else(|| self.parse_rule_set_declaration())
    }

    pub fn scss_parse_function_argument(&mut self) -> Option<NodeId> {
        let node = self.create(Class::FunctionArgument);
        let pos = self.mark();
        let argument = self.scss_parse_variable();
        if let Some(argument) = argument {
            if !self.accept(TT::Colon) {
                if self.accept(TT::Ellipsis) {
                    self.set_node_at(node, Field::Value, Some(argument), 0);
                    return Some(self.finish(node));
                } else {
                    self.restore_at_mark(pos);
                }
            } else {
                self.set_node_at(node, Field::Identifier, Some(argument), 0);
            }
        }
        let e = self.parse_expr(true);
        if self.set_node_at(node, Field::Value, e, 0) {
            self.accept(TT::Ellipsis);
            let p = self.parse_prio();
            self.add_child(node, p);
            return Some(self.finish(node));
        }
        let p = self.try_parse_prio();
        if self.set_node_at(node, Field::Value, p, 0) {
            return Some(self.finish(node));
        }
        None
    }

    pub fn scss_parse_list_element(&mut self) -> Option<NodeId> {
        let node = self.create(Class::ListEntry);
        let child = self.parse_binary_expr(None, None)?;
        if self.accept(TT::Colon) {
            self.set_node_at(node, Field::Key, Some(child), 0);
            let v = self.parse_binary_expr(None, None);
            if !self.set_node_at(node, Field::Value, v, 1) {
                return Some(self.finish_err(node, ParseError::ExpressionExpected));
            }
        } else {
            self.set_node_at(node, Field::Value, Some(child), 1);
        }
        Some(self.finish(node))
    }

    fn scss_parse_use(&mut self) -> Option<NodeId> {
        if !self.peek_keyword("@use") {
            return None;
        }
        let node = self.create(Class::Use);
        self.consume_token();
        let s = self.parse_string_literal();
        if !self.add_child(node, s) {
            return Some(self.finish_err(node, ParseError::StringLiteralExpected));
        }
        if !self.peek(TT::SemiColon) && !self.peek(TT::EOF) {
            if !(self.peek(TT::Ident) && contains_as_or_with(self.text())) {
                return Some(self.finish_err(node, ParseError::UnknownKeyword));
            }
            if self.accept_ident("as") {
                let i = self.parse_ident();
                if !self.set_node_at(node, Field::Identifier, i, 0) && !self.accept_delim("*") {
                    return Some(self.finish_err(node, ParseError::IdentifierOrWildcardExpected));
                }
            }
            if self.accept_ident("with") {
                let c = self.scss_parse_module_config();
                if !self.set_node(node, Field::Parameters, c) {
                    return Some(self.finish_resync(
                        node,
                        ParseError::LeftParenthesisExpected,
                        Some(&[TT::ParenthesisR]),
                        None,
                    ));
                }
            }
        }
        if !self.accept(TT::SemiColon) && !self.accept(TT::EOF) {
            return Some(self.finish_err(node, ParseError::SemiColonExpected));
        }
        Some(self.finish(node))
    }

    fn scss_parse_module_config(&mut self) -> Option<NodeId> {
        let node = self.create_node(NodeType::ModuleConfig);
        if !self.accept(TT::ParenthesisL) {
            return None;
        }
        let d = self.scss_parse_module_config_declaration();
        if !self.add_child(node, d) {
            return Some(self.finish_err(node, ParseError::VariableNameExpected));
        }
        while self.accept(TT::Comma) {
            if self.peek(TT::ParenthesisR) {
                break;
            }
            let d = self.scss_parse_module_config_declaration();
            if !self.add_child(node, d) {
                return Some(self.finish_err(node, ParseError::VariableNameExpected));
            }
        }
        if !self.accept(TT::ParenthesisR) {
            return Some(self.finish_err(node, ParseError::RightParenthesisExpected));
        }
        Some(self.finish(node))
    }

    fn scss_parse_module_config_declaration(&mut self) -> Option<NodeId> {
        let node = self.create(Class::ModuleConfiguration);
        let v = self.scss_parse_variable();
        if !self.set_node_at(node, Field::Identifier, v, 0) {
            return None;
        }
        let ok = self.accept(TT::Colon) && {
            let e = self.parse_expr(true);
            self.set_node_at(node, Field::Value, e, 0)
        };
        if !ok {
            return Some(self.finish_resync(
                node,
                ParseError::VariableValueExpected,
                Some(&[]),
                Some(&[TT::Comma, TT::ParenthesisR]),
            ));
        }
        if self.accept(TT::Exclamation) && (self.has_whitespace() || !self.accept_ident("default")) {
            return Some(self.finish_err(node, ParseError::UnknownKeyword));
        }
        Some(self.finish(node))
    }

    fn scss_parse_forward(&mut self) -> Option<NodeId> {
        if !self.peek_keyword("@forward") {
            return None;
        }
        let node = self.create(Class::Forward);
        self.consume_token();
        let s = self.parse_string_literal();
        if !self.add_child(node, s) {
            return Some(self.finish_err(node, ParseError::StringLiteralExpected));
        }
        if self.accept_ident("as") {
            let identifier = self.parse_ident();
            if !self.set_node_at(node, Field::Identifier, identifier, 0) {
                return Some(self.finish_err(node, ParseError::IdentifierExpected));
            }
            if self.has_whitespace() || !self.accept_delim("*") {
                return Some(self.finish_err(node, ParseError::WildcardExpected));
            }
        }
        if self.accept_ident("with") {
            let c = self.scss_parse_module_config();
            if !self.set_node(node, Field::Parameters, c) {
                return Some(self.finish_resync(node, ParseError::LeftParenthesisExpected, Some(&[TT::ParenthesisR]), None));
            }
        } else if self.peek_ident("hide") || self.peek_ident("show") {
            let v = self.scss_parse_forward_visibility();
            if !self.add_child(node, v) {
                return Some(self.finish_err(node, ParseError::IdentifierOrVariableExpected));
            }
        }
        if !self.accept(TT::SemiColon) && !self.accept(TT::EOF) {
            return Some(self.finish_err(node, ParseError::SemiColonExpected));
        }
        Some(self.finish(node))
    }

    fn scss_parse_forward_visibility(&mut self) -> Option<NodeId> {
        let node = self.create(Class::ForwardVisibility);
        let i = self.parse_ident();
        self.set_node_at(node, Field::Identifier, i, 0);
        loop {
            let c = match self.scss_parse_variable() {
                Some(v) => Some(v),
                None => self.parse_ident(),
            };
            if !self.add_child(node, c) {
                break;
            }
            self.accept(TT::Comma);
        }
        if self.ast.child_count(node) > 1 { Some(node) } else { None }
    }
}
