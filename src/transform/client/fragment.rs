//! The `Fragment` visitor, `process_children` (`shared/fragment.js`), the template node
//! dispatch, and the shared template helpers of `3-transform/utils.js` (`clean_nodes`,
//! namespaces)

use std::cell::RefCell;
use std::rc::Rc;

use crate::analyze::nodes::P;
use crate::ast::{Attr, AttrValue, Chunk, FragId, Node as TNode, NodeId};
use crate::estree::builders as b;
use crate::estree::Node;

use super::super::js::PathNode;
use super::template::TextPart;
use super::utils::{ChunkRef, Memoize};
use super::{shared, Client, Memoizer, State, Template, TEMPLATE_FRAGMENT, TEMPLATE_USE_IMPORT_NODE};

/// A node of a cleaned fragment: a template node, a text node whose whitespace `clean_nodes`
/// trimmed (the JS mutates the node), or the comment added after a lone `<script>`
#[derive(Clone, Debug)]
pub enum Child {
    Node(NodeId),
    Text { node: NodeId, data: String, raw: String },
    Comment,
}

impl Child {
    pub fn node_id(&self) -> Option<NodeId> {
        match self {
            Child::Node(n) | Child::Text { node: n, .. } => Some(*n),
            Child::Comment => None,
        }
    }

    pub fn ty(&self, ast: &crate::ast::Ast) -> &'static str {
        match self {
            Child::Text { .. } => "Text",
            Child::Comment => "Comment",
            Child::Node(n) => ast.nodes[*n].type_name(),
        }
    }

    /// `(data, raw)` of a Text
    pub fn text(&self, ast: &crate::ast::Ast) -> Option<(String, String)> {
        match self {
            Child::Text { data, raw, .. } => Some((data.clone(), raw.clone())),
            Child::Node(n) => match &ast.nodes[*n] {
                TNode::Text { data, raw, .. } => Some((data.to_string(), raw.to_string())),
                _ => None,
            },
            Child::Comment => None,
        }
    }
}

