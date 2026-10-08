//! Port of `htmlxtojsx_v2/index.ts` (`convertHtmlxToJsx`) and its node handlers.
//!
//! Walks the legacy AST in the order `estree-walker` visits the JS objects (object key
//! order), including the JS expressions inside (through [`EsWalker`]).

use std::collections::{HashMap, HashSet};

use oxc_ast::ast::Expression;
use oxc_ast_visit::Visit;
use oxc_span::GetSpan;

use super::elements::{Elements, Kind, NodeInfo};
use super::eswalk::{Ctx, EsHandler, EsWalker};
use super::htmlx::Verbatim;
use super::transform::*;
use crate::ast::{Ast, Attr, Chunk, Declaration, Expr, Node, Pattern};
use crate::js::{JsComment, JsExpr};
use crate::legacy::*;
use crate::magic_string::MagicString;

pub struct Options {
    pub typings_namespace: String,
    pub preserve_attribute_case: bool,
    pub svelte5_plus: bool,
    pub emit_jsdoc: bool,
    pub is_ts_file: bool,
    /// `mode === 'ts'`
    pub mode_ts: bool,
    pub accessors: bool,
}

static SVG_ATTRIBUTES: std::sync::LazyLock<HashSet<&'static str>> =
    std::sync::LazyLock::new(|| include_str!("svgattributes.txt").split_whitespace().collect());

const NUMBER_ONLY_ATTRIBUTES: &[&str] = &[
    "aria-colcount", "aria-colindex", "aria-colspan", "aria-level", "aria-posinset", "aria-rowcount",
    "aria-rowindex", "aria-rowspan", "aria-setsize", "aria-valuemax", "aria-valuemin", "aria-valuenow", "results",
    "span", "marginheight", "marginwidth", "maxlength", "minlength", "currenttime", "defaultplaybackrate",
    "volume", "high", "low", "optimum", "start", "size", "border", "cols", "rows", "colspan", "rowspan", "tabindex",
];

const ONE_WAY_BINDINGS: &[&str] = &[
    "clientWidth", "clientHeight", "offsetWidth", "offsetHeight", "duration", "seeking", "ended", "readyState",
    "naturalWidth", "naturalHeight",
];

const ONE_WAY_BINDINGS_NOT_ON_ELEMENT: &[(&str, &str)] = &[
    ("contentRect", "DOMRectReadOnly"),
    ("contentBoxSize", "ResizeObserverSize[]"),
    ("borderBoxSize", "ResizeObserverSize[]"),
    ("devicePixelContentBoxSize", "ResizeObserverSize[]"),
    ("buffered", "import('svelte/elements').SvelteMediaTimeRange[]"),
    ("played", "import('svelte/elements').SvelteMediaTimeRange[]"),
    ("seekable", "import('svelte/elements').SvelteMediaTimeRange[]"),
];

/// A parent in the walk, with what the handlers ask of it
#[derive(Clone, Copy)]
enum Parent<'r, 'm, 'a> {
    Root,
    Element(&'r LElement<'m, 'a>),
    Block { ty: &'static str, children: &'r [LNode<'m, 'a>] },
    Verbatim(&'r Verbatim<'a>),
    /// an attribute (for its value chunks)
    Attribute { ty: &'static str },
    Other(&'static str),
}

impl Parent<'_, '_, '_> {
    fn ty(&self) -> &'static str {
        match self {
            Parent::Root => "Fragment",
            Parent::Element(el) => el.kind,
            Parent::Block { ty, .. } => ty,
            Parent::Verbatim(v) => {
                if v.is_style {
                    "Style"
                } else {
                    "Script"
                }
            }
            Parent::Attribute { ty } => ty,
            Parent::Other(ty) => ty,
        }
    }
    fn name(&self) -> &str {
        match self {
            Parent::Element(el) => el.name,
            Parent::Verbatim(v) => {
                if v.is_style {
                    "style"
                } else {
                    "script"
                }
            }
            _ => "",
        }
    }
}

/// Comments attached to an attribute-like node by handleLeading/TrailingStartComment
#[derive(Default, Clone)]
struct AttachedComments {
    leading: Vec<usize>,
    trailing: Vec<usize>,
}

pub struct Converter<'s, 'm, 'a> {
    pub str: MagicString<'s>,
    ast: &'m Ast<'a>,
    opts: &'s Options,
    comments: &'m [JsComment],
    comment_newline: Vec<bool>,
    attached: HashMap<usize, AttachedComments>,
    els: Elements<'a>,
    element: Option<usize>,
    element_before_snippet: Vec<Option<usize>>,
    /// parents of non-root snippets, in insertion order, by identity (children pointer)
    pending_snippet_hoist: Vec<(&'static str, &'m [LNode<'m, 'a>])>,
    pending_snippet_seen: HashSet<usize>,
    pub uses_props: bool,
    pub uses_rest_props: bool,
    pub uses_slots: bool,
    pub is_runes: bool,
    // stores (Stores.ts + Scope.ts)
    is_declaration: bool,
    scopes: Vec<Scope>,
    current_scope: usize,
    possible_stores: Vec<(String, usize)>,
}

#[derive(Default)]
struct Scope {
    declared: HashSet<String>,
    parent: Option<usize>,
}

impl EsHandler for Converter<'_, '_, '_> {
    fn identifier(&mut self, name: &str, _start: usize, _end: usize, ctx: &Ctx) {
        // handleIdentifier
        match name {
            "$$props" => {
                self.uses_props = true;
                return;
            }
            "$$restProps" => {
                self.uses_rest_props = true;
                return;
            }
            "$$slots" => {
                self.uses_slots = true;
                return;
            }
            _ => {}
        }
        // stores.handleIdentifier
        if !name.starts_with('$') || matches!(name, "$$props" | "$$restProps" | "$$slots") {
            return;
        }
        if self.is_declaration {
            if *ctx == Ctx::PropertyKey {
                return;
            }
            self.scopes[self.current_scope].declared.insert(name.to_string());
        } else {
            if *ctx == (Ctx::MemberProperty { computed: false }) {
                return;
            }
            if *ctx == Ctx::PropertyKey {
                return;
            }
            self.possible_stores.push((name.to_string(), self.current_scope));
        }
    }

    fn scope_push(&mut self) {
        self.scopes.push(Scope { declared: HashSet::new(), parent: Some(self.current_scope) });
        self.current_scope = self.scopes.len() - 1;
    }

    fn scope_pop(&mut self) {
        self.current_scope = self.scopes[self.current_scope].parent.unwrap_or(0);
    }

    fn set_declaration(&mut self, value: bool) {
        self.is_declaration = value;
    }

    fn await_expression(&mut self, in_function: bool) {
        if !in_function {
            self.is_runes = true;
        }
    }
}

impl<'s, 'm, 'a> Converter<'s, 'm, 'a> {
    pub fn new(source: &'s str, opts: &'s Options, ast: &'m Ast<'a>, comments: &'m [JsComment]) -> Self {
        Converter {
            str: MagicString::new(source),
            ast,
            opts,
            comments,
            comment_newline: vec![false; comments.len()],
            attached: HashMap::new(),
            els: Elements::new(),
            element: None,
            element_before_snippet: Vec::new(),
            pending_snippet_hoist: Vec::new(),
            pending_snippet_seen: HashSet::new(),
            uses_props: false,
            uses_rest_props: false,
            uses_slots: false,
            is_runes: false,
            is_declaration: false,
            scopes: vec![Scope::default()],
            current_scope: 0,
            possible_stores: Vec::new(),
        }
    }

