//! Port of vscode-css-languageservice's `parser/cssParser.ts`. The SCSS and LESS overrides live
//! in `scss.rs` / `less.rs`; methods that a dialect overrides dispatch on `self.dialect`, and the
//! base implementation is the `css_*` method (what `super.x()` calls).

use super::Dialect;
use super::nodes::{Ast, Class, Field, Issue, NodeId, NodeType};
use super::scanner::{Scanner, TT, Token};

/// `ParseError` / `SCSSParseError` issue types
#[allow(dead_code)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ParseError {
    NumberExpected,
    ConditionExpected,
    RuleOrSelectorExpected,
    DotExpected,
    ColonExpected,
    SemiColonExpected,
    TermExpected,
    ExpressionExpected,
    OperatorExpected,
    IdentifierExpected,
    PercentageExpected,
    URIOrStringExpected,
    URIExpected,
    VariableNameExpected,
    VariableValueExpected,
    PropertyValueExpected,
    LeftCurlyExpected,
    RightCurlyExpected,
    LeftSquareBracketExpected,
    RightSquareBracketExpected,
    LeftParenthesisExpected,
    RightParenthesisExpected,
    CommaExpected,
    PageDirectiveOrDeclarationExpected,
    UnknownAtRule,
    UnknownKeyword,
    SelectorExpected,
    StringLiteralExpected,
    WhitespaceExpected,
    MediaQueryExpected,
    IdentifierOrWildcardExpected,
    WildcardExpected,
    IdentifierOrVariableExpected,
    ScssFromExpected,
    ScssThroughOrToExpected,
    ScssInExpected,
}

impl ParseError {
    pub fn id(self) -> &'static str {
        use ParseError::*;
        match self {
            NumberExpected => "css-numberexpected",
            ConditionExpected => "css-conditionexpected",
            RuleOrSelectorExpected => "css-ruleorselectorexpected",
            DotExpected => "css-dotexpected",
            ColonExpected => "css-colonexpected",
            SemiColonExpected => "css-semicolonexpected",
            TermExpected => "css-termexpected",
            ExpressionExpected => "css-expressionexpected",
            OperatorExpected => "css-operatorexpected",
            IdentifierExpected => "css-identifierexpected",
            PercentageExpected => "css-percentageexpected",
            URIOrStringExpected => "css-uriorstringexpected",
            URIExpected => "css-uriexpected",
            VariableNameExpected => "css-varnameexpected",
            VariableValueExpected => "css-varvalueexpected",
            PropertyValueExpected => "css-propertyvalueexpected",
            LeftCurlyExpected => "css-lcurlyexpected",
            RightCurlyExpected => "css-rcurlyexpected",
            LeftSquareBracketExpected => "css-rbracketexpected",
            RightSquareBracketExpected => "css-lbracketexpected",
            LeftParenthesisExpected => "css-lparentexpected",
            RightParenthesisExpected => "css-rparentexpected",
            CommaExpected => "css-commaexpected",
            PageDirectiveOrDeclarationExpected => "css-pagedirordeclexpected",
            UnknownAtRule => "css-unknownatrule",
            UnknownKeyword => "css-unknownkeyword",
            SelectorExpected => "css-selectorexpected",
            StringLiteralExpected => "css-stringliteralexpected",
            WhitespaceExpected => "css-whitespaceexpected",
            MediaQueryExpected => "css-mediaqueryexpected",
            IdentifierOrWildcardExpected => "css-idorwildcardexpected",
            WildcardExpected => "css-wildcardexpected",
            IdentifierOrVariableExpected => "css-idorvarexpected",
            ScssFromExpected => "scss-fromexpected",
            ScssThroughOrToExpected => "scss-throughexpected",
            ScssInExpected => "scss-fromexpected",
        }
    }

    pub fn message(self) -> &'static str {
        use ParseError::*;
        match self {
            NumberExpected => "number expected",
            ConditionExpected => "condition expected",
            RuleOrSelectorExpected => "at-rule or selector expected",
            DotExpected => "dot expected",
            ColonExpected => "colon expected",
            SemiColonExpected => "semi-colon expected",
            TermExpected => "term expected",
            ExpressionExpected => "expression expected",
            OperatorExpected => "operator expected",
            IdentifierExpected => "identifier expected",
            PercentageExpected => "percentage expected",
            URIOrStringExpected => "uri or string expected",
            URIExpected => "URI expected",
            VariableNameExpected => "variable name expected",
            VariableValueExpected => "variable value expected",
            PropertyValueExpected => "property value expected",
            LeftCurlyExpected => "{ expected",
            RightCurlyExpected => "} expected",
            LeftSquareBracketExpected => "[ expected",
            RightSquareBracketExpected => "] expected",
            LeftParenthesisExpected => "( expected",
            RightParenthesisExpected => ") expected",
            CommaExpected => "comma expected",
            PageDirectiveOrDeclarationExpected => "page directive or declaraton expected",
            UnknownAtRule => "at-rule unknown",
            UnknownKeyword => "unknown keyword",
            SelectorExpected => "selector expected",
            StringLiteralExpected => "string literal expected",
            WhitespaceExpected => "whitespace expected",
            MediaQueryExpected => "media query expected",
            IdentifierOrWildcardExpected => "identifier or wildcard expected",
            WildcardExpected => "wildcard expected",
            IdentifierOrVariableExpected => "identifier or variable expected",
            ScssFromExpected => "'from' expected",
            ScssThroughOrToExpected => "'through' or 'to' expected",
            ScssInExpected => "'in' expected",
        }
    }
}

/// The `parseDeclaration` callbacks passed to `_parseBody` / `_parseDeclarations`
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DeclFn {
    RuleSetDeclaration,
    KeyframeSelector,
    Declaration,
    PageDeclaration,
    MediaDeclaration(bool),
    SupportsDeclaration(bool),
    LayerDeclaration(bool),
    ContainerDeclaration(bool),
    StartingStyleDeclaration(bool),
    ScopeDeclaration,
    StylesheetStatement,
    ScssFunctionBodyDeclaration,
    ScssMixinReferenceBodyStatement,
    LessDetachedRuleSetBody,
    LessMixInBodyDeclaration,
}

#[derive(Clone, Copy)]
pub struct Mark {
    prev: Option<Token>,
    curr: Token,
    pos: usize,
}

const PAGE_BOX_DIRECTIVES: [&str; 16] = [
    "@bottom-center",
    "@bottom-left",
    "@bottom-left-corner",
    "@bottom-right",
    "@bottom-right-corner",
    "@left-bottom",
    "@left-middle",
    "@left-top",
    "@right-bottom",
    "@right-middle",
    "@right-top",
    "@top-center",
    "@top-left",
    "@top-left-corner",
    "@top-right",
    "@top-right-corner",
];

/// The unwind payload when the parse nests deeper than allowed
pub struct DepthExceeded;

pub struct Parser<'a> {
    pub scanner: Scanner<'a>,
    pub ast: Ast,
    pub token: Token,
    pub prev_token: Option<Token>,
    last_error_token: Option<u32>,
    depth: u32,
    max_depth: u32,
    pub dialect: Dialect,
}

/// `text === token.text.toLowerCase()` (with the length check `peekIdent` does first)
pub fn eq_lower(t: &[u16], kw: &str) -> bool {
    if t.len() != kw.len() {
        return false;
    }
    if t.iter().all(|&c| c < 0x80) {
        return t.iter().zip(kw.bytes()).all(|(&c, k)| (c as u8).to_ascii_lowercase() == k);
    }
    String::from_utf16_lossy(t).to_lowercase() == kw
}

pub fn eq_str(t: &[u16], s: &str) -> bool {
    t.len() == s.len() && t.iter().zip(s.bytes()).all(|(&c, k)| c == k as u16)
}

/// ASCII case-insensitive comparison (a JS regexp with the `i` flag and no `u` flag)
pub fn eq_ignore_case(t: &[u16], s: &str) -> bool {
    t.len() == s.len() && t.iter().zip(s.bytes()).all(|(&c, k)| c < 0x80 && (c as u8).eq_ignore_ascii_case(&k))
}

pub fn is_word(c: u16) -> bool {
    c < 0x80 && ((c as u8).is_ascii_alphanumeric() || c == b'_' as u16)
}

impl<'a> Parser<'a> {
    pub fn new(src: &'a [u16], dialect: Dialect, max_depth: u32) -> Self {
        Parser {
            scanner: Scanner::new(src, dialect),
            ast: Ast::new(),
            token: Token::initial(),
            prev_token: None,
            last_error_token: None,
            depth: 0,
            max_depth,
            dialect,
        }
    }

    // --- recursion guard ---
    //
    // Every recursive cycle of the grammar goes through one of the guarded methods below. Past
    // `max_depth` the parse is abandoned by unwinding with `DepthExceeded` (`resume_unwind`
    // doesn't run the panic hook); the caller catches it.

    #[inline]
    fn enter(&mut self) {
        if self.depth >= self.max_depth {
            std::panic::resume_unwind(Box::new(DepthExceeded));
        }
        self.depth += 1;
    }

    pub fn parse_declarations(&mut self, f: DeclFn) -> Option<NodeId> {
        self.enter();
        let r = self.parse_declarations_impl(f);
        self.depth -= 1;
        r
    }

    pub fn parse_selector(&mut self, is_nested: bool) -> Option<NodeId> {
        self.enter();
        let r = self.parse_selector_impl(is_nested);
        self.depth -= 1;
        r
    }

    pub fn parse_supports_condition(&mut self) -> NodeId {
        self.enter();
        let r = self.parse_supports_condition_impl();
        self.depth -= 1;
        r
    }

    pub fn parse_media_condition(&mut self) -> NodeId {
        self.enter();
        let r = self.parse_media_condition_impl();
        self.depth -= 1;
        r
    }

    fn parse_container_query(&mut self) -> NodeId {
        self.enter();
        let r = self.parse_container_query_impl();
        self.depth -= 1;
        r
    }

    fn parse_style_query(&mut self) -> NodeId {
        self.enter();
        let r = self.parse_style_query_impl();
        self.depth -= 1;
        r
    }

    pub fn parse_binary_expr(&mut self, preparsed_left: Option<NodeId>, preparsed_oper: Option<NodeId>) -> Option<NodeId> {
        self.enter();
        let r = self.parse_binary_expr_impl(preparsed_left, preparsed_oper);
        self.depth -= 1;
        r
    }

    pub fn scss_internal_parse_if_statement(&mut self, parse_statement: DeclFn) -> NodeId {
        self.enter();
        let r = self.scss_internal_parse_if_statement_impl(parse_statement);
        self.depth -= 1;
        r
    }

    // --- token helpers ---

