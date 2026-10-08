//! The server transform's template visitors (`template_visitors` in `transform-server.js`)
//! and the shared template helpers of `3-transform/utils.js` (`clean_nodes`, namespaces).

use crate::analyze::nodes::P;
use crate::ast::{Expr, FragId, Node as TNode, NodeId};
use crate::estree::builders as b;
use crate::estree::{Node, NodeKind};
use crate::transform::js;

use super::super::js::PathNode;
use super::utils::{build_template, create_child_block, prepend_block_marker, PromiseOptimiser, BLOCK_CLOSE, BLOCK_OPEN, BLOCK_OPEN_ELSE, EMPTY_COMMENT};
use super::{shared, Server, State};

/// A node of a cleaned fragment: a template node, or a text node whose whitespace
/// `clean_nodes` trimmed (the JS mutates the node), or the comment it adds after a lone
/// `<script>`
#[derive(Clone, Debug)]
pub enum Child<'s> {
    Node(NodeId),
    Text { node: NodeId, data: String, raw: String },
    Comment(&'s str),
}

impl<'s> Child<'s> {
    pub fn node_id(&self) -> Option<NodeId> {
        match self {
            Child::Node(n) | Child::Text { node: n, .. } => Some(*n),
            Child::Comment(_) => None,
        }
    }

    /// Text, Comment or ExpressionTag
    pub fn is_text_like(&self, ast: &crate::ast::Ast) -> bool {
        match self {
            Child::Text { .. } | Child::Comment(_) => true,
            Child::Node(n) => matches!(ast.nodes[*n], TNode::Text { .. } | TNode::Comment { .. } | TNode::ExpressionTag { .. }),
        }
    }

    /// `(data, is_comment)` for Text and Comment nodes
    pub fn text_data(&self, ast: &crate::ast::Ast) -> Option<(String, bool)> {
        match self {
            Child::Text { data, .. } => Some((data.clone(), false)),
            Child::Comment(d) => Some((d.to_string(), true)),
            Child::Node(n) => match &ast.nodes[*n] {
                TNode::Text { data, .. } => Some((data.to_string(), false)),
                TNode::Comment { data, .. } => Some((data.to_string(), true)),
                _ => None,
            },
        }
    }

    fn ty(&self, ast: &crate::ast::Ast) -> &'static str {
        match self {
            Child::Text { .. } => "Text",
            Child::Comment(_) => "Comment",
            Child::Node(n) => ast.nodes[*n].type_name(),
        }
    }
}

/// What `clean_nodes` returns
pub struct Cleaned<'s> {
    pub hoisted: Vec<NodeId>,
    pub trimmed: Vec<Child<'s>>,
    pub is_standalone: bool,
    pub is_text_first: bool,
}

/// The parent a fragment's nodes are cleaned for
#[derive(Clone, Copy)]
pub enum Parent {
    /// the root Fragment (the JS passes the fragment itself)
    Root,
    Node(NodeId),
}

/// `[ \t\r\n]`, the whitespace of `regex_starts_with_whitespaces` & co
fn is_whitespace(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\r' | '\n')
}