/// What `clean_nodes` returns
pub struct Cleaned {
    pub hoisted: Vec<NodeId>,
    pub trimmed: Vec<Child>,
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

/// How `process_children` gets the first node
#[derive(Clone)]
pub enum Initial {
    /// `() => id`
    Const(Node),
    /// `(is_text) => b.call(callee, arg, is_text && b.true)`
    Call(&'static str, Node),
}

impl Initial {
    fn get(&self, is_text: bool) -> Node {
        match self {
            Initial::Const(n) => n.clone(),
            Initial::Call(callee, arg) => b::call(*callee, vec![Some(arg.clone()), if is_text { Some(b::r#true()) } else { None }]),
        }
    }
}

/// `is_text_attribute(attribute)`: a value of exactly one Text chunk
pub fn is_text_attribute(value: &AttrValue) -> bool {
    matches!(value, AttrValue::Sequence(chunks) if chunks.len() == 1 && matches!(chunks[0], Chunk::Text { .. }))
}

thread_local! {
    /// The `value` attribute the analysis makes out of a `<textarea>`'s dynamic children (its
    /// value is filled in from the textarea's fragment)
    pub static TEXTAREA_VALUE: &'static Attr<'static> = Box::leak(Box::new(Attr::Attribute {
        start: usize::MAX,
        end: usize::MAX,
        name: "value",
        name_loc: None,
        value: AttrValue::Sequence(vec![]),
    }));

    /// The `class=""`/`style=""` attributes the analysis appends (`create_attribute`)
    static SYNTHETIC: (&'static Attr<'static>, &'static Attr<'static>) = {
        let make = |name: &'static str| -> &'static Attr<'static> {
            Box::leak(Box::new(Attr::Attribute {
                start: usize::MAX,
                end: usize::MAX,
                name,
                name_loc: None,
                value: AttrValue::Sequence(vec![Chunk::Text { start: usize::MAX, end: usize::MAX, raw: "", data: "".into() }]),
            }))
        };
        (make("class"), make("style"))
    };
}

impl<'a, 's> Client<'a, 's> {
    /// An element's attributes, with the analysis' synthetic `class`/`style` at the end
    pub fn element_attributes(&self, n: NodeId) -> Vec<&'s Attr<'s>> {
        let TNode::Element(el) = &self.ast().nodes[n] else { return vec![] };
        let mut out: Vec<&'s Attr<'s>> = el.attributes.iter().collect();
        if self.an.textarea_values.contains(&n) {
            out.push(TEXTAREA_VALUE.with(|a| *a));
        }
        if self.synthetic_class.contains(&n) {
            out.push(SYNTHETIC.with(|s| s.0));
        }
        if self.synthetic_style.contains(&n) {
            out.push(SYNTHETIC.with(|s| s.1));
        }
        out
    }

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

    /// `sort_const_tags(nodes, state)`: `{@const}` tags in topological order (legacy mode)
    fn sort_const_tags(&self, nodes: &[NodeId], scope: crate::analyze::scope::ScopeId) -> Vec<NodeId> {
        let ast = self.ast();
        let mut other = Vec::new();
        // (binding, tag node, deps)
        let mut tags: Vec<(crate::analyze::scope::BindingId, NodeId, Vec<crate::analyze::scope::BindingId>)> = Vec::new();
        for &n in nodes {
            if let TNode::ConstTag { id, init, .. } = &ast.nodes[n] {
                let pattern = self.convert_const_pattern(id);
                let bindings: Vec<_> = super::super::js::extract_identifiers(&pattern)
                    .into_iter()
                    .filter_map(|i| self.get(scope, super::super::js::ident(i).unwrap()))
                    .collect();
                let init = self.convert_expr(init);
                let mut deps = Vec::new();
                self.collect_deps(&init, None, scope, &mut deps);
                for bnd in bindings {
                    match tags.iter_mut().find(|t| t.0 == bnd) {
                        Some(t) => {
                            t.1 = n;
                            t.2 = deps.clone();
                        }
                        None => tags.push((bnd, n, deps.clone())),
                    }
                }
            } else {
                other.push(n);
            }
        }
        if tags.is_empty() {
            return nodes.to_vec();
        }
        let mut sorted: Vec<NodeId> = Vec::new();
        fn add(tag: usize, tags: &[(u32, NodeId, Vec<u32>)], sorted: &mut Vec<NodeId>, depth: usize) {
            if sorted.contains(&tags[tag].1) || depth > 1000 {
                return;
            }
            for dep in &tags[tag].2 {
                if let Some(i) = tags.iter().position(|t| t.0 == *dep) {
                    add(i, tags, sorted, depth + 1);
                }
            }
            sorted.push(tags[tag].1);
        }
        for i in 0..tags.len() {
            add(i, &tags, &mut sorted, 0);
        }
        sorted.extend(other);
        sorted
    }

    /// The references of an expression (`walk` with `set_scope` and an `Identifier` visitor)
    fn collect_deps(&self, n: &Node, parent: Option<&Node>, scope: crate::analyze::scope::ScopeId, out: &mut Vec<crate::analyze::scope::BindingId>) {
        let scope = n.origin.and_then(|o| self.scope_of_key(o)).unwrap_or(scope);
        if let crate::estree::NodeKind::Identifier(i) = &n.kind {
            if super::super::js::is_reference(n, parent) {
                if let Some(bnd) = self.get(scope, &i.name) {
                    if !out.contains(&bnd) {
                        out.push(bnd);
                    }
                }
            }
            return;
        }
        n.for_each_child(&mut |c| self.collect_deps(c, Some(n), scope, out));
    }

    /// `clean_nodes(parent, nodes, path, namespace, state, preserve_whitespace, preserve_comments)`
    pub fn clean_nodes(&self, parent: Parent, nodes: &[NodeId], namespace: &str, scope: crate::analyze::scope::ScopeId, preserve_whitespace: bool, preserve_comments: bool) -> Cleaned {
        let ast = self.ast();
        let nodes = if !self.an.runes { self.sort_const_tags(nodes, scope) } else { nodes.to_vec() };
        let mut hoisted = Vec::new();
        let mut regular: Vec<Child> = Vec::new();
        for &n in &nodes {
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

        let mut trimmed: Vec<Child>;
        if !preserve_whitespace {
            trimmed = Vec::new();
            while let Some(first) = regular.first() {
                match first.text(ast) {
                    Some((data, _)) if !data.chars().any(|c| !is_whitespace(c)) => {
                        regular.remove(0);
                    }
                    _ => break,
                }
            }
            if let Some(first) = regular.first_mut() {
                if let Some((data, raw)) = first.text(ast) {
                    let node = first.node_id().unwrap();
                    *first = Child::Text {
                        node,
                        data: data.trim_start_matches(is_whitespace).to_string(),
                        raw: raw.trim_start_matches(is_whitespace).to_string(),
                    };
                }
            }
            while let Some(last) = regular.last() {
                match last.text(ast) {
                    Some((data, _)) if !data.chars().any(|c| !is_whitespace(c)) => {
                        regular.pop();
                    }
                    _ => break,
                }
            }
            if let Some(last) = regular.last_mut() {
                if let Some((data, raw)) = last.text(ast) {
                    let node = last.node_id().unwrap();
                    *last = Child::Text {
                        node,
                        data: data.trim_end_matches(is_whitespace).to_string(),
                        raw: raw.trim_end_matches(is_whitespace).to_string(),
                    };
                }
            }

            let parent_name = self.parent_element_name(parent);
            let in_text = self.path.iter().any(|p| {
                matches!(p, PathNode::Tpl(P::Node(n)) if matches!(&ast.nodes[*n], TNode::Element(el) if el.kind == "RegularElement" && el.name == "text"))
            });
            let can_remove_entirely = (namespace == "svg" && parent_name != Some("text") && !in_text)
                || matches!(parent_name, Some("select" | "tr" | "table" | "tbody" | "thead" | "tfoot" | "colgroup" | "datalist"));

            // the JS mutates each text node, and reads the previous one's mutated data
            let mut modified: Vec<Option<String>> = vec![None; regular.len()];
            for i in 0..regular.len() {
                let node = &regular[i];
                let Some((mut data, mut raw)) = node.text(ast) else {
                    trimmed.push(node.clone());
                    continue;
                };
                let prev_ty = if i > 0 { Some(regular[i - 1].ty(ast)) } else { None };
                let next_ty = regular.get(i + 1).map(|c| c.ty(ast));
                if prev_ty != Some("ExpressionTag") {
                    let prev_data = if i > 0 { modified[i - 1].clone().or_else(|| regular[i - 1].text(ast).map(|(d, _)| d)) } else { None };
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
            if let Some((data, _)) = trimmed.first().and_then(|f| f.text(ast)) {
                if data == "\n" || data == "\r\n" {
                    trimmed.remove(0);
                }
            }
        }

        // a lone <script> gets a comment after it
        if trimmed.len() == 1 {
            if let Child::Node(n) = &trimmed[0] {
                if matches!(&ast.nodes[*n], TNode::Element(el) if el.kind == "RegularElement" && el.name == "script") {
                    trimmed.push(Child::Comment);
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
                            && !el.attributes.iter().any(|a| matches!(a, Attr::Attribute { name, .. } if name.starts_with("--")))
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

    /// `determine_namespace_for_children(node, namespace)`
    pub fn determine_namespace_for_children(&self, n: NodeId) -> &'static str {
        let TNode::Element(el) = &self.ast().nodes[n] else { return "html" };
        if el.name == "foreignObject" {
            return "html";
        }
        let m = self.an.node_meta.get(&n).cloned().unwrap_or_default();
        if m.svg {
            "svg"
        } else if m.mathml {
            "mathml"
        } else {
            "html"
        }
    }

    // -----------------------------------------------------------------------------------
    // dispatch

    /// The template root: `walk(analysis.template.ast)` visits the root Fragment
    pub fn visit_root_fragment(&mut self, st: &State) -> Node {
        let f = self.an.root.fragment;
        self.visit_fragment(f, st)
    }

    /// `context.visit(fragment, state)` (with `set_scope`)
    pub fn visit_fragment(&mut self, f: FragId, st: &State) -> Node {
        let scoped;
        let st = match self.scope_of_key(P::Fragment(f).key()) {
            Some(scope) if scope != st.scope => {
                scoped = self.with_scope(scope, st);
                &scoped
            }
            _ => st,
        };
        let nodes = self.ast().fragments[f].nodes.clone();
        self.fragment(f, &nodes, st)
    }

    /// `context.visit({ ...fragment, nodes }, state)`: a fragment with other nodes (no scope)
    pub fn visit_virtual_fragment(&mut self, f: FragId, nodes: &[NodeId], st: &State) -> Node {
        self.fragment(f, nodes, st)
    }

    /// The parent of the node being visited (the node on the path before it)
    pub fn tpl_parent(&self) -> Option<PathNode<'s>> {
        let len = self.path.len();
        if len >= 2 { Some(self.path[len - 2]) } else { None }
    }

    /// The `Fragment` visitor
    fn fragment(&mut self, f: FragId, nodes: &[NodeId], st: &State) -> Node {
        self.path.push(PathNode::Tpl(P::Fragment(f)));
        let parent = match self.tpl_parent() {
            Some(PathNode::Tpl(P::Node(n))) => Parent::Node(n),
            _ => Parent::Root,
        };
        let namespace = self.infer_namespace(st.namespace, parent, nodes);
        let cleaned = self.clean_nodes(parent, nodes, namespace, st.scope, st.preserve_whitespace, self.options.preserve_comments);

        if cleaned.hoisted.is_empty() && cleaned.trimmed.is_empty() {
            self.path.pop();
            return b::block(vec![]);
        }

        let ast = self.ast();
        let trimmed = &cleaned.trimmed;
        let is_single_element = trimmed.len() == 1 && trimmed[0].ty(ast) == "RegularElement";
        let is_single_child_not_needing_template = trimmed.len() == 1
            && match &trimmed[0] {
                Child::Node(n) => match &ast.nodes[*n] {
                    TNode::Element(el) => el.kind == "SvelteFragment" || el.kind == "TitleElement",
                    TNode::IfBlock { elseif: true, .. } => match parent {
                        Parent::Node(p) => self.an.node_meta.get(&p).and_then(|m| m.flattened.as_ref()).is_some_and(|fl| fl.contains(n)),
                        Parent::Root => false,
                    },
                    _ => false,
                },
                _ => false,
            };

        let mut body: Vec<Node> = Vec::new();
        let mut close: Option<Node> = None;

        let state = State {
            is_standalone: cleaned.is_standalone,
            init: shared(),
            snippets: shared(),
            consts: shared(),
            let_directives: shared(),
            update: shared(),
            after_update: shared(),
            memoizer: Rc::new(RefCell::new(Memoizer::default())),
            template: Rc::new(RefCell::new(Template::default())),
            transform: super::copy_transform(&st.transform),
            namespace,
            bound_contenteditable: st.bound_contenteditable,
            async_consts: Rc::new(RefCell::new(None)),
            ..st.clone()
        };

        for &h in &cleaned.hoisted {
            self.visit_node(h, &state);
        }

        if is_single_element {
            let n = trimmed[0].node_id().unwrap();
            let TNode::Element(el) = &ast.nodes[n] else { unreachable!() };
            let name = self.generate(st.scope, el.name);
            let mut id = b::id(name.as_str());
            id.loc = Some(self.conv.location(oxc_span::Span::new(el.name_loc.start as u32, el.name_loc.end as u32)));
            self.visit_node(n, &State { node: id.clone(), ..state.clone() });
            let flags = if state.template.borrow().needs_import_node { TEMPLATE_USE_IMPORT_NODE } else { 0 };
            let template_name = self.transform_template(&state, "root", flags);
            state.init.borrow_mut().insert(0, b::var(id.clone(), b::call(template_name, ())));
            close = Some(b::stmt(b::call("$.append", vec![b::id("$$anchor"), id])));
        } else if is_single_child_not_needing_template {
            self.visit_child(&trimmed[0], &state);
        } else if trimmed.len() == 1 && trimmed[0].ty(ast) == "Text" {
            let id = b::id(self.generate(st.scope, "text"));
            let (data, _) = trimmed[0].text(ast).unwrap();
            state.init.borrow_mut().insert(0, b::var(id.clone(), b::call("$.text", vec![b::literal(data.as_str())])));
            close = Some(b::stmt(b::call("$.append", vec![b::id("$$anchor"), id])));
        } else if !trimmed.is_empty() {
            let id = b::id(self.generate(st.scope, "fragment"));
            let use_space_template = trimmed.iter().any(|c| c.ty(ast) == "ExpressionTag") && trimmed.iter().all(|c| matches!(c.ty(ast), "Text" | "ExpressionTag"));
            if use_space_template {
                let id = b::id(self.generate(st.scope, "text"));
                self.process_children(trimmed, Initial::Const(id.clone()), false, &state);
                state.init.borrow_mut().insert(0, b::var(id.clone(), b::call("$.text", ())));
                close = Some(b::stmt(b::call("$.append", vec![b::id("$$anchor"), id])));
            } else if cleaned.is_standalone {
                self.process_children(trimmed, Initial::Const(b::id("$$anchor")), false, &state);
            } else {
                self.process_children(trimmed, Initial::Call("$.first_child", id.clone()), false, &state);
                let mut flags = TEMPLATE_FRAGMENT;
                if state.template.borrow().needs_import_node {
                    flags |= TEMPLATE_USE_IMPORT_NODE;
                }
                let template_name = self.transform_template(&state, "root", flags);
                state.init.borrow_mut().insert(0, b::var(id.clone(), b::call(template_name, ())));
                close = Some(b::stmt(b::call("$.append", vec![b::id("$$anchor"), id])));
            }
        }
        self.path.pop();

        body.extend(state.snippets.borrow().iter().cloned());
        body.extend(state.let_directives.borrow().iter().cloned());
        body.extend(state.consts.borrow().iter().cloned());
        if let Some(ac) = state.async_consts.borrow().as_ref() {
            if !ac.thunks.is_empty() {
                body.push(b::var(ac.id.clone(), b::call("$.run", vec![b::array(ac.thunks.clone())])));
            }
        }
        if cleaned.is_text_first {
            body.push(b::stmt(b::call("$.next", ())));
        }
        body.extend(state.init.borrow().iter().cloned());
        if !state.update.borrow().is_empty() {
            let s = self.build_render_statement(&state);
            body.push(s);
        }
        body.extend(state.after_update.borrow().iter().cloned());
        if let Some(c) = close {
            body.push(c);
        }
        b::block(body)
    }

    /// `context.visit(node, state)` for a cleaned child
    pub fn visit_child(&mut self, child: &Child, st: &State) {
        match child {
            Child::Comment => st.template.borrow_mut().push_comment(Some(String::new())),
            _ => self.visit_node(child.node_id().unwrap(), st),
        }
    }

    /// `process_children(nodes, initial, is_element, context)`
    pub fn process_children(&mut self, nodes: &[Child], initial: Initial, is_element: bool, st: &State) {
        let ast = self.ast();
        let within_bound_contenteditable = st.bound_contenteditable;
        let mut prev = initial;
        let mut skipped: usize = 0;
        let mut sequence: Vec<Child> = Vec::new();

        fn get_node(prev: &Initial, skipped: usize, is_text: bool) -> Node {
            if skipped == 0 {
                return prev.get(is_text);
            }
            b::call(
                "$.sibling",
                vec![
                    Some(prev.get(false)),
                    if is_text || skipped != 1 { Some(b::literal(skipped as f64)) } else { None },
                    if is_text { Some(b::r#true()) } else { None },
                ],
            )
        }

        let flush_node = |this: &mut Self, prev: &mut Initial, skipped: &mut usize, is_text: bool, name: &str, loc: Option<crate::estree::SourceLocation>| -> Node {
            let expression = get_node(prev, *skipped, is_text);
            let mut id = expression.clone();
            if !id.is("Identifier") {
                id = b::id(this.generate(st.scope, name));
                id.loc = loc;
                st.init.borrow_mut().push(b::var(id.clone(), expression));
            }
            *prev = Initial::Const(id.clone());
            *skipped = 1;
            id
        };

        let flush_sequence = |this: &mut Self, prev: &mut Initial, skipped: &mut usize, sequence: &[Child]| {
            if sequence.iter().all(|c| c.ty(ast) == "Text") {
                *skipped += 1;
                let parts = sequence.iter().map(|c| {
                    let (data, raw) = c.text(ast).unwrap();
                    TextPart { data, raw }
                });
                st.template.borrow_mut().push_text(parts.collect());
                return;
            }
            st.template.borrow_mut().push_text(vec![TextPart { data: " ".into(), raw: " ".into() }]);
            let (values, texts) = chunk_refs(this, sequence);
            let mut local = None;
            let (value, has_state) = this.build_template_chunk(&values, &texts, st, Memoize::State, &mut local);
            let is_text = sequence.len() == 1;
            let id = flush_node(this, prev, skipped, is_text, "text", None);
            let update = b::stmt(b::call("$.set_text", vec![id.clone(), value.clone()]));
            if has_state && !within_bound_contenteditable {
                st.update.borrow_mut().push(update);
            } else {
                st.init.borrow_mut().push(b::stmt(b::assignment("=", b::member(id, "nodeValue"), value)));
            }
        };

        for node in nodes {
            let ty = node.ty(ast);
            if ty == "Text" || ty == "ExpressionTag" {
                sequence.push(node.clone());
                continue;
            }
            if !sequence.is_empty() {
                flush_sequence(self, &mut prev, &mut skipped, &sequence);
                sequence.clear();
            }
            let mut child_state = st.clone();
            let n = node.node_id();
            if n.is_some_and(|n| self.is_static_element(n)) {
                skipped += 1;
            } else if n.is_some_and(|n| {
                matches!(ast.nodes[n], TNode::EachBlock { .. } | TNode::HtmlTag { .. }) && nodes.len() == 1 && is_element && !self.meta_is_async(self.meta_of_node(n))
            }) {
                self.is_controlled.insert(n.unwrap());
            } else {
                let (name, loc) = match n.map(|n| &ast.nodes[n]) {
                    Some(TNode::Element(el)) if el.kind == "RegularElement" => (
                        el.name,
                        Some(self.conv.location(oxc_span::Span::new(el.name_loc.start as u32, el.name_loc.end as u32))),
                    ),
                    _ => ("node", None),
                };
                let id = flush_node(self, &mut prev, &mut skipped, false, name, loc);
                child_state = State { node: id, ..st.clone() };
            }
            self.visit_child(node, &child_state);
        }
        if !sequence.is_empty() {
            flush_sequence(self, &mut prev, &mut skipped, &sequence);
        }
        if skipped > 1 {
            skipped -= 1;
            st.init.borrow_mut().push(b::stmt(b::call("$.next", vec![if skipped != 1 { Some(b::literal(skipped as f64)) } else { None }])));
        }
    }

    /// `is_static_element(node)`
    pub fn is_static_element(&self, n: NodeId) -> bool {
        let TNode::Element(el) = &self.ast().nodes[n] else { return false };
        if el.kind != "RegularElement" {
            return false;
        }
        if self.an.fragment_dynamic.contains(&el.fragment) {
            return false;
        }
        if self.an.is_custom_element_node(n) {
            return false;
        }
        for a in self.element_attributes(n) {
            let Attr::Attribute { name, value, .. } = a else { return false };
            if crate::analyze::utils::is_event_attribute(a) {
                return false;
            }
            if crate::analyze::utils::cannot_be_set_statically(name) {
                return false;
            }
            if *name == "dir" {
                return false;
            }
            if matches!(el.name, "input" | "textarea" | "select") && matches!(*name, "value" | "checked") {
                return false;
            }
            if el.name == "option" && *name == "value" {
                return false;
            }
            if !matches!(value, AttrValue::True) && !is_text_attribute(value) {
                return false;
            }
        }
        true
    }
}

/// The values of a Text/ExpressionTag sequence as template chunks (with the trimmed texts)
pub fn chunk_refs<'s>(c: &Client<'_, 's>, sequence: &[Child]) -> (Vec<ChunkRef<'s>>, Vec<String>) {
    let ast = c.ast();
    let mut values = Vec::new();
    let mut texts = Vec::new();
    for child in sequence {
        match child {
            Child::Text { data, .. } => {
                texts.push(data.clone());
                values.push(ChunkRef::OwnedText(texts.len() - 1));
            }
            Child::Node(n) => match &ast.nodes[*n] {
                TNode::Text { data, .. } => {
                    texts.push(data.to_string());
                    values.push(ChunkRef::OwnedText(texts.len() - 1));
                }
                TNode::ExpressionTag { expression, .. } => values.push(ChunkRef::Expr(expression, P::Node(*n).key())),
                _ => {}
            },
            Child::Comment => {}
        }
    }
    (values, texts)
}