    pub fn text(&self) -> &[u16] {
        self.scanner.text(&self.token)
    }

    pub fn peek_ident(&self, text: &str) -> bool {
        self.token.ty == TT::Ident && eq_lower(self.text(), text)
    }

    pub fn peek_keyword(&self, text: &str) -> bool {
        self.token.ty == TT::AtKeyword && eq_lower(self.text(), text)
    }

    pub fn peek_delim(&self, text: &str) -> bool {
        self.token.ty == TT::Delim && eq_str(self.text(), text)
    }

    pub fn peek(&self, ty: TT) -> bool {
        self.token.ty == ty
    }

    pub fn has_whitespace(&self) -> bool {
        match &self.prev_token {
            Some(p) => p.offset + p.len != self.token.offset,
            None => false,
        }
    }

    pub fn consume_token(&mut self) {
        self.prev_token = Some(self.token);
        self.token = self.scanner.scan();
    }

    pub fn accept_unicode_range(&mut self) -> bool {
        if let Some(t) = self.scanner.try_scan_unicode() {
            self.prev_token = Some(t);
            self.token = self.scanner.scan();
            return true;
        }
        false
    }

    pub fn mark(&self) -> Mark {
        Mark { prev: self.prev_token, curr: self.token, pos: self.scanner.pos() }
    }

    pub fn restore_at_mark(&mut self, mark: Mark) {
        self.prev_token = mark.prev;
        self.token = mark.curr;
        self.scanner.go_back_to(mark.pos);
    }

    pub fn accept_one_keyword(&mut self, keywords: &[&str]) -> bool {
        if self.token.ty == TT::AtKeyword {
            for kw in keywords {
                if eq_lower(self.text(), kw) {
                    self.consume_token();
                    return true;
                }
            }
        }
        false
    }

    pub fn accept(&mut self, ty: TT) -> bool {
        if self.token.ty == ty {
            self.consume_token();
            return true;
        }
        false
    }

    pub fn accept_ident(&mut self, text: &str) -> bool {
        if self.peek_ident(text) {
            self.consume_token();
            return true;
        }
        false
    }

    pub fn accept_keyword(&mut self, text: &str) -> bool {
        if self.peek_keyword(text) {
            self.consume_token();
            return true;
        }
        false
    }

    pub fn accept_delim(&mut self, text: &str) -> bool {
        if self.peek_delim(text) {
            self.consume_token();
            return true;
        }
        false
    }

    /// `acceptRegexp(regex)` for a predicate on the token text
    pub fn accept_regexp(&mut self, test: fn(&[u16]) -> bool) -> bool {
        if test(self.text()) {
            self.consume_token();
            return true;
        }
        false
    }

    pub fn parse_regexp(&mut self, test: fn(&[u16]) -> bool) -> NodeId {
        let node = self.create_node(NodeType::Identifier);
        while self.accept_regexp(test) {}
        self.finish(node)
    }

    pub fn accept_unquoted_string(&mut self) -> bool {
        let pos = self.scanner.pos();
        self.scanner.go_back_to(self.token.offset as usize);
        if let Some(unquoted) = self.scanner.scan_unquoted_string() {
            self.token = unquoted;
            self.consume_token();
            return true;
        }
        self.scanner.go_back_to(pos);
        false
    }

    pub fn resync(&mut self, resync_tokens: Option<&[TT]>, stop_tokens: Option<&[TT]>) -> bool {
        loop {
            if resync_tokens.is_some_and(|r| r.contains(&self.token.ty)) {
                self.consume_token();
                return true;
            } else if stop_tokens.is_some_and(|s| s.contains(&self.token.ty)) {
                return true;
            } else {
                if self.token.ty == TT::EOF {
                    return false;
                }
                self.token = self.scanner.scan();
            }
        }
    }

    pub fn create_node(&mut self, ty: NodeType) -> NodeId {
        self.ast.alloc(Class::Node, ty, self.token.offset, self.token.len)
    }

    pub fn create(&mut self, class: Class) -> NodeId {
        self.ast.alloc(class, NodeType::Undefined, self.token.offset, self.token.len)
    }

    pub fn finish(&mut self, node: NodeId) -> NodeId {
        if self.ast.class(node) != Class::Nodelist
            && let Some(prev) = self.prev_token
        {
            let prev_end = prev.offset + prev.len;
            let n = self.ast.get_mut(node);
            n.length = if prev_end > n.offset { prev_end - n.offset } else { 0 };
        }
        node
    }

    pub fn finish_err(&mut self, node: NodeId, error: ParseError) -> NodeId {
        self.finish_resync(node, error, None, None)
    }

    pub fn finish_resync(&mut self, node: NodeId, error: ParseError, resync: Option<&[TT]>, stop: Option<&[TT]>) -> NodeId {
        if self.ast.class(node) != Class::Nodelist {
            self.mark_error(node, error, resync, stop);
        }
        self.finish(node)
    }

    pub fn mark_error(&mut self, node: NodeId, error: ParseError, resync: Option<&[TT]>, stop: Option<&[TT]>) {
        if self.last_error_token != Some(self.token.id) {
            let (offset, length) = (self.token.offset, self.token.len);
            self.ast.add_issue(node, Issue { error, offset, length });
            self.last_error_token = Some(self.token.id);
        }
        if resync.is_some() || stop.is_some() {
            self.resync(resync, stop);
        }
    }

    // --- node helpers ---

    pub fn add_child(&mut self, node: NodeId, child: Option<NodeId>) -> bool {
        self.ast.add_child(node, child)
    }

    pub fn set_node(&mut self, node: NodeId, field: Field, child: Option<NodeId>) -> bool {
        self.ast.set_node(node, field, child, -1)
    }

    pub fn set_node_at(&mut self, node: NodeId, field: Field, child: Option<NodeId>, index: i32) -> bool {
        self.ast.set_node(node, field, child, index)
    }

    /// lazily created `Nodelist` getters (`getSelectors()`, `getArguments()`, ...)
    pub fn nodelist(&mut self, node: NodeId, field: Field) -> NodeId {
        if let Some(l) = self.ast.field(node, field) {
            return l;
        }
        let l = self.ast.new_nodelist(node);
        self.ast.set_field(node, field, l);
        l
    }

    /// `setDeclarations`
    pub fn set_declarations(&mut self, node: NodeId, decls: Option<NodeId>) -> bool {
        self.set_node(node, Field::Declarations, decls)
    }

    // --- entry ---

    pub fn parse_stylesheet(&mut self) -> NodeId {
        self.token = self.scanner.scan();
        self.css_parse_stylesheet()
    }

    pub fn call_decl(&mut self, f: DeclFn) -> Option<NodeId> {
        match f {
            DeclFn::RuleSetDeclaration => self.parse_rule_set_declaration(),
            DeclFn::KeyframeSelector => self.parse_keyframe_selector(),
            DeclFn::Declaration => self.parse_declaration(None),
            DeclFn::PageDeclaration => self.parse_page_declaration(),
            DeclFn::MediaDeclaration(n) => self.parse_media_declaration(n),
            DeclFn::SupportsDeclaration(n) => self.parse_supports_declaration(n),
            DeclFn::LayerDeclaration(n) => self.parse_layer_declaration(n),
            DeclFn::ContainerDeclaration(n) => self.parse_container_declaration(n),
            DeclFn::StartingStyleDeclaration(n) => self.parse_starting_style_declaration(n),
            DeclFn::ScopeDeclaration => self.parse_scope_declaration(),
            DeclFn::StylesheetStatement => self.parse_stylesheet_statement(false),
            DeclFn::ScssFunctionBodyDeclaration => self.scss_parse_function_body_declaration(),
            DeclFn::ScssMixinReferenceBodyStatement => self.scss_parse_mixin_reference_body_statement(),
            DeclFn::LessDetachedRuleSetBody => self.less_parse_detached_rule_set_body(),
            DeclFn::LessMixInBodyDeclaration => self.less_parse_mixin_body_declaration(),
        }
    }

    fn css_parse_stylesheet(&mut self) -> NodeId {
        let node = self.create(Class::Stylesheet);
        loop {
            let c = self.parse_charset();
            if !self.add_child(node, c) {
                break;
            }
        }
        let mut in_recovery = false;
        loop {
            loop {
                let mut has_match = false;
                if let Some(statement) = self.parse_stylesheet_statement(false) {
                    self.add_child(node, Some(statement));
                    has_match = true;
                    in_recovery = false;
                    if !self.peek(TT::EOF) && self.needs_semicolon_after(statement) && !self.accept(TT::SemiColon) {
                        self.mark_error(node, ParseError::SemiColonExpected, None, None);
                    }
                }
                while self.accept(TT::SemiColon) || self.accept(TT::CDO) || self.accept(TT::CDC) {
                    has_match = true;
                    in_recovery = false;
                }
                if !has_match {
                    break;
                }
            }
            if self.peek(TT::EOF) {
                break;
            }
            if !in_recovery {
                if self.peek(TT::AtKeyword) {
                    self.mark_error(node, ParseError::UnknownAtRule, None, None);
                } else {
                    self.mark_error(node, ParseError::RuleOrSelectorExpected, None, None);
                }
                in_recovery = true;
            }
            self.consume_token();
            if self.peek(TT::EOF) {
                break;
            }
        }
        self.finish(node)
    }

    pub fn parse_stylesheet_statement(&mut self, is_nested: bool) -> Option<NodeId> {
        match self.dialect {
            Dialect::Scss => self.scss_parse_stylesheet_statement(is_nested),
            Dialect::Less => self.less_parse_stylesheet_statement(is_nested),
            Dialect::Css => {
                if self.peek(TT::AtKeyword) {
                    return self.css_parse_stylesheet_at_statement(is_nested);
                }
                self.parse_ruleset(is_nested)
            }
        }
    }

    pub fn css_parse_stylesheet_at_statement(&mut self, is_nested: bool) -> Option<NodeId> {
        if let Some(n) = self.parse_import() {
            return Some(n);
        }
        if let Some(n) = self.parse_media(is_nested) {
            return Some(n);
        }
        if let Some(n) = self.parse_scope() {
            return Some(n);
        }
        if let Some(n) = self.parse_page() {
            return Some(n);
        }
        if let Some(n) = self.parse_font_face() {
            return Some(n);
        }
        if let Some(n) = self.parse_keyframe() {
            return Some(n);
        }
        if let Some(n) = self.parse_supports(is_nested) {
            return Some(n);
        }
        if let Some(n) = self.parse_layer(is_nested) {
            return Some(n);
        }
        if let Some(n) = self.parse_property_at_rule() {
            return Some(n);
        }
        if let Some(n) = self.parse_view_port() {
            return Some(n);
        }
        if let Some(n) = self.parse_namespace() {
            return Some(n);
        }
        if let Some(n) = self.parse_document() {
            return Some(n);
        }
        if let Some(n) = self.parse_container(is_nested) {
            return Some(n);
        }
        if let Some(n) = self.parse_starting_style_at_rule(is_nested) {
            return Some(n);
        }
        self.parse_unknown_at_rule()
    }