impl<'a, 's> Server<'a, 's> {
    fn parent_type(&self, parent: Parent) -> &'static str {
        match parent {
            Parent::Root => "Fragment",
            Parent::Node(n) => self.ast().nodes[n].type_name(),
        }
    }

    fn parent_element_name(&self, parent: Parent) -> Option<&'s str> {
        match parent {
            Parent::Node(n) => match &self.ast().nodes[n] {
                TNode::Element(el) if el.kind == "RegularElement" => Some(el.name),
                _ => None,
            },
            Parent::Root => None,
        }
    }

    /// `clean_nodes(parent, nodes, path, namespace, state, preserve_whitespace, preserve_comments)`
    pub fn clean_nodes(&self, parent: Parent, nodes: &[NodeId], namespace: &str, preserve_whitespace: bool, preserve_comments: bool) -> Cleaned<'s> {
        let ast = self.ast();
        // TODO: `sort_const_tags` in legacy mode
        let mut hoisted = Vec::new();
        let mut regular: Vec<Child<'s>> = Vec::new();
        for &n in nodes {
            let node = &ast.nodes[n];
            if matches!(node, TNode::Comment { .. }) && !preserve_comments {
                continue;
            }
            let hoist = match node {
                TNode::ConstTag { .. } | TNode::DeclarationTag { .. } | TNode::DebugTag { .. } | TNode::SnippetBlock { .. } => true,
                TNode::Element(el) => matches!(el.kind, "SvelteBody" | "SvelteWindow" | "SvelteDocument" | "SvelteHead" | "TitleElement"),
                _ => false,
            };
            if hoist {
                hoisted.push(n);
            } else {
                regular.push(Child::Node(n));
            }
        }

        let text_of = |c: &Child<'s>| -> Option<(String, String)> {
            match c {
                Child::Text { data, raw, .. } => Some((data.clone(), raw.clone())),
                Child::Node(n) => match &ast.nodes[*n] {
                    TNode::Text { data, raw, .. } => Some((data.to_string(), raw.to_string())),
                    _ => None,
                },
                _ => None,
            }
        };

        let mut trimmed: Vec<Child<'s>>;
        if !preserve_whitespace {
            trimmed = Vec::new();
            // leading/trailing whitespace-only text nodes
            while let Some(first) = regular.first() {
                match text_of(first) {
                    Some((data, _)) if !data.chars().any(|c| !is_whitespace(c)) => {
                        regular.remove(0);
                    }
                    _ => break,
                }
            }
            if let Some(first) = regular.first_mut() {
                if let Some((data, raw)) = text_of(first) {
                    let node = first.node_id().unwrap();
                    *first = Child::Text { node, data: data.trim_start_matches(is_whitespace).to_string(), raw: raw.trim_start_matches(is_whitespace).to_string() };
                }
            }
            while let Some(last) = regular.last() {
                match text_of(last) {
                    Some((data, _)) if !data.chars().any(|c| !is_whitespace(c)) => {
                        regular.pop();
                    }
                    _ => break,
                }
            }
            if let Some(last) = regular.last_mut() {
                if let Some((data, raw)) = text_of(last) {
                    let node = last.node_id().unwrap();
                    *last = Child::Text { node, data: data.trim_end_matches(is_whitespace).to_string(), raw: raw.trim_end_matches(is_whitespace).to_string() };
                }
            }

            let parent_name = self.parent_element_name(parent);
            let in_text = self.path.iter().any(|p| matches!(p, PathNode::Tpl(P::Node(n)) if matches!(&ast.nodes[*n], TNode::Element(el) if el.kind == "RegularElement" && el.name == "text")));
            let can_remove_entirely = (namespace == "svg" && parent_name != Some("text") && !in_text)
                || matches!(parent_name, Some("select" | "tr" | "table" | "tbody" | "thead" | "tfoot" | "colgroup" | "datalist"));

            // the JS mutates each text node, and reads the previous one's mutated data
            let mut modified: Vec<Option<String>> = vec![None; regular.len()];
            for i in 0..regular.len() {
                let node = &regular[i];
                let Some((mut data, mut raw)) = text_of(node) else {
                    trimmed.push(node.clone());
                    continue;
                };
                let prev_ty = if i > 0 { Some(regular[i - 1].ty(ast)) } else { None };
                let next_ty = regular.get(i + 1).map(|c| c.ty(ast));
                if prev_ty != Some("ExpressionTag") {
                    let prev_data = if i > 0 { modified[i - 1].clone().or_else(|| text_of(&regular[i - 1]).map(|(d, _)| d)) } else { None };
                    let prev_ends_ws = prev_data.is_some_and(|d| d.ends_with(is_whitespace));
                    let replacement = if prev_ends_ws { "" } else { " " };
                    data = replace_leading_ws(&data, replacement);
                    raw = replace_leading_ws(&raw, replacement);
                }
                if next_ty != Some("ExpressionTag") {
                    data = replace_trailing_ws(&data, " ");
                    raw = replace_trailing_ws(&raw, " ");
                }
                modified[i] = Some(data.clone());
                if !data.is_empty() && (data != " " || !can_remove_entirely) {
                    trimmed.push(Child::Text { node: node.node_id().unwrap(), data, raw });
                }
            }
        } else {
            trimmed = regular;
        }

        // a lone newline as the first child of a <pre> is dropped
        if self.parent_element_name(parent) == Some("pre") {
            if let Some((data, _)) = trimmed.first().and_then(text_of) {
                if data == "\n" || data == "\r\n" {
                    trimmed.remove(0);
                }
            }
        }

        // a lone <script> gets a comment after it
        if trimmed.len() == 1 {
            if let Child::Node(n) = &trimmed[0] {
                if matches!(&ast.nodes[*n], TNode::Element(el) if el.kind == "RegularElement" && el.name == "script") {
                    trimmed.push(Child::Comment(""));
                }
            }
        }

        let first = trimmed.first();
        let is_standalone = trimmed.len() == 1
            && match first {
                Some(Child::Node(n)) => match &ast.nodes[*n] {
                    TNode::RenderTag { .. } => !self.an.node_meta.get(n).is_some_and(|m| m.dynamic),
                    TNode::Element(el) if el.kind == "Component" => {
                        !self.options.hmr
                            && !self.an.node_meta.get(n).is_some_and(|m| m.dynamic)
                            && !el.attributes.iter().any(|a| matches!(a, crate::ast::Attr::Attribute { name, .. } if name.starts_with("--")))
                    }
                    _ => false,
                },
                _ => false,
            };
        let is_text_first = matches!(self.parent_type(parent), "Fragment" | "SnippetBlock" | "EachBlock" | "SvelteComponent" | "SvelteBoundary" | "Component" | "SvelteSelf")
            && first.is_some_and(|f| matches!(f.ty(ast), "Text" | "ExpressionTag"));

        Cleaned { hoisted, trimmed, is_standalone, is_text_first }
    }

    /// `infer_namespace(namespace, parent, nodes)`
    pub fn infer_namespace(&self, namespace: &'static str, parent: Parent, nodes: &[NodeId]) -> &'static str {
        let ast = self.ast();
        if let Parent::Node(p) = parent {
            if let TNode::Element(el) = &ast.nodes[p] {
                if el.kind == "RegularElement" && el.name == "foreignObject" {
                    return "html";
                }
                if el.kind == "RegularElement" || el.kind == "SvelteElement" {
                    let m = self.an.node_meta.get(&p).cloned().unwrap_or_default();
                    return if m.svg { "svg" } else if m.mathml { "mathml" } else { "html" };
                }
            }
        }
        let resets = match parent {
            Parent::Root => true,
            Parent::Node(p) => matches!(self.parent_type(Parent::Node(p)), "Fragment" | "Root" | "Component" | "SvelteComponent" | "SvelteFragment" | "SnippetBlock" | "SlotElement"),
        };
        if resets {
            let ns = self.check_nodes_for_namespace(nodes, "keep");
            if ns != "keep" && ns != "maybe_html" {
                return ns;
            }
        }
        let mut new_namespace: Option<&'static str> = None;
        for &n in nodes {
            let TNode::Element(el) = &ast.nodes[n] else { continue };
            if el.kind != "RegularElement" {
                continue;
            }
            let m = self.an.node_meta.get(&n).cloned().unwrap_or_default();
            if m.mathml {
                new_namespace = Some(if new_namespace.is_none() || new_namespace == Some("mathml") { "mathml" } else { "html" });
            } else if m.svg {
                new_namespace = Some(if new_namespace.is_none() || new_namespace == Some("svg") { "svg" } else { "html" });
            } else {
                return "html";
            }
        }
        new_namespace.unwrap_or(namespace)
    }

    fn check_nodes_for_namespace(&self, nodes: &[NodeId], mut namespace: &'static str) -> &'static str {
        for &n in nodes {
            let mut stopped = false;
            self.ns_walk(n, &mut namespace, &mut stopped);
            if namespace == "html" {
                return namespace;
            }
        }
        namespace
    }

    fn ns_walk(&self, n: NodeId, namespace: &mut &'static str, stopped: &mut bool) {
        if *stopped {
            return;
        }
        let ast = self.ast();
        match &ast.nodes[n] {
            TNode::Element(el) if el.kind == "RegularElement" || el.kind == "SvelteElement" => {
                let m = self.an.node_meta.get(&n).cloned().unwrap_or_default();
                if !m.svg && !m.mathml {
                    *namespace = "html";
                    *stopped = true;
                } else if *namespace == "keep" {
                    *namespace = if m.svg { "svg" } else { "mathml" };
                }
            }
            TNode::Text { data, .. } => {
                if !crate::analyze::utils::js_trim(data).is_empty() {
                    *namespace = "maybe_html";
                }
            }
            TNode::EachBlock { body, fallback, .. } => {
                // zimmerframe visits the expression too, which can't contain template nodes
                for f in [Some(*body), *fallback].into_iter().flatten() {
                    self.ns_walk_fragment(f, namespace, stopped);
                }
            }
            TNode::IfBlock { consequent, alternate, .. } => {
                for f in [Some(*consequent), *alternate].into_iter().flatten() {
                    self.ns_walk_fragment(f, namespace, stopped);
                }
            }
            TNode::AwaitBlock { pending, then, catch, .. } => {
                for f in [*pending, *then, *catch].into_iter().flatten() {
                    self.ns_walk_fragment(f, namespace, stopped);
                }
            }
            TNode::KeyBlock { fragment, .. } => self.ns_walk_fragment(*fragment, namespace, stopped),
            _ => {}
        }
    }

    fn ns_walk_fragment(&self, f: FragId, namespace: &mut &'static str, stopped: &mut bool) {
        for &n in &self.ast().fragments[f].nodes {
            self.ns_walk(n, namespace, stopped);
        }
    }

    // -----------------------------------------------------------------------------------
    // expressions in the template

    /// `node.metadata.expression` of a template node
    pub fn meta_of_node(&self, n: NodeId) -> u32 {
        self.an.meta_of.get(&P::Node(n).key()).copied().unwrap_or(0)
    }

    /// `metadata.is_async()`
    pub fn meta_is_async(&self, meta: u32) -> bool {
        self.an.metas[meta as usize].has_await || !self.an.meta_blockers(meta).is_empty()
    }

    /// `metadata.blockers()`
    pub fn meta_blockers_array(&self, meta: u32) -> Node {
        b::array(self.an.meta_blockers(meta).into_iter().map(|bl| super::utils::blocker_expression(self, bl)).collect::<Vec<_>>())
    }

    /// `scope.evaluate(expression)`
    pub fn evaluate_expr(&self, e: &'s Expr<'s>, scope: crate::analyze::scope::ScopeId) -> crate::analyze::evaluate::Evaluation {
        self.an.sc.evaluate(self.an.ast, crate::analyze::nodes::template_expr(e), scope)
    }

    /// The ESTree form of a template expression
    pub fn convert_expr(&self, e: &'s Expr<'s>) -> Node {
        match e {
            Expr::Js(js) => self.conv.expression(js.effective_root()),
            Expr::Ident { name, start, end, loc } => {
                let mut id = b::id(name.as_str());
                id.span = Some(crate::estree::Span::new(*start as u32, *end as u32));
                if matches!(loc, crate::ast::IdentLoc::Svelte) {
                    id.loc = Some(self.conv.location(oxc_span::Span::new(*start as u32, *end as u32)));
                }
                id.origin = Some(P::TplExpr(e).key());
                id
            }
            Expr::Literal { value, .. } => b::literal(value.as_str()),
        }
    }

    /// `context.visit(expression)` for an expression of template node `n`
    pub fn visit_template_expr(&mut self, n: NodeId, e: &'s Expr<'s>, st: &State) -> Node {
        let node = self.convert_expr(e);
        self.path.push(PathNode::Tpl(P::Node(n)));
        let out = self.visit_js(&node, st);
        self.path.pop();
        out
    }

    // -----------------------------------------------------------------------------------
    // Fragment

    /// The template root: `walk(analysis.template.ast)` visits the root Fragment
    pub fn visit_root_fragment(&mut self, st: &State) -> Node {
        let f = self.an.root.fragment;
        self.fragment(f, Parent::Root, st)
    }

    /// The `Fragment` visitor (with `set_scope`)
    pub fn fragment(&mut self, f: FragId, parent: Parent, st: &State) -> Node {
        let nodes = self.ast().fragments[f].nodes.clone();
        let scoped;
        let st = match self.scope_of_key(P::Fragment(f).key()) {
            Some(scope) if scope != st.scope => {
                scoped = State { scope, ..st.clone() };
                &scoped
            }
            _ => st,
        };
        self.fragment_nodes(f, &nodes, parent, st)
    }

    /// The `Fragment` visitor for `{ ...fragment, nodes }` (a component's slot)
    pub fn fragment_nodes(&mut self, f: FragId, nodes: &[NodeId], parent: Parent, st: &State) -> Node {
        let nodes = nodes.to_vec();
        let namespace = self.infer_namespace(st.namespace, parent, &nodes);
        let cleaned = self.clean_nodes(parent, &nodes, namespace, st.preserve_whitespace, self.options.preserve_comments);
        let state = State { init: shared(), template: shared(), namespace, is_standalone: cleaned.is_standalone, async_consts: Default::default(), ..st.clone() };

        self.path.push(PathNode::Tpl(P::Fragment(f)));
        for &h in &cleaned.hoisted {
            self.visit_node(h, &state);
        }
        if cleaned.is_text_first {
            state.template.borrow_mut().push(b::literal(EMPTY_COMMENT));
        }
        self.process_children(&cleaned.trimmed, &state);
        self.path.pop();

        self.push_async_consts(&state);
        let mut body = std::mem::take(&mut *state.init.borrow_mut());
        body.extend(build_template(std::mem::take(&mut *state.template.borrow_mut())));
        b::block(body)
    }

    /// `visit(node, { ...state })` for a child of `process_children`
    pub fn visit_child_node(&mut self, child: &Child<'s>, st: &State) {
        if let Some(n) = child.node_id() {
            self.visit_node(n, st);
        }
    }

    /// `context.visit(node, state)` for a template node (with `set_scope`)
    pub fn visit_node(&mut self, n: NodeId, st: &State) {
        let scoped;
        let st = match self.scope_of_key(P::Node(n).key()) {
            Some(scope) if scope != st.scope => {
                scoped = State { scope, ..st.clone() };
                &scoped
            }
            _ => st,
        };
        let ast = self.ast();
        self.path.push(PathNode::Tpl(P::Node(n)));
        match &ast.nodes[n] {
            TNode::Element(el) => match el.kind {
                "RegularElement" => {
                    self.path.pop();
                    self.regular_element(n, st);
                    return;
                }
                "TitleElement" => self.title_element(n, st),
                "SvelteElement" => self.svelte_element(n, st),
                "SvelteBoundary" => self.svelte_boundary(n, st),
                "SlotElement" => self.slot_element(n, st),
                "SvelteFragment" => {
                    let block = self.fragment(el.fragment, Parent::Node(n), st);
                    st.template.borrow_mut().push(block);
                }
                "SvelteComponent" => {
                    let e = el.expression.as_ref().map(|e| self.visit_template_expr_here(e, st)).unwrap_or_else(b::void0);
                    self.build_inline_component(n, e, st);
                }
                "SvelteSelf" => {
                    let name = self.an.name.clone();
                    self.build_inline_component(n, b::id(name.as_str()), st);
                }
                "Component" => {
                    let expression = self.component_expression(el.name, st);
                    self.build_inline_component(n, expression, st);
                }
                "SvelteHead" => self.svelte_head(n, st),
                _ => {}
            },
            TNode::IfBlock { .. } => self.if_block(n, st),
            TNode::EachBlock { .. } => self.each_block(n, st),
            TNode::HtmlTag { expression, .. } => self.html_tag(n, expression, st),
            TNode::KeyBlock { fragment, .. } => {
                let meta = self.meta_of_node(n);
                let is_async = self.meta_is_async(meta);
                if is_async {
                    st.template.borrow_mut().push(b::literal(BLOCK_OPEN));
                }
                let block = self.fragment(*fragment, Parent::Node(n), st);
                st.template.borrow_mut().extend([b::literal(EMPTY_COMMENT), block, b::literal(EMPTY_COMMENT)]);
                if is_async {
                    st.template.borrow_mut().push(b::literal(BLOCK_CLOSE));
                }
            }
            TNode::AwaitBlock { .. } => self.await_block(n, st),
            TNode::SnippetBlock { .. } => self.snippet_block(n, st),
            TNode::RenderTag { expression, .. } => self.render_tag(n, expression, st),
            TNode::ConstTag { id, init, .. } => {
                let id = self.convert_pattern(id);
                let id = self.visit_js(&id, st);
                let init = self.convert_expr(init);
                let init = self.visit_js(&init, st);
                if self.an.promises_id.contains_key(&n) {
                    let ids = js::extract_identifiers(&id).into_iter().cloned().collect();
                    let assignment = b::stmt(b::assignment("=", id, init));
                    self.add_async_declaration(n, st, ids, vec![assignment], "let");
                } else {
                    st.init.borrow_mut().push(b::r#const(id, init));
                }
            }
            TNode::DeclarationTag { declaration, .. } => {
                if let crate::ast::Declaration::Js(stmt) = declaration {
                    let node = self.conv.statement(&stmt.stmt);
                    let declaration = self.visit_js(&node, st);
                    match (&declaration.kind, self.an.promises_id.contains_key(&n) && node.is("VariableDeclaration")) {
                        (NodeKind::VariableDeclaration(v), true) => {
                            // `build_async_declaration_parts(declaration)`
                            let mut ids: Vec<Node> = Vec::new();
                            for d in &v.declarations {
                                let NodeKind::VariableDeclarator(d) = &d.kind else { continue };
                                for id in js::extract_identifiers(&d.id) {
                                    let name = js::ident(id).unwrap_or("");
                                    match ids.iter_mut().find(|x| js::ident(x) == Some(name)) {
                                        Some(x) => *x = id.clone(),
                                        None => ids.push(id.clone()),
                                    }
                                }
                            }
                            let assignments = v
                                .declarations
                                .iter()
                                .filter_map(|d| match &d.kind {
                                    NodeKind::VariableDeclarator(d) => d.init.as_ref().map(|i| b::stmt(b::assignment("=", (*d.id).clone(), (**i).clone()))),
                                    _ => None,
                                })
                                .collect();
                            let kind = v.kind.as_str().to_string();
                            self.add_async_declaration(n, st, ids, assignments, &kind);
                        }
                        _ => st.init.borrow_mut().push(declaration),
                    }
                }
            }
            TNode::DebugTag { identifiers, .. } => self.debug_tag(identifiers, st),
            _ => {}
        }
        self.path.pop();
    }

    /// `var promises = $$renderer.run([...thunks])` of a fragment's async `{@const}` tags
    fn push_async_consts(&mut self, state: &State) {
        if let Some(ac) = state.async_consts.borrow().as_ref() {
            if !ac.thunks.is_empty() {
                state.init.borrow_mut().push(b::var(b::id(ac.id.as_str()), b::call("$$renderer.run", vec![b::array(ac.thunks.clone())])));
            }
        }
    }

    /// `add_async_declaration(context, metadata, ids, assignments, kind)`
    fn add_async_declaration(&mut self, n: NodeId, st: &State, ids: Vec<Node>, assignments: Vec<Node>, kind: &str) {
        let promises = self.an.promises_id[&n];
        if st.async_consts.borrow().is_none() {
            *st.async_consts.borrow_mut() = Some(super::AsyncConsts { id: self.an.promise_ids[promises as usize].clone(), thunks: Vec::new() });
        }
        for id in &ids {
            let name = js::ident(id).unwrap_or("");
            st.init.borrow_mut().push(if kind == "var" { b::var(b::id(name), None) } else { b::r#let(b::id(name), None) });
        }
        let current = st.async_consts.borrow().as_ref().map(|a| a.id.clone());
        let meta = self.meta_of_node(n);
        let blockers: Vec<Node> = self.an.metas[meta as usize]
            .references
            .iter()
            .filter_map(|&r| self.binding(r).blocker)
            .filter(|bl| {
                let obj = match bl.object {
                    Some(o) => self.an.promise_ids[o as usize].as_str(),
                    None => "$$promises",
                };
                current.as_deref() != Some(obj)
            })
            .map(|bl| super::utils::blocker_expression(self, bl))
            .collect();
        let mut thunks = Vec::new();
        if blockers.len() == 1 {
            thunks.push(b::thunk(blockers.into_iter().next().unwrap()));
        } else if !blockers.is_empty() {
            thunks.push(b::thunk(b::call("Promise.all", vec![b::array(blockers)])));
        }
        // keep the number of thunks pushed in sync with analysis phase
        let has_await = self.an.metas[meta as usize].has_await || assignments.iter().any(b::has_await_expression);
        let body = if assignments.len() == 1 {
            let assignment = assignments.into_iter().next().unwrap();
            match assignment.kind {
                NodeKind::ExpressionStatement(e) => *e.expression,
                _ => assignment,
            }
        } else {
            b::block(assignments)
        };
        thunks.push(b::thunk_with(body, has_await));
        if let Some(ac) = st.async_consts.borrow_mut().as_mut() {
            ac.thunks.extend(thunks);
        }
    }

    /// A template pattern (`{#each}` context, `{:then}` value, `{@const}` id) as ESTree
    pub fn convert_pattern(&self, p: &'s crate::ast::Pattern<'s>) -> Node {
        match p {
            crate::ast::Pattern::Ident { name, start, end, .. } => {
                let mut id = b::id(name.as_str());
                id.span = Some(crate::estree::Span::new(*start as u32, *end as u32));
                id.loc = Some(self.conv.location(oxc_span::Span::new(*start as u32, *end as u32)));
                id.origin = Some(P::PatIdent(p).key());
                id
            }
            crate::ast::Pattern::Destructure { assign, .. } => match assign.inner() {
                oxc_ast::ast::Expression::AssignmentExpression(a) => self.conv.assignment_target(&a.left),
                other => self.conv.expression(other),
            },
        }
    }

    fn if_block(&mut self, n: NodeId, st: &State) {
        let ast = self.ast();
        let TNode::IfBlock { test, consequent, alternate, .. } = &ast.nodes[n] else { return };
        let mut consequent_block = self.fragment(*consequent, Parent::Node(n), st);
        prepend_block_marker(&mut consequent_block, "<!--[0-->");
        let test = self.visit_template_expr_here(test, st);
        let mut branches = vec![(test, consequent_block)];
        let mut index = 1;
        let mut alt = *alternate;
        let flattened = self.an.node_meta.get(&n).and_then(|m| m.flattened.clone()).unwrap_or_default();
        for elseif in flattened {
            let TNode::IfBlock { test, consequent, alternate, .. } = &ast.nodes[elseif] else { continue };
            self.path.push(PathNode::Tpl(P::Node(elseif)));
            let mut branch = self.fragment(*consequent, Parent::Node(elseif), st);
            prepend_block_marker(&mut branch, &format!("<!--[{index}-->"));
            index += 1;
            let t = self.visit_template_expr_here(test, st);
            self.path.pop();
            branches.push((t, branch));
            alt = *alternate;
        }
        let mut final_alternate = match alt {
            Some(f) => self.fragment(f, Parent::Node(n), st),
            None => b::block(vec![]),
        };
        prepend_block_marker(&mut final_alternate, "<!--[-1-->");
        let mut statement = final_alternate;
        let mut first = true;
        for (test, block) in branches.into_iter().rev() {
            statement = b::r#if(test, block, Some(statement));
            first = false;
        }
        let _ = first;
        let meta = self.meta_of_node(n);
        let blockers = self.meta_blockers_array(meta);
        let has_await = self.an.metas[meta as usize].has_await;
        let mut t = st.template.borrow_mut();
        t.extend(create_child_block(vec![statement], blockers, has_await));
        t.push(b::literal(BLOCK_CLOSE));
    }

    /// `context.visit(expression)` with the current template node on the path
    pub fn visit_template_expr_here(&mut self, e: &'s Expr<'s>, st: &State) -> Node {
        let node = self.convert_expr(e);
        self.visit_js(&node, st)
    }

    fn each_block(&mut self, n: NodeId, st: &State) {
        let ast = self.ast();
        let TNode::EachBlock { expression, context, body, fallback, index, .. } = &ast.nodes[n] else { return };
        let meta = self.an.node_meta.get(&n).cloned().unwrap_or_default();
        // the collection is evaluated in the parent scope (the block's scope is the body's)
        let collection = self.visit_template_expr_here(expression, st);
        let index_id = if meta.contains_group_binding || index.is_none() {
            b::id(self.an.sc.each_index.get(&n).cloned().unwrap_or_else(|| "$$index".into()).as_str())
        } else {
            b::id(index.as_deref().unwrap())
        };
        let array_id = self.an.sc.unique("each_array");
        let mut statements = vec![b::r#const(b::id(array_id.as_str()), b::call("$.ensure_array_like", vec![collection]))];
        let mut each = Vec::new();
        if let Some(c) = context {
            let pattern = self.convert_pattern(c);
            each.push(b::r#let(pattern, b::member_with(b::id(array_id.as_str()), index_id.clone(), true, false)));
        }
        let index_name = super::super::js::ident(&index_id).map(str::to_string);
        if let Some(i) = index {
            if index_name.as_deref() != Some(i.as_str()) {
                each.push(b::r#let(b::id(i.as_str()), index_id.clone()));
            }
        }
        let body_scope = self.an.sc.each_scope.get(&n).copied().unwrap_or(st.scope);
        let body_state = State { scope: body_scope, ..st.clone() };
        let new_body = self.fragment(*body, Parent::Node(n), &body_state);
        if let crate::estree::NodeKind::BlockStatement(bl) = new_body.kind {
            each.extend(bl.body);
        }
        let for_loop = b::r#for(
            Some(b::declaration(
                "let",
                vec![b::declarator(index_id.clone(), b::literal(0.0)), b::declarator(b::id("$$length"), b::member(b::id(array_id.as_str()), "length"))],
            )),
            Some(b::binary("<", index_id.clone(), b::id("$$length"))),
            Some(b::update_with("++", index_id, false)),
            b::block(each),
        );
        if let Some(f) = fallback {
            let open = b::stmt(b::call(b::id("$$renderer.push"), vec![b::literal(BLOCK_OPEN)]));
            let mut fallback_block = self.fragment(*f, Parent::Node(n), st);
            prepend_block_marker(&mut fallback_block, BLOCK_OPEN_ELSE);
            statements.push(b::r#if(
                b::binary("!==", b::member(b::id(array_id.as_str()), "length"), b::literal(0.0)),
                b::block(vec![open, for_loop]),
                Some(fallback_block),
            ));
        } else {
            st.template.borrow_mut().push(b::literal(BLOCK_OPEN));
            statements.push(for_loop);
        }
        let em = self.meta_of_node(n);
        let blockers = self.meta_blockers_array(em);
        let has_await = self.an.metas[em as usize].has_await;
        let mut t = st.template.borrow_mut();
        t.extend(create_child_block(statements, blockers, has_await));
        t.push(b::literal(BLOCK_CLOSE));
    }

    fn html_tag(&mut self, n: NodeId, expression: &'s Expr<'s>, st: &State) {
        let e = self.visit_template_expr_here(expression, st);
        let expression = b::call("$.html", vec![e]);
        let meta = self.meta_of_node(n);
        if self.meta_is_async(meta) {
            let blockers = self.meta_blockers_array(meta);
            let has_await = self.an.metas[meta as usize].has_await;
            st.template.borrow_mut().extend(create_child_block(vec![b::stmt(b::call("$$renderer.push", vec![expression]))], blockers, has_await));
        } else {
            st.template.borrow_mut().push(expression);
        }
    }

    fn await_block(&mut self, n: NodeId, st: &State) {
        let ast = self.ast();
        let TNode::AwaitBlock { expression, value, pending, then, .. } = &ast.nodes[n] else { return };
        let meta = self.meta_of_node(n);
        let mut expression = self.visit_template_expr_here(expression, st);
        let has_await = self.an.metas[meta as usize].has_await;
        if has_await {
            expression = b::call(b::arrow_with(vec![], expression, true), ());
        }
        let pending_block = match pending {
            Some(f) => self.fragment(*f, Parent::Node(n), st),
            None => b::block(vec![]),
        };
        let params = match value {
            Some(v) => {
                let p = self.convert_pattern(v);
                vec![self.visit_js(&p, st)]
            }
            None => vec![],
        };
        let then_block = match then {
            Some(f) => self.fragment(*f, Parent::Node(n), st),
            None => b::block(vec![]),
        };
        let statement = b::stmt(b::call(
            "$.await",
            vec![b::id("$$renderer"), expression, b::thunk(pending_block), b::arrow(params, then_block)],
        ));
        let blockers = self.meta_blockers_array(meta);
        let mut t = st.template.borrow_mut();
        t.extend(create_child_block(vec![statement], blockers, has_await));
        t.push(b::literal(BLOCK_CLOSE));
    }

    fn snippet_block(&mut self, n: NodeId, st: &State) {
        let ast = self.ast();
        let TNode::SnippetBlock { expression, parameters, body, .. } = &ast.nodes[n] else { return };
        let name = self.convert_expr(expression);
        let mut params = vec![b::id("$$renderer")];
        if let Some(arrow) = parameters {
            let mut conv = crate::estree::convert::Converter::new(self.locator, self.an.root.ts);
            conv.preserve_parens = true;
            let converted = conv.expression(&arrow.expr);
            if let crate::estree::NodeKind::ArrowFunctionExpression(a) = converted.kind {
                params.extend(a.params);
            }
        }
        let body_scope = self.scope_of_key(P::Fragment(*body).key()).unwrap_or(st.scope);
        let body_block = self.fragment(*body, Parent::Node(n), &State { scope: body_scope, ..st.clone() });
        if let Some(n) = super::super::js::ident(&name) {
            self.snippet_fns.push(n.to_string());
        }
        let mut f = b::function_declaration(name.clone(), params, body_block);
        let can_hoist = self.an.node_meta.get(&n).is_some_and(|m| m.can_hoist);
        let mut out = Vec::new();
        if self.dev {
            if let crate::estree::NodeKind::FunctionDeclaration(fd) = &mut f.kind {
                if let crate::estree::NodeKind::BlockStatement(bl) = &mut fd.body.kind {
                    bl.body.insert(0, b::stmt(b::call("$.validate_snippet_args", vec![b::id("$$renderer")])));
                }
            }
            out.push(b::stmt(b::call("$.prevent_snippet_stringification", vec![name])));
        }
        out.push(f);
        if can_hoist {
            self.hoisted.extend(out);
        } else {
            st.init.borrow_mut().extend(out);
        }
    }

    fn render_tag(&mut self, n: NodeId, expression: &'s Expr<'s>, st: &State) {
        let mut opt = PromiseOptimiser::default();
        let call = self.convert_expr(expression);
        let is_optional = matches!(call.kind, crate::estree::NodeKind::ChainExpression(_));
        let inner = super::super::js::unwrap_optional(&call);
        let crate::estree::NodeKind::CallExpression(c) = &inner.kind else { return };
        let callee = self.visit_js(&c.callee, st);
        let meta = self.meta_of_node(n);
        let snippet_function = opt.transform(self, callee, meta);
        let mut args = vec![b::id("$$renderer")];
        // the argument metadata is keyed by the oxc arguments
        let arg_metas: Vec<u32> = match expression {
            Expr::Js(js) => {
                let e = match js.inner() {
                    oxc_ast::ast::Expression::ChainExpression(ch) => match &ch.expression {
                        oxc_ast::ast::ChainElement::CallExpression(c) => Some(&**c),
                        _ => None,
                    },
                    oxc_ast::ast::Expression::CallExpression(c) => Some(&**c),
                    _ => None,
                };
                e.map(|c| c.arguments.iter().map(|a| self.an.meta_of.get(&crate::analyze::nodes::argument(a).key()).copied().unwrap_or(0)).collect()).unwrap_or_default()
            }
            _ => vec![],
        };
        for (i, a) in c.arguments.iter().enumerate() {
            let v = self.visit_js(a, st);
            let m = arg_metas.get(i).copied().unwrap_or(0);
            args.push(opt.transform(self, v, m));
        }
        let statement = b::stmt(if is_optional { b::maybe_call(snippet_function, args) } else { b::call(snippet_function, args) });
        st.template.borrow_mut().extend(opt.render_block(vec![statement]));
        if !opt.is_async() && !st.is_standalone {
            st.template.borrow_mut().push(b::literal(EMPTY_COMMENT));
        }
    }

    fn debug_tag(&mut self, identifiers: &'s crate::ast::DebugArgs<'s>, st: &State) {
        let ids: Vec<Node> = match identifiers {
            crate::ast::DebugArgs::All => vec![],
            crate::ast::DebugArgs::One(e) => vec![self.convert_expr(e)],
            crate::ast::DebugArgs::Sequence(e) => match self.convert_expr(e).kind {
                crate::estree::NodeKind::SequenceExpression(s) => s.expressions,
                kind => vec![Node::new(kind)],
            },
        };
        let mut blockers = Vec::new();
        for id in &ids {
            if let Some(name) = super::super::js::ident(id) {
                if let Some(bl) = self.get(st.scope, name).and_then(|b| self.binding(b).blocker) {
                    blockers.push(super::utils::blocker_expression(self, bl));
                }
            }
        }
        let mut props = Vec::new();
        for id in &ids {
            let v = self.visit_js(id, st);
            props.push(b::prop("init", id.clone(), v));
        }
        st.template.borrow_mut().extend(create_child_block(
            vec![b::stmt(b::call("console.log", vec![b::object(props)])), b::debugger()],
            b::array(blockers),
            false,
        ));
    }

    fn title_element(&mut self, n: NodeId, st: &State) {
        let ast = self.ast();
        let TNode::Element(el) = &ast.nodes[n] else { return };
        let children: Vec<Child<'s>> = ast.fragments[el.fragment].nodes.iter().map(|&c| Child::Node(c)).collect();
        let inner = State { template: shared(), ..st.clone() };
        inner.template.borrow_mut().push(b::literal("<title>"));
        self.process_children(&children, &inner);
        inner.template.borrow_mut().push(b::literal("</title>"));
        let template = std::mem::take(&mut *inner.template.borrow_mut());
        st.init.borrow_mut().push(b::stmt(b::call(
            "$$renderer.title",
            vec![b::arrow(vec![b::id("$$renderer")], b::block(build_template(template)))],
        )));
    }

    fn svelte_head(&mut self, n: NodeId, st: &State) {
        let ast = self.ast();
        let TNode::Element(el) = &ast.nodes[n] else { return };
        let block = self.fragment(el.fragment, Parent::Node(n), st);
        st.template.borrow_mut().push(b::stmt(b::call(
            "$.head",
            vec![b::literal(super::super::hash(&self.filename).as_str()), b::id("$$renderer"), b::arrow(vec![b::id("$$renderer")], block)],
        )));
    }

    // -----------------------------------------------------------------------------------
    // RegularElement

    fn regular_element(&mut self, n: NodeId, st: &State) {
        let ast = self.ast();
        let TNode::Element(el) = &ast.nodes[n] else { return };
        let name = if st.namespace == "html" { el.name.to_lowercase() } else { el.name.to_string() };
        let meta = self.an.node_meta.get(&n).cloned().unwrap_or_default();
        let namespace = if el.name == "foreignObject" {
            "html"
        } else if meta.svg {
            "svg"
        } else if meta.mathml {
            "mathml"
        } else {
            "html"
        };
        let has_child_declarations = !ast.fragments[el.fragment].transparent;
        let fragment_scope = self.scope_of_key(P::Fragment(el.fragment).key()).unwrap_or(st.scope);
        let state = State {
            namespace,
            scope: fragment_scope,
            preserve_whitespace: st.preserve_whitespace || el.name == "pre" || el.name == "textarea",
            init: shared(),
            template: shared(),
            async_consts: Default::default(),
            ..st.clone()
        };
        let attribute_state = State { scope: st.scope, ..state.clone() };
        let node_is_void = crate::analyze::utils::is_void(&name);
        let mut optimiser = PromiseOptimiser::default();

        self.path.push(PathNode::Tpl(P::Node(n)));

        // <select value> and <option> have their attributes handled by the runtime
        let is_select_special = name == "select"
            && el.attributes.iter().any(|a| match a {
                crate::ast::Attr::Attribute { name, .. } | crate::ast::Attr::Directive { kind: "BindDirective", name, .. } => {
                    *name == "value" || name.to_lowercase() == "defaultvalue"
                }
                crate::ast::Attr::Spread { .. } => true,
                _ => false,
            });
        let is_option_special = name == "option";
        let is_special = is_select_special || is_option_special;
        let mut body = None;
        if !is_special {
            state.template.borrow_mut().push(b::literal(format!("<{name}").as_str()));
            body = self.build_element_attributes(n, &attribute_state, &mut optimiser);
            state.template.borrow_mut().push(b::literal(if node_is_void { "/>" } else { ">" }));
        }

        let frag_nodes = ast.fragments[el.fragment].nodes.clone();
        if (name == "script" || name == "style") && frag_nodes.len() == 1 {
            if let TNode::Text { data, .. } = &ast.nodes[frag_nodes[0]] {
                state.template.borrow_mut().push(b::literal(data.as_ref()));
            }
            state.template.borrow_mut().push(b::literal(format!("</{name}>").as_str()));
            let mut statements = std::mem::take(&mut *state.init.borrow_mut());
            statements.extend(build_template(std::mem::take(&mut *state.template.borrow_mut())));
            st.template.borrow_mut().extend(optimiser.render(statements));
            self.path.pop();
            return;
        }

        let cleaned = self.clean_nodes(Parent::Node(n), &frag_nodes, namespace, state.preserve_whitespace, self.options.preserve_comments);
        for &h in &cleaned.hoisted {
            self.visit_node(h, &state);
        }

        if self.dev {
            let (line, column) = self.locate(el.start);
            state.template.borrow_mut().push(b::stmt(b::call(
                "$.push_element",
                vec![b::id("$$renderer"), b::literal(name.as_str()), b::literal(line as f64), b::literal(column as f64)],
            )));
        }

        if is_select_special || is_option_special {
            let customizable = self.is_customizable_select(n);
            let body_node = if is_option_special {
                if let Some(svn) = meta.synthetic_value_node {
                    let TNode::ExpressionTag { expression, .. } = &ast.nodes[svn] else { unreachable!() };
                    self.path.push(PathNode::Tpl(P::Node(svn)));
                    let e = self.visit_template_expr_here(expression, &state);
                    self.path.pop();
                    let m = self.meta_of_node(svn);
                    optimiser.transform(self, e, m)
                } else {
                    let inner = state.with_new_arrays();
                    self.process_children(&cleaned.trimmed, &inner);
                    let mut statements = state.init.borrow().clone();
                    statements.extend(build_template(std::mem::take(&mut *inner.template.borrow_mut())));
                    if self.dev {
                        let (line, column) = self.locate(el.start);
                        statements.insert(
                            0,
                            b::stmt(b::call("$.push_element", vec![b::id("$$renderer"), b::literal(name.as_str()), b::literal(line as f64), b::literal(column as f64)])),
                        );
                        statements.push(b::stmt(b::call("$.pop_element", ())));
                    }
                    b::arrow(vec![b::id("$$renderer")], b::block(statements))
                }
            } else {
                let inner = state.with_new_arrays();
                self.process_children(&cleaned.trimmed, &inner);
                let mut statements = state.init.borrow().clone();
                statements.extend(build_template(std::mem::take(&mut *inner.template.borrow_mut())));
                b::arrow(vec![b::id("$$renderer")], b::block(statements))
            };
            self.path.pop();
            let mut args = self.prepare_element_spread_object(n, st, &mut optimiser);
            if customizable {
                args.push(Some(b::r#true()));
            }
            let mut call_args = vec![args.remove(0), Some(body_node)];
            call_args.extend(args);
            let statement = b::stmt(b::call(if is_option_special { "$$renderer.option" } else { "$$renderer.select" }, call_args));
            let mut statements = std::mem::take(&mut *state.init.borrow_mut());
            statements.push(statement);
            st.template.borrow_mut().extend(optimiser.render(statements));
            return;
        }

        if let Some(body) = body {
            let inner = state.with_new_arrays();
            self.process_children(&cleaned.trimmed, &inner);
            let id = if matches!(body.kind, crate::estree::NodeKind::Identifier(_)) {
                body
            } else {
                let id = b::id(self.an.sc.generate(state.scope, "$$body").as_str());
                state.template.borrow_mut().push(b::r#const(id.clone(), body));
                id
            };
            let mut inner_body = std::mem::take(&mut *inner.init.borrow_mut());
            inner_body.extend(build_template(std::mem::take(&mut *inner.template.borrow_mut())));
            state.template.borrow_mut().push(b::r#if(id.clone(), b::block(build_template(vec![id])), b::block(inner_body)));
        } else {
            self.process_children(&cleaned.trimmed, &state);
            if (name == "optgroup" || name == "select") && self.is_customizable_select(n) {
                state.template.borrow_mut().push(b::literal("<!>"));
            }
        }

        if !node_is_void {
            state.template.borrow_mut().push(b::literal(format!("</{name}>").as_str()));
        }
        if self.dev {
            state.template.borrow_mut().push(b::stmt(b::call("$.pop_element", ())));
        }
        self.path.pop();

        if has_child_declarations {
            self.push_async_consts(&state);
        }
        let init = std::mem::take(&mut *state.init.borrow_mut());
        let template = std::mem::take(&mut *state.template.borrow_mut());
        if has_child_declarations {
            let mut block_body = init;
            block_body.extend(build_template(template));
            st.template.borrow_mut().extend(optimiser.render(vec![b::block(block_body)]));
        } else if optimiser.is_async() {
            let mut statements = init;
            statements.extend(build_template(template));
            st.template.borrow_mut().extend(optimiser.render(statements));
        } else {
            st.init.borrow_mut().extend(init);
            st.template.borrow_mut().extend(template);
        }
    }

    fn svelte_element(&mut self, n: NodeId, st: &State) {
        let ast = self.ast();
        let TNode::Element(el) = &ast.nodes[n] else { return };
        let mut tag = match &el.tag {
            Some(t) => self.visit_template_expr_here(t, st),
            None => b::void0(),
        };
        if self.dev {
            if !matches!(tag.kind, crate::estree::NodeKind::Identifier(_)) {
                let tag_id = self.an.sc.generate(st.scope, "$$tag");
                st.init.borrow_mut().push(b::r#const(b::id(tag_id.as_str()), tag));
                tag = b::id(tag_id.as_str());
            }
            st.init.borrow_mut().push(b::stmt(b::call("$.validate_dynamic_element_tag", vec![b::thunk(tag.clone())])));
            if !ast.fragments[el.fragment].nodes.is_empty() {
                st.init.borrow_mut().push(b::stmt(b::call("$.validate_void_dynamic_element", vec![b::thunk(tag.clone())])));
            }
        }
        let meta = self.an.node_meta.get(&n).cloned().unwrap_or_default();
        let namespace = if el.name == "foreignObject" { "html" } else if meta.svg { "svg" } else if meta.mathml { "mathml" } else { "html" };
        let state = State { namespace, template: shared(), init: shared(), ..st.clone() };
        let mut opt = PromiseOptimiser::default();
        let mut statements = Vec::new();
        self.build_element_attributes(n, &state, &mut opt);
        if self.dev {
            let (line, column) = self.locate(el.start);
            statements.push(b::stmt(b::call("$.push_element", vec![b::id("$$renderer"), tag.clone(), b::literal(line as f64), b::literal(column as f64)])));
        }
        let mut attributes = std::mem::take(&mut *state.init.borrow_mut());
        attributes.extend(build_template(std::mem::take(&mut *state.template.borrow_mut())));
        let children = self.fragment(el.fragment, Parent::Node(n), &state);
        let children_empty = matches!(&children.kind, crate::estree::NodeKind::BlockStatement(bl) if bl.body.is_empty());
        let args = vec![
            Some(b::id("$$renderer")),
            Some(tag),
            (!attributes.is_empty()).then(|| b::thunk(b::block(attributes))),
            (!children_empty).then(|| b::thunk(children)),
        ];
        statements.extend(opt.render(vec![b::stmt(b::call("$.element", args))]));
        if self.dev {
            statements.push(b::stmt(b::call("$.pop_element", ())));
        }
        let em = self.meta_of_node(n);
        let blockers = self.meta_blockers_array(em);
        let has_await = self.an.metas[em as usize].has_await;
        st.template.borrow_mut().extend(create_child_block(statements, blockers, has_await));
    }

    fn svelte_boundary(&mut self, n: NodeId, st: &State) {
        use crate::ast::Attr;
        let ast = self.ast();
        let TNode::Element(el) = &ast.nodes[n] else { return };
        let frag_nodes = &ast.fragments[el.fragment].nodes;
        let snippet_named = |name: &str| {
            frag_nodes.iter().copied().find(|&c| matches!(&ast.nodes[c], TNode::SnippetBlock { expression, .. } if crate::analyze::scope::ident(crate::analyze::nodes::template_expr(expression)).is_some_and(|i| i.name == name)))
        };
        let failed_snippet = snippet_named("failed");
        let pending_snippet = snippet_named("pending");
        let attr_named = |name: &str| el.attributes.iter().find(|a| matches!(a, Attr::Attribute { name: n, .. } if *n == name));
        let failed_attribute = attr_named("failed");
        let pending_attribute = attr_named("pending");
        let is_pending_attr_nullish = match pending_attribute {
            Some(Attr::Attribute { value: crate::ast::AttrValue::Expression(c), .. }) => match c.as_ref() {
                crate::ast::Chunk::Expression { expression, .. } => !self.evaluate_expr(expression, st.scope).is_defined,
                _ => false,
            },
            _ => false,
        };
        let children_nodes: Vec<NodeId> = frag_nodes.iter().copied().filter(|&c| Some(c) != failed_snippet && Some(c) != pending_snippet).collect();
        let scope = self.scope_of_key(P::Fragment(el.fragment).key()).unwrap_or(st.scope);
        let children_block = self.fragment_nodes(el.fragment, &children_nodes, Parent::Node(n), &State { scope, ..st.clone() });

        let pending_attribute_block = |s: &mut Self, a: &'s Attr<'s>| -> (Node, Node) {
            let Attr::Attribute { value, .. } = a else { unreachable!() };
            let callee = s.build_attribute_value(value, st, &mut PromiseOptimiser::identity(), false, true, false);
            let pending = b::call(callee.clone(), vec![b::id("$$renderer")]);
            (callee, b::block(build_template(vec![b::literal(BLOCK_OPEN_ELSE), b::stmt(pending), b::literal(BLOCK_CLOSE)])))
        };

        let children_body = if pending_attribute.is_some() || pending_snippet.is_some() {
            if pending_attribute.is_some() && is_pending_attr_nullish && pending_snippet.is_none() {
                let (callee, pending_block) = pending_attribute_block(self, pending_attribute.unwrap());
                b::block(vec![b::r#if(
                    callee,
                    pending_block,
                    Some(b::block(build_template(vec![b::literal(BLOCK_OPEN), children_block, b::literal(BLOCK_CLOSE)]))),
                )])
            } else if let Some(a) = pending_attribute {
                pending_attribute_block(self, a).1
            } else {
                let TNode::SnippetBlock { body, .. } = &ast.nodes[pending_snippet.unwrap()] else { unreachable!() };
                self.path.push(PathNode::Tpl(P::Node(pending_snippet.unwrap())));
                let block = self.fragment(*body, Parent::Node(pending_snippet.unwrap()), st);
                self.path.pop();
                b::block(build_template(vec![b::literal(BLOCK_OPEN_ELSE), block, b::literal(BLOCK_CLOSE)]))
            }
        } else {
            b::block(build_template(vec![b::literal(BLOCK_OPEN), children_block, b::literal(BLOCK_CLOSE)]))
        };

        if failed_snippet.is_none() && failed_attribute.is_none() {
            if let crate::estree::NodeKind::BlockStatement(bl) = children_body.kind {
                st.template.borrow_mut().extend(bl.body);
            }
            return;
        }
        let mut props = Vec::new();
        let init = shared();
        if let (Some(Attr::Attribute { value, .. }), None) = (failed_attribute, failed_snippet) {
            let callee = self.build_attribute_value(value, st, &mut PromiseOptimiser::identity(), false, true, false);
            props.push(b::init("failed", callee));
        } else if let Some(fs) = failed_snippet {
            self.visit_node(fs, &State { init: init.clone(), ..st.clone() });
            let TNode::SnippetBlock { expression, .. } = &ast.nodes[fs] else { unreachable!() };
            props.push(b::init("failed", self.convert_expr(expression)));
        }
        let boundary = b::stmt(b::call("$$renderer.boundary", vec![b::object(props), b::arrow(vec![b::id("$$renderer")], children_body)]));
        let init = std::mem::take(&mut *init.borrow_mut());
        st.template.borrow_mut().push(if init.is_empty() {
            boundary
        } else {
            let mut body = init;
            body.push(boundary);
            b::block(body)
        });
    }

    fn slot_element(&mut self, n: NodeId, st: &State) {
        use crate::ast::Attr;
        let ast = self.ast();
        let TNode::Element(el) = &ast.nodes[n] else { return };
        let mut props = Vec::new();
        let mut spreads = Vec::new();
        let mut opt = PromiseOptimiser::default();
        let mut name = b::literal("default");
        for a in &el.attributes {
            match a {
                Attr::Spread { expression, .. } => {
                    self.path.push(PathNode::Tpl(P::Attr(a)));
                    let e = self.visit_template_expr_here(expression, st);
                    self.path.pop();
                    let meta = self.an.meta_of.get(&P::Attr(a).key()).copied().unwrap_or(0);
                    spreads.push(opt.transform(self, e, meta));
                }
                Attr::Attribute { name: attr_name, value, .. } => {
                    let v = self.build_attribute_value(value, st, &mut opt, false, true, false);
                    if *attr_name == "name" {
                        name = v;
                    } else if *attr_name != "slot" {
                        props.push(b::init(attr_name, v));
                    }
                }
                _ => {}
            }
        }
        let props_expression = if spreads.is_empty() {
            b::object(props)
        } else {
            let mut all = vec![b::object(props)];
            all.extend(spreads);
            b::call("$.spread_props", vec![b::array(all)])
        };
        let fallback = if ast.fragments[el.fragment].nodes.is_empty() {
            b::null()
        } else {
            b::thunk(self.fragment(el.fragment, Parent::Node(n), st))
        };
        let slot = b::call("$.slot", vec![b::id("$$renderer"), b::id("$$props"), name, props_expression, fallback]);
        let mut t = st.template.borrow_mut();
        t.push(b::literal(BLOCK_OPEN));
        t.extend(opt.render_block(vec![b::stmt(slot)]));
        t.push(b::literal(BLOCK_CLOSE));
    }

    /// `is_customizable_select_element(node)`
    pub fn is_customizable_select(&self, n: NodeId) -> bool {
        match &self.ast().nodes[n] {
            TNode::Element(el) => crate::analyze::visit::is_customizable_select_element(self.ast(), el),
            _ => false,
        }
    }

    /// `locator(offset)`: 1-based line, 0-based column (UTF-16)
    pub fn locate(&self, offset: usize) -> (usize, usize) {
        let (line, column, _) = self.locator.line_column(offset);
        (line, column)
    }
}

/// `text.replace(regex_starts_with_whitespaces, replacement)`
fn replace_leading_ws(s: &str, replacement: &str) -> String {
    let trimmed = s.trim_start_matches(is_whitespace);
    if trimmed.len() == s.len() {
        return s.to_string();
    }
    format!("{replacement}{trimmed}")
}

/// `text.replace(regex_ends_with_whitespaces, replacement)`
fn replace_trailing_ws(s: &str, replacement: &str) -> String {
    let trimmed = s.trim_end_matches(is_whitespace);
    if trimmed.len() == s.len() {
        return s.to_string();
    }
    format!("{trimmed}{replacement}")
}
