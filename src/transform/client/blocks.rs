//! The template visitors for blocks, tags and special elements (`IfBlock.js`, `EachBlock.js`,
//! `AwaitBlock.js`, `KeyBlock.js`, `HtmlTag.js`, `ConstTag.js`, `DeclarationTag.js`,
//! `DebugTag.js`, `RenderTag.js`, `SnippetBlock.js`, `SlotElement.js`, `SvelteBoundary.js`,
//! `SvelteElement.js`, `TitleElement.js`, `SvelteHead.js`, ...)

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::analyze::nodes::P;
use crate::analyze::scope::Kind;
use crate::ast::{Attr, AttrValue, Expr, Node as TNode, NodeId, Pattern};
use crate::estree::builders as b;
use crate::estree::{Node, NodeKind};

use super::super::js::{self, PathNode};
use super::fragment::is_text_attribute;
use super::utils::{ChunkRef, Memoize};
use super::{
    call_fn, copy_transform, get_value, get_value_fn, shared, Client, Memoizer, State, Transform, EACH_INDEX_REACTIVE, EACH_IS_ANIMATED,
    EACH_IS_CONTROLLED, EACH_ITEM_IMMUTABLE, EACH_ITEM_REACTIVE,
};

impl<'a, 's> Client<'a, 's> {
    /// `context.visit(node, state)` for a template node (with `set_scope`)
    pub fn visit_node(&mut self, n: NodeId, st: &State) {
        let scoped;
        let st = match self.scope_of_key(P::Node(n).key()) {
            Some(scope) if scope != st.scope => {
                scoped = self.with_scope(scope, st);
                &scoped
            }
            _ => st,
        };
        let ast = self.ast();
        self.path.push(PathNode::Tpl(P::Node(n)));
        match &ast.nodes[n] {
            TNode::Comment { data, .. } => st.template.borrow_mut().push_comment(Some(data.to_string())),
            TNode::IfBlock { .. } => self.if_block(n, st),
            TNode::EachBlock { .. } => self.each_block(n, st),
            TNode::AwaitBlock { .. } => self.await_block(n, st),
            TNode::KeyBlock { .. } => self.key_block(n, st),
            TNode::HtmlTag { .. } => self.html_tag(n, st),
            TNode::ConstTag { .. } => self.const_tag(n, st),
            TNode::DeclarationTag { .. } => self.declaration_tag(n, st),
            TNode::DebugTag { .. } => self.debug_tag(n, st),
            TNode::RenderTag { .. } => self.render_tag(n, st),
            TNode::SnippetBlock { .. } => self.snippet_block(n, st),
            TNode::Element(el) => match el.kind {
                "RegularElement" => self.regular_element(n, st),
                "Component" => {
                    let loc = Some(self.conv.location(oxc_span::Span::new(el.name_loc.start as u32, el.name_loc.end as u32)));
                    let component = self.build_component(n, el.name, loc, st);
                    st.init.borrow_mut().push(component);
                }
                "SvelteComponent" => {
                    let component = self.build_component(n, "$$component", None, st);
                    st.init.borrow_mut().push(component);
                }
                "SvelteSelf" => {
                    let loc = Some(self.conv.location(oxc_span::Span::new(el.name_loc.start as u32, el.name_loc.end as u32)));
                    let name = self.an.name.clone();
                    let component = self.build_component(n, &name, loc, st);
                    st.init.borrow_mut().push(component);
                }
                "SlotElement" => self.slot_element(n, st),
                "SvelteBoundary" => self.svelte_boundary(n, st),
                "SvelteElement" => self.svelte_element(n, st),
                "TitleElement" => self.title_element(n, st),
                "SvelteHead" => {
                    let mut head = b::id("$.head");
                    head.loc = Some(self.conv.location(oxc_span::Span::new(el.name_loc.start as u32, el.name_loc.end as u32)));
                    let block = self.visit_fragment(el.fragment, st);
                    let hash = super::super::hash(&self.filename);
                    st.init.borrow_mut().push(b::stmt(b::call(head, vec![b::literal(hash.as_str()), b::arrow(vec![b::id("$$anchor")], block)])));
                }
                "SvelteBody" => self.visit_special_element(n, "$.document.body", st),
                "SvelteDocument" => self.visit_special_element(n, "$.document", st),
                "SvelteWindow" => self.visit_special_element(n, "$.window", st),
                "SvelteFragment" => {
                    for a in &el.attributes {
                        if matches!(a, Attr::Directive { kind: "LetDirective", .. }) {
                            self.visit_attr(a, st);
                        }
                    }
                    let block = self.visit_fragment(el.fragment, st);
                    if let NodeKind::BlockStatement(bl) = block.kind {
                        st.init.borrow_mut().extend(bl.body);
                    }
                }
                _ => {}
            },
            _ => {}
        }
        self.path.pop();
    }

    /// A template pattern (`{#each}` context, `{:then}` value, `{@const}` id) as ESTree
    pub fn convert_const_pattern(&self, p: &'s Pattern<'s>) -> Node {
        match p {
            Pattern::Ident { name, start, end, .. } => {
                let mut id = b::id(name.as_str());
                id.span = Some(crate::estree::Span::new(*start as u32, *end as u32));
                id.loc = Some(self.conv.location(oxc_span::Span::new(*start as u32, *end as u32)));
                id.origin = Some(P::PatIdent(p).key());
                id
            }
            Pattern::Destructure { assign, .. } => match assign.inner() {
                oxc_ast::ast::Expression::AssignmentExpression(a) => self.conv.assignment_target(&a.left),
                other => self.conv.expression(other),
            },
        }
    }

    /// `visit_special_element(node, id, context)`
    fn visit_special_element(&mut self, n: NodeId, id: &str, st: &State) {
        let TNode::Element(el) = &self.ast().nodes[n] else { return };
        let state = State { node: b::id(id), ..st.clone() };
        for a in &el.attributes {
            if matches!(a, Attr::Directive { kind: "OnDirective", .. }) {
                if let Some(e) = self.visit_attr(a, &state) {
                    st.init.borrow_mut().push(b::stmt(e));
                }
            } else {
                self.visit_attr(a, &state);
            }
        }
    }