    pub fn try_parse_ruleset(&mut self, is_nested: bool) -> Option<NodeId> {
        let mark = self.mark();
        if self.parse_selector(is_nested).is_some() {
            while self.accept(TT::Comma) && self.parse_selector(is_nested).is_some() {}
            if self.accept(TT::CurlyL) {
                self.restore_at_mark(mark);
                return self.parse_ruleset(is_nested);
            }
        }
        self.restore_at_mark(mark);
        None
    }

    pub fn parse_ruleset(&mut self, is_nested: bool) -> Option<NodeId> {
        let node = self.create(Class::RuleSet);
        let selectors = self.nodelist(node, Field::Selectors);
        let s = self.parse_selector(is_nested);
        if !self.add_child(selectors, s) {
            return None;
        }
        while self.accept(TT::Comma) {
            let s = self.parse_selector(is_nested);
            if !self.add_child(selectors, s) {
                return Some(self.finish_err(node, ParseError::SelectorExpected));
            }
        }
        Some(self.parse_body(node, DeclFn::RuleSetDeclaration))
    }

    pub fn parse_rule_set_declaration_at_statement(&mut self) -> Option<NodeId> {
        if let Some(n) = self.parse_media(true) {
            return Some(n);
        }
        if let Some(n) = self.parse_scope() {
            return Some(n);
        }
        if let Some(n) = self.parse_supports(true) {
            return Some(n);
        }
        if let Some(n) = self.parse_layer(true) {
            return Some(n);
        }
        if let Some(n) = self.parse_container(true) {
            return Some(n);
        }
        if let Some(n) = self.parse_starting_style_at_rule(true) {
            return Some(n);
        }
        self.parse_unknown_at_rule()
    }

    pub fn parse_rule_set_declaration(&mut self) -> Option<NodeId> {
        match self.dialect {
            Dialect::Scss => self.scss_parse_rule_set_declaration(),
            Dialect::Less => self.less_parse_rule_set_declaration(),
            Dialect::Css => {
                if self.peek(TT::AtKeyword) {
                    return self.parse_rule_set_declaration_at_statement();
                }
                if !self.peek(TT::Ident) {
                    return self.parse_ruleset(true);
                }
                self.try_parse_ruleset(true).or_else(|| self.parse_declaration(None))
            }
        }
    }

    pub fn needs_semicolon_after(&self, node: NodeId) -> bool {
        use NodeType as T;
        match self.ast.ty(node) {
            T::Keyframe
            | T::ViewPort
            | T::Media
            | T::Ruleset
            | T::Namespace
            | T::If
            | T::For
            | T::Each
            | T::While
            | T::MixinDeclaration
            | T::FunctionDeclaration
            | T::MixinContentDeclaration
            | T::Scope => false,
            T::ExtendsReference
            | T::MixinContentReference
            | T::ReturnStatement
            | T::MediaQuery
            | T::Debug
            | T::Import
            | T::AtApplyRule
            | T::CustomPropertyDeclaration => true,
            T::VariableDeclaration => self.ast.get(node).needs_semicolon,
            T::MixinReference => self.ast.field(node, Field::Content).is_none(),
            T::Declaration => self.ast.field(node, Field::NestedProperties).is_none(),
            _ => false,
        }
    }

    fn parse_declarations_impl(&mut self, f: DeclFn) -> Option<NodeId> {
        let node = self.create(Class::Declarations);
        if !self.accept(TT::CurlyL) {
            return None;
        }
        let mut decl = self.call_decl(f);
        while self.add_child(node, decl) {
            if self.peek(TT::CurlyR) {
                break;
            }
            if self.needs_semicolon_after(decl.unwrap()) && !self.accept(TT::SemiColon) {
                return Some(self.finish_resync(
                    node,
                    ParseError::SemiColonExpected,
                    Some(&[TT::SemiColon, TT::CurlyR]),
                    None,
                ));
            }
            while self.accept(TT::SemiColon) {}
            decl = self.call_decl(f);
        }
        if !self.accept(TT::CurlyR) {
            return Some(self.finish_resync(
                node,
                ParseError::RightCurlyExpected,
                Some(&[TT::CurlyR, TT::SemiColon]),
                None,
            ));
        }
        Some(self.finish(node))
    }

    pub fn parse_body(&mut self, node: NodeId, f: DeclFn) -> NodeId {
        let decls = self.parse_declarations(f);
        if !self.set_declarations(node, decls) {
            return self.finish_resync(node, ParseError::LeftCurlyExpected, Some(&[TT::CurlyR, TT::SemiColon]), None);
        }
        self.finish(node)
    }