    fn original(&self) -> &'s str {
        self.str.original
    }

    // --- walking -------------------------------------------------------------------------

    pub fn convert(&mut self, root: &'m LegacyRoot<'m, 'a>, verbatim: &'m [Verbatim<'a>]) -> Result<()> {
        for child in &root.children {
            self.node(child, Parent::Root)?;
        }
        for v in verbatim {
            self.verbatim(v)?;
        }
        // hoist inner snippets to top of containing element
        for (ty, children) in std::mem::take(&mut self.pending_snippet_hoist) {
            self.hoist_snippet_block(ty, children)?;
        }
        if !self.opts.mode_ts {
            self.blank_other_script_tags(root, verbatim)?;
        }
        Ok(())
    }

    fn verbatim(&mut self, v: &'m Verbatim<'a>) -> Result<()> {
        if v.is_style {
            // handleStyleTag
            self.str.remove(v.start, v.end)?;
        }
        // attributes: handleLeading/TrailingComment only (handleAttribute skips script/style)
        for (i, attr) in v.attributes.iter().enumerate() {
            self.leading_start_comment(attr.start);
            if i + 1 == v.attributes.len() {
                self.trailing_end_comment(attr.start, attr.end);
            }
        }
        Ok(())
    }

    fn children(&mut self, children: &'m [LNode<'m, 'a>], parent: Parent<'m, 'm, 'a>) -> Result<()> {
        for child in children {
            self.node(child, parent)?;
        }
        Ok(())
    }

    fn walk_js<F: FnOnce(&mut EsWalker<'_, Self>)>(&mut self, f: F) {
        let mut walker = EsWalker::new(self);
        f(&mut walker);
    }

    /// Walk an expression the way the legacy AST holds it
    fn expr(&mut self, e: &'m Expr<'a>) {
        match e {
            Expr::Js(js) => self.js_expr(js),
            Expr::Ident { name, start, end, .. } => self.identifier(name, *start, *end, &Ctx::Other),
            Expr::Literal { .. } => {}
        }
    }

    fn js_expr(&mut self, js: &'m JsExpr<'a>) {
        let mut root = js.inner();
        let mut strip = None;
        if let Some(fix) = &js.fix {
            if fix.seq_first {
                if let Expression::SequenceExpression(seq) = root {
                    root = seq.expressions[0].without_parentheses();
                }
            }
            strip = fix.strip_as_end;
        }
        self.walk_js(|w| {
            w.strip_as_end = strip;
            w.visit_expression(root);
        });
    }

    fn pattern(&mut self, p: &'m Pattern<'a>, with_type: bool) {
        match p {
            Pattern::Ident { name, start, end, type_ann } => {
                self.identifier(name, *start, *end, &Ctx::Other);
                if with_type {
                    if let Some(t) = type_ann {
                        self.type_ann(t);
                    }
                }
            }
            Pattern::Destructure { assign, type_ann } => {
                if let Expression::AssignmentExpression(a) = assign.inner() {
                    self.walk_js(|w| w.visit_assignment_target(&a.left));
                }
                if with_type {
                    if let Some(t) = type_ann {
                        self.type_ann(t);
                    }
                }
            }
        }
    }

    fn type_ann(&mut self, t: &'m crate::ast::TypeAnn<'a>) {
        let mut e = t.expr.inner();
        if t.expr.fix.as_ref().is_some_and(|f| f.seq_first) {
            if let Expression::SequenceExpression(seq) = e {
                e = seq.expressions[0].without_parentheses();
            }
        }
        if let Expression::TSAsExpression(as_expr) = e {
            self.walk_js(|w| w.visit_ts_type(&as_expr.type_annotation));
        }
    }

    fn node(&mut self, node: &'m LNode<'m, 'a>, parent: Parent<'m, 'm, 'a>) -> Result<()> {
        match node {
            LNode::Text(t) => self.text(t, parent),
            LNode::Comment { start, end, .. } => {
                self.str.overwrite(*start, *end, "", true)?;
                Ok(())
            }
            LNode::MustacheTag { start, end, expression } => {
                self.mustache_tag(*start, *end, parent)?;
                self.expr(expression);
                Ok(())
            }
            LNode::RawMustacheTag { start, end, expression } => {
                // handleRawHtml
                self.str.overwrite(*start, expression.start(), " ", false)?;
                let e = with_trailing_property_access(self.original(), expression.end());
                self.str.overwrite(e, *end, ";", false)?;
                self.expr(expression);
                Ok(())
            }
            LNode::DebugTag { start, end, identifiers, .. } => {
                let exprs = debug_identifiers(identifiers);
                let mut cursor = *start;
                for (s, e) in &exprs {
                    self.str.overwrite(cursor, *s, ";", true)?;
                    cursor = *e;
                }
                self.str.overwrite(cursor, *end, ";", true)?;
                match identifiers {
                    crate::ast::DebugArgs::All => {}
                    crate::ast::DebugArgs::One(e) => self.expr(e),
                    crate::ast::DebugArgs::Sequence(Expr::Js(js)) => {
                        if let Expression::SequenceExpression(seq) = js.inner() {
                            for e in &seq.expressions {
                                self.walk_js(|w| w.visit_expression(e));
                            }
                        }
                    }
                    crate::ast::DebugArgs::Sequence(e) => self.expr(e),
                }
                Ok(())
            }
            LNode::ConstTag { start, end, id, init } => {
                // handleConstTag: the legacy expression is `id = init` starting after `{@const `
                let expr_start = start + 2 + "const ".len();
                self.str.overwrite(*start, expr_start, "const ", false)?;
                let e = with_trailing_property_access(self.original(), end - 1);
                self.str.overwrite(e, *end, ";", false)?;
                // AssignmentExpression { left, right }
                self.pattern(id, false);
                self.expr(init);
                Ok(())
            }
            LNode::DeclarationTag { .. } => self.declaration_tag(node),
            LNode::RenderTag { start, end, expression } => {
                self.str.overwrite(*start, expression.start(), ";__sveltets_2_ensureSnippet(", true)?;
                let e = with_trailing_property_access(self.original(), expression.end());
                self.str.overwrite(e, *end, ");", false)?;
                self.expr(expression);
                Ok(())
            }
            LNode::IfBlock { start, end, expression, children, else_block, elseif } => {
                self.handle_if(*start, *end, expression, children, *elseif)?;
                self.expr(expression);
                self.children(children, Parent::Block { ty: "IfBlock", children })?;
                if let Some(else_block) = else_block {
                    self.else_block(else_block, "IfBlock")?;
                }
                Ok(())
            }
            LNode::EachBlock { children, context, expression, key, else_block, .. } => {
                self.handle_each(node)?;
                self.children(children, Parent::Block { ty: "EachBlock", children })?;
                if let Some(c) = context {
                    self.pattern(c, true);
                }
                self.expr(expression);
                if let Some(k) = key {
                    self.expr(k);
                }
                if let Some(else_block) = else_block {
                    self.else_block(else_block, "EachBlock")?;
                }
                Ok(())
            }
            LNode::KeyBlock { start, end, expression, children } => {
                self.str.overwrite(*start, expression.start(), "", true)?;
                let e = with_trailing_property_access(self.original(), expression.end());
                let close = index_of(self.original(), "}", e).map_or(0, |i| i + 1);
                self.str.overwrite(e, close, "; {", false)?;
                let end_key = last_index_of(self.original(), "{", end.unwrap_or(0).saturating_sub(1)).unwrap_or(0);
                if !implicitly_closed(end_key, children, expression) {
                    self.str.overwrite(end_key, end.unwrap_or(0), "}", true)?;
                }
                self.expr(expression);
                self.children(children, Parent::Block { ty: "KeyBlock", children })
            }
            LNode::AwaitBlock { expression, value, error, pending, then, catch, .. } => {
                self.expr(expression);
                if let Some(v) = value {
                    self.pattern(v, true);
                }
                if let Some(e) = error {
                    self.pattern(e, true);
                }
                for (ty, b) in [("PendingBlock", pending), ("ThenBlock", then), ("CatchBlock", catch)] {
                    self.children(&b.children, Parent::Block { ty, children: &b.children })?;
                }
                self.handle_await(node)
            }
            LNode::SnippetBlock { .. } => self.snippet(node, parent),
            LNode::Element(el) => self.element(el, parent),
        }
    }

    fn else_block(&mut self, b: &'m LElseBlock<'m, 'a>, parent_ty: &'static str) -> Result<()> {
        if parent_ty == "IfBlock" {
            // handleElse: {:else} → } else {
            let original = self.original();
            let else_end = last_index_of(original, "}", b.start).unwrap_or(0);
            let elseword = last_index_of(original, ":else", else_end).unwrap_or(0);
            let else_start = last_index_of(original, "{", elseword).unwrap_or(0);
            self.str.overwrite(else_start, else_start + 1, "}", false)?;
            self.str.overwrite(else_end, else_end + 1, "{", false)?;
            let colon = index_of(original, ":", elseword).unwrap_or(0);
            self.str.remove(colon, colon + 1)?;
        }
        self.children(&b.children, Parent::Block { ty: "ElseBlock", children: &b.children })
    }

    fn text(&mut self, t: &LText, parent: Parent) -> Result<()> {
        if t.data.is_empty() || parent.ty() == "Attribute" {
            return Ok(());
        }
        let mut replacement: String = t.data.chars().filter(|c| crate::parser::utils::is_whitespace_char(*c)).collect();
        if replacement.is_empty() {
            replacement = " ".into();
        }
        self.str.overwrite(t.start, t.end, &replacement, true)?;
        Ok(())
    }

    fn mustache_tag(&mut self, start: usize, end: usize, parent: Parent) -> Result<()> {
        if matches!(parent.ty(), "Attribute" | "StyleDirective") {
            return Ok(());
        }
        let text = &self.original()[start + 1..end - 1];
        if text.trim_start_matches(crate::parser::utils::is_whitespace_char).starts_with('{') {
            self.str.overwrite(start, start + 1, ";(", true)?;
            self.str.overwrite(end - 1, end, ");", true)?;
            return Ok(());
        }
        self.str.overwrite(start, start + 1, "", true)?;
        self.str.overwrite(end - 1, end, ";", true)?;
        Ok(())
    }

    fn declaration_tag(&mut self, node: &'m LNode<'m, 'a>) -> Result<()> {
        // the DeclarationTag keeps its modern shape
        let LNode::DeclarationTag { id, .. } = node else { unreachable!() };
        let Node::DeclarationTag { start, end, declaration } = &self.ast.nodes[*id] else { unreachable!() };
        let (decl_start, decl_end) = match declaration {
            Declaration::Js(stmt) => (stmt.stmt.span().start as usize, stmt.stmt.span().end as usize),
            Declaration::Loose { start, end, .. } => (*start, *end),
        };
        // handleDeclarationTag: `{let x = y}` --> `let x = y;`
        self.str.remove(*start, decl_start)?;
        self.str.overwrite(decl_end, *end, ";", false)?;
        if let Declaration::Js(stmt) = declaration {
            self.walk_js(|w| w.visit_statement(&stmt.stmt));
        }
        Ok(())
    }

    fn handle_if(&mut self, start: usize, end: Option<usize>, expression: &Expr, children: &[LNode], elseif: bool) -> Result<()> {
        let original = self.original();
        if elseif {
            let s = last_index_of(original, "{", expression.start()).unwrap_or(0);
            self.str.overwrite(s, expression.start(), "} else if (", false)?;
        } else {
            self.str.overwrite(start, expression.start(), "if(", false)?;
        }
        let expression_end = with_trailing_property_access(original, expression.end());
        let close = index_of(original, "}", expression_end).map_or(0, |i| i + 1);
        self.str.overwrite(expression_end, close, "){", false)?;
        let end = end.unwrap_or(0);
        let endif = last_index_of(original, "{", end.saturating_sub(1)).unwrap_or(0);
        if implicitly_closed(endif, children, expression) {
            self.str.prepend_left(end, "}")?;
        } else {
            self.str.overwrite(endif, end, "}", false)?;
        }
        Ok(())
    }

    fn handle_each(&mut self, node: &'m LNode<'m, 'a>) -> Result<()> {
        let LNode::EachBlock { start, end, children, context, expression, index, key, else_block } = node else { unreachable!() };
        let original = self.original();
        let from = key.map(|k| k.end()).filter(|e| *e > 0).or(context.map(pattern_end).filter(|e| *e > 0)).unwrap_or(expression.end());
        let start_end = index_of(original, "}", from).map_or(0, |i| i + 1);
        let contains_comma = original[expression.start()..expression.end()].contains(',');
        let expression_end = expr_get_end(expression);
        let context_end = context.map(pattern_get_end);
        let same = context.is_some_and(|c| original[expression.start()..expression_end] == original[pattern_start(c)..context_end.unwrap()]);
        let (open, close) = if contains_comma { ("(", ")") } else { ("", "") };
        let mut transforms: Ts = if same {
            vec![
                format!("{{ const $$_each = __sveltets_2_ensureArray({open}").into(),
                T::Range(expression.start(), expression.end()),
                format!("{close}); for(let ").into(),
                T::Range(pattern_start(context.unwrap()), context_end.unwrap()),
                " of $$_each){".into(),
            ]
        } else {
            vec![
                "for(let ".into(),
                match context {
                    Some(c) => T::Range(pattern_start(c), context_end.unwrap()),
                    None => "$$each_item".into(),
                },
                format!(" of __sveltets_2_ensureArray({open}").into(),
                T::Range(expression.start(), expression.end()),
                format!("{close})){{{}", if context.is_some() { "" } else { "$$each_item;" }).into(),
            ]
        };
        if let Some(index) = index {
            let from = context.map(pattern_end).filter(|e| *e > 0).unwrap_or(expression.end());
            let index_start = index_of(original, index, from).unwrap_or(usize::MAX);
            transforms.push("let ".into());
            transforms.push(T::Range(index_start, index_start.wrapping_add(index.len())));
            transforms.push(" = 1;".into());
        }
        if let Some(k) = key {
            transforms.push(T::Range(k.start(), k.end()));
            transforms.push(";".into());
        }
        transform(&mut self.str, *start, start_end, &transforms)?;

        let end = end.unwrap_or(0);
        let end_each = last_index_of(original, "{", end.saturating_sub(1)).unwrap_or(0);
        let suffix = if same { "}" } else { "" };
        if let Some(else_block) = else_block {
            let else_end = last_index_of(original, "}", else_block.start).unwrap_or(0);
            let else_start = last_index_of(original, "{", else_end).unwrap_or(0);
            self.str.overwrite(else_start, else_end + 1, &format!("}}{suffix}"), true)?;
            if !implicitly_closed(end_each, children, expression) {
                self.str.remove(end_each, end)?;
            }
        } else {
            let closing = format!("}}{suffix}");
            if implicitly_closed(end_each, children, expression) {
                self.str.prepend_left(end, &closing)?;
            } else {
                self.str.overwrite(end_each, end, &closing, true)?;
            }
        }
        Ok(())
    }

    fn handle_await(&mut self, node: &'m LNode<'m, 'a>) -> Result<()> {
        let LNode::AwaitBlock { start, end, expression, value, error, pending, then, catch } = node else { unreachable!() };
        let original = self.original();
        let mut t: Ts = vec!["{ ".into()];
        if !pending.skip {
            t.push(T::Range(pending.start.unwrap_or(0), pending.end.unwrap_or(0)));
        }
        if error.is_some() || !catch.skip {
            t.push("try { ".into());
        }
        if value.is_some() {
            t.push("const $$_value = ".into());
        }
        let expression_end = with_trailing_property_access(original, expression.end());
        t.push("await (".into());
        t.push(T::Range(expression.start(), expression_end));
        t.push(");".into());
        if let Some(v) = value {
            t.push("{ const ".into());
            t.push(T::Range(pattern_start(v), pattern_type_end(v)));
            t.push(" = $$_value; ".into());
        }
        if !then.skip {
            if pending.skip {
                t.push(T::Range(then.start.unwrap_or(0), then.end.unwrap_or(0)));
            } else if let (Some(first), Some(last)) = (then.children.first(), then.children.last()) {
                t.push(T::Range(lnode_start(first), lnode_end(last)));
            }
        }
        if value.is_some() {
            t.push("}".into());
        }
        if error.is_some() || !catch.skip {
            t.push("} catch($$_e) { ".into());
            if let Some(e) = error {
                t.push("const ".into());
                t.push(T::Range(pattern_start(e), pattern_type_end(e)));
                t.push(" = __sveltets_2_any();".into());
            }
            if !catch.skip {
                if let (Some(first), Some(last)) = (catch.children.first(), catch.children.last()) {
                    t.push(T::Range(lnode_start(first), lnode_end(last)));
                }
            }
            t.push("}".into());
        }
        t.push("}".into());
        transform(&mut self.str, *start, end.unwrap_or(0), &t)
    }

    // --- elements ------------------------------------------------------------------------

    fn node_info(&self, el: &'m LElement<'m, 'a>) -> NodeInfo<'a> {
        let children = el.children.as_deref().unwrap_or(&[]);
        let attr_first_chunk = |name: &str| {
            el.attributes.iter().find_map(|a| match a {
                LAttr::Attribute { attr, value } if attr.name() == Some(name) => match value {
                    LAttrValue::Chunks(chunks) => chunks.first().map(chunk_range),
                    LAttrValue::True => None,
                },
                _ => None,
            })
        };
        let is_dash = el.attributes.iter().find_map(|a| match a {
            LAttr::Attribute { attr, value } if attr.name() == Some("is") => Some(match value {
                LAttrValue::Chunks(chunks) => match chunks.first() {
                    Some(LChunk::Text(Chunk::Text { data, .. })) => data.contains('-'),
                    _ => false,
                },
                LAttrValue::True => false,
            }),
            _ => None,
        });
        NodeInfo {
            name: el.name,
            start: el.start,
            end: el.end.unwrap_or(0),
            first_child_start: children.first().map(lnode_start),
            has_children: !children.is_empty(),
            tag: el.tag.as_ref().map(|t| match t {
                LTag::Static(s) => Err(s.to_string()),
                LTag::Expr(e) => Ok((e.start(), e.end())),
            }),
            expression: el.expression.map(|e| (e.start(), e.end())),
            slot_name_value: if el.name == "slot" { attr_first_chunk("name") } else { None },
            is_attr_has_dash: is_dash.unwrap_or(false),
        }
    }

    fn element(&mut self, el: &'m LElement<'m, 'a>, parent: Parent<'m, 'm, 'a>) -> Result<()> {
        let is_component = el.kind == "InlineComponent";
        let doctype = el.name == "!DOCTYPE";
        if is_component {
            let info = self.node_info(el);
            let id = self.els.new_component(&mut self.str, info, self.element)?;
            self.element = Some(id);
            if self.opts.svelte5_plus {
                self.handle_implicit_children(el, id);
            }
            // handleComponentLet: template scope only (slot types come later)
        } else {
            if el.kind == "Options" {
                self.handle_svelte_options(el);
            }
            if doctype {
                self.str.remove(el.start, el.end.unwrap_or(0))?;
            } else {
                let info = self.node_info(el);
                let ns = self.opts.typings_namespace.clone();
                let id = self.els.new_element(&mut self.str, info, &ns, self.element)?;
                self.element = Some(id);
            }
        }

        let me = Parent::Element(el);
        // svelte:element's `tag` / svelte:component's `expression` come before the attributes
        if let Some(LTag::Expr(e)) = &el.tag {
            self.expr(e);
        }
        if let Some(e) = el.expression {
            self.expr(e);
        }
        let n = el.attributes.len();
        for (i, attr) in el.attributes.iter().enumerate() {
            self.attribute(attr, me, i + 1 == n)?;
        }
        if let Some(children) = &el.children {
            self.children(children, me)?;
        }

        if !doctype {
            let id = self.element.expect("current element");
            self.els.perform_transformation(&mut self.str, id)?;
            self.element = self.els.parent(id);
        }
        let _ = parent;
        Ok(())
    }

    fn handle_svelte_options(&mut self, el: &LElement) {
        for a in &el.attributes {
            if let LAttr::Attribute { attr, value } = a {
                let truthy = match value {
                    LAttrValue::True => true,
                    LAttrValue::Chunks(chunks) => match chunks.first() {
                        Some(LChunk::MustacheTag { expression, .. }) => literal_truthy(expression),
                        _ => continue,
                    },
                };
                match attr.name() {
                    Some("runes") => self.is_runes = truthy,
                    _ => {}
                }
            }
        }
    }

    fn handle_implicit_children(&mut self, el: &LElement, id: usize) {
        let children = el.children.as_deref().unwrap_or(&[]);
        if children.is_empty() {
            return;
        }
        let mut has_slot = false;
        for child in children {
            if let LNode::Element(c) = child {
                if matches!(c.kind, "InlineComponent" | "Element" | "SlotTemplate") {
                    let named_slot = c.attributes.iter().any(|a| match a {
                        LAttr::Attribute { attr, value } if attr.name() == Some("slot") => match value {
                            LAttrValue::Chunks(chunks) => !matches!(chunks.first(), Some(LChunk::Text(Chunk::Text { data, .. })) if data == "default"),
                            LAttrValue::True => true,
                        },
                        _ => false,
                    });
                    if named_slot {
                        continue;
                    }
                }
                if c.kind == "Slot" {
                    continue;
                }
            }
            match child {
                LNode::Comment { .. } => continue,
                LNode::Text(t) if t.data.trim().is_empty() => continue,
                LNode::SnippetBlock { .. } => {}
                _ => {
                    has_slot = true;
                    break;
                }
            }
        }
        if has_slot {
            self.els.add_attribute(id, vec!["children".into()], Some(vec!["() => { return __sveltets_2_any(0); }".into()]));
        }
    }

    // --- attributes ----------------------------------------------------------------------

    fn attribute(&mut self, a: &'m LAttr<'m, 'a>, parent: Parent<'m, 'm, 'a>, is_last: bool) -> Result<()> {
        let attr = match a {
            LAttr::Attribute { attr, .. } | LAttr::Other { attr, .. } => *attr,
        };
        let ty = a.legacy_type();
        let (start, end) = (attr.start(), attr.end());
        let with_comments = matches!(
            ty,
            "AttachTag" | "Binding" | "Action" | "Transition" | "Animation" | "Attribute" | "Spread" | "EventHandler" | "Let"
        );
        if with_comments {
            self.leading_start_comment(start);
            if is_last {
                self.trailing_end_comment(start, end);
            }
        }
        let element = self.element;
        match a {
            LAttr::Attribute { value, .. } if ty == "Attribute" => {
                if let Some(el) = element {
                    self.handle_attribute(start, attr.name().unwrap_or(""), value, None, parent, el)?;
                }
                self.attr_value(value, "Attribute")
            }
            LAttr::Attribute { value, .. } => {
                // StyleDirective
                if let Some(el) = element {
                    self.handle_style_directive(start, end, value, el)?;
                }
                self.attr_value(value, "StyleDirective")
            }
            LAttr::Other { .. } => {
                let expression = match attr {
                    Attr::Spread { expression, .. } | Attr::Attach { expression, .. } => Some(expression),
                    Attr::Directive { expression, .. } => expression.as_ref(),
                    _ => None,
                };
                let name = attr.name().unwrap_or("");
                if let Some(el) = element {
                    match ty {
                        "AttachTag" => {
                            let e = expression.unwrap();
                            let mut name_t = self.leading_t(start);
                            name_t.push("[Symbol(\"@attach\")]".into());
                            let mut value = vec![T::Range(e.start(), e.end())];
                            value.extend(self.trailing_t(start));
                            self.els.add_attribute(el, name_t, Some(value));
                        }
                        "Spread" => {
                            let mut t = self.leading_t(start);
                            t.push(T::Range(start + 1, end - 1));
                            t.extend(self.trailing_t(start));
                            self.els.add_attribute(el, t, None);
                        }
                        "Binding" => self.handle_binding(attr, expression.unwrap(), parent, el)?,
                        "Class" => {
                            let e = expression.unwrap();
                            let r = range_with_trailing_property_access(self.original(), (e.start(), e.end()));
                            self.els.append_to_start_end(el, vec![T::Range(r.0, r.1), ";".into()]);
                        }
                        "Action" => {
                            self.store_directive(start, name);
                            if self.els.kind(el) == Kind::Component {
                                // InlineComponent has no addAction
                                return Err(crate::magic_string::MagicStringError("element.addAction is not a function".into()));
                            }
                            let leading = self.leading_t(start);
                            let trailing = self.trailing_t(start);
                            let str = &self.str;
                            self.els.add_action(str, el, start, name, expression.map(|e| (e.start(), e.end())), leading, trailing);
                        }
                        "Transition" | "Animation" => {
                            self.store_directive(start, name);
                            let mut t = self.trailing_t(start);
                            let trailing = std::mem::take(&mut t);
                            let mut ts = self.leading_t(start);
                            ts.push(if ty == "Transition" { "__sveltets_2_ensureTransition(" } else { "__sveltets_2_ensureAnimation(" }.into());
                            let (a, b) = directive_name_range(self.original(), start, name);
                            ts.push(T::Range(a, b));
                            let ns = self.els.typings_namespace(el).to_string();
                            let tag = self.els.tag_name(el);
                            ts.push(
                                if ty == "Transition" {
                                    format!("({ns}.mapElementTag('{tag}')")
                                } else {
                                    format!("({ns}.mapElementTag('{tag}'),__sveltets_2_AnimationMove")
                                }
                                .into(),
                            );
                            if let Some(e) = expression {
                                ts.push(",(".into());
                                let r = range_with_trailing_property_access(self.original(), (e.start(), e.end()));
                                ts.push(T::Range(r.0, r.1));
                                ts.push(")".into());
                            }
                            ts.push("));".into());
                            ts.extend(trailing);
                            self.els.append_to_start_end(el, ts);
                        }
                        "EventHandler" => self.handle_event_handler(start, name, expression, el)?,
                        "Let" => self.handle_let(start, end, name, expression, parent, el)?,
                        _ => {}
                    }
                }
                if let Some(e) = expression {
                    self.expr(e);
                }
                Ok(())
            }
        }
    }

    fn attr_value(&mut self, value: &'m LAttrValue<'m, 'a>, ty: &'static str) -> Result<()> {
        if let LAttrValue::Chunks(chunks) = value {
            for c in chunks {
                match c {
                    LChunk::Text(Chunk::Text { start, end, data, .. }) => {
                        // handleText only skips text inside `Attribute`s (not `StyleDirective`s)
                        if ty != "Attribute" {
                            let t = LText { start: *start, end: *end, raw: None, data: data.clone() };
                            self.text(&t, Parent::Attribute { ty })?;
                        }
                    }
                    LChunk::Text(_) => {}
                    LChunk::MustacheTag { expression, .. } | LChunk::AttributeShorthand { expression, .. } => {
                        self.expr(expression);
                    }
                }
            }
        }
        Ok(())
    }

    fn store_directive(&mut self, start: usize, name: &str) {
        // stores.handleDirective
        if !name.starts_with('$') || matches!(name, "$$props" | "$$restProps" | "$$slots") || self.is_declaration {
            return;
        }
        let _ = start;
        self.possible_stores.push((name.to_string(), self.current_scope));
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_attribute(
        &mut self,
        start: usize,
        name: &str,
        value: &LAttrValue,
        let_expression: Option<(usize, usize)>,
        parent: Parent,
        el: usize,
    ) -> Result<()> {
        let original = self.original();
        if parent.name() == "!DOCTYPE" || matches!(parent.ty(), "Style" | "Script") || (name == "name" && parent.ty() == "Slot") {
            return Ok(());
        }
        let is_element = self.els.kind(el) == Kind::Element;

        if name == "slot" {
            if let LAttrValue::Chunks(chunks) = value {
                if let [LChunk::Text(Chunk::Text { start: s, end: e, .. })] = &chunks[..] {
                    if self.els.parent(el).is_some_and(|p| self.els.kind(p) == Kind::Component) {
                        self.els.add_slot_name(el, vec![T::Range(*s, *e)]);
                        return Ok(());
                    }
                }
            }
        }

        let add = |this: &mut Self, mut name_t: Ts, mut value_t: Option<Ts>| {
            if is_element {
                if name.starts_with("data-") && !name.starts_with("data-sveltekit-") {
                    name_t.insert(0, "...__sveltets_2_empty({".into());
                    value_t.get_or_insert_with(|| vec!["__sveltets_2_any()".into()]).push("})".into());
                }
            } else if name.starts_with("--") {
                name_t.insert(0, "...__sveltets_2_cssProp({".into());
                value_t.get_or_insert_with(|| vec!["\"\"".into()]).push("})".into());
            }
            this.els.add_attribute(el, name_t, value_t);
        };

        let mut attribute_name = self.leading_t(start);
        let trailing = self.trailing_t(start);

        // the shorthand `{x}`
        if let LAttrValue::Chunks(chunks) = value {
            if let [LChunk::AttributeShorthand { start: s, end: e, .. }] = &chunks[..] {
                let (mut s, e) = (*s, *e);
                if s == e {
                    s -= 1;
                    self.str.overwrite(s, e, " ", true)?;
                }
                let mut t = vec![T::Range(s, e)];
                t.extend(trailing);
                add(self, t, None);
                return Ok(());
            }
        }

        let transformed = if is_element && parent.ty() == "Element" { self.transform_attribute_case(name, el) } else { name.to_string() };
        if transformed != name {
            self.str.overwrite(start, start + name.len(), &format!("\"{transformed}"), false)?;
        } else {
            let first = next_char(original, start);
            self.str.overwrite(start, first, &format!("\"{}", char_at(original, start)), true)?;
        }
        attribute_name.push(T::Range(start, start + name.len()));
        attribute_name.push("\"".into());

        let chunks = match value {
            LAttrValue::True => {
                let mut v: Ts = vec![if name == "popover" { "\"\"" } else { "true" }.into()];
                v.extend(trailing);
                add(self, attribute_name, Some(v));
                return Ok(());
            }
            LAttrValue::Chunks(chunks) => chunks,
        };
        let _ = let_expression;
        if chunks.is_empty() {
            let mut v: Ts = vec!["\"\"".into()];
            v.extend(trailing);
            add(self, attribute_name, Some(v));
            return Ok(());
        }
        if chunks.len() == 1 {
            match &chunks[0] {
                LChunk::Text(Chunk::Text { start: s, end: e, data, .. }) => {
                    let (s, e) = (*s, *e);
                    if s == e {
                        add(self, attribute_name, Some(vec![T::Range(s - 1, e + 1)]));
                        return Ok(());
                    }
                    let last = prev_char(original, e);
                    let lb = byte_at(original, last);
                    let has_brackets = lb == Some(b'}')
                        || ((lb == Some(b'"') || lb == Some(b'\'')) && last > 0 && byte_at(original, last - 1) == Some(b'}'));
                    let needs_number = !has_brackets
                        && parent.ty() == "Element"
                        && NUMBER_ONLY_ATTRIBUTES.contains(&name.to_lowercase().as_str())
                        && !js_is_nan(data);
                    let has_backtick = data.contains('`');
                    let quote = if !has_backtick {
                        "`".to_string()
                    } else {
                        match byte_at(original, s.wrapping_sub(1)) {
                            Some(q @ (b'"' | b'\'')) => (q as char).to_string(),
                            _ => "\"".to_string(),
                        }
                    };
                    let mut v = Ts::new();
                    if !needs_number {
                        v.push(quote.clone().into());
                    }
                    if let Some(escaped) = try_escape_attribute_value(data, !has_backtick) {
                        self.str.overwrite(s, e, &escaped, true)?;
                    }
                    v.push(T::Range(s, e));
                    if !needs_number {
                        v.push(quote.into());
                    }
                    v.extend(trailing);
                    add(self, attribute_name, Some(v));
                }
                LChunk::MustacheTag { expression, .. } => {
                    let (mut s, e) = range_with_trailing_property_access(original, (expression.start(), expression.end()));
                    if s == e {
                        s -= 1;
                        self.str.overwrite(s, e, " ", true)?;
                    }
                    let mut v = vec![T::Range(s, e)];
                    v.extend(trailing);
                    add(self, attribute_name, Some(v));
                }
                _ => {}
            }
            return Ok(());
        }
        // multiple values: a template string
        for c in chunks {
            if let LChunk::MustacheTag { start: s, .. } = c {
                self.str.append_right(*s, "$")?;
            }
        }
        let first = chunk_range(&chunks[0]).0;
        let last = chunk_range(chunks.last().unwrap()).1;
        let mut v: Ts = vec!["`".into(), T::Range(first, last), "`".into()];
        v.extend(trailing);
        add(self, attribute_name, Some(v));
        Ok(())
    }

    fn transform_attribute_case(&self, name: &str, el: usize) -> String {
        let is_svg = SVG_ATTRIBUTES.contains(name);
        if !self.opts.preserve_attribute_case
            && !is_svg
            && !(self.els.kind(el) == Kind::Element && self.els.is_custom_element(el))
            && !(self.opts.svelte5_plus && name.starts_with("on"))
        {
            name.to_lowercase()
        } else {
            name.to_string()
        }
    }

    fn handle_style_directive(&mut self, start: usize, end: usize, value: &LAttrValue, el: usize) -> Result<()> {
        let original = self.original();
        let ensure = "__sveltets_2_ensureType(String, Number, ";
        let chunks = match value {
            LAttrValue::Chunks(c) if !c.is_empty() => c,
            _ => {
                let colon = index_of(original, ":", start).map_or(0, |c| c + 1);
                self.els.append_to_start_end(el, vec![ensure.into(), T::Range(colon, end), ");".into()]);
                return Ok(());
            }
        };
        if chunks.len() > 1 {
            for c in chunks {
                if let LChunk::MustacheTag { start: s, .. } = c {
                    self.str.append_right(*s, "$")?;
                }
            }
            let first = chunk_range(&chunks[0]).0;
            let last = chunk_range(chunks.last().unwrap()).1;
            self.els.append_to_start_end(el, vec![format!("{ensure}`").into(), T::Range(first, last), "`);".into()]);
            return Ok(());
        }
        match &chunks[0] {
            LChunk::Text(Chunk::Text { start: s, end: e, .. }) => {
                let quote = match byte_at(original, s.wrapping_sub(1)) {
                    Some(q @ (b'"' | b'\'')) => (q as char).to_string(),
                    _ => "\"".to_string(),
                };
                self.els.append_to_start_end(el, vec![format!("{ensure}{quote}").into(), T::Range(*s, *e), format!("{quote});").into()]);
            }
            c => {
                let (s, e) = chunk_range(c);
                self.els.append_to_start_end(el, vec![ensure.into(), T::Range(s + 1, e - 1), ");".into()]);
            }
        }
        Ok(())
    }

    fn handle_binding(&mut self, attr: &Attr, expression: &Expr, parent: Parent, el: usize) -> Result<()> {
        let original = self.original();
        let Attr::Directive { start, end, name, .. } = attr else { unreachable!() };
        let (start, end, name) = (*start, *end, *name);
        let seq = match expression {
            Expr::Js(js) => match js.inner() {
                Expression::SequenceExpression(seq) => Some((seq.expressions.first(), seq.expressions.get(1))),
                _ => None,
            },
            _ => None,
        };
        let leading = self.leading_t(start);
        let trailing = self.trailing_t(start);
        let is_element = self.els.kind(el) == Kind::Element;

        if name == "this" && matches!(parent.ty(), "InlineComponent" | "Element" | "Body" | "Slot") {
            if let Some((_, set)) = seq {
                let set = set.expect("setter");
                let el_name = self.els.name(el);
                let mut t = leading;
                t.push("(".into());
                t.push(T::Range(oxc_start(set), oxc_get_end(set)));
                t.push(format!(")({el_name});").into());
                t.extend(trailing);
                self.els.append_to_start_end(el, t);
            } else {
                let el_name = self.els.name(el);
                self.one_way_binding(expression, &format!(" = {el_name}"), el, leading, trailing);
            }
            return Ok(());
        }

        if seq.is_none() {
            if is_element && name == "group" && parent.name() == "input" {
                self.one_way_binding(expression, " = __sveltets_2_any(null)", el, leading, trailing);
                return Ok(());
            }
            if ONE_WAY_BINDINGS.contains(&name) && is_element {
                let el_name = self.els.name(el);
                self.one_way_binding(expression, &format!("= {el_name}.{name}"), el, leading, trailing);
                return Ok(());
            }
            if let Some((_, ty)) = ONE_WAY_BINDINGS_NOT_ON_ELEMENT.iter().find(|(n, _)| *n == name) {
                if is_element {
                    let ts_syntax = self.opts.is_ts_file || !self.opts.emit_jsdoc;
                    let value = if ts_syntax { format!("null as {ty}") } else { format!("/** @type {{{ty}}} */ (null)") };
                    let mut t = leading;
                    t.push(T::Range(expression.start(), expr_get_end(expression)));
                    t.push(format!("= {};", surround_with_ignore_comments(&value)).into());
                    t.extend(trailing);
                    self.els.append_to_start_end(el, t);
                    return Ok(());
                }
            }
            let expression_str = &original[expression.start()..expr_get_end(expression)];
            self.els.append_to_start_end(
                el,
                vec![surround_with_ignore_comments(&format!("() => {expression_str} = __sveltets_2_any(null);")).into()],
            );
        }

        let is_shorthand = expression.start() == start + "bind:".len();
        let preserve_bind = self.opts.typings_namespace == "svelteHTML";
        let eq = || last_index_of(original, "=", expression.start()).unwrap_or(usize::MAX);
        let mut name_t = leading;
        if preserve_bind && is_element {
            if is_shorthand {
                name_t.push(format!("\"{}\"", &original[start..end]).into());
            } else {
                name_t.push("\"".into());
                name_t.push(T::Range(start, eq()));
                name_t.push("\"".into());
            }
        } else if is_shorthand {
            name_t.push(T::Range(expression.start(), expression.end()));
        } else {
            name_t.push(T::Range(start + "bind:".len(), eq()));
        }

        let value: Option<Ts> = if is_shorthand {
            if preserve_bind && is_element {
                let r = range_with_trailing_property_access(original, (expression.start(), expression.end()));
                let mut v = vec![T::Range(r.0, r.1)];
                v.extend(trailing.clone());
                Some(v)
            } else {
                None
            }
        } else if let Some((get, set)) = seq {
            let (get, set) = (get.unwrap(), set.unwrap());
            let r = range_with_trailing_property_access(original, (oxc_start(set), oxc_end(set)));
            let mut v: Ts = vec!["__sveltets_2_get_set_binding(".into(), T::Range(oxc_start(get), oxc_end(get)), ",".into(), T::Range(r.0, r.1), ")".into()];
            v.extend(trailing.clone());
            Some(v)
        } else {
            let r = range_with_trailing_property_access(original, (expression.start(), expression.end()));
            let mut v = vec![T::Range(r.0, r.1)];
            v.extend(trailing.clone());
            Some(v)
        };
        if value.is_none() {
            name_t.extend(trailing);
        }
        if self.opts.svelte5_plus && !is_element {
            let el_name = self.els.name(el);
            self.els.append_to_start_end(el, vec![format!("{el_name}.$$bindings = '{name}';").into()]);
        }
        self.els.add_attribute(el, name_t, value);
        Ok(())
    }

    fn one_way_binding(&mut self, expression: &Expr, assignment: &str, el: usize, leading: Ts, trailing: Ts) {
        let end = expr_get_end(expression);
        let has_type = expr_is_ts(expression);
        let mut t = leading;
        t.push(T::Range(expression.start(), end));
        t.push(format!("{assignment}{}", if has_type { "" } else { ";" }).into());
        t.extend(trailing);
        if has_type {
            t.push(T::Range(end, expression.end()));
            t.push(";".into());
        }
        self.els.append_to_start_end(el, t);
    }

    fn handle_event_handler(&mut self, start: usize, name: &str, expression: Option<&Expr>, el: usize) -> Result<()> {
        let original = self.original();
        let name_start = index_of(original, ":", start).map_or(0, |c| c + 1);
        let name_end = name_start + name.len();
        let leading = self.leading_t(start);
        let trailing = self.trailing_t(start);
        let expr_range = expression.map(|e| range_with_trailing_property_access(original, (e.start(), e.end())));
        if self.els.kind(el) == Kind::Element {
            surround_with(&mut self.str, (name_start, name_end), "\"on:", "\"")?;
            let mut n = leading;
            n.push(T::Range(name_start, name_end));
            let mut v: Ts = match expr_range {
                Some(r) => vec![T::Range(r.0, r.1)],
                None => vec!["undefined".into()],
            };
            v.extend(trailing);
            self.els.add_attribute(el, n, Some(v));
        } else {
            self.els.add_event(&mut self.str, el, (name_start, name_end), expr_range, leading, trailing)?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_let(&mut self, start: usize, end: usize, name: &str, expression: Option<&Expr>, parent: Parent, el: usize) -> Result<()> {
        let add_slot_let = |this: &mut Self| {
            let mut t = this.leading_t(start);
            t.push(T::Range(start + "let:".len(), start + "let:".len() + name.len()));
            if let Some(e) = expression {
                t.push(":".into());
                t.push(T::Range(e.start(), e.end()));
            }
            t.extend(this.trailing_t(start));
            this.els.add_slot_let(el, t);
        };
        if self.els.kind(el) == Kind::Component {
            add_slot_let(self);
        } else if self.els.parent(el).is_some_and(|p| self.els.kind(p) == Kind::Component) {
            add_slot_let(self);
        } else {
            // a regular attribute named `let:x`, valued like `x={...}`
            let full_name = format!("let:{name}");
            self.handle_let_attribute(start, end, &full_name, expression, parent, el)?;
        }
        Ok(())
    }

    fn handle_let_attribute(&mut self, start: usize, _end: usize, name: &str, expression: Option<&Expr>, parent: Parent, el: usize) -> Result<()> {
        // handleAttribute with { name: 'let:' + name, value: [MustacheTag] | true }
        let original = self.original();
        if parent.name() == "!DOCTYPE" || matches!(parent.ty(), "Style" | "Script") {
            return Ok(());
        }
        let is_element = self.els.kind(el) == Kind::Element;
        let mut attribute_name = self.leading_t(start);
        let trailing = self.trailing_t(start);
        let transformed = if is_element && parent.ty() == "Element" { self.transform_attribute_case(name, el) } else { name.to_string() };
        if transformed != name {
            self.str.overwrite(start, start + name.len(), &format!("\"{transformed}"), false)?;
        } else {
            let first = next_char(original, start);
            self.str.overwrite(start, first, &format!("\"{}", char_at(original, start)), true)?;
        }
        attribute_name.push(T::Range(start, start + name.len()));
        attribute_name.push("\"".into());
        let value = match expression {
            None => {
                let mut v: Ts = vec!["true".into()];
                v.extend(trailing);
                v
            }
            Some(e) => {
                let (mut s, en) = range_with_trailing_property_access(original, (e.start(), e.end()));
                if s == en {
                    s -= 1;
                    self.str.overwrite(s, en, " ", true)?;
                }
                let mut v = vec![T::Range(s, en)];
                v.extend(trailing);
                v
            }
        };
        self.els.add_attribute(el, attribute_name, Some(value));
        Ok(())
    }

    // --- comments ------------------------------------------------------------------------

    fn leading_start_comment(&mut self, node_start: usize) {
        if self.comments.is_empty() || self.attached.get(&node_start).is_some_and(|c| !c.leading.is_empty()) {
            return;
        }
        let original = self.original();
        let mut leading = Vec::new();
        let mut search_end = node_start;
        for i in (0..self.comments.len()).rev() {
            let c = &self.comments[i];
            if c.end > search_end {
                continue;
            }
            if !is_blank(&original[c.end..search_end]) {
                break;
            }
            leading.insert(0, i);
            search_end = c.start;
        }
        if !leading.is_empty() {
            for &i in &leading {
                if preceded_by_newline(original, self.comments[i].start) {
                    self.comment_newline[i] = true;
                }
            }
            self.attached.entry(node_start).or_default().leading = leading;
        }
    }

    fn trailing_end_comment(&mut self, node_start: usize, node_end: usize) {
        if self.comments.is_empty() || self.attached.get(&node_start).is_some_and(|c| !c.trailing.is_empty()) {
            return;
        }
        let original = self.original();
        let Some(tag_end) = index_of(original, ">", node_end) else { return };
        let mut trailing = Vec::new();
        let mut search_start = node_end;
        for (i, c) in self.comments.iter().enumerate() {
            if c.start < search_start {
                continue;
            }
            if c.end > tag_end {
                break;
            }
            if !is_blank(&original[search_start..c.start]) {
                break;
            }
            trailing.push(i);
            search_start = c.end;
        }
        if trailing.is_empty() {
            return;
        }
        let rest = &original[search_start..tag_end];
        let rest_trimmed = rest.trim_matches(crate::parser::utils::is_whitespace_char);
        if !(rest_trimmed.is_empty() || rest_trimmed == "/") {
            return;
        }
        for &i in &trailing {
            if preceded_by_newline(original, self.comments[i].start) {
                self.comment_newline[i] = true;
            }
        }
        self.attached.entry(node_start).or_default().trailing = trailing;
    }

    fn leading_t(&self, node_start: usize) -> Ts {
        let Some(c) = self.attached.get(&node_start).filter(|c| !c.leading.is_empty()) else { return Ts::new() };
        let mut t = Ts::new();
        for &i in &c.leading {
            if self.comment_newline[i] {
                t.push("\n".into());
            }
            t.push(T::Range(self.comments[i].start, self.comments[i].end));
        }
        t.push("\n".into());
        t
    }

    fn trailing_t(&self, node_start: usize) -> Ts {
        let Some(c) = self.attached.get(&node_start).filter(|c| !c.trailing.is_empty()) else { return Ts::new() };
        let mut t = Ts::new();
        for &i in &c.trailing {
            t.push(if self.comment_newline[i] { "\n" } else { " " }.into());
            t.push(T::Range(self.comments[i].start, self.comments[i].end));
        }
        t.push("\n".into());
        t
    }

    // --- snippets ------------------------------------------------------------------------

    fn snippet(&mut self, node: &'m LNode<'m, 'a>, parent: Parent<'m, 'm, 'a>) -> Result<()> {
        let LNode::SnippetBlock { start, end, expression, parameters, type_params, children } = node else { unreachable!() };
        self.scope_push();
        let parent_component = match (self.element, parent) {
            (Some(el), Parent::Element(p)) if self.els.kind(el) == Kind::Component && p.kind == "InlineComponent" => Some(el),
            (Some(el), _) if self.els.kind(el) == Kind::Element && self.els.arena[el].tag_name == "svelte:boundary" => Some(el),
            _ => None,
        };
        self.element_before_snippet.push(self.element);
        self.element = None;

        self.handle_snippet(*start, end.unwrap_or(0), expression, *parameters, *type_params, children, parent_component)?;
        match parent {
            Parent::Root => {}
            Parent::Element(p) => {
                if let Some(c) = &p.children {
                    self.add_pending_hoist(p.kind, c);
                }
            }
            Parent::Block { ty, children } => self.add_pending_hoist(ty, children),
            _ => {}
        }

        // children of the SnippetBlock: expression, parameters, children
        self.expr(expression);
        if let Some(arrow) = parameters {
            if let Expression::ArrowFunctionExpression(f) = &arrow.expr {
                self.walk_js(|w| {
                    for p in &f.params.items {
                        w.visit_formal_parameter(p);
                    }
                    if let Some(rest) = &f.params.rest {
                        w.visit_formal_parameter_rest(rest);
                    }
                });
            }
        }
        self.children(children, Parent::Block { ty: "SnippetBlock", children })?;

        self.scope_pop();
        self.element = self.element_before_snippet.pop().flatten();
        Ok(())
    }

    fn add_pending_hoist(&mut self, ty: &'static str, children: &'m [LNode<'m, 'a>]) {
        if self.pending_snippet_seen.insert(children.as_ptr() as usize) {
            self.pending_snippet_hoist.push((ty, children));
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_snippet(
        &mut self,
        start: usize,
        end: usize,
        expression: &Expr,
        parameters: Option<&JsExpr>,
        type_params: Option<&str>,
        children: &[LNode],
        component: Option<usize>,
    ) -> Result<()> {
        let original = self.original();
        let is_implicit = component.is_some();
        let end_snippet = last_index_of(original, "{", end.saturating_sub(1)).unwrap_or(0);
        let ts_syntax = self.opts.is_ts_file || !self.opts.emit_jsdoc;
        let after = if is_implicit { "};return __sveltets_2_any(0)}" } else { "};return __sveltets_2_any(0)};" };
        if implicitly_closed(end_snippet, children, expression) {
            self.str.prepend_left(end, after)?;
        } else {
            self.str.overwrite(end_snippet, end, after, true)?;
        }

        // parameters (ESTree nodes with their raw positions)
        let mut params: Vec<ParamInfo> = parameters.map(snippet_params).unwrap_or_default();
        // acorn attaches comments before the first parameter as its `leadingComments`
        if let (Some(first), Some(ctx)) = (params.first_mut(), parameters.and_then(|p| p.comments)) {
            first.leading_comment_start = self.comments[..ctx.upto as usize]
                .iter()
                .find(|c| c.start >= ctx.index as usize && c.start < first.start)
                .map(|c| c.start);
        }
        let last = params.last();
        let start_end = index_of(original, "}", last.map_or(expression.end(), |p| p.type_end)).map_or(0, |i| i + 1);
        let parameters_range = match (params.first(), last) {
            (Some(first), Some(last)) => Some((first.leading_comment_start.unwrap_or(first.start), last.type_end)),
            _ => None,
        };
        let after_parameters = format!(" => {{ async (){IGNORE_POSITION_COMMENT} => {{");

        if let Some(component) = component {
            let empty_id = expression.start() == expression.end();
            if empty_id {
                self.str.overwrite(start, expression.start() - 1, "", true)?;
                self.str.overwrite(expression.start() - 1, expression.start(), " ", true)?;
            } else {
                self.str.overwrite(start, expression.start(), "", true)?;
            }
            let mut t: Ts = vec!["(".into()];
            if let Some((ps, pe)) = parameters_range {
                t.push(T::Range(ps, pe));
                self.str.overwrite(expression.end(), ps, "", true)?;
                self.str.overwrite(pe, start_end, "", true)?;
            } else {
                self.str.overwrite(expression.end(), start_end, "", true)?;
            }
            t.push(format!("){after_parameters}").into());
            t.push(T::Range(start_end, end));
            let name = (expression.start() - usize::from(empty_id), expression.end());
            if self.els.kind(component) == Kind::Component {
                self.els.add_implicit_snippet_prop(original, component, name, t);
            } else {
                self.els.add_attribute(component, vec![T::Range(name.0, name.1)], Some(t));
            }
        } else {
            let generics = match type_params {
                Some(tp) if ts_syntax => format!("<{tp}>"),
                _ => String::new(),
            };
            let returns = if ts_syntax { "" } else { "/** @returns {ReturnType<import('svelte').Snippet>} */ " };
            let mut t: Ts = vec![
                "const ".into(),
                T::Range(expression.start(), expression.end()),
                IGNORE_POSITION_COMMENT.into(),
                format!(" = {returns}{generics}(").into(),
            ];
            if let Some((ps, pe)) = parameters_range {
                t.push(T::Range(ps, pe));
            }
            t.push(")".into());
            t.push(if ts_syntax { surround_with_ignore_comments(": ReturnType<import('svelte').Snippet>") } else { String::new() }.into());
            t.push(after_parameters.into());
            transform(&mut self.str, start, start_end, &t)?;
        }
        Ok(())
    }

    fn hoist_snippet_block(&mut self, ty: &'static str, children: &[LNode]) -> Result<()> {
        if matches!(ty, "InlineComponent" | "SvelteBoundary") {
            return Ok(());
        }
        let mut target: Option<usize> = None;
        for node in children {
            if !matches!(node, LNode::SnippetBlock { .. }) {
                if target.is_none() {
                    match node {
                        LNode::Text(t) if t.data.trim().is_empty() => {}
                        LNode::Text(t) => target = Some(t.end),
                        other => target = Some(lnode_start(other)),
                    }
                }
                continue;
            }
            let Some(target) = target else { continue };
            let (s, e) = (lnode_start(node), lnode_end(node));
            if s == target {
                continue;
            }
            self.str.move_(s, e, target)?;
        }
        Ok(())
    }

    fn blank_other_script_tags(&mut self, root: &LegacyRoot, verbatim: &[Verbatim]) -> Result<()> {
        // scripts nested inside elements, or inside {@html}, aren't top level
        let mut nested: Vec<(usize, usize)> = Vec::new();
        collect_nested_scripts(&root.children, true, &mut nested);
        for v in verbatim.iter().filter(|v| !v.is_style) {
            let top_level = !nested.iter().any(|&(s, e)| (s == v.start && e == v.end) || (s <= v.start && e >= v.end && s != usize::MAX));
            if !top_level {
                self.str.remove(v.start, v.end)?;
            }
        }
        Ok(())
    }
}

/// Ranges that make a script not top level: `<script>` elements below the root
/// (`checkIfElementIsScriptTag`) and `{@html}` tags containing one (`checkIfContainsScriptTag`)
fn collect_nested_scripts(children: &[LNode], at_root: bool, out: &mut Vec<(usize, usize)>) {
    for c in children {
        match c {
            LNode::Element(el) => {
                if !at_root && el.name == "script" {
                    out.push((el.start, el.end.unwrap_or(0)));
                }
                if let Some(ch) = &el.children {
                    collect_nested_scripts(ch, false, out);
                }
            }
            LNode::RawMustacheTag { start, end, .. } => out.push((*start, *end)),
            LNode::IfBlock { children, else_block, .. } => {
                collect_nested_scripts(children, false, out);
                if let Some(e) = else_block {
                    collect_nested_scripts(&e.children, false, out);
                }
            }
            LNode::EachBlock { children, else_block, .. } => {
                collect_nested_scripts(children, false, out);
                if let Some(e) = else_block {
                    collect_nested_scripts(&e.children, false, out);
                }
            }
            LNode::AwaitBlock { pending, then, catch, .. } => {
                for b in [pending, then, catch] {
                    collect_nested_scripts(&b.children, false, out);
                }
            }
            LNode::KeyBlock { children, .. } | LNode::SnippetBlock { children, .. } => collect_nested_scripts(children, false, out),
            _ => {}
        }
    }
}

// --- helpers --------------------------------------------------------------------------------

struct ParamInfo {
    start: usize,
    /// `typeAnnotation?.end ?? end`
    type_end: usize,
    leading_comment_start: Option<usize>,
}

fn snippet_params(arrow: &JsExpr) -> Vec<ParamInfo> {
    let Expression::ArrowFunctionExpression(f) = &arrow.expr else { return Vec::new() };
    let mut out = Vec::new();
    for p in &f.params.items {
        // acorn: the param node is the pattern (an AssignmentPattern with a default), with
        // `typeAnnotation` on the pattern's left side
        let pattern_span = p.pattern.span();
        let start = pattern_span.start as usize;
        let end = match &p.initializer {
            Some(init) => init.span().end as usize,
            None => p.type_annotation.as_ref().map_or(pattern_span.end, |t| t.span.end) as usize,
        };
        out.push(ParamInfo { start, type_end: end, leading_comment_start: None });
    }
    if let Some(rest) = &f.params.rest {
        let end = rest.type_annotation.as_ref().map_or(rest.span.end, |t| t.span.end) as usize;
        out.push(ParamInfo { start: rest.span.start as usize, type_end: end, leading_comment_start: None });
    }
    out
}

fn debug_identifiers(args: &crate::ast::DebugArgs) -> Vec<(usize, usize)> {
    match args {
        crate::ast::DebugArgs::All => Vec::new(),
        crate::ast::DebugArgs::One(e) => vec![(e.start(), e.end())],
        crate::ast::DebugArgs::Sequence(Expr::Js(js)) => match js.inner() {
            Expression::SequenceExpression(seq) => seq.expressions.iter().map(|e| (oxc_start(e), oxc_end(e))).collect(),
            _ => Vec::new(),
        },
        crate::ast::DebugArgs::Sequence(e) => vec![(e.start(), e.end())],
    }
}

fn chunk_range(c: &LChunk) -> (usize, usize) {
    match c {
        LChunk::Text(Chunk::Text { start, end, .. }) => (*start, *end),
        LChunk::Text(Chunk::Expression { start, end, .. }) => (*start, *end),
        LChunk::MustacheTag { start, end, .. } | LChunk::AttributeShorthand { start, end, .. } => (*start, *end),
    }
}

pub fn lnode_start(n: &LNode) -> usize {
    match n {
        LNode::Text(t) => t.start,
        other => other.start().unwrap_or(0),
    }
}

pub fn lnode_end(n: &LNode) -> usize {
    match n {
        LNode::Text(t) => t.end,
        LNode::Comment { end, .. }
        | LNode::MustacheTag { end, .. }
        | LNode::RawMustacheTag { end, .. }
        | LNode::DebugTag { end, .. }
        | LNode::ConstTag { end, .. }
        | LNode::RenderTag { end, .. } => *end,
        LNode::IfBlock { end, .. }
        | LNode::EachBlock { end, .. }
        | LNode::AwaitBlock { end, .. }
        | LNode::KeyBlock { end, .. }
        | LNode::SnippetBlock { end, .. } => end.unwrap_or(0),
        LNode::Element(el) => el.end.unwrap_or(0),
        LNode::DeclarationTag { end, .. } => *end,
    }
}

/// `isImplicitlyClosedBlock(end, block)`
fn implicitly_closed(end: usize, children: &[LNode], expression: &Expr) -> bool {
    end < children.last().map_or(expression.end(), lnode_end)
}

fn oxc_start(e: &Expression) -> usize {
    e.without_parentheses().span().start as usize
}

fn oxc_end(e: &Expression) -> usize {
    e.without_parentheses().span().end as usize
}

/// `getEnd(node)` for an oxc expression
fn oxc_get_end(e: &Expression) -> usize {
    match e.without_parentheses() {
        Expression::TSAsExpression(x) => oxc_end(&x.expression),
        Expression::TSSatisfiesExpression(x) => oxc_end(&x.expression),
        Expression::TSNonNullExpression(x) => oxc_end(&x.expression),
        other => other.span().end as usize,
    }
}

/// `getEnd(expression)`: the end excluding a type assertion
fn expr_get_end(e: &Expr) -> usize {
    match e {
        Expr::Js(js) if js.fix.is_none() => oxc_get_end(js.inner()),
        other => other.end(),
    }
}

fn expr_is_ts(e: &Expr) -> bool {
    match e {
        Expr::Js(js) => matches!(
            js.inner(),
            Expression::TSAsExpression(_) | Expression::TSSatisfiesExpression(_) | Expression::TSNonNullExpression(_)
        ),
        _ => false,
    }
}

fn pattern_start(p: &Pattern) -> usize {
    p.start()
}

/// The pattern's `end` in the legacy AST (extended over a type annotation for destructuring)
fn pattern_end(p: &Pattern) -> usize {
    match p {
        Pattern::Ident { end, .. } => *end,
        Pattern::Destructure { assign, type_ann } => match type_ann {
            Some(t) => t.end,
            None => match assign.inner() {
                Expression::AssignmentExpression(a) => a.left.span().end as usize,
                other => other.span().end as usize,
            },
        },
    }
}

/// `getEnd(pattern)`: `typeAnnotation?.start ?? end`
fn pattern_get_end(p: &Pattern) -> usize {
    match p {
        Pattern::Ident { type_ann: Some(t), .. } | Pattern::Destructure { type_ann: Some(t), .. } => t.start,
        other => pattern_end(other),
    }
}

/// `value.typeAnnotation?.end ?? value.end`
fn pattern_type_end(p: &Pattern) -> usize {
    match p {
        Pattern::Ident { type_ann: Some(t), .. } | Pattern::Destructure { type_ann: Some(t), .. } => t.end,
        other => pattern_end(other),
    }
}

fn literal_truthy(e: &Expr) -> bool {
    match e {
        Expr::Js(js) => match js.inner() {
            Expression::BooleanLiteral(b) => b.value,
            Expression::NumericLiteral(n) => n.value != 0.0 && !n.value.is_nan(),
            Expression::StringLiteral(s) => !s.value.is_empty(),
            Expression::NullLiteral(_) => false,
            _ => false,
        },
        _ => false,
    }
}

/// `/\n[ \t]*$/.test(original.slice(max(start - 100, 0), start))`
fn preceded_by_newline(original: &str, start: usize) -> bool {
    let mut from = start.saturating_sub(100);
    while !original.is_char_boundary(from) {
        from -= 1;
    }
    let before = original[from..start].trim_end_matches([' ', '\t']);
    before.ends_with('\n')
}

/// `isNaN(str)` (string → number coercion)
fn js_is_nan(s: &str) -> bool {
    let t = s.trim_matches(crate::parser::utils::is_whitespace_char);
    if t.is_empty() {
        return false;
    }
    let lower = t.to_ascii_lowercase();
    if let Some(hex) = lower.strip_prefix("0x") {
        return hex.is_empty() || !hex.chars().all(|c| c.is_ascii_hexdigit());
    }
    if let Some(bin) = lower.strip_prefix("0b") {
        return bin.is_empty() || !bin.chars().all(|c| c == '0' || c == '1');
    }
    if let Some(oct) = lower.strip_prefix("0o") {
        return oct.is_empty() || !oct.chars().all(|c| ('0'..='7').contains(&c));
    }
    if matches!(t, "Infinity" | "+Infinity" | "-Infinity") {
        return false;
    }
    let valid = t.parse::<f64>().is_ok() && !lower.contains("inf") && !lower.contains("nan");
    !valid
}

/// `tryEscapeAttributeValue`
fn try_escape_attribute_value(s: &str, use_template_literal: bool) -> Option<String> {
    if !s.contains('\\') && (use_template_literal || !s.contains('\n')) {
        return None;
    }
    let json = serde_json::to_string(s).unwrap();
    Some(json[1..json.len() - 1].to_string())
}