    fn if_block(&mut self, n: NodeId, st: &State) {
        let ast = self.ast();
        let TNode::IfBlock { test, elseif, .. } = &ast.nodes[n] else { return };
        st.template.borrow_mut().push_comment(None);
        let mut statements = Vec::new();
        let meta = self.meta_of_node(n);
        let has_await = self.an.metas[meta as usize].has_await;
        let has_blockers = self.meta_has_blockers(meta);
        let expression = self.build_expression(test, meta, st);

        let flattened = self.an.node_meta.get(&n).and_then(|m| m.flattened.clone()).unwrap_or_default();
        let mut branches = vec![n];
        branches.extend(flattened);
        let mut ifs: Vec<(Node, Node)> = Vec::new();
        let mut last_alt = n;
        for (index, &branch) in branches.iter().enumerate() {
            let TNode::IfBlock { test: btest, consequent, .. } = &ast.nodes[branch] else { continue };
            let consequent = self.visit_fragment(*consequent, st);
            let consequent_id = b::id(self.generate(st.scope, "consequent"));
            statements.push(b::var(consequent_id.clone(), b::arrow(vec![b::id("$$anchor")], consequent)));
            let bmeta = self.meta_of_node(branch);
            let test = if self.an.metas[bmeta as usize].has_await {
                b::call("$.get", vec![b::id("$$condition")])
            } else {
                let expression = self.build_expression(btest, bmeta, st);
                if self.an.metas[bmeta as usize].has_call {
                    let derived_id = b::id(self.generate(st.scope, "d"));
                    statements.push(b::var(derived_id.clone(), b::call("$.derived", vec![b::arrow(vec![], expression)])));
                    b::call("$.get", vec![derived_id])
                } else {
                    expression
                }
            };
            let render_call = b::stmt(b::call("$$render", vec![Some(consequent_id), if index != 0 { Some(b::literal(index as f64)) } else { None }]));
            ifs.push((test, render_call));
            last_alt = branch;
        }

        let mut alternate_stmt: Option<Node> = None;
        if let TNode::IfBlock { alternate: Some(alt), .. } = &ast.nodes[last_alt] {
            let alternate = self.visit_fragment(*alt, st);
            let alternate_id = b::id(self.generate(st.scope, "alternate"));
            statements.push(b::var(alternate_id.clone(), b::arrow(vec![b::id("$$anchor")], alternate)));
            alternate_stmt = Some(b::stmt(b::call("$$render", vec![alternate_id, b::literal(-1.0)])));
        }

        let mut chain = alternate_stmt;
        for (test, render) in ifs.into_iter().rev() {
            chain = Some(b::r#if(test, render, chain));
        }
        let mut args = vec![st.node.clone(), b::arrow(vec![b::id("$$render")], b::block(chain.into_iter().collect()))];
        if *elseif {
            args.push(b::r#true());
        }
        let start = ast.nodes[n].start();
        statements.push(self.add_svelte_meta(b::call("$.if", args), Some(start), "if", None));

        if has_await || has_blockers {
            let blockers = self.meta_blockers_array(meta);
            let thunk = if has_await { b::array(vec![self.async_thunk(expression, meta)]) } else { b::void0() };
            let params = if has_await { vec![st.node.clone(), b::id("$$condition")] } else { vec![st.node.clone()] };
            st.init.borrow_mut().push(b::stmt(b::call("$.async", vec![st.node.clone(), blockers, thunk, b::arrow(params, b::block(statements))])));
        } else {
            st.init.borrow_mut().push(b::block(statements));
        }
    }

    fn key_block(&mut self, n: NodeId, st: &State) {
        let ast = self.ast();
        let TNode::KeyBlock { expression, fragment, .. } = &ast.nodes[n] else { return };
        st.template.borrow_mut().push_comment(None);
        let meta = self.meta_of_node(n);
        let has_await = self.an.metas[meta as usize].has_await;
        let has_blockers = self.meta_has_blockers(meta);
        let expression = self.build_expression(expression, meta, st);
        let key = b::thunk(if has_await { b::call("$.get", vec![b::id("$$key")]) } else { expression.clone() });
        let body = self.visit_fragment(*fragment, st);
        let statement = self.add_svelte_meta(
            b::call("$.key", vec![st.node.clone(), key, b::arrow(vec![b::id("$$anchor")], body)]),
            Some(ast.nodes[n].start()),
            "key",
            None,
        );
        if has_await || has_blockers {
            let blockers = self.meta_blockers_array(meta);
            let thunk = if has_await { b::array(vec![self.async_thunk(expression, meta)]) } else { b::void0() };
            let params = if has_await { vec![st.node.clone(), b::id("$$key")] } else { vec![st.node.clone()] };
            st.init.borrow_mut().push(b::stmt(b::call("$.async", vec![st.node.clone(), blockers, thunk, b::arrow(params, b::block(vec![statement]))])));
        } else {
            st.init.borrow_mut().push(statement);
        }
    }

    fn html_tag(&mut self, n: NodeId, st: &State) {
        let ast = self.ast();
        let TNode::HtmlTag { expression, .. } = &ast.nodes[n] else { return };
        let is_controlled = self.is_controlled.contains(&n);
        if !is_controlled {
            st.template.borrow_mut().push_comment(None);
        }
        let meta = self.meta_of_node(n);
        let has_await = self.an.metas[meta as usize].has_await;
        let has_blockers = self.meta_has_blockers(meta);
        let expression = self.build_expression(expression, meta, st);
        let html = if has_await { b::call("$.get", vec![b::id("$$html")]) } else { expression.clone() };
        let is_svg = !is_controlled && st.namespace == "svg";
        let is_mathml = !is_controlled && st.namespace == "mathml";
        let flag = |c: bool| if c { Some(b::r#true()) } else { None };
        let statement = b::stmt(b::call(
            "$.html",
            vec![
                Some(st.node.clone()),
                Some(b::thunk(html)),
                flag(is_controlled),
                flag(is_svg),
                flag(is_mathml),
                flag(self.is_ignored_key(P::Node(n).key(), "hydration_html_changed")),
            ],
        ));
        if has_await || has_blockers {
            let blockers = self.meta_blockers_array(meta);
            let thunk = if has_await { b::array(vec![self.async_thunk(expression, meta)]) } else { b::void0() };
            let params = if has_await { vec![st.node.clone(), b::id("$$html")] } else { vec![st.node.clone()] };
            st.init.borrow_mut().push(b::stmt(b::call("$.async", vec![st.node.clone(), blockers, thunk, b::arrow(params, b::block(vec![statement]))])));
        } else {
            st.init.borrow_mut().push(statement);
        }
    }

    fn each_block(&mut self, n: NodeId, st: &State) {
        let ast = self.ast();
        let TNode::EachBlock { expression, context, body, fallback, index, key, .. } = &ast.nodes[n] else { return };
        let each_meta = self.an.node_meta.get(&n).cloned().unwrap_or_default();
        let meta = self.meta_of_node(n);

        // the collection is evaluated in the parent scope
        let parent_scope = self.an.sc.scope(st.scope).parent.unwrap_or(st.scope);
        let parent_scope_state = State { scope: parent_scope, ..st.clone() };
        let collection = self.build_expression(expression, meta, &parent_scope_state);

        let is_controlled = self.is_controlled.contains(&n);
        if !is_controlled {
            st.template.borrow_mut().push_comment(None);
        }

        let mut flags: u32 = 0;
        if each_meta.keyed && index.is_some() {
            flags |= EACH_INDEX_REACTIVE;
        }
        let context_name = match context {
            Some(Pattern::Ident { name, .. }) => Some(name.as_str()),
            _ => None,
        };
        let key_name = key.as_ref().and_then(|k| match self.convert_expr(k).kind {
            NodeKind::Identifier(i) => Some(i.name.to_string()),
            _ => None,
        });
        let key_is_item = key_name.is_some() && context_name.is_some() && key_name.as_deref() == context_name;

        let deps = self.an.metas[meta as usize].dependencies.clone();
        let uses_store = deps.iter().any(|&d| self.binding(d).kind == Kind::StoreSub);
        let depth = self.an.sc.scope(st.scope).function_depth;
        for &d in &deps {
            if self.an.sc.scope(self.binding(d).scope).function_depth >= depth {
                continue;
            }
            if !self.an.runes || !key_is_item || uses_store {
                flags |= EACH_ITEM_REACTIVE;
                break;
            }
        }
        if self.an.runes && !uses_store {
            flags |= EACH_ITEM_IMMUTABLE;
        }
        if key.is_some()
            && ast.fragments[*body].nodes.iter().any(|&c| match &ast.nodes[c] {
                TNode::Element(el) if el.kind == "RegularElement" || el.kind == "SvelteElement" => {
                    el.attributes.iter().any(|a| matches!(a, Attr::Directive { kind: "AnimateDirective", .. }))
                }
                _ => false,
            })
        {
            flags |= EACH_IS_ANIMATED;
        }
        if is_controlled {
            flags |= EACH_IS_CONTROLLED;
        }

        let store_to_invalidate =
            deps.iter().find(|&&d| self.binding(d).kind == Kind::StoreSub).map(|&d| self.binding(d).node.name.to_string()).unwrap_or_default();

        let mut collection_id: Option<Node> = None;
        let names: Vec<String> = self.an.sc.scope(st.scope).declarations.iter().map(|(k, _)| k.to_string()).collect();
        for name in names {
            if self.get(parent_scope, &name).is_some() {
                collection_id = Some(b::id(self.an.sc.unique("$$array")));
                break;
            }
        }

        let child_state = State {
            transform: copy_transform(&st.transform),
            store_to_invalidate: Some(store_to_invalidate.clone()).filter(|s| !s.is_empty()),
            ..st.clone()
        };
        let key_state = State { transform: copy_transform(&st.transform), ..st.clone() };

        let index_id = if each_meta.contains_group_binding || index.is_none() {
            b::id(self.an.sc.each_index.get(&n).cloned().unwrap_or_else(|| "$$index".into()).as_str())
        } else {
            b::id(index.as_deref().unwrap())
        };
        let item = match context {
            Some(c @ Pattern::Ident { .. }) => self.convert_const_pattern(c),
            _ => b::id("$$item"),
        };

        let uses_index: Rc<Cell<bool>> = Rc::new(Cell::new(each_meta.contains_group_binding));
        let key_uses_index: Rc<Cell<bool>> = Rc::new(Cell::new(false));

        if let Some(index) = index {
            let ui = uses_index.clone();
            let reactive = flags & EACH_INDEX_REACTIVE != 0;
            child_state.transform.borrow_mut().insert(
                index.clone(),
                Transform::read(Rc::new(move |_, node| {
                    ui.set(true);
                    if reactive { get_value(node.clone()) } else { node.clone() }
                })),
            );
            let kui = key_uses_index.clone();
            key_state.transform.borrow_mut().insert(
                index.clone(),
                Transform::read(Rc::new(move |_, node| {
                    kui.set(true);
                    node.clone()
                })),
            );
        }

        let mut declarations: Vec<Node> = Vec::new();
        let invalidate_store = if store_to_invalidate.is_empty() {
            None
        } else {
            Some(b::call("$.invalidate_store", vec![b::id("$$stores"), b::literal(store_to_invalidate.as_str())]))
        };
        let mut sequence: Vec<Node> = Vec::new();

        if !self.an.runes {
            let mut transitive: Vec<Node> = Vec::new();
            let mut seen_bindings: Vec<crate::analyze::scope::BindingId> = Vec::new();
            if let Some(cid) = &collection_id {
                transitive.push(cid.clone());
                child_state.transform.borrow_mut().insert(js::ident(cid).unwrap().to_string(), Transform::read(call_fn()));
            } else {
                for &d in &each_meta.transitive_deps {
                    if !seen_bindings.contains(&d) {
                        seen_bindings.push(d);
                        transitive.push(self.id_copy(self.binding(d).node));
                    }
                }
            }
            // the each blocks around this one
            let len = self.path.len();
            let parents: Vec<NodeId> = self.path[..len.saturating_sub(1)]
                .iter()
                .filter_map(|p| match p {
                    PathNode::Tpl(P::Node(e)) if matches!(ast.nodes[*e], TNode::EachBlock { .. }) => Some(*e),
                    _ => None,
                })
                .collect();
            for e in parents {
                let deps = self.an.node_meta.get(&e).map(|m| m.transitive_deps.clone()).unwrap_or_default();
                for d in deps {
                    if !seen_bindings.contains(&d) {
                        seen_bindings.push(d);
                        transitive.push(self.id_copy(self.binding(d).node));
                    }
                }
            }
            if !transitive.is_empty() {
                let visited: Vec<Node> = transitive.iter().map(|t| self.visit_js(t, &child_state)).collect();
                sequence.push(b::call("$.invalidate_inner_signals", vec![b::thunk(b::sequence(visited))]));
            }
        }
        if let Some(i) = invalidate_store {
            sequence.push(i);
        }

        if let Some(Pattern::Ident { name, .. }) = context {
            let bid = self.get(st.scope, name);
            let reassigned = bid.is_some_and(|b| self.binding(b).reassigned);
            let index_reactive = flags & EACH_INDEX_REACTIVE != 0;
            let item_reactive = flags & EACH_ITEM_REACTIVE != 0;
            let collection_c = collection.clone();
            let cid = collection_id.clone();
            let idx = index_id.clone();
            let read: super::ReadFn = Rc::new(move |_, node| {
                if reassigned {
                    let obj = match &cid {
                        Some(c) => b::call(c.clone(), ()),
                        None => collection_c.clone(),
                    };
                    return b::member_with(obj, if index_reactive { get_value(idx.clone()) } else { idx.clone() }, true, false);
                }
                if item_reactive { get_value(node.clone()) } else { node.clone() }
            });
            let collection_c = collection.clone();
            let cid = collection_id.clone();
            let idx = index_id.clone();
            let seq = sequence.clone();
            let ui = uses_index.clone();
            let assign: super::AssignFn = Rc::new(move |_, _, value, _| {
                ui.set(true);
                let obj = match &cid {
                    Some(c) => b::call(c.clone(), ()),
                    None => collection_c.clone(),
                };
                let left = b::member_with(obj, if index_reactive { get_value(idx.clone()) } else { idx.clone() }, true, false);
                let mut exprs = vec![b::assignment("=", left, value)];
                exprs.extend(seq.iter().cloned());
                b::sequence(exprs)
            });
            let seq = sequence.clone();
            let ui = uses_index.clone();
            let mutate: super::MutateFn = Rc::new(move |_, _, mutation| {
                ui.set(true);
                let mut exprs = vec![mutation];
                exprs.extend(seq.iter().cloned());
                b::sequence(exprs)
            });
            child_state.transform.borrow_mut().insert(name.clone(), Transform { read, assign: Some(assign), mutate: Some(mutate), update: None });
            key_state.transform.borrow_mut().remove(name.as_str());
        } else if let Some(c) = context {
            let unwrapped = if flags & EACH_ITEM_REACTIVE != 0 { b::call("$.get", vec![item.clone()]) } else { item.clone() };
            let pattern = self.convert_const_pattern(c);
            let (inserts, paths) = js::extract_paths(&pattern, unwrapped);
            let mut names = Vec::new();
            for (_, value) in inserts {
                let name = self.generate(st.scope, "$$array");
                names.push(name.clone());
                child_state.transform.borrow_mut().insert(name.clone(), Transform::read(get_value_fn()));
                let mut value = value;
                js::rename_placeholders(&mut value, &names);
                let expression = self.visit_js(&b::thunk(value), &child_state);
                declarations.push(b::var(b::id(name.as_str()), b::call("$.derived", vec![expression])));
            }
            for path in paths {
                let name = js::ident(&path.node).unwrap_or("").to_string();
                let needs_derived = path.has_default_value;
                let mut e = path.expression;
                js::rename_placeholders(&mut e, &names);
                let f = b::thunk(self.visit_js(&e, &child_state));
                declarations.push(b::r#let(path.node.clone(), if needs_derived { b::call("$.derived_safe_equal", vec![f]) } else { f }));
                let read = if needs_derived { get_value_fn() } else { call_fn() };
                let mut update_expression = path.update_expression;
                js::rename_placeholders(&mut update_expression, &names);
                let seq = sequence.clone();
                let assign: super::AssignFn = Rc::new(move |_, _, value, _| {
                    let mut exprs = vec![b::assignment("=", update_expression.clone(), value)];
                    exprs.extend(seq.iter().cloned());
                    b::sequence(exprs)
                });
                let seq = sequence.clone();
                let mutate: super::MutateFn = Rc::new(move |_, _, mutation| {
                    let mut exprs = vec![mutation];
                    exprs.extend(seq.iter().cloned());
                    b::sequence(exprs)
                });
                child_state.transform.borrow_mut().insert(name.clone(), Transform { read: read.clone(), assign: Some(assign), mutate: Some(mutate), update: None });
                if self.dev {
                    let r = read(self, &b::id(name.as_str()));
                    declarations.push(b::stmt(r));
                }
                key_state.transform.borrow_mut().remove(name.as_str());
            }
        }

        let block = self.visit_fragment(*body, &child_state);

        let mut key_function = b::id("$.index");
        if each_meta.keyed {
            let pattern = self.convert_const_pattern(context.as_ref().unwrap());
            let pattern = self.visit_js(&pattern, &key_state);
            let expression = self.visit_expr(key.as_ref().unwrap(), &key_state);
            key_function = b::arrow(if key_uses_index.get() { vec![pattern, index_id.clone()] } else { vec![pattern] }, expression);
        }

        if let Some(i) = index {
            if each_meta.contains_group_binding {
                declarations.push(b::r#let(b::id(i.as_str()), index_id.clone()));
            }
        }

        let has_await = self.an.metas[meta as usize].has_await;
        let get_collection = if has_await { self.async_thunk(collection.clone(), meta) } else { b::thunk(collection.clone()) };
        let thunk = if has_await { b::thunk(b::call("$.get", vec![b::id("$$collection")])) } else { get_collection.clone() };

        let mut render_args = vec![b::id("$$anchor"), item];
        if uses_index.get() || collection_id.is_some() {
            render_args.push(index_id.clone());
        }
        if let Some(c) = &collection_id {
            render_args.push(c.clone());
        }
        let mut body_stmts = declarations;
        if let NodeKind::BlockStatement(bl) = block.kind {
            body_stmts.extend(bl.body);
        }
        let mut args = vec![st.node.clone(), b::literal(flags as f64), thunk, key_function, b::arrow(render_args, b::block(body_stmts))];
        if let Some(f) = fallback {
            let fb = self.visit_fragment(*f, st);
            args.push(b::arrow(vec![b::id("$$anchor")], fb));
        }
        let statements = vec![self.add_svelte_meta(b::call("$.each", args), Some(ast.nodes[n].start()), "each", None)];
        if self.meta_is_async(meta) {
            let blockers = self.meta_blockers_array(meta);
            let values = if has_await { b::array(vec![get_collection]) } else { b::void0() };
            let params = if has_await { vec![st.node.clone(), b::id("$$collection")] } else { vec![st.node.clone()] };
            st.init.borrow_mut().push(b::stmt(b::call("$.async", vec![st.node.clone(), blockers, values, b::arrow(params, b::block(statements))])));
        } else {
            st.init.borrow_mut().extend(statements);
        }
    }

    fn await_block(&mut self, n: NodeId, st: &State) {
        let ast = self.ast();
        let TNode::AwaitBlock { expression, value, error, pending, then, catch, .. } = &ast.nodes[n] else { return };
        st.template.borrow_mut().push_comment(None);
        let meta = self.meta_of_node(n);
        let input = self.build_expression(expression, meta, st);
        let expression = if self.an.metas[meta as usize].has_await { self.async_thunk(input, meta) } else { b::thunk(input) };

        let mut then_block = None;
        let mut catch_block = None;
        if let Some(then) = then {
            let then_state = st.with_transform_copy();
            let argument = value.as_ref().map(|v| self.create_derived_block_argument(v, &then_state));
            let mut args = vec![b::id("$$anchor")];
            let mut declarations = Vec::new();
            if let Some((id, decls)) = argument {
                args.push(id);
                declarations = decls;
            }
            let block = self.visit_fragment(*then, &then_state);
            if let NodeKind::BlockStatement(bl) = block.kind {
                declarations.extend(bl.body);
            }
            then_block = Some(b::arrow(args, b::block(declarations)));
        }
        if let Some(catch) = catch {
            // the JS shares the transform object here
            let catch_state = st.clone();
            let argument = error.as_ref().map(|v| self.create_derived_block_argument(v, &catch_state));
            let mut args = vec![b::id("$$anchor")];
            let mut declarations = Vec::new();
            if let Some((id, decls)) = argument {
                args.push(id);
                declarations = decls;
            }
            let block = self.visit_fragment(*catch, &catch_state);
            if let NodeKind::BlockStatement(bl) = block.kind {
                declarations.extend(bl.body);
            }
            catch_block = Some(b::arrow(args, b::block(declarations)));
        }
        let pending_block = match pending {
            Some(p) => {
                let block = self.visit_fragment(*p, st);
                b::arrow(vec![b::id("$$anchor")], block)
            }
            None => b::null(),
        };
        let stmt = self.add_svelte_meta(
            b::call("$.await", vec![Some(st.node.clone()), Some(expression), Some(pending_block), then_block, catch_block]),
            Some(ast.nodes[n].start()),
            "await",
            None,
        );
        if self.meta_has_blockers(meta) || self.an.metas[meta as usize].has_await {
            let blockers = self.meta_blockers_array(meta);
            st.init.borrow_mut().push(b::stmt(b::call(
                "$.async",
                vec![st.node.clone(), blockers, b::array(Vec::<Node>::new()), b::arrow(vec![st.node.clone()], b::block(vec![stmt]))],
            )));
        } else {
            st.init.borrow_mut().push(stmt);
        }
    }

    /// `create_derived_block_argument(node, context)`: the parameter and declarations
    fn create_derived_block_argument(&mut self, p: &'s Pattern<'s>, st: &State) -> (Node, Vec<Node>) {
        let node = self.convert_const_pattern(p);
        if let NodeKind::Identifier(i) = &node.kind {
            st.transform.borrow_mut().insert(i.name.to_string(), Transform::read(get_value_fn()));
            return (node, Vec::new());
        }
        let pattern = self.visit_js(&node, st);
        let identifiers: Vec<Node> = js::extract_identifiers(&node).into_iter().cloned().collect();
        let id = b::id("$$source");
        let value = b::id("$$value");
        let block = b::block(vec![
            b::var(pattern, b::call("$.get", vec![id.clone()])),
            b::r#return(b::object(identifiers.iter().map(|i| b::prop("init", i.clone(), i.clone())).collect())),
        ]);
        let mut declarations = vec![b::var(value.clone(), self.create_derived(block, None))];
        for ident in identifiers {
            st.transform.borrow_mut().insert(js::ident(&ident).unwrap().to_string(), Transform::read(get_value_fn()));
            let d = self.create_derived(b::member(b::call("$.get", vec![value.clone()]), ident.clone()), None);
            declarations.push(b::var(ident, d));
        }
        (id, declarations)
    }

    fn const_tag(&mut self, n: NodeId, st: &State) {
        let ast = self.ast();
        let TNode::ConstTag { id, init, .. } = &ast.nodes[n] else { return };
        let meta = self.meta_of_node(n);
        let pattern = self.convert_const_pattern(id);
        if let NodeKind::Identifier(i) = &pattern.kind {
            let name = i.name.to_string();
            let init = self.build_expression(init, meta, st);
            let mut expression = self.create_derived(init, Some(meta));
            if self.dev {
                expression = b::call("$.tag", vec![expression, b::literal(name.as_str())]);
            }
            st.transform.borrow_mut().insert(name, Transform::read(get_value_fn()));
            self.add_const_declaration(n, st, pattern, expression);
        } else {
            let identifiers: Vec<Node> = js::extract_identifiers(&pattern).into_iter().cloned().collect();
            let tmp = b::id(self.generate(st.scope, "computed_const"));
            let transform = copy_transform(&st.transform);
            for i in &identifiers {
                transform.borrow_mut().remove(js::ident(i).unwrap());
            }
            let child_state = State { transform, ..st.clone() };
            let is_simple_object_pattern = match &pattern.kind {
                NodeKind::ObjectPattern(o) => o.properties.iter().all(|p| match &p.kind {
                    NodeKind::Property(p) => !p.computed && js::ident(&p.key).is_some() && js::ident(&p.value).is_some() && js::ident(&p.key) == js::ident(&p.value),
                    _ => false,
                }),
                _ => false,
            };
            let init = self.build_expression(init, meta, &child_state);
            let block = if is_simple_object_pattern {
                b::block(vec![b::r#return(init)])
            } else {
                let visited = self.visit_js(&pattern, &child_state);
                b::block(vec![
                    b::r#const(visited, init),
                    b::r#return(b::object(identifiers.iter().map(|i| b::prop("init", i.clone(), i.clone())).collect())),
                ])
            };
            let mut expression = self.create_derived(block, Some(meta));
            if self.dev {
                expression = b::call("$.tag", vec![expression, b::literal("[@const]")]);
            }
            self.add_const_declaration(n, st, tmp.clone(), expression);
            for i in identifiers {
                let tmp = tmp.clone();
                st.transform
                    .borrow_mut()
                    .insert(js::ident(&i).unwrap().to_string(), Transform::read(Rc::new(move |_, node| b::member(b::call("$.get", vec![tmp.clone()]), node.clone()))));
            }
        }
    }

    /// `add_const_declaration(context, id, expression, metadata)`
    fn add_const_declaration(&mut self, n: NodeId, st: &State, id: Node, expression: Node) {
        let after = if self.dev { vec![b::stmt(b::call("$.get", vec![id.clone()]))] } else { vec![] };
        if self.an.promises_id.contains_key(&n) {
            let assignment = b::stmt(b::assignment("=", id.clone(), expression));
            self.add_async_declaration(n, st, vec![id], vec![assignment], "let");
        } else {
            st.consts.borrow_mut().push(b::r#const(id, expression));
            st.consts.borrow_mut().extend(after);
        }
    }

    /// `add_async_declaration(context, metadata, ids, assignments, kind)`
    fn add_async_declaration(&mut self, n: NodeId, st: &State, ids: Vec<Node>, assignments: Vec<Node>, kind: &str) {
        let promises = self.an.promises_id[&n];
        let promises_name = self.an.promise_ids[promises as usize].clone();
        if st.async_consts.borrow().is_none() {
            *st.async_consts.borrow_mut() = Some(super::AsyncConsts { id: b::id(promises_name.as_str()), thunks: Vec::new() });
        }
        for id in &ids {
            let name = js::ident(id).unwrap_or("");
            st.consts.borrow_mut().push(if kind == "var" { b::var(b::id(name), None) } else { b::r#let(b::id(name), None) });
        }
        let current = st.async_consts.borrow().as_ref().map(|a| js::ident(&a.id).unwrap_or("").to_string());
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
            .map(|bl| self.blocker_expression(bl))
            .collect();
        let mut thunks = Vec::new();
        if blockers.len() == 1 {
            thunks.push(b::thunk(b::member(blockers.into_iter().next().unwrap(), "promise")));
        } else if !blockers.is_empty() {
            thunks.push(b::thunk(b::call("$.wait", vec![b::array(blockers)])));
        }
        let has_await = self.an.metas[meta as usize].has_await || assignments.iter().any(b::has_await_expression);
        let body = if assignments.len() == 1 {
            match assignments[0].kind.clone() {
                NodeKind::ExpressionStatement(e) => *e.expression,
                kind => Node::new(kind),
            }
        } else {
            b::block(assignments)
        };
        thunks.push(if has_await { self.async_thunk(body, meta) } else { b::thunk(body) });
        if let Some(ac) = st.async_consts.borrow_mut().as_mut() {
            ac.thunks.extend(thunks);
        }
    }

    fn declaration_tag(&mut self, n: NodeId, st: &State) {
        let ast = self.ast();
        let TNode::DeclarationTag { declaration, .. } = &ast.nodes[n] else { return };
        self.add_state_transformers(st);
        let crate::ast::Declaration::Js(stmt) = declaration else { return };
        let node = self.conv.statement(&stmt.stmt);
        let visited = self.visit_js(&node, st);
        if self.an.promises_id.contains_key(&n) && node.is("VariableDeclaration") {
            if let NodeKind::VariableDeclaration(v) = &visited.kind {
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
                let assignments: Vec<Node> = v
                    .declarations
                    .iter()
                    .filter_map(|d| match &d.kind {
                        NodeKind::VariableDeclarator(d) => d.init.as_ref().map(|i| b::stmt(b::assignment("=", (*d.id).clone(), (**i).clone()))),
                        _ => None,
                    })
                    .collect();
                let kind = v.kind.as_str();
                self.add_async_declaration(n, st, ids, assignments, kind);
                return;
            }
        }
        st.consts.borrow_mut().push(visited);
    }

    fn debug_tag(&mut self, n: NodeId, st: &State) {
        let ast = self.ast();
        let TNode::DebugTag { identifiers, .. } = &ast.nodes[n] else { return };
        let ids: Vec<Node> = match identifiers {
            crate::ast::DebugArgs::All => vec![],
            crate::ast::DebugArgs::One(e) => vec![self.convert_expr(e)],
            crate::ast::DebugArgs::Sequence(e) => match self.convert_expr(e).kind {
                NodeKind::SequenceExpression(s) => s.expressions,
                kind => vec![Node::new(kind)],
            },
        };
        let mut blockers = Vec::new();
        for id in &ids {
            if let Some(bl) = js::ident(id).and_then(|name| self.get(st.scope, name)).and_then(|b| self.binding(b).blocker) {
                blockers.push(self.blocker_expression(bl));
            }
        }
        let mut props = Vec::new();
        for id in &ids {
            let v = self.visit_js(id, st);
            let visited = b::call("$.snapshot", vec![v]);
            props.push(b::prop("init", id.clone(), if self.an.runes { visited } else { b::call("$.untrack", vec![b::thunk(visited)]) }));
        }
        let mut args = vec![b::thunk(b::block(vec![b::stmt(b::call("console.log", vec![b::object(props)])), b::debugger()]))];
        if !blockers.is_empty() {
            args.push(b::array(Vec::<Node>::new()));
            args.push(b::array(Vec::<Node>::new()));
            args.push(b::array(blockers));
        }
        st.init.borrow_mut().push(b::stmt(b::call("$.template_effect", args)));
    }

    fn render_tag(&mut self, n: NodeId, st: &State) {
        let ast = self.ast();
        let TNode::RenderTag { expression, .. } = &ast.nodes[n] else { return };
        st.template.borrow_mut().push_comment(None);
        let converted = self.convert_expr(expression);
        let is_chain = converted.is("ChainExpression");
        let call = js::unwrap_optional(&converted).clone();
        let NodeKind::CallExpression(c) = &call.kind else { return };

        // the argument metadata is keyed by the oxc arguments
        let arg_metas: Vec<u32> = match expression {
            Expr::Js(js) => {
                let e = match js.effective_root() {
                    oxc_ast::ast::Expression::ChainExpression(ch) => match &ch.expression {
                        oxc_ast::ast::ChainElement::CallExpression(c) => Some(&**c),
                        _ => None,
                    },
                    oxc_ast::ast::Expression::CallExpression(c) => Some(&**c),
                    _ => None,
                };
                e.map(|c| c.arguments.iter().map(|a| self.meta_of_key(crate::analyze::nodes::argument(a).key())).collect()).unwrap_or_default()
            }
            _ => vec![],
        };

        let mut args = Vec::new();
        let mut memoizer = Some(Memoizer::default());
        for (i, arg) in c.arguments.iter().enumerate() {
            let m = arg_metas.get(i).copied().unwrap_or(0);
            let mut expression = self.build_expression_node(arg, m, st);
            let (memoized, did) = memoizer.as_mut().unwrap().add(self, expression.clone(), m, false);
            if did {
                expression = b::call("$.get", vec![memoized]);
            }
            args.push(b::thunk(expression));
        }
        let memoizer = memoizer.take().unwrap();
        memoizer.apply(self);
        let mut statements = memoizer.deriveds(self.an.runes);
        let meta = self.meta_of_node(n);
        let mut snippet_function = self.build_expression_node(&c.callee, meta, st);
        let start = ast.nodes[n].start();
        if self.an.node_meta.get(&n).is_some_and(|m| m.dynamic) {
            if is_chain {
                snippet_function = b::logical("??", snippet_function, b::id("$.noop"));
            }
            let mut call_args = vec![st.node.clone(), b::thunk(snippet_function)];
            call_args.extend(args);
            statements.push(self.add_svelte_meta(b::call("$.snippet", call_args), Some(start), "render", None));
        } else {
            let mut call_args = vec![st.node.clone()];
            call_args.extend(args);
            let call = if !is_chain { b::call(snippet_function, call_args) } else { b::maybe_call(snippet_function, call_args) };
            statements.push(self.add_svelte_meta(call, Some(start), "render", None));
        }
        let async_values = memoizer.async_values(self);
        let blockers = memoizer.blockers(self);
        if async_values.is_some() || blockers.is_some() {
            let mut params = vec![st.node.clone()];
            params.extend(memoizer.async_ids());
            st.init.borrow_mut().push(b::stmt(b::call(
                "$.async",
                vec![Some(st.node.clone()), blockers, memoizer.async_values(self), Some(b::arrow(params, b::block(statements)))],
            )));
            if st.is_standalone {
                st.init.borrow_mut().push(b::stmt(b::call("$.next", ())));
            }
        } else {
            let s = if statements.len() == 1 { statements.pop().unwrap() } else { b::block(statements) };
            st.init.borrow_mut().push(s);
        }
    }

    fn snippet_block(&mut self, n: NodeId, st: &State) {
        let ast = self.ast();
        let TNode::SnippetBlock { expression, parameters, body, .. } = &ast.nodes[n] else { return };
        let mut args = vec![b::id("$$anchor")];
        let mut declarations: Vec<Node> = Vec::new();
        let transform = copy_transform(&st.transform);
        let child_state = State { transform: transform.clone(), ..st.clone() };

        let params: Vec<Node> = match parameters {
            Some(arrow) => {
                let mut conv = crate::estree::convert::Converter::new(self.locator, self.an.root.ts);
                conv.preserve_parens = true;
                match conv.expression(&arrow.expr).kind {
                    NodeKind::ArrowFunctionExpression(a) => a.params,
                    _ => vec![],
                }
            }
            None => vec![],
        };
        for (i, argument) in params.iter().enumerate() {
            if let NodeKind::Identifier(id) = &argument.kind {
                args.push(b::assignment_pattern(argument.clone(), b::id("$.noop")));
                transform.borrow_mut().insert(id.name.to_string(), Transform::read(call_fn()));
                continue;
            }
            let arg_alias = format!("$$arg{i}");
            args.push(b::id(arg_alias.as_str()));
            let (inserts, paths) = js::extract_paths(argument, b::maybe_call(b::id(arg_alias.as_str()), ()));
            let mut names = Vec::new();
            for (_, value) in inserts {
                let name = self.generate(st.scope, "$$array");
                names.push(name.clone());
                transform.borrow_mut().insert(name.clone(), Transform::read(get_value_fn()));
                let mut value = value;
                js::rename_placeholders(&mut value, &names);
                let v = self.visit_js(&b::thunk(value), st);
                declarations.push(b::var(b::id(name.as_str()), b::call("$.derived", vec![v])));
            }
            for path in paths {
                let name = js::ident(&path.node).unwrap_or("").to_string();
                let needs_derived = path.has_default_value;
                let mut e = path.expression;
                js::rename_placeholders(&mut e, &names);
                let f = b::thunk(self.visit_js(&e, &child_state));
                declarations.push(b::r#let(path.node.clone(), if needs_derived { b::call("$.derived_safe_equal", vec![f]) } else { f }));
                let read = if needs_derived { get_value_fn() } else { call_fn() };
                transform.borrow_mut().insert(name.clone(), Transform::read(read.clone()));
                if self.dev {
                    let r = read(self, &b::id(name.as_str()));
                    declarations.push(b::stmt(r));
                }
            }
        }
        let block = self.visit_fragment(*body, &child_state);
        let mut body_stmts = vec![if self.dev { b::stmt(b::call("$.validate_snippet_args", vec![b::spread(b::id("arguments"))])) } else { b::empty() }];
        body_stmts.extend(declarations);
        if let NodeKind::BlockStatement(bl) = block.kind {
            body_stmts.extend(bl.body);
        }
        let body = b::block(body_stmts);
        let snippet = if self.dev {
            b::call("$.wrap_snippet", vec![b::id(self.an.name.as_str()), b::r#function(None, args, body)])
        } else {
            b::arrow(args, body)
        };
        let name = self.convert_expr(expression);
        let declaration = b::r#const(name, snippet);
        // top-level snippets are hoisted so that the `<script>` can reference them
        let top_level = self.path.len() == 2 && matches!(self.path[0], PathNode::Tpl(P::Fragment(_)));
        if top_level {
            if self.an.node_meta.get(&n).is_some_and(|m| m.can_hoist) {
                self.module_level_snippets.push(declaration);
            } else {
                self.instance_level_snippets.push(declaration);
            }
        } else {
            st.snippets.borrow_mut().push(declaration);
        }
    }

    fn slot_element(&mut self, n: NodeId, st: &State) {
        let ast = self.ast();
        let TNode::Element(el) = &ast.nodes[n] else { return };
        st.template.borrow_mut().push_comment(None);
        let mut props = Vec::new();
        let mut spreads = Vec::new();
        let lets = shared();
        let mut memoizer = Some(Memoizer::default());
        let mut name = b::literal("default");
        for a in &el.attributes {
            match a {
                Attr::Spread { .. } => {
                    if let Some(v) = self.visit_attr(a, st) {
                        spreads.push(b::thunk(v));
                    }
                }
                Attr::Attribute { name: an, value, .. } => {
                    let (v, has_state) = self.build_attribute_value(value, st, Memoize::SlotProp, &mut memoizer);
                    if *an == "name" {
                        name = v;
                    } else if *an != "slot" {
                        if has_state {
                            props.push(b::get(an, vec![b::r#return(v)]));
                        } else {
                            props.push(b::init(an, v));
                        }
                    }
                }
                Attr::Directive { kind: "LetDirective", .. } => {
                    let s = State { let_directives: lets.clone(), ..st.clone() };
                    self.visit_attr(a, &s);
                }
                _ => {}
            }
        }
        let memoizer = memoizer.unwrap();
        memoizer.apply(self);
        st.init.borrow_mut().extend(lets.borrow().iter().cloned());
        let mut statements = memoizer.deriveds(self.an.runes);
        let props_expression = if spreads.is_empty() {
            b::object(props)
        } else {
            let mut args = vec![b::object(props)];
            args.extend(spreads);
            b::call("$.spread_props", args)
        };
        let fallback = if ast.fragments[el.fragment].nodes.is_empty() {
            b::null()
        } else {
            let block = self.visit_fragment(el.fragment, st);
            b::arrow(vec![b::id("$$anchor")], block)
        };
        statements.push(b::stmt(b::call("$.slot", vec![st.node.clone(), b::id("$$props"), name, props_expression, fallback])));
        let async_values = memoizer.async_values(self);
        let blockers = memoizer.blockers(self);
        if async_values.is_some() || blockers.is_some() {
            let mut params = vec![st.node.clone()];
            params.extend(memoizer.async_ids());
            st.init.borrow_mut().push(b::stmt(b::call("$.async", vec![Some(st.node.clone()), blockers, async_values, Some(b::arrow(params, b::block(statements)))])));
        } else {
            let s = if statements.len() == 1 { statements.pop().unwrap() } else { b::block(statements) };
            st.init.borrow_mut().push(s);
        }
    }

    fn svelte_boundary(&mut self, n: NodeId, st: &State) {
        let ast = self.ast();
        let TNode::Element(el) = &ast.nodes[n] else { return };
        let mut props = Vec::new();
        for a in &el.attributes {
            let Attr::Attribute { name, value, .. } = a else { continue };
            let (expr, owner) = match value {
                AttrValue::True => continue,
                AttrValue::Expression(c) => match &**c {
                    crate::ast::Chunk::Expression { expression, .. } => (expression, P::Chunk(c).key()),
                    _ => continue,
                },
                AttrValue::Sequence(chunks) => match chunks.first() {
                    Some(c @ crate::ast::Chunk::Expression { expression, .. }) => (expression, P::Chunk(c).key()),
                    _ => continue,
                },
            };
            let expression = self.visit_expr(expr, st);
            let meta = self.meta_of_key(owner);
            if self.an.metas[meta as usize].has_state {
                props.push(b::get(name, vec![b::r#return(expression)]));
            } else {
                props.push(b::init(name, expression));
            }
        }
        let mut nodes = Vec::new();
        let const_tags = shared();
        let mut hoisted = Vec::new();
        let mut has_const = false;
        let mut has_declaration = false;
        let frag_scope = self.scope_of_key(P::Fragment(el.fragment).key()).unwrap_or(st.scope);
        let is_async = self.options.experimental_async;
        for &child in &ast.fragments[el.fragment].nodes {
            if matches!(ast.nodes[child], TNode::ConstTag { .. }) {
                has_const = true;
                if !is_async {
                    let s = State { consts: const_tags.clone(), scope: frag_scope, ..st.clone() };
                    self.visit_node(child, &s);
                }
            }
            if matches!(ast.nodes[child], TNode::DeclarationTag { .. }) {
                has_declaration = true;
            }
        }
        for &child in &ast.fragments[el.fragment].nodes {
            match &ast.nodes[child] {
                TNode::ConstTag { .. } => {
                    if is_async {
                        nodes.push(child);
                    }
                    continue;
                }
                TNode::SnippetBlock { expression, .. } => {
                    let sname = self.convert_expr(expression);
                    let sname_str = js::ident(&sname).unwrap_or("").to_string();
                    let special = sname_str == "failed" || sname_str == "pending";
                    if is_async && (has_const || has_declaration) && !special {
                        nodes.push(child);
                    } else {
                        let statements = shared();
                        let s = State { snippets: statements.clone(), ..st.clone() };
                        self.visit_node(child, &s);
                        let mut snippet = statements.borrow().first().cloned().unwrap_or_else(b::empty);
                        if !is_async {
                            let consts: Vec<Node> = const_tags.borrow().iter().filter(|c| c.is("VariableDeclaration")).cloned().collect();
                            insert_into_snippet_body(&mut snippet, consts, self.dev);
                        }
                        if special {
                            props.push(b::prop("init", sname.clone(), sname));
                        }
                        hoisted.push(snippet);
                    }
                    continue;
                }
                _ => nodes.push(child),
            }
        }
        let s = State { scope: frag_scope, ..st.clone() };
        let block = self.visit_virtual_fragment(el.fragment, &nodes, &s);
        let mut block_body = match block.kind {
            NodeKind::BlockStatement(bl) => bl.body,
            _ => vec![],
        };
        if !is_async {
            let consts = const_tags.borrow().clone();
            for (i, c) in consts.into_iter().enumerate() {
                block_body.insert(i, c);
            }
        }
        let boundary = b::stmt(b::call("$.boundary", vec![st.node.clone(), b::object(props), b::arrow(vec![b::id("$$anchor")], b::block(block_body))]));
        st.template.borrow_mut().push_comment(None);
        if hoisted.is_empty() {
            st.init.borrow_mut().push(boundary);
        } else {
            hoisted.push(boundary);
            st.init.borrow_mut().push(b::block(hoisted));
        }
    }

    fn svelte_element(&mut self, n: NodeId, st: &State) {
        let ast = self.ast();
        let TNode::Element(el) = &ast.nodes[n] else { return };
        st.template.borrow_mut().push_comment(None);
        let mut attributes: Vec<&'s Attr<'s>> = Vec::new();
        let mut dynamic_namespace: Option<&'s AttrValue<'s>> = None;
        let mut class_directives = Vec::new();
        let mut style_directives = Vec::new();
        let mut statements: Vec<Node> = Vec::new();
        let element_id = b::id(self.generate(st.scope, "$$element"));
        let inner = State {
            node: element_id.clone(),
            init: shared(),
            update: shared(),
            after_update: shared(),
            memoizer: Rc::new(RefCell::new(Memoizer::default())),
            ..st.clone()
        };
        for a in self.element_attributes(n) {
            match a {
                Attr::Attribute { name, value, .. } => {
                    if *name == "xmlns" && !is_text_attribute(value) {
                        dynamic_namespace = Some(value);
                    }
                    attributes.push(a);
                }
                Attr::Spread { .. } => attributes.push(a),
                Attr::Directive { kind: "ClassDirective", .. } => class_directives.push(a),
                Attr::StyleDirective { .. } => style_directives.push(a),
                Attr::Directive { kind: "LetDirective", .. } => {
                    if let Some(e) = self.visit_attr(a, st) {
                        statements.push(e);
                    }
                }
                Attr::Directive { kind: "OnDirective", .. } => {
                    if let Some(h) = self.visit_attr(a, &inner) {
                        inner.after_update.borrow_mut().push(b::stmt(h));
                    }
                }
                _ => {
                    self.visit_attr(a, &inner);
                }
            }
        }
        let single_class = attributes.len() == 1
            && matches!(attributes[0], Attr::Attribute { name, value, .. } if name.to_lowercase() == "class" && is_text_attribute(value));
        if single_class {
            self.build_set_class(n, &element_id, attributes[0], &class_directives, &inner, false);
        } else if !attributes.is_empty() {
            self.build_attribute_effect(&attributes, &class_directives, &style_directives, &inner, n, &element_id, false);
        }

        let meta = self.meta_of_node(n);
        let has_await = self.an.metas[meta as usize].has_await;
        let has_blockers = self.meta_has_blockers(meta);
        let expression = match &el.tag {
            Some(t) => self.visit_expr(t, st),
            None => b::void0(),
        };
        let get_tag = b::thunk(if has_await { b::call("$.get", vec![b::id("$$tag")]) } else { expression.clone() });

        let mut inner_stmts = inner.init.borrow().clone();
        if !inner.update.borrow().is_empty() {
            let s = self.build_render_statement(&inner);
            inner_stmts.push(s);
        }
        inner_stmts.extend(inner.after_update.borrow().iter().cloned());
        let ns = self.determine_namespace_for_children(n);
        let block = self.visit_fragment(el.fragment, &State { namespace: ns, ..st.clone() });
        if let NodeKind::BlockStatement(bl) = block.kind {
            inner_stmts.extend(bl.body);
        }

        if self.dev {
            statements.push(b::stmt(b::call("$.validate_dynamic_element_tag", vec![get_tag.clone()])));
            if !ast.fragments[el.fragment].nodes.is_empty() {
                statements.push(b::stmt(b::call("$.validate_void_dynamic_element", vec![get_tag.clone()])));
            }
        }
        let location = if self.dev { Some(self.locate(el.start)) } else { None };
        let m = self.an.node_meta.get(&n).cloned().unwrap_or_default();
        let dyn_ns = dynamic_namespace.map(|v| {
            let mut local = None;
            let (value, _) = self.build_attribute_value(v, st, Memoize::None, &mut local);
            b::thunk(value)
        });
        statements.push(b::stmt(b::call(
            "$.element",
            vec![
                Some(st.node.clone()),
                Some(get_tag),
                Some(if m.svg || m.mathml { b::r#true() } else { b::r#false() }),
                if inner_stmts.is_empty() { None } else { Some(b::arrow(vec![element_id, b::id("$$anchor")], b::block(inner_stmts))) },
                dyn_ns,
                location.map(|(l, c)| b::array(vec![b::literal(l as f64), b::literal(c as f64)])),
            ],
        )));
        if has_await || has_blockers {
            let blockers = self.meta_blockers_array(meta);
            let thunk = if has_await { b::array(vec![self.async_thunk(expression, meta)]) } else { b::void0() };
            let params = if has_await { vec![st.node.clone(), b::id("$$tag")] } else { vec![st.node.clone()] };
            st.init.borrow_mut().push(b::stmt(b::call("$.async", vec![st.node.clone(), blockers, thunk, b::arrow(params, b::block(statements))])));
        } else {
            let s = if statements.len() == 1 { statements.pop().unwrap() } else { b::block(statements) };
            st.init.borrow_mut().push(s);
        }
    }

    fn title_element(&mut self, n: NodeId, st: &State) {
        let ast = self.ast();
        let TNode::Element(el) = &ast.nodes[n] else { return };
        let mut memoizer = Some(Memoizer::default());
        let mut values = Vec::new();
        for &c in &ast.fragments[el.fragment].nodes {
            match &ast.nodes[c] {
                TNode::Text { data, .. } => values.push(ChunkRef::Text(data)),
                TNode::ExpressionTag { expression, .. } => values.push(ChunkRef::Expr(expression, P::Node(c).key())),
                _ => {}
            }
        }
        let (value, has_state) = self.build_template_chunk(&values, &[], st, Memoize::Local, &mut memoizer);
        let evaluated = self.evaluate(&value, st.scope);
        let mut title = b::id("title");
        title.loc = Some(self.conv.location(oxc_span::Span::new(el.name_loc.start as u32, el.name_loc.end as u32)));
        let rhs = if evaluated.is_known {
            match &evaluated.value {
                Some(v) => evaluated_literal(v),
                None => b::void0(),
            }
        } else if evaluated.is_defined {
            value
        } else {
            b::logical("??", value, b::literal(""))
        };
        let statement = b::stmt(b::assignment("=", b::member(b::id("$.document"), title), rhs));
        let memoizer = memoizer.unwrap();
        if has_state {
            let ids = memoizer.apply(self);
            st.after_update.borrow_mut().push(b::stmt(b::call(
                "$.deferred_template_effect",
                vec![Some(b::arrow(ids, b::block(vec![statement]))), memoizer.sync_values(), memoizer.async_values(self), memoizer.blockers(self)],
            )));
        } else {
            st.after_update.borrow_mut().push(b::stmt(b::call("$.effect", vec![b::thunk(b::block(vec![statement]))])));
        }
    }
}

/// `b.literal(evaluated.value)`
fn evaluated_literal(v: &crate::analyze::evaluate::Val) -> Node {
    use crate::analyze::evaluate::Val;
    match v {
        Val::Str(s) => b::literal(s.as_str()),
        Val::Num(n) => b::literal(*n),
        Val::Bool(x) => b::literal(*x),
        Val::Null => b::null(),
        Val::Undefined => b::id("undefined"),
        other => b::literal(other.to_js_string().unwrap_or_default().as_str()),
    }
}

/// `snippet_fn.body.body.unshift(...consts)` for the snippet declaration `const x = (...) => {...}`
fn insert_into_snippet_body(snippet: &mut Node, consts: Vec<Node>, dev: bool) {
    let NodeKind::VariableDeclaration(v) = &mut snippet.kind else { return };
    let Some(d) = v.declarations.first_mut() else { return };
    let NodeKind::VariableDeclarator(d) = &mut d.kind else { return };
    let Some(init) = d.init.as_deref_mut() else { return };
    let f = if dev {
        match &mut init.kind {
            NodeKind::CallExpression(c) => match c.arguments.get_mut(1) {
                Some(f) => f,
                None => return,
            },
            _ => return,
        }
    } else {
        init
    };
    let body = match &mut f.kind {
        NodeKind::ArrowFunctionExpression(a) => &mut a.body,
        NodeKind::FunctionExpression(f) => &mut f.body,
        _ => return,
    };
    if let NodeKind::BlockStatement(bl) = &mut body.kind {
        for (i, c) in consts.into_iter().enumerate() {
            bl.body.insert(i, c);
        }
    }
}