    fn parse_selector_impl(&mut self, is_nested: bool) -> Option<NodeId> {
        if self.dialect == Dialect::Less {
            return self.less_parse_selector(is_nested);
        }
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
            let c = self.parse_combinator();
            self.add_child(node, c);
        }
        if has_content { Some(self.finish(node)) } else { None }
    }

    pub fn parse_declaration(&mut self, stop_tokens: Option<&[TT]>) -> Option<NodeId> {
        if self.dialect == Dialect::Scss {
            return self.scss_parse_declaration(stop_tokens);
        }
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
        let e = self.parse_expr(false);
        if !self.set_node(node, Field::Value, e) {
            return Some(self.finish_err(node, ParseError::PropertyValueExpected));
        }
        let p = self.parse_prio();
        self.add_child(node, p);
        Some(self.finish(node))
    }

    pub fn try_parse_custom_property_declaration(&mut self, stop_tokens: Option<&[TT]>) -> Option<NodeId> {
        if !(self.peek(TT::Ident) && starts_with_dashdash(self.text())) {
            return None;
        }
        let node = self.create(Class::CustomPropertyDeclaration);
        let p = self.parse_property();
        if !self.set_node(node, Field::Property, p) {
            return None;
        }
        if !self.accept(TT::Colon) {
            return Some(self.finish_resync(node, ParseError::ColonExpected, Some(&[TT::Colon]), None));
        }
        if let Some(prev) = self.prev_token {
            self.ast.get_mut(node).colon_position = Some(prev.offset);
        }
        let mark = self.mark();
        if self.peek(TT::CurlyL) {
            let property_set = self.create(Class::CustomPropertySet);
            let declarations = self.parse_declarations(DeclFn::RuleSetDeclaration);
            if self.set_declarations(property_set, declarations) && !self.ast.is_erroneous(declarations.unwrap(), true) {
                let p = self.parse_prio();
                self.add_child(property_set, p);
                if self.peek(TT::SemiColon) {
                    self.finish(property_set);
                    self.set_node(node, Field::PropertySet, Some(property_set));
                    return Some(self.finish(node));
                }
            }
            self.restore_at_mark(mark);
        }
        let expression = self.parse_expr(false);
        if let Some(e) = expression
            && !self.ast.is_erroneous(e, true)
        {
            self.parse_prio();
            let stop = stop_tokens.unwrap_or(&[]);
            if stop.contains(&self.token.ty) || self.peek(TT::SemiColon) || self.peek(TT::EOF) {
                self.set_node(node, Field::Value, Some(e));
                return Some(self.finish(node));
            }
        }
        self.restore_at_mark(mark);
        let v = self.parse_custom_property_value(stop_tokens);
        self.add_child(node, Some(v));
        let p = self.parse_prio();
        self.add_child(node, p);
        if let Some(colon) = self.ast.get(node).colon_position
            && self.token.offset == colon + 1
        {
            return Some(self.finish_err(node, ParseError::PropertyValueExpected));
        }
        Some(self.finish(node))
    }

    fn parse_custom_property_value(&mut self, stop_tokens: Option<&[TT]>) -> NodeId {
        let stop_tokens = stop_tokens.unwrap_or(&[TT::CurlyR]);
        let node = self.create(Class::Node);
        let (mut curly, mut parens, mut brackets) = (0i32, 0i32, 0i32);
        loop {
            let top = curly == 0 && parens == 0 && brackets == 0;
            match self.token.ty {
                TT::SemiColon | TT::Exclamation => {
                    if top {
                        break;
                    }
                }
                TT::CurlyL => curly += 1,
                TT::CurlyR => {
                    curly -= 1;
                    if curly < 0 {
                        if stop_tokens.contains(&self.token.ty) && parens == 0 && brackets == 0 {
                            break;
                        }
                        return self.finish_err(node, ParseError::LeftCurlyExpected);
                    }
                }
                TT::ParenthesisL => parens += 1,
                TT::ParenthesisR => {
                    parens -= 1;
                    if parens < 0 {
                        if stop_tokens.contains(&self.token.ty) && brackets == 0 && curly == 0 {
                            break;
                        }
                        return self.finish_err(node, ParseError::LeftParenthesisExpected);
                    }
                }
                TT::BracketL => brackets += 1,
                TT::BracketR => {
                    brackets -= 1;
                    if brackets < 0 {
                        return self.finish_err(node, ParseError::LeftSquareBracketExpected);
                    }
                }
                TT::BadString => break,
                TT::EOF => {
                    let error = if brackets > 0 {
                        ParseError::RightSquareBracketExpected
                    } else if parens > 0 {
                        ParseError::RightParenthesisExpected
                    } else {
                        ParseError::RightCurlyExpected
                    };
                    return self.finish_err(node, error);
                }
                _ => {}
            }
            self.consume_token();
        }
        self.finish(node)
    }

    pub fn try_to_parse_declaration(&mut self, stop_tokens: Option<&[TT]>) -> Option<NodeId> {
        let mark = self.mark();
        if self.parse_property().is_some() && self.accept(TT::Colon) {
            self.restore_at_mark(mark);
            return self.parse_declaration(stop_tokens);
        }
        self.restore_at_mark(mark);
        None
    }

    pub fn parse_property(&mut self) -> Option<NodeId> {
        let node = self.create(Class::Property);
        let mark = self.mark();
        if (self.accept_delim("*") || self.accept_delim("_")) && self.has_whitespace() {
            self.restore_at_mark(mark);
            return None;
        }
        let id = self.parse_property_identifier();
        if self.set_node(node, Field::Identifier, id) {
            return Some(self.finish(node));
        }
        None
    }

    pub fn parse_property_identifier(&mut self) -> Option<NodeId> {
        if self.dialect == Dialect::Less {
            return self.less_parse_property_identifier(false);
        }
        self.parse_ident()
    }

    fn parse_charset(&mut self) -> Option<NodeId> {
        if !self.peek(TT::Charset) {
            return None;
        }
        let node = self.create(Class::Node);
        self.consume_token();
        if !self.accept(TT::String) {
            return Some(self.finish_err(node, ParseError::IdentifierExpected));
        }
        if !self.accept(TT::SemiColon) {
            return Some(self.finish_err(node, ParseError::SemiColonExpected));
        }
        Some(self.finish(node))
    }

    pub fn parse_import(&mut self) -> Option<NodeId> {
        match self.dialect {
            Dialect::Scss => return self.scss_parse_import(),
            Dialect::Less => return self.less_parse_import(),
            Dialect::Css => {}
        }
        if !self.peek_keyword("@import") {
            return None;
        }
        let node = self.create(Class::Import);
        self.consume_token();
        if !self.add_uri_or_string(node) {
            return Some(self.finish_err(node, ParseError::URIOrStringExpected));
        }
        Some(self.complete_parse_import(node))
    }

    /// `node.addChild(this._parseURILiteral()) || node.addChild(this._parseStringLiteral())`
    pub fn add_uri_or_string(&mut self, node: NodeId) -> bool {
        let u = self.parse_uri_literal();
        if self.add_child(node, u) {
            return true;
        }
        let s = self.parse_string_literal();
        self.add_child(node, s)
    }

    pub fn complete_parse_import(&mut self, node: NodeId) -> NodeId {
        if self.accept_ident("layer") && self.accept(TT::ParenthesisL) {
            let l = self.parse_layer_name();
            if !self.add_child(node, l) {
                return self.finish_resync(node, ParseError::IdentifierExpected, Some(&[TT::SemiColon]), None);
            }
            if !self.accept(TT::ParenthesisR) {
                return self.finish_resync(node, ParseError::RightParenthesisExpected, Some(&[TT::ParenthesisR]), Some(&[]));
            }
        }
        if self.accept_ident("supports") && self.accept(TT::ParenthesisL) {
            let d = match self.try_to_parse_declaration(None) {
                Some(d) => Some(d),
                None => Some(self.parse_supports_condition()),
            };
            self.add_child(node, d);
            if !self.accept(TT::ParenthesisR) {
                return self.finish_resync(node, ParseError::RightParenthesisExpected, Some(&[TT::ParenthesisR]), Some(&[]));
            }
        }
        if !self.peek(TT::SemiColon) && !self.peek(TT::EOF) {
            let m = self.parse_media_query_list();
            self.ast.adopt_child(node, m, -1);
        }
        self.finish(node)
    }

    fn parse_namespace(&mut self) -> Option<NodeId> {
        if !self.peek_keyword("@namespace") {
            return None;
        }
        let node = self.create(Class::Namespace);
        self.consume_token();
        let u = self.parse_uri_literal();
        if !self.add_child(node, u) {
            let i = self.parse_ident();
            self.add_child(node, i);
            if !self.add_uri_or_string(node) {
                return Some(self.finish_resync(node, ParseError::URIExpected, Some(&[TT::SemiColon]), None));
            }
        }
        if !self.accept(TT::SemiColon) {
            return Some(self.finish_err(node, ParseError::SemiColonExpected));
        }
        Some(self.finish(node))
    }

    pub fn parse_font_face(&mut self) -> Option<NodeId> {
        if !self.peek_keyword("@font-face") {
            return None;
        }
        let node = self.create(Class::FontFace);
        self.consume_token();
        Some(self.parse_body(node, DeclFn::RuleSetDeclaration))
    }

    fn parse_view_port(&mut self) -> Option<NodeId> {
        if !self.peek_keyword("@-ms-viewport") && !self.peek_keyword("@-o-viewport") && !self.peek_keyword("@viewport") {
            return None;
        }
        let node = self.create(Class::ViewPort);
        self.consume_token();
        Some(self.parse_body(node, DeclFn::RuleSetDeclaration))
    }

    pub fn parse_keyframe(&mut self) -> Option<NodeId> {
        if !(self.peek(TT::AtKeyword) && is_keyframe_keyword(self.text())) {
            return None;
        }
        let node = self.create(Class::Keyframe);
        let at_node = self.create(Class::Node);
        self.consume_token();
        let at_node = self.finish(at_node);
        self.set_node_at(node, Field::Keyword, Some(at_node), 0);
        // `if (atNode.matches('@-ms-keyframes')) this.markError(atNode, ParseError.UnknownKeyword)`
        // never fires: the text provider is only attached to the stylesheet after parsing, so
        // `matches` compares against 'unknown'
        let id = self.parse_keyframe_ident();
        if !self.set_node_at(node, Field::Identifier, id, 0) {
            return Some(self.finish_resync(node, ParseError::IdentifierExpected, Some(&[TT::CurlyR]), None));
        }
        Some(self.parse_body(node, DeclFn::KeyframeSelector))
    }

    pub fn parse_keyframe_ident(&mut self) -> Option<NodeId> {
        if self.dialect == Dialect::Less {
            return self.parse_ident().or_else(|| self.less_parse_variable(false, false));
        }
        self.parse_ident()
    }

    pub fn parse_keyframe_selector(&mut self) -> Option<NodeId> {
        match self.dialect {
            Dialect::Scss => self.scss_parse_keyframe_selector(),
            Dialect::Less => self.less_parse_detached_rule_set_mixin().or_else(|| self.css_parse_keyframe_selector()),
            Dialect::Css => self.css_parse_keyframe_selector(),
        }
    }

    fn css_parse_keyframe_selector(&mut self) -> Option<NodeId> {
        let node = self.create(Class::KeyframeSelector);
        let mut has_content = false;
        let i = self.parse_ident();
        if self.add_child(node, i) {
            has_content = true;
        }
        if self.accept(TT::Percentage) {
            has_content = true;
        }
        if !has_content {
            return None;
        }
        while self.accept(TT::Comma) {
            has_content = false;
            let i = self.parse_ident();
            if self.add_child(node, i) {
                has_content = true;
            }
            if self.accept(TT::Percentage) {
                has_content = true;
            }
            if !has_content {
                return Some(self.finish_err(node, ParseError::PercentageExpected));
            }
        }
        Some(self.parse_body(node, DeclFn::RuleSetDeclaration))
    }

    pub fn try_parse_keyframe_selector(&mut self) -> Option<NodeId> {
        let node = self.create(Class::KeyframeSelector);
        let pos = self.mark();
        let mut has_content = false;
        let i = self.parse_ident();
        if self.add_child(node, i) {
            has_content = true;
        }
        if self.accept(TT::Percentage) {
            has_content = true;
        }
        if !has_content {
            return None;
        }
        while self.accept(TT::Comma) {
            has_content = false;
            let i = self.parse_ident();
            if self.add_child(node, i) {
                has_content = true;
            }
            if self.accept(TT::Percentage) {
                has_content = true;
            }
            if !has_content {
                self.restore_at_mark(pos);
                return None;
            }
        }
        if !self.peek(TT::CurlyL) {
            self.restore_at_mark(pos);
            return None;
        }
        Some(self.parse_body(node, DeclFn::RuleSetDeclaration))
    }

    pub fn parse_property_at_rule(&mut self) -> Option<NodeId> {
        if !self.peek_keyword("@property") {
            return None;
        }
        let node = self.create(Class::PropertyAtRule);
        self.consume_token();
        if !(self.peek(TT::Ident) && starts_with_dashdash(self.text())) {
            return Some(self.finish_err(node, ParseError::IdentifierExpected));
        }
        let name = self.parse_ident();
        let Some(name) = name else {
            return Some(self.finish_err(node, ParseError::IdentifierExpected));
        };
        self.ast.adopt_child(node, name, -1);
        self.ast.set_field(node, Field::Name, name);
        Some(self.parse_body(node, DeclFn::Declaration))
    }

    pub fn parse_starting_style_at_rule(&mut self, is_nested: bool) -> Option<NodeId> {
        if !self.peek_keyword("@starting-style") {
            return None;
        }
        let node = self.create(Class::StartingStyleAtRule);
        self.consume_token();
        Some(self.parse_body(node, DeclFn::StartingStyleDeclaration(is_nested)))
    }

    fn parse_starting_style_declaration(&mut self, is_nested: bool) -> Option<NodeId> {
        self.nested_declaration(is_nested)
    }

    /// The shared body of `_parseStartingStyleDeclaration`, `_parseLayerDeclaration`,
    /// `_parseSupportsDeclaration`, `_parseContainerDeclaration` and (for CSS/SCSS)
    /// `_parseMediaDeclaration`
    fn nested_declaration(&mut self, is_nested: bool) -> Option<NodeId> {
        if is_nested {
            if let Some(n) = self.try_parse_ruleset(true) {
                return Some(n);
            }
            if let Some(n) = self.try_to_parse_declaration(None) {
                return Some(n);
            }
            return self.parse_stylesheet_statement(true);
        }
        self.parse_stylesheet_statement(false)
    }

    pub fn parse_layer(&mut self, is_nested: bool) -> Option<NodeId> {
        if !self.peek_keyword("@layer") {
            return None;
        }
        let node = self.create(Class::Layer);
        self.consume_token();
        let names = self.parse_layer_name_list();
        if let Some(names) = names {
            self.set_node(node, Field::Names, Some(names));
        }
        if (names.is_none() || self.ast.child_count(names.unwrap()) == 1) && self.peek(TT::CurlyL) {
            return Some(self.parse_body(node, DeclFn::LayerDeclaration(is_nested)));
        }
        if !self.accept(TT::SemiColon) {
            return Some(self.finish_err(node, ParseError::SemiColonExpected));
        }
        Some(self.finish(node))
    }

    fn parse_layer_declaration(&mut self, is_nested: bool) -> Option<NodeId> {
        self.nested_declaration(is_nested)
    }

    fn parse_layer_name_list(&mut self) -> Option<NodeId> {
        let node = self.create_node(NodeType::LayerNameList);
        let l = self.parse_layer_name();
        if !self.add_child(node, l) {
            return None;
        }
        while self.accept(TT::Comma) {
            let l = self.parse_layer_name();
            if !self.add_child(node, l) {
                return Some(self.finish_err(node, ParseError::IdentifierExpected));
            }
        }
        Some(self.finish(node))
    }

    fn parse_layer_name(&mut self) -> Option<NodeId> {
        let node = self.create_node(NodeType::LayerName);
        let i = self.parse_ident();
        if !self.add_child(node, i) {
            return None;
        }
        while !self.has_whitespace() && self.accept_delim(".") {
            if self.has_whitespace() {
                return Some(self.finish_err(node, ParseError::IdentifierExpected));
            }
            let i = self.parse_ident();
            if !self.add_child(node, i) {
                return Some(self.finish_err(node, ParseError::IdentifierExpected));
            }
        }
        Some(self.finish(node))
    }

    pub fn parse_supports(&mut self, is_nested: bool) -> Option<NodeId> {
        if !self.peek_keyword("@supports") {
            return None;
        }
        let node = self.create(Class::Supports);
        self.consume_token();
        let c = self.parse_supports_condition();
        self.add_child(node, Some(c));
        Some(self.parse_body(node, DeclFn::SupportsDeclaration(is_nested)))
    }

    fn parse_supports_declaration(&mut self, is_nested: bool) -> Option<NodeId> {
        self.nested_declaration(is_nested)
    }

    fn parse_supports_condition_impl(&mut self) -> NodeId {
        if self.dialect == Dialect::Scss
            && let Some(i) = self.scss_parse_interpolation()
        {
            return i;
        }
        self.css_parse_supports_condition()
    }

    fn css_parse_supports_condition(&mut self) -> NodeId {
        let node = self.create(Class::SupportsCondition);
        if self.accept_ident("not") {
            let c = self.parse_supports_condition_in_parens();
            self.add_child(node, Some(c));
        } else {
            let c = self.parse_supports_condition_in_parens();
            self.add_child(node, Some(c));
            if self.peek(TT::Ident) && (eq_ignore_case(self.text(), "and") || eq_ignore_case(self.text(), "or")) {
                let text = String::from_utf16_lossy(self.text()).to_lowercase();
                while self.accept_ident(&text) {
                    let c = self.parse_supports_condition_in_parens();
                    self.add_child(node, Some(c));
                }
            }
        }
        self.finish(node)
    }

    fn parse_supports_condition_in_parens(&mut self) -> NodeId {
        let node = self.create(Class::SupportsCondition);
        if self.accept(TT::ParenthesisL) {
            let d = self.try_to_parse_declaration(Some(&[TT::ParenthesisR]));
            if !self.add_child(node, d) {
                // `this._parseSupportsCondition()` always returns a node
                self.parse_supports_condition();
            }
            if !self.accept(TT::ParenthesisR) {
                return self.finish_resync(node, ParseError::RightParenthesisExpected, Some(&[TT::ParenthesisR]), Some(&[]));
            }
            return self.finish(node);
        } else if self.peek(TT::Ident) {
            let pos = self.mark();
            self.consume_token();
            if !self.has_whitespace() && self.accept(TT::ParenthesisL) {
                let mut open = 1;
                while self.token.ty != TT::EOF && open != 0 {
                    if self.token.ty == TT::ParenthesisL {
                        open += 1;
                    } else if self.token.ty == TT::ParenthesisR {
                        open -= 1;
                    }
                    self.consume_token();
                }
                return self.finish(node);
            } else {
                self.restore_at_mark(pos);
            }
        }
        self.finish_resync(node, ParseError::LeftParenthesisExpected, Some(&[]), Some(&[TT::ParenthesisL]))
    }

    pub fn parse_media_declaration(&mut self, is_nested: bool) -> Option<NodeId> {
        if self.dialect == Dialect::Less {
            return self.less_parse_media_declaration(is_nested);
        }
        self.nested_declaration(is_nested)
    }

    pub fn parse_media(&mut self, is_nested: bool) -> Option<NodeId> {
        if !self.peek_keyword("@media") {
            return None;
        }
        let node = self.create(Class::Media);
        self.consume_token();
        let l = self.parse_media_query_list();
        if !self.add_child(node, Some(l)) {
            return Some(self.finish_err(node, ParseError::MediaQueryExpected));
        }
        Some(self.parse_body(node, DeclFn::MediaDeclaration(is_nested)))
    }

    pub fn parse_media_query_list(&mut self) -> NodeId {
        let node = self.create(Class::Medialist);
        let q = self.parse_media_query();
        if !self.add_child(node, q) {
            return self.finish_err(node, ParseError::MediaQueryExpected);
        }
        while self.accept(TT::Comma) {
            let q = self.parse_media_query();
            if !self.add_child(node, q) {
                return self.finish_err(node, ParseError::MediaQueryExpected);
            }
        }
        self.finish(node)
    }

    fn parse_media_query(&mut self) -> Option<NodeId> {
        if self.dialect == Dialect::Less {
            if let Some(n) = self.css_parse_media_query() {
                return Some(n);
            }
            let node = self.create(Class::MediaQuery);
            let v = self.less_parse_variable(false, false);
            if self.add_child(node, v) {
                return Some(self.finish(node));
            }
            return None;
        }
        self.css_parse_media_query()
    }

    fn css_parse_media_query(&mut self) -> Option<NodeId> {
        let node = self.create(Class::MediaQuery);
        let pos = self.mark();
        self.accept_ident("not");
        if !self.peek(TT::ParenthesisL) {
            self.accept_ident("only");
            let i = self.parse_ident();
            if !self.add_child(node, i) {
                return None;
            }
            if self.accept_ident("and") {
                let c = self.parse_media_condition();
                self.add_child(node, Some(c));
            }
        } else {
            self.restore_at_mark(pos);
            let c = self.parse_media_condition();
            self.add_child(node, Some(c));
        }
        Some(self.finish(node))
    }

    fn parse_ratio(&mut self) -> Option<NodeId> {
        let pos = self.mark();
        let node = self.create(Class::RatioValue);
        self.parse_numeric()?;
        if !self.accept_delim("/") {
            self.restore_at_mark(pos);
            return None;
        }
        if self.parse_numeric().is_none() {
            return Some(self.finish_err(node, ParseError::NumberExpected));
        }
        Some(self.finish(node))
    }

    fn parse_media_condition_impl(&mut self) -> NodeId {
        if self.dialect == Dialect::Scss
            && let Some(i) = self.scss_parse_interpolation()
        {
            return i;
        }
        let node = self.create(Class::MediaCondition);
        self.accept_ident("not");
        let mut parse_expression = true;
        while parse_expression {
            if !self.accept(TT::ParenthesisL) {
                return self.finish_resync(node, ParseError::LeftParenthesisExpected, Some(&[]), Some(&[TT::CurlyL]));
            }
            if self.peek(TT::ParenthesisL) || self.peek_ident("not") {
                let c = self.parse_media_condition();
                self.add_child(node, Some(c));
            } else {
                let f = self.parse_media_feature();
                self.add_child(node, Some(f));
            }
            if !self.accept(TT::ParenthesisR) {
                return self.finish_resync(node, ParseError::RightParenthesisExpected, Some(&[]), Some(&[TT::CurlyL]));
            }
            parse_expression = self.accept_ident("and") || self.accept_ident("or");
        }
        self.finish(node)
    }

    fn parse_media_feature(&mut self) -> NodeId {
        const STOP: &[TT] = &[TT::ParenthesisR];
        let node = self.create(Class::MediaFeature);
        let name = self.parse_media_feature_name();
        if self.add_child(node, name) {
            if self.accept(TT::Colon) {
                let v = self.parse_media_feature_value();
                if !self.add_child(node, v) {
                    return self.finish_resync(node, ParseError::TermExpected, Some(&[]), Some(STOP));
                }
            } else if self.parse_media_feature_range_operator() {
                let v = self.parse_media_feature_value();
                if !self.add_child(node, v) {
                    return self.finish_resync(node, ParseError::TermExpected, Some(&[]), Some(STOP));
                }
                if self.parse_media_feature_range_operator() {
                    let v = self.parse_media_feature_value();
                    if !self.add_child(node, v) {
                        return self.finish_resync(node, ParseError::TermExpected, Some(&[]), Some(STOP));
                    }
                }
            }
        } else {
            let v = self.parse_media_feature_value();
            if self.add_child(node, v) {
                if !self.parse_media_feature_range_operator() {
                    return self.finish_resync(node, ParseError::OperatorExpected, Some(&[]), Some(STOP));
                }
                let n = self.parse_media_feature_name();
                if !self.add_child(node, n) {
                    return self.finish_resync(node, ParseError::IdentifierExpected, Some(&[]), Some(STOP));
                }
                if self.parse_media_feature_range_operator() {
                    let v = self.parse_media_feature_value();
                    if !self.add_child(node, v) {
                        return self.finish_resync(node, ParseError::TermExpected, Some(&[]), Some(STOP));
                    }
                }
            } else {
                return self.finish_resync(node, ParseError::IdentifierExpected, Some(&[]), Some(STOP));
            }
        }
        self.finish(node)
    }

    fn parse_media_feature_range_operator(&mut self) -> bool {
        if self.dialect == Dialect::Scss && (self.accept(TT::SmallerEqualsOperator) || self.accept(TT::GreaterEqualsOperator)) {
            return true;
        }
        if self.accept_delim("<") || self.accept_delim(">") {
            if !self.has_whitespace() {
                self.accept_delim("=");
            }
            return true;
        } else if self.accept_delim("=") {
            return true;
        }
        false
    }

    fn parse_media_feature_name(&mut self) -> Option<NodeId> {
        match self.dialect {
            Dialect::Scss => {
                if let Some(n) = self.scss_parse_module_member() {
                    return Some(n);
                }
                if let Some(n) = self.parse_function() {
                    return Some(n);
                }
                if let Some(n) = self.parse_ident() {
                    return Some(n);
                }
                self.scss_parse_variable()
            }
            Dialect::Less => self.parse_ident().or_else(|| self.less_parse_variable(false, false)),
            Dialect::Css => self.parse_ident(),
        }
    }

    fn parse_media_feature_value(&mut self) -> Option<NodeId> {
        self.parse_ratio().or_else(|| self.parse_term_expression())
    }

    pub fn parse_scope(&mut self) -> Option<NodeId> {
        if !self.peek_keyword("@scope") {
            return None;
        }
        let node = self.create(Class::Scope);
        self.consume_token();
        let l = self.parse_scope_limits();
        self.add_child(node, Some(l));
        Some(self.parse_body(node, DeclFn::ScopeDeclaration))
    }

    fn parse_scope_declaration(&mut self) -> Option<NodeId> {
        if let Some(n) = self.try_parse_ruleset(true) {
            return Some(n);
        }
        if let Some(n) = self.try_to_parse_declaration(None) {
            return Some(n);
        }
        self.parse_stylesheet_statement(true)
    }

    fn parse_scope_limits(&mut self) -> NodeId {
        let node = self.create(Class::ScopeLimits);
        if self.accept(TT::ParenthesisL) {
            let s = self.parse_selector(true);
            if !self.set_node(node, Field::ScopeStart, s) {
                return self.finish_resync(node, ParseError::SelectorExpected, Some(&[]), Some(&[TT::ParenthesisR]));
            }
            if !self.accept(TT::ParenthesisR) {
                return self.finish_resync(node, ParseError::RightParenthesisExpected, Some(&[]), Some(&[TT::CurlyL]));
            }
        }
        if self.accept_ident("to") {
            if !self.accept(TT::ParenthesisL) {
                return self.finish_resync(node, ParseError::LeftParenthesisExpected, Some(&[]), Some(&[TT::CurlyL]));
            }
            let s = self.parse_selector(true);
            if !self.set_node(node, Field::ScopeEnd, s) {
                return self.finish_resync(node, ParseError::SelectorExpected, Some(&[]), Some(&[TT::ParenthesisR]));
            }
            if !self.accept(TT::ParenthesisR) {
                return self.finish_resync(node, ParseError::RightParenthesisExpected, Some(&[]), Some(&[TT::CurlyL]));
            }
        }
        self.finish(node)
    }

    fn parse_page_declaration(&mut self) -> Option<NodeId> {
        self.parse_page_margin_box().or_else(|| self.parse_rule_set_declaration())
    }

    fn parse_page(&mut self) -> Option<NodeId> {
        if !self.peek_keyword("@page") {
            return None;
        }
        let node = self.create(Class::Page);
        self.consume_token();
        let s = self.parse_page_selector();
        if self.add_child(node, s) {
            while self.accept(TT::Comma) {
                let s = self.parse_page_selector();
                if !self.add_child(node, s) {
                    return Some(self.finish_err(node, ParseError::IdentifierExpected));
                }
            }
        }
        Some(self.parse_body(node, DeclFn::PageDeclaration))
    }

    fn parse_page_margin_box(&mut self) -> Option<NodeId> {
        if !self.peek(TT::AtKeyword) {
            return None;
        }
        let node = self.create(Class::PageBoxMarginBox);
        if !self.accept_one_keyword(&PAGE_BOX_DIRECTIVES) {
            self.mark_error(node, ParseError::UnknownAtRule, Some(&[]), Some(&[TT::CurlyL]));
        }
        Some(self.parse_body(node, DeclFn::RuleSetDeclaration))
    }

    fn parse_page_selector(&mut self) -> Option<NodeId> {
        if !self.peek(TT::Ident) && !self.peek(TT::Colon) {
            return None;
        }
        let node = self.create(Class::Node);
        let i = self.parse_ident();
        self.add_child(node, i);
        if self.accept(TT::Colon) {
            let i = self.parse_ident();
            if !self.add_child(node, i) {
                return Some(self.finish_err(node, ParseError::IdentifierExpected));
            }
        }
        Some(self.finish(node))
    }

    fn parse_document(&mut self) -> Option<NodeId> {
        if !self.peek_keyword("@-moz-document") {
            return None;
        }
        let node = self.create(Class::Document);
        self.consume_token();
        self.resync(Some(&[]), Some(&[TT::CurlyL]));
        Some(self.parse_body(node, DeclFn::StylesheetStatement))
    }

    fn parse_container_declaration(&mut self, is_nested: bool) -> Option<NodeId> {
        self.nested_declaration(is_nested)
    }

    pub fn parse_container(&mut self, is_nested: bool) -> Option<NodeId> {
        if !self.peek_keyword("@container") {
            return None;
        }
        let node = self.create(Class::Container);
        self.consume_token();
        let i = self.parse_ident();
        self.add_child(node, i);
        let q = self.parse_container_query();
        self.add_child(node, Some(q));
        Some(self.parse_body(node, DeclFn::ContainerDeclaration(is_nested)))
    }

    fn parse_container_query_impl(&mut self) -> NodeId {
        let node = self.create(Class::Node);
        if self.accept_ident("not") {
            let c = self.parse_container_query_in_parens();
            self.add_child(node, Some(c));
        } else {
            let c = self.parse_container_query_in_parens();
            self.add_child(node, Some(c));
            if self.peek_ident("and") {
                while self.accept_ident("and") {
                    let c = self.parse_container_query_in_parens();
                    self.add_child(node, Some(c));
                }
            } else if self.peek_ident("or") {
                while self.accept_ident("or") {
                    let c = self.parse_container_query_in_parens();
                    self.add_child(node, Some(c));
                }
            }
        }
        self.finish(node)
    }

    fn parse_container_query_in_parens(&mut self) -> NodeId {
        let node = self.create(Class::Node);
        if self.accept(TT::ParenthesisL) {
            if self.peek_ident("not") || self.peek(TT::ParenthesisL) {
                let q = self.parse_container_query();
                self.add_child(node, Some(q));
            } else {
                let f = self.parse_media_feature();
                self.add_child(node, Some(f));
            }
            if !self.accept(TT::ParenthesisR) {
                return self.finish_resync(node, ParseError::RightParenthesisExpected, Some(&[]), Some(&[TT::CurlyL]));
            }
        } else if self.accept_ident("style") {
            if self.has_whitespace() || !self.accept(TT::ParenthesisL) {
                return self.finish_resync(node, ParseError::LeftParenthesisExpected, Some(&[]), Some(&[TT::CurlyL]));
            }
            let q = self.parse_style_query();
            self.add_child(node, Some(q));
            if !self.accept(TT::ParenthesisR) {
                return self.finish_resync(node, ParseError::RightParenthesisExpected, Some(&[]), Some(&[TT::CurlyL]));
            }
        } else {
            return self.finish_resync(node, ParseError::LeftParenthesisExpected, Some(&[]), Some(&[TT::CurlyL]));
        }
        self.finish(node)
    }

    fn parse_style_query_impl(&mut self) -> NodeId {
        let node = self.create(Class::Node);
        if self.accept_ident("not") {
            let s = self.parse_style_in_parens();
            self.add_child(node, Some(s));
        } else if self.peek(TT::ParenthesisL) {
            let s = self.parse_style_in_parens();
            self.add_child(node, Some(s));
            if self.peek_ident("and") {
                while self.accept_ident("and") {
                    let s = self.parse_style_in_parens();
                    self.add_child(node, Some(s));
                }
            } else if self.peek_ident("or") {
                while self.accept_ident("or") {
                    let s = self.parse_style_in_parens();
                    self.add_child(node, Some(s));
                }
            }
        } else {
            let d = self.parse_declaration(Some(&[TT::ParenthesisR]));
            self.add_child(node, d);
        }
        self.finish(node)
    }

    fn parse_style_in_parens(&mut self) -> NodeId {
        let node = self.create(Class::Node);
        if self.accept(TT::ParenthesisL) {
            let q = self.parse_style_query();
            self.add_child(node, Some(q));
            if !self.accept(TT::ParenthesisR) {
                return self.finish_resync(node, ParseError::RightParenthesisExpected, Some(&[]), Some(&[TT::CurlyL]));
            }
        } else {
            return self.finish_resync(node, ParseError::LeftParenthesisExpected, Some(&[]), Some(&[TT::CurlyL]));
        }
        self.finish(node)
    }

    pub fn parse_unknown_at_rule(&mut self) -> Option<NodeId> {
        if !self.peek(TT::AtKeyword) {
            return None;
        }
        let node = self.create(Class::UnknownAtRule);
        let name = self.parse_unknown_at_rule_name();
        self.add_child(node, Some(name));
        let (mut curly_l_count, mut curly, mut parens, mut brackets) = (0i32, 0i32, 0i32, 0i32);
        loop {
            match self.token.ty {
                TT::SemiColon => {
                    if curly == 0 && parens == 0 && brackets == 0 {
                        break;
                    }
                }
                TT::EOF => {
                    return Some(if curly > 0 {
                        self.finish_err(node, ParseError::RightCurlyExpected)
                    } else if brackets > 0 {
                        self.finish_err(node, ParseError::RightSquareBracketExpected)
                    } else if parens > 0 {
                        self.finish_err(node, ParseError::RightParenthesisExpected)
                    } else {
                        self.finish(node)
                    });
                }
                TT::CurlyL => {
                    curly_l_count += 1;
                    curly += 1;
                }
                TT::CurlyR => {
                    curly -= 1;
                    if curly_l_count > 0 && curly == 0 {
                        self.consume_token();
                        if brackets > 0 {
                            return Some(self.finish_err(node, ParseError::RightSquareBracketExpected));
                        } else if parens > 0 {
                            return Some(self.finish_err(node, ParseError::RightParenthesisExpected));
                        }
                        break;
                    }
                    if curly < 0 {
                        if parens == 0 && brackets == 0 {
                            break;
                        }
                        return Some(self.finish_err(node, ParseError::LeftCurlyExpected));
                    }
                }
                TT::ParenthesisL => parens += 1,
                TT::ParenthesisR => {
                    parens -= 1;
                    if parens < 0 {
                        return Some(self.finish_err(node, ParseError::LeftParenthesisExpected));
                    }
                }
                TT::BracketL => brackets += 1,
                TT::BracketR => {
                    brackets -= 1;
                    if brackets < 0 {
                        return Some(self.finish_err(node, ParseError::LeftSquareBracketExpected));
                    }
                }
                _ => {}
            }
            self.consume_token();
        }
        // note: not `finish`ed
        Some(node)
    }

    fn parse_unknown_at_rule_name(&mut self) -> NodeId {
        let node = self.create(Class::Node);
        if self.accept(TT::AtKeyword) {
            return self.finish(node);
        }
        node
    }

    pub fn parse_operator(&mut self) -> Option<NodeId> {
        match self.dialect {
            Dialect::Scss => {
                if self.peek(TT::EqualsOperator)
                    || self.peek(TT::NotEqualsOperator)
                    || self.peek(TT::GreaterEqualsOperator)
                    || self.peek(TT::SmallerEqualsOperator)
                    || self.peek_delim(">")
                    || self.peek_delim("<")
                    || self.peek_ident("and")
                    || self.peek_ident("or")
                    || self.peek_delim("%")
                {
                    let node = self.create_node(NodeType::Operator);
                    self.consume_token();
                    return Some(self.finish(node));
                }
            }
            Dialect::Less => {
                if let Some(n) = self.less_parse_guard_operator() {
                    return Some(n);
                }
            }
            Dialect::Css => {}
        }
        if self.peek_delim("/")
            || self.peek_delim("*")
            || self.peek_delim("+")
            || self.peek_delim("-")
            || self.peek(TT::Dashmatch)
            || self.peek(TT::Includes)
            || self.peek(TT::SubstringOperator)
            || self.peek(TT::PrefixOperator)
            || self.peek(TT::SuffixOperator)
            || self.peek_delim("=")
        {
            let node = self.create_node(NodeType::Operator);
            self.consume_token();
            return Some(self.finish(node));
        }
        None
    }

    fn parse_unary_operator(&mut self) -> Option<NodeId> {
        if self.dialect == Dialect::Scss && self.peek_ident("not") {
            let node = self.create(Class::Node);
            self.consume_token();
            return Some(self.finish(node));
        }
        if !self.peek_delim("+") && !self.peek_delim("-") {
            return None;
        }
        let node = self.create(Class::Node);
        self.consume_token();
        Some(self.finish(node))
    }

    pub fn parse_combinator(&mut self) -> Option<NodeId> {
        if self.peek_delim(">") {
            let node = self.create(Class::Node);
            self.consume_token();
            let mark = self.mark();
            if !self.has_whitespace() && self.accept_delim(">") {
                if !self.has_whitespace() && self.accept_delim(">") {
                    self.ast.get_mut(node).ty = NodeType::SelectorCombinatorShadowPiercingDescendant;
                    return Some(self.finish(node));
                }
                self.restore_at_mark(mark);
            }
            self.ast.get_mut(node).ty = NodeType::SelectorCombinatorParent;
            return Some(self.finish(node));
        } else if self.peek_delim("+") {
            let node = self.create(Class::Node);
            self.consume_token();
            self.ast.get_mut(node).ty = NodeType::SelectorCombinatorSibling;
            return Some(self.finish(node));
        } else if self.peek_delim("~") {
            let node = self.create(Class::Node);
            self.consume_token();
            self.ast.get_mut(node).ty = NodeType::SelectorCombinatorAllSiblings;
            return Some(self.finish(node));
        } else if self.peek_delim("/") {
            let node = self.create(Class::Node);
            self.consume_token();
            let mark = self.mark();
            if !self.has_whitespace() && self.accept_ident("deep") && !self.has_whitespace() && self.accept_delim("/") {
                self.ast.get_mut(node).ty = NodeType::SelectorCombinatorShadowPiercingDescendant;
                return Some(self.finish(node));
            }
            self.restore_at_mark(mark);
        }
        None
    }

    pub fn parse_simple_selector(&mut self) -> Option<NodeId> {
        let node = self.create(Class::SimpleSelector);
        let mut c = 0;
        let first = match self.parse_element_name() {
            Some(n) => Some(n),
            None => self.parse_nesting_selector(),
        };
        if self.add_child(node, first) {
            c += 1;
        }
        while c == 0 || !self.has_whitespace() {
            let b = self.parse_simple_selector_body();
            if !self.add_child(node, b) {
                break;
            }
            c += 1;
        }
        if c > 0 { Some(self.finish(node)) } else { None }
    }

    pub fn parse_nesting_selector(&mut self) -> Option<NodeId> {
        if self.dialect != Dialect::Css {
            // the SCSS and LESS overrides are identical
            if self.peek_delim("&") {
                let node = self.create_node(NodeType::SelectorCombinator);
                self.consume_token();
                while !self.has_whitespace()
                    && (self.accept_delim("-") || self.accept(TT::Num) || self.accept(TT::Dimension) || {
                        let i = self.parse_ident();
                        self.add_child(node, i)
                    } || self.accept_delim("&"))
                {}
                return Some(self.finish(node));
            }
            return None;
        }
        if self.peek_delim("&") {
            let node = self.create_node(NodeType::SelectorCombinator);
            self.consume_token();
            return Some(self.finish(node));
        }
        None
    }

    fn parse_simple_selector_body(&mut self) -> Option<NodeId> {
        if self.dialect == Dialect::Scss
            && let Some(n) = self.scss_parse_selector_placeholder()
        {
            return Some(n);
        }
        if let Some(n) = self.parse_pseudo() {
            return Some(n);
        }
        if let Some(n) = self.parse_hash() {
            return Some(n);
        }
        if let Some(n) = self.parse_class() {
            return Some(n);
        }
        self.parse_attrib()
    }

    pub fn parse_selector_ident(&mut self) -> Option<NodeId> {
        if self.dialect == Dialect::Less {
            return self.less_parse_selector_ident();
        }
        self.parse_ident()
    }

    fn parse_hash(&mut self) -> Option<NodeId> {
        if !self.peek(TT::Hash) && !self.peek_delim("#") {
            return None;
        }
        let node = self.create_node(NodeType::IdentifierSelector);
        if self.accept_delim("#") {
            if self.has_whitespace() {
                return Some(self.finish_err(node, ParseError::IdentifierExpected));
            }
            let i = self.parse_selector_ident();
            if !self.add_child(node, i) {
                return Some(self.finish_err(node, ParseError::IdentifierExpected));
            }
        } else {
            self.consume_token();
        }
        Some(self.finish(node))
    }

    fn parse_class(&mut self) -> Option<NodeId> {
        if !self.peek_delim(".") {
            return None;
        }
        let node = self.create_node(NodeType::ClassSelector);
        self.consume_token();
        if self.has_whitespace() {
            return Some(self.finish_err(node, ParseError::IdentifierExpected));
        }
        let i = self.parse_selector_ident();
        if !self.add_child(node, i) {
            return Some(self.finish_err(node, ParseError::IdentifierExpected));
        }
        Some(self.finish(node))
    }

    pub fn parse_element_name(&mut self) -> Option<NodeId> {
        if self.dialect == Dialect::Scss {
            let pos = self.mark();
            let node = self.css_parse_element_name();
            if node.is_some() && !self.has_whitespace() && self.peek(TT::ParenthesisL) {
                self.restore_at_mark(pos);
                return None;
            }
            return node;
        }
        self.css_parse_element_name()
    }

    fn css_parse_element_name(&mut self) -> Option<NodeId> {
        let pos = self.mark();
        let node = self.create_node(NodeType::ElementNameSelector);
        let p = self.parse_namespace_prefix();
        self.add_child(node, p);
        let i = self.parse_selector_ident();
        if !self.add_child(node, i) && !self.accept_delim("*") {
            self.restore_at_mark(pos);
            return None;
        }
        Some(self.finish(node))
    }

    fn parse_namespace_prefix(&mut self) -> Option<NodeId> {
        let pos = self.mark();
        let node = self.create_node(NodeType::NamespacePrefix);
        let i = self.parse_ident();
        if !self.add_child(node, i) {
            self.accept_delim("*");
        }
        if !self.accept_delim("|") {
            self.restore_at_mark(pos);
            return None;
        }
        Some(self.finish(node))
    }

    fn parse_attrib(&mut self) -> Option<NodeId> {
        if !self.peek(TT::BracketL) {
            return None;
        }
        let node = self.create(Class::AttributeSelector);
        self.consume_token();
        let p = self.parse_namespace_prefix();
        self.set_node(node, Field::NamespacePrefix, p);
        let i = self.parse_ident();
        if !self.set_node(node, Field::Identifier, i) {
            return Some(self.finish_err(node, ParseError::IdentifierExpected));
        }
        let o = self.parse_operator();
        if self.set_node(node, Field::Operator, o) {
            let v = self.parse_binary_expr(None, None);
            self.set_node(node, Field::Value, v);
            self.accept_ident("i");
            self.accept_ident("s");
        }
        if !self.accept(TT::BracketR) {
            return Some(self.finish_err(node, ParseError::RightSquareBracketExpected));
        }
        Some(self.finish(node))
    }

    /// `tryAsSelector` in `_parsePseudo`
    fn try_as_selector(&mut self) -> Option<NodeId> {
        let pos = self.mark();
        let selectors = self.create_node(NodeType::SelectorList);
        let result = (|| {
            let s = self.parse_selector(true);
            if !self.add_child(selectors, s) {
                return None;
            }
            loop {
                if !self.accept(TT::Comma) {
                    break;
                }
                let s = self.parse_selector(true);
                if !self.add_child(selectors, s) {
                    break;
                }
            }
            if self.peek(TT::ParenthesisR) {
                return Some(self.finish(selectors));
            }
            None
        })();
        if result.is_none() {
            self.restore_at_mark(pos);
        }
        result
    }

    pub fn parse_pseudo(&mut self) -> Option<NodeId> {
        if self.dialect == Dialect::Less {
            return self.less_parse_pseudo();
        }
        self.css_parse_pseudo()
    }

    pub fn css_parse_pseudo(&mut self) -> Option<NodeId> {
        let node = self.try_parse_pseudo_identifier()?;
        if !self.has_whitespace() && self.accept(TT::ParenthesisL) {
            let s = self.try_as_selector();
            let has_selector = self.add_child(node, s);
            if !has_selector {
                while !self.peek_ident("of") && {
                    let t = self.parse_term();
                    self.add_child(node, t) || {
                        let o = self.parse_operator();
                        self.add_child(node, o)
                    }
                } {}
                if self.accept_ident("of") {
                    let s = self.try_as_selector();
                    if !self.add_child(node, s) {
                        return Some(self.finish_err(node, ParseError::SelectorExpected));
                    }
                }
            }
            if !self.accept(TT::ParenthesisR) {
                return Some(self.finish_err(node, ParseError::RightParenthesisExpected));
            }
        }
        Some(self.finish(node))
    }

    fn try_parse_pseudo_identifier(&mut self) -> Option<NodeId> {
        if self.dialect == Dialect::Scss
            && let Some(i) = self.scss_parse_interpolation()
        {
            return Some(i);
        }
        if !self.peek(TT::Colon) {
            return None;
        }
        let pos = self.mark();
        let node = self.create_node(NodeType::PseudoSelector);
        self.consume_token();
        if self.has_whitespace() {
            self.restore_at_mark(pos);
            return None;
        }
        self.accept(TT::Colon);
        if self.has_whitespace() {
            return Some(self.finish_err(node, ParseError::IdentifierExpected));
        }
        let i = self.parse_ident();
        if !self.add_child(node, i) {
            return Some(self.finish_err(node, ParseError::IdentifierExpected));
        }
        Some(self.finish(node))
    }

    pub fn try_parse_prio(&mut self) -> Option<NodeId> {
        let mark = self.mark();
        if let Some(p) = self.parse_prio() {
            return Some(p);
        }
        self.restore_at_mark(mark);
        None
    }

    pub fn parse_prio(&mut self) -> Option<NodeId> {
        if !self.peek(TT::Exclamation) {
            return None;
        }
        let node = self.create_node(NodeType::Prio);
        if self.accept(TT::Exclamation) && self.accept_ident("important") {
            return Some(self.finish(node));
        }
        None
    }

    pub fn parse_expr(&mut self, stop_on_comma: bool) -> Option<NodeId> {
        let node = self.create(Class::Expression);
        let b = self.parse_binary_expr(None, None);
        if !self.add_child(node, b) {
            return None;
        }
        loop {
            if self.peek(TT::Comma) {
                if stop_on_comma {
                    return Some(self.finish(node));
                }
                self.consume_token();
            }
            let b = self.parse_binary_expr(None, None);
            if !self.add_child(node, b) {
                break;
            }
        }
        Some(self.finish(node))
    }

    fn parse_unicode_range(&mut self) -> Option<NodeId> {
        if !self.peek_ident("u") {
            return None;
        }
        let node = self.create(Class::UnicodeRange);
        if !self.accept_unicode_range() {
            return None;
        }
        Some(self.finish(node))
    }

    fn parse_named_line(&mut self) -> Option<NodeId> {
        if !self.peek(TT::BracketL) {
            return None;
        }
        let node = self.create_node(NodeType::GridLine);
        self.consume_token();
        loop {
            let i = self.parse_ident();
            if !self.add_child(node, i) {
                break;
            }
        }
        if !self.accept(TT::BracketR) {
            return Some(self.finish_err(node, ParseError::RightSquareBracketExpected));
        }
        Some(self.finish(node))
    }

    fn parse_binary_expr_impl(&mut self, preparsed_left: Option<NodeId>, preparsed_oper: Option<NodeId>) -> Option<NodeId> {
        let mut node = self.create(Class::BinaryExpression);
        let left = match preparsed_left {
            Some(l) => Some(l),
            None => self.parse_term(),
        };
        if !self.set_node(node, Field::Left, left) {
            return None;
        }
        let oper = match preparsed_oper {
            Some(o) => Some(o),
            None => self.parse_operator(),
        };
        if !self.set_node(node, Field::Operator, oper) {
            return Some(self.finish(node));
        }
        let right = self.parse_term();
        if !self.set_node(node, Field::Right, right) {
            return Some(self.finish_err(node, ParseError::TermExpected));
        }
        node = self.finish(node);
        if let Some(operator) = self.parse_operator() {
            node = self.parse_binary_expr(Some(node), Some(operator)).unwrap();
        }
        Some(self.finish(node))
    }

    pub fn parse_term(&mut self) -> Option<NodeId> {
        let node = self.create(Class::Term);
        let o = self.parse_unary_operator();
        self.set_node(node, Field::Operator, o);
        let e = self.parse_term_expression();
        if self.set_node(node, Field::Expression, e) {
            return Some(self.finish(node));
        }
        None
    }

    pub fn parse_term_expression(&mut self) -> Option<NodeId> {
        match self.dialect {
            Dialect::Scss => {
                if let Some(n) = self.scss_parse_module_member() {
                    return Some(n);
                }
                if let Some(n) = self.scss_parse_variable() {
                    return Some(n);
                }
                if let Some(n) = self.parse_nesting_selector() {
                    return Some(n);
                }
                self.css_parse_term_expression()
            }
            Dialect::Less => {
                if let Some(n) = self.less_parse_variable(false, false) {
                    return Some(n);
                }
                if let Some(n) = self.less_parse_escaped() {
                    return Some(n);
                }
                if let Some(n) = self.css_parse_term_expression() {
                    return Some(n);
                }
                self.less_try_parse_mixin_reference(false)
            }
            Dialect::Css => self.css_parse_term_expression(),
        }
    }

    fn css_parse_term_expression(&mut self) -> Option<NodeId> {
        if let Some(n) = self.parse_uri_literal() {
            return Some(n);
        }
        if let Some(n) = self.parse_unicode_range() {
            return Some(n);
        }
        if let Some(n) = self.parse_function() {
            return Some(n);
        }
        if let Some(n) = self.parse_ident() {
            return Some(n);
        }
        if let Some(n) = self.parse_string_literal() {
            return Some(n);
        }
        if let Some(n) = self.parse_numeric() {
            return Some(n);
        }
        if let Some(n) = self.parse_hex_color() {
            return Some(n);
        }
        if let Some(n) = self.parse_operation() {
            return Some(n);
        }
        self.parse_named_line()
    }

    fn parse_operation(&mut self) -> Option<NodeId> {
        if !self.peek(TT::ParenthesisL) {
            return None;
        }
        let node = self.create(Class::Node);
        self.consume_token();
        if self.dialect == Dialect::Scss {
            loop {
                let e = self.scss_parse_list_element();
                if !self.add_child(node, e) {
                    break;
                }
                self.accept(TT::Comma);
            }
        } else {
            let e = self.parse_expr(false);
            self.add_child(node, e);
        }
        if !self.accept(TT::ParenthesisR) {
            return Some(self.finish_err(node, ParseError::RightParenthesisExpected));
        }
        Some(self.finish(node))
    }

    pub fn parse_numeric(&mut self) -> Option<NodeId> {
        if matches!(
            self.token.ty,
            TT::Num
                | TT::Percentage
                | TT::Resolution
                | TT::Length
                | TT::EMS
                | TT::EXS
                | TT::Angle
                | TT::Time
                | TT::Dimension
                | TT::ContainerQueryLength
                | TT::Freq
        ) {
            let node = self.create(Class::NumericValue);
            self.consume_token();
            return Some(self.finish(node));
        }
        None
    }

    pub fn parse_string_literal(&mut self) -> Option<NodeId> {
        if !self.peek(TT::String) && !self.peek(TT::BadString) {
            return None;
        }
        let node = self.create_node(NodeType::StringLiteral);
        self.consume_token();
        Some(self.finish(node))
    }

    pub fn parse_uri_literal(&mut self) -> Option<NodeId> {
        if !(self.peek(TT::Ident) && (eq_ignore_case(self.text(), "url") || eq_ignore_case(self.text(), "url-prefix"))) {
            return None;
        }
        let pos = self.mark();
        let node = self.create_node(NodeType::URILiteral);
        self.accept(TT::Ident);
        if self.has_whitespace() || !self.peek(TT::ParenthesisL) {
            self.restore_at_mark(pos);
            return None;
        }
        self.scanner.in_url = true;
        self.consume_token();
        let a = self.parse_url_argument();
        self.add_child(node, a);
        self.scanner.in_url = false;
        if !self.accept(TT::ParenthesisR) {
            return Some(self.finish_err(node, ParseError::RightParenthesisExpected));
        }
        Some(self.finish(node))
    }

    fn parse_url_argument(&mut self) -> Option<NodeId> {
        if self.dialect != Dialect::Css {
            // SCSS and LESS
            let pos = self.mark();
            let node = self.css_parse_url_argument();
            if node.is_none() || !self.peek(TT::ParenthesisR) {
                self.restore_at_mark(pos);
                let node = self.create(Class::Node);
                let b = self.parse_binary_expr(None, None);
                self.add_child(node, b);
                return Some(self.finish(node));
            }
            return node;
        }
        self.css_parse_url_argument()
    }

    fn css_parse_url_argument(&mut self) -> Option<NodeId> {
        let node = self.create(Class::Node);
        if !self.accept(TT::String) && !self.accept(TT::BadString) && !self.accept_unquoted_string() {
            return None;
        }
        Some(self.finish(node))
    }

    pub fn parse_ident(&mut self) -> Option<NodeId> {
        if self.dialect == Dialect::Scss {
            return self.scss_parse_ident();
        }
        if !self.peek(TT::Ident) {
            return None;
        }
        let node = self.create(Class::Identifier);
        self.consume_token();
        Some(self.finish(node))
    }

    pub fn parse_function(&mut self) -> Option<NodeId> {
        if self.dialect == Dialect::Less {
            return self.less_parse_function();
        }
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
        let a = self.parse_function_argument();
        if self.add_child(args, a) {
            while self.accept(TT::Comma) {
                if self.peek(TT::ParenthesisR) {
                    break;
                }
                let a = self.parse_function_argument();
                let args = self.nodelist(node, Field::Arguments);
                if !self.add_child(args, a) {
                    self.mark_error(node, ParseError::ExpressionExpected, None, None);
                }
            }
        }
        if !self.accept(TT::ParenthesisR) {
            return Some(self.finish_err(node, ParseError::RightParenthesisExpected));
        }
        Some(self.finish(node))
    }

    pub fn parse_function_identifier(&mut self) -> Option<NodeId> {
        if self.dialect == Dialect::Less && self.peek_delim("%") {
            let node = self.create(Class::Identifier);
            self.consume_token();
            return Some(self.finish(node));
        }
        if !self.peek(TT::Ident) {
            return None;
        }
        let node = self.create(Class::Identifier);
        if self.accept_ident("progid") {
            if self.accept(TT::Colon) {
                while self.accept(TT::Ident) && self.accept_delim(".") {}
            }
            return Some(self.finish(node));
        }
        self.consume_token();
        Some(self.finish(node))
    }

    fn parse_function_argument(&mut self) -> Option<NodeId> {
        if self.dialect == Dialect::Scss {
            return self.scss_parse_function_argument();
        }
        let node = self.create(Class::FunctionArgument);
        let e = self.parse_expr(true);
        if self.set_node_at(node, Field::Value, e, 0) {
            return Some(self.finish(node));
        }
        None
    }

    fn parse_hex_color(&mut self) -> Option<NodeId> {
        if self.peek(TT::Hash) && is_hex_color(self.text()) {
            let node = self.create(Class::HexColorValue);
            self.consume_token();
            return Some(self.finish(node));
        }
        None
    }
}

