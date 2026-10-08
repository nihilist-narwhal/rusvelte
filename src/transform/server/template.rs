//! The server transform's template visitors (`template_visitors` in `transform-server.js`)
//! and the shared template helpers of `3-transform/utils.js` (`clean_nodes`, namespaces).

use crate::analyze::nodes::P;
use crate::ast::{Expr, FragId, Node as TNode, NodeId};
use crate::estree::builders as b;
use crate::estree::Node;

use super::super::js::PathNode;
use super::utils::{build_template, PromiseOptimiser, EMPTY_COMMENT};
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

            for i in 0..regular.len() {
                let node = &regular[i];
                let Some((mut data, mut raw)) = text_of(node) else {
                    trimmed.push(node.clone());
                    continue;
                };
                let prev_ty = if i > 0 { Some(regular[i - 1].ty(ast)) } else { None };
                let next_ty = regular.get(i + 1).map(|c| c.ty(ast));
                if prev_ty != Some("ExpressionTag") {
                    let prev_ends_ws = i > 0 && text_of(&regular[i - 1]).is_some_and(|(d, _)| d.ends_with(is_whitespace));
                    let replacement = if prev_ends_ws { "" } else { " " };
                    data = replace_leading_ws(&data, replacement);
                    raw = replace_leading_ws(&raw, replacement);
                }
                if next_ty != Some("ExpressionTag") {
                    data = replace_trailing_ws(&data, " ");
                    raw = replace_trailing_ws(&raw, " ");
                }
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
        b::array(self.an.meta_blockers(meta).into_iter().map(super::utils::blocker_expression).collect::<Vec<_>>())
    }

    /// `scope.evaluate(expression)`
    pub fn evaluate_expr(&self, e: &'s Expr<'s>, scope: crate::analyze::scope::ScopeId) -> crate::analyze::evaluate::Evaluation {
        self.an.sc.evaluate(self.an.ast, crate::analyze::nodes::template_expr(e), scope)
    }

    /// The ESTree form of a template expression
    pub fn convert_expr(&self, e: &Expr<'s>) -> Node {
        match e {
            Expr::Js(js) => self.conv.expression(js.inner()),
            Expr::Ident { name, .. } => b::id(name.as_str()),
            Expr::Literal { value, .. } => b::literal(value.as_str()),
        }
    }

    /// `context.visit(expression)` for an expression of template node `n`
    pub fn visit_template_expr(&mut self, n: NodeId, e: &Expr<'s>, st: &State) -> Node {
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

    /// The `Fragment` visitor
    pub fn fragment(&mut self, f: FragId, parent: Parent, st: &State) -> Node {
        let nodes = self.ast().fragments[f].nodes.clone();
        let namespace = self.infer_namespace(st.namespace, parent, &nodes);
        let cleaned = self.clean_nodes(parent, &nodes, namespace, st.preserve_whitespace, self.options.preserve_comments);
        let state = State { init: shared(), template: shared(), namespace, is_standalone: cleaned.is_standalone, async_consts: None, ..st.clone() };

        self.path.push(PathNode::Tpl(P::Fragment(f)));
        for &h in &cleaned.hoisted {
            self.visit_node(h, &state);
        }
        if cleaned.is_text_first {
            state.template.borrow_mut().push(b::literal(EMPTY_COMMENT));
        }
        self.process_children(&cleaned.trimmed, &state);
        self.path.pop();

        if let Some(ac) = &state.async_consts {
            let ac = ac.borrow();
            if !ac.thunks.is_empty() {
                state.init.borrow_mut().push(b::var(b::id(ac.id.as_str()), b::call("$$renderer.run", vec![b::array(ac.thunks.clone())])));
            }
        }
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

    /// `context.visit(node, state)` for a template node
    pub fn visit_node(&mut self, n: NodeId, st: &State) {
        let ast = self.ast();
        match &ast.nodes[n] {
            TNode::Element(el) => match el.kind {
                "RegularElement" => self.regular_element(n, st),
                _ => {}
            },
            _ => {}
        }
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
            async_consts: None,
            ..st.clone()
        };
        let attribute_state = State { scope: st.scope, ..state.clone() };
        let node_is_void = crate::analyze::utils::is_void(&name);
        let mut optimiser = PromiseOptimiser::default();

        self.path.push(PathNode::Tpl(P::Node(n)));

        // TODO: <select value> / <option> special cases
        state.template.borrow_mut().push(b::literal(format!("<{name}").as_str()));
        let body = self.build_element_attributes(n, &attribute_state, &mut optimiser);
        state.template.borrow_mut().push(b::literal(if node_is_void { "/>" } else { ">" }));

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
        }

        if !node_is_void {
            state.template.borrow_mut().push(b::literal(format!("</{name}>").as_str()));
        }
        if self.dev {
            state.template.borrow_mut().push(b::stmt(b::call("$.pop_element", ())));
        }
        self.path.pop();

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