/// `/^--/`
pub fn starts_with_dashdash(t: &[u16]) -> bool {
    t.len() >= 2 && t[0] == b'-' as u16 && t[1] == b'-' as u16
}

/// `/^@(\-(webkit|ms|moz|o)\-)?keyframes$/i`
fn is_keyframe_keyword(t: &[u16]) -> bool {
    ["@keyframes", "@-webkit-keyframes", "@-ms-keyframes", "@-moz-keyframes", "@-o-keyframes"]
        .iter()
        .any(|k| eq_ignore_case(t, k))
}

/// `/^#([A-Fa-f0-9]{3}|[A-Fa-f0-9]{4}|[A-Fa-f0-9]{6}|[A-Fa-f0-9]{8})$/`
fn is_hex_color(t: &[u16]) -> bool {
    matches!(t.len(), 4 | 5 | 7 | 9)
        && t[0] == b'#' as u16
        && t[1..].iter().all(|&c| c < 0x80 && (c as u8).is_ascii_hexdigit())
}

/// `textProvider(offset, length)` = `text.substr(offset, length)`
pub fn node_text(src: &[u16], offset: i32, length: i32) -> &[u16] {
    let len = src.len() as i64;
    let mut start = offset as i64;
    if start < 0 {
        start = (len + start).max(0);
    }
    let start = start.min(len);
    let end = (start + (length as i64).max(0)).min(len);
    &src[start as usize..end as usize]
}
