//! `shared/component.js`: `build_component`

use crate::analyze::nodes::P;
use crate::analyze::scope::Kind;
use crate::ast::{Attr, AttrValue, Chunk, Node as TNode, NodeId};
use crate::estree::builders as b;
use crate::estree::{Node, NodeKind, SourceLocation};

use super::super::js;
use super::utils::Memoize;
use super::{shared, Client, Memoizer, State};

/// An entry of `props_and_spreads`
enum PropOrSpread {
    Props(Vec<Node>),
    Spread(Node),
}

fn push_prop(list: &mut Vec<PropOrSpread>, prop: Node) {
    match list.last_mut() {
        Some(PropOrSpread::Props(p)) => p.push(prop),
        _ => list.push(PropOrSpread::Props(vec![prop])),
    }
}

impl<'a, 's> Client<'a, 's> {
    /// `build_component(node, component_name, loc, context)` (the component is on the path)
    pub fn build_component(&mut self, n: NodeId, component_name: &str, loc: Option<SourceLocation>, st: &State) -> Node {
        let ast = self.ast();
        let TNode::Element(el) = &ast.nodes[n] else { return b::empty() };
        let anchor = st.node.clone();
        let mut props_and_spreads: Vec<PropOrSpread> = Vec::new();
        let mut delayed_props: Vec<Node> = Vec::new();
        let lets = shared();
        let scopes = self.an.sc.component_scopes.get(&n).cloned().unwrap_or_default();
        let scope_for = |name: &str| scopes.iter().find(|(k, _)| *k == name).map(|(_, s)| *s);
        let default_state = State { scope: scope_for("default").unwrap_or(st.scope), transform: super::copy_transform(&st.transform), ..st.clone() };
        let mut children: Vec<(&'s str, Vec<NodeId>)> = Vec::new();
        let mut events: Vec<(String, Vec<Node>)> = Vec::new();
        let mut memoizer = Some(Memoizer::default());
        let mut custom_css_props = Vec::new();
        let mut bind_this: Option<Node> = None;
        let mut binding_initializers = Vec::new();
        let is_dynamic_component = el.kind == "Component" && self.an.node_meta.get(&n).is_some_and(|m| m.dynamic);
        let is_component_dynamic = el.kind == "SvelteComponent" || is_dynamic_component;
        let intermediate_name = if is_dynamic_component { self.generate(st.scope, el.name) } else { "$$component".to_string() };
        let mut slot_scope_applies_to_itself = crate::analyze::scope::determine_slot(ast, n).is_some();
        let mut has_children_prop = false;

        if slot_scope_applies_to_itself {
            for a in &el.attributes {
                if matches!(a, Attr::Directive { kind: "LetDirective", .. }) {
                    let s = State { let_directives: lets.clone(), ..st.clone() };
                    self.visit_attr(a, &s);
                }
            }
        }

        for a in &el.attributes {
            match a {
                Attr::Directive { kind: "LetDirective", .. } => {
                    if !slot_scope_applies_to_itself {
                        let s = State { let_directives: lets.clone(), ..default_state.clone() };
                        self.visit_attr(a, &s);
                    }
                }
                Attr::Directive { kind: "OnDirective", name, modifiers, expression, .. } => {
                    if expression.is_none() {
                        self.needs_props = true;
                    }
                    let meta = self.meta_of_key(P::Attr(a).key());
                    let converted = expression.as_ref().map(|e| self.convert_expr(e));
                    let mut handler = self.build_event_handler(converted.as_ref(), meta, st);
                    if modifiers.contains(&"once") {
                        handler = b::call("$.once", vec![handler]);
                    }
                    match events.iter_mut().find(|(k, _)| k == name) {
                        Some(e) => e.1.push(handler),
                        None => events.push((name.to_string(), vec![handler])),
                    }
                }
                Attr::Spread { .. } => {
                    let expression = self.visit_attr(a, st).unwrap_or_else(b::void0);
                    let meta = self.meta_of_key(P::Attr(a).key());
                    let (memoized, is_memoized) = memoizer.as_mut().unwrap().add(self, expression.clone(), meta, false);
                    let md = &self.an.metas[meta as usize];
                    if is_memoized || md.has_state || md.has_await {
                        props_and_spreads.push(PropOrSpread::Spread(b::thunk(if is_memoized { b::call("$.get", vec![memoized]) } else { expression })));
                    } else {
                        props_and_spreads.push(PropOrSpread::Spread(expression));
                    }
                }
                Attr::Attribute { name, value, .. } => {
                    if name.starts_with("--") {
                        let (v, _) = self.build_attribute_value(value, st, Memoize::CssProp, &mut memoizer);
                        custom_css_props.push(b::init(name, v));
                        continue;
                    }
                    if *name == "slot" {
                        slot_scope_applies_to_itself = true;
                    }
                    if *name == "children" {
                        has_children_prop = true;
                    }
                    let chunks: &[Chunk] = match value {
                        AttrValue::True => &[],
                        AttrValue::Expression(c) => std::slice::from_ref(&**c),
                        AttrValue::Sequence(c) => c,
                    };
                    let has_complex = chunks.iter().any(|c| match c {
                        Chunk::Expression { expression, .. } => {
                            let conv = self.convert_expr(expression);
                            !matches!(conv.kind, NodeKind::Identifier(_) | NodeKind::MemberExpression(_))
                        }
                        _ => false,
                    });
                    let (v, has_state) = self.build_attribute_value(value, st, Memoize::ComponentProp { complex: has_complex }, &mut memoizer);
                    if has_state {
                        push_prop(&mut props_and_spreads, b::get(name, vec![b::r#return(v)]));
                    } else {
                        push_prop(&mut props_and_spreads, b::init(name, v));
                    }
                }
                Attr::Directive { kind: "BindDirective", name, expression: Some(expression), name_loc, .. } => {
                    let raw = self.convert_expr(expression);
                    let local_state = State { memoizer: std::rc::Rc::new(std::cell::RefCell::new(memoizer.take().unwrap())), ..st.clone() };
                    let visited = self.visit_js(&raw, &local_state);
                    memoizer = Some(std::mem::take(&mut *local_state.memoizer.borrow_mut()));
                    let meta = self.meta_of_key(P::Attr(a).key());
                    memoizer.as_mut().unwrap().check_blockers(self, meta);

                    if self.dev && *name != "this" && !self.is_ignored_key(P::Node(n).key(), "ownership_invalid_binding") && !raw.is("SequenceExpression") {
                        if let Some(left) = js::object(&raw).and_then(js::ident) {
                            if let Some(bid) = self.get(st.scope, left) {
                                if matches!(self.binding(bid).kind, Kind::BindableProp | Kind::Prop) {
                                    self.needs_mutation_validation = true;
                                    let bname = self.binding(bid).node.name;
                                    binding_initializers.push(b::stmt(b::call(
                                        "$$ownership_validator.binding",
                                        vec![
                                            b::literal(bname),
                                            b::id(if is_component_dynamic { intermediate_name.as_str() } else { component_name }),
                                            b::thunk(visited.clone()),
                                        ],
                                    )));
                                }
                            }
                        }
                    }

                    if let NodeKind::SequenceExpression(s) = &visited.kind {
                        if *name == "this" {
                            bind_this = Some(raw.clone());
                        } else {
                            let get_id = b::id(self.generate(st.scope, "bind_get"));
                            let set_id = b::id(self.generate(st.scope, "bind_set"));
                            st.init.borrow_mut().push(b::var(get_id.clone(), s.expressions[0].clone()));
                            st.init.borrow_mut().push(b::var(set_id.clone(), s.expressions.get(1).cloned().unwrap_or_else(b::void0)));
                            push_prop(&mut props_and_spreads, b::get(name, vec![b::r#return(b::call(get_id, ()))]));
                            push_prop(&mut props_and_spreads, b::set(name, vec![b::stmt(b::call(set_id, vec![b::id("$$value")]))]));
                        }
                    } else {
                        if self.dev && visited.is("MemberExpression") && self.an.runes && !self.is_ignored_key(P::Node(n).key(), "binding_property_non_reactive") {
                            self.validate_binding(st, a, expression, &visited);
                        }
                        if *name == "this" {
                            bind_this = Some(raw.clone());
                        } else {
                            let is_store_sub = js::ident(&raw).and_then(|i| self.get(st.scope, i)).is_some_and(|b| self.binding(b).kind == Kind::StoreSub);
                            let mut get = if is_store_sub {
                                b::get(name, vec![b::stmt(b::call("$.mark_store_binding", ())), b::r#return(visited.clone())])
                            } else {
                                b::get(name, vec![b::r#return(visited.clone())])
                            };
                            let assignment = b::assignment("=", raw.clone(), b::id("$$value"));
                            let assigned = self.visit_js(&assignment, st);
                            let mut set = b::set(name, vec![b::stmt(assigned)]);
                            let key_loc = Some(self.conv.location(oxc_span::Span::new(name_loc.start as u32, name_loc.end as u32)));
                            for p in [&mut get, &mut set] {
                                if let NodeKind::Property(p) = &mut p.kind {
                                    p.key.loc = key_loc;
                                }
                            }
                            delayed_props.push(get);
                            delayed_props.push(set);
                        }
                    }
                }
                Attr::Attach { expression, .. } => {
                    let conv = self.convert_expr(expression);
                    let evaluated = self.evaluate(&conv, st.scope);
                    let mut e = self.visit_js(&conv, st);
                    let meta = self.meta_of_key(P::Attr(a).key());
                    if self.an.metas[meta as usize].has_state {
                        e = b::arrow(
                            vec![b::id("$$node")],
                            b::call(if evaluated.is_function { e } else { b::logical("||", e, b::id("$.noop")) }, vec![b::id("$$node")]),
                        );
                    }
                    memoizer.as_mut().unwrap().check_blockers(self, meta);
                    push_prop(&mut props_and_spreads, b::prop_with("init", b::call("$.attachment", ()), e, true));
                }
                _ => {}
            }
        }

        for p in delayed_props {
            push_prop(&mut props_and_spreads, p);
        }
        if slot_scope_applies_to_itself {
            st.init.borrow_mut().extend(lets.borrow().iter().cloned());
        }
        if !events.is_empty() {
            let events_expression = b::object(
                events.into_iter().map(|(k, v)| b::init(&k, if v.len() > 1 { b::array(v) } else { v.into_iter().next().unwrap() })).collect(),
            );
            push_prop(&mut props_and_spreads, b::init("$$events", events_expression));
        }

        let snippet_declarations = shared();
        let mut serialized_slots = Vec::new();
        for &child in &ast.fragments[el.fragment].nodes {
            if let TNode::SnippetBlock { expression, .. } = &ast.nodes[child] {
                let s = State { snippets: snippet_declarations.clone(), ..st.clone() };
                self.visit_node(child, &s);
                let id = self.convert_expr(expression);
                push_prop(&mut props_and_spreads, b::prop("init", id.clone(), id.clone()));
                let sname = js::ident(&id).unwrap_or("").to_string();
                serialized_slots.push(b::init(if sname == "children" { "default" } else { sname.as_str() }, b::r#true()));
                continue;
            }
            let slot_name = crate::analyze::scope::determine_slot(ast, child).unwrap_or("default");
            match children.iter_mut().find(|(k, _)| *k == slot_name) {
                Some(e) => e.1.push(child),
                None => children.push((slot_name, vec![child])),
            }
        }

        for (slot_name, nodes) in &children {
            let state = if *slot_name == "default" {
                if slot_scope_applies_to_itself { st.clone() } else { default_state.clone() }
            } else {
                State { scope: scope_for(slot_name).unwrap_or(st.scope), transform: super::copy_transform(&st.transform), ..st.clone() }
            };
            let block = self.visit_virtual_fragment(el.fragment, nodes, &state);
            let NodeKind::BlockStatement(bl) = block.kind else { continue };
            if bl.body.is_empty() {
                continue;
            }
            let mut body = if *slot_name == "default" && !slot_scope_applies_to_itself { lets.borrow().clone() } else { vec![] };
            body.extend(bl.body);
            let slot_fn = b::arrow(vec![b::id("$$anchor"), b::id("$$slotProps")], b::block(body));
            if *slot_name == "default" && !has_children_prop {
                let no_lets = lets.borrow().is_empty()
                    && nodes.iter().all(|&c| match &ast.nodes[c] {
                        TNode::Element(e) if e.kind == "SvelteFragment" => !e.attributes.iter().any(|x| matches!(x, Attr::Directive { kind: "LetDirective", .. })),
                        _ => true,
                    });
                if no_lets {
                    let v = if self.dev { b::call("$.wrap_snippet", vec![b::id(self.an.name.as_str()), slot_fn]) } else { slot_fn };
                    push_prop(&mut props_and_spreads, b::init("children", v));
                    serialized_slots.push(b::init(slot_name, b::r#true()));
                } else {
                    serialized_slots.push(b::init(slot_name, slot_fn));
                    push_prop(&mut props_and_spreads, b::init("children", b::id("$.invalid_default_snippet")));
                }
            } else {
                serialized_slots.push(b::init(slot_name, slot_fn));
            }
        }

        if !serialized_slots.is_empty() {
            push_prop(&mut props_and_spreads, b::init("$$slots", b::object(serialized_slots)));
        }
        if !self.an.runes && el.attributes.iter().any(|a| matches!(a, Attr::Directive { kind: "BindDirective", .. })) {
            push_prop(&mut props_and_spreads, b::init("$$legacy", b::r#true()));
        }

        let props_expression = if props_and_spreads.is_empty() || (props_and_spreads.len() == 1 && matches!(props_and_spreads[0], PropOrSpread::Props(_))) {
            match props_and_spreads.pop() {
                Some(PropOrSpread::Props(p)) => b::object(p),
                _ => b::object(vec![]),
            }
        } else {
            b::call(
                "$.spread_props",
                props_and_spreads
                    .into_iter()
                    .map(|p| match p {
                        PropOrSpread::Props(p) => b::object(p),
                        PropOrSpread::Spread(s) => s,
                    })
                    .collect::<Vec<_>>(),
            )
        };

        // `fn(node_id)`
        let build_call = |this: &mut Self, node_id: Node| -> Node {
            let mut callee = if is_component_dynamic {
                b::id(intermediate_name.as_str())
            } else {
                let id = b::member_id(component_name);
                this.visit_js(&id, st)
            };
            callee.loc = loc;
            let mut call = b::call(callee, vec![node_id, props_expression.clone()]);
            if let Some(bt) = &bind_this {
                call = this.build_bind_this(bt, call, st);
            }
            call
        };

        if el.kind != "SvelteSelf" {
            let meta = self.meta_of_node(n);
            memoizer.as_mut().unwrap().check_blockers(self, meta);
        }

        let memoizer = memoizer.unwrap();
        let mut statements: Vec<Node> = snippet_declarations.borrow().clone();
        statements.extend(memoizer.deriveds(self.an.runes));

        let dynamic_call = |this: &mut Self, node_id: Node| -> Node {
            let inner = build_call(this, b::id("$$anchor"));
            let tag = if el.kind == "Component" {
                let id = b::member_id(component_name);
                this.visit_js(&id, st)
            } else {
                match &el.expression {
                    Some(e) => this.visit_expr(e, st),
                    None => b::void0(),
                }
            };
            let mut body = binding_initializers.clone();
            body.push(b::stmt(inner));
            b::call("$.component", vec![node_id, b::thunk(tag), b::arrow(vec![b::id("$$anchor"), b::id(intermediate_name.as_str())], b::block(body))])
        };

        if !is_component_dynamic {
            statements.extend(binding_initializers.clone());
        }

        if !custom_css_props.is_empty() {
            if st.namespace == "svg" {
                st.template.borrow_mut().push_element("g", el.start, false);
            } else {
                st.template.borrow_mut().push_element("svelte-css-wrapper", el.start, false);
                st.template.borrow_mut().set_prop("style", Some("display: contents".into()));
            }
            st.template.borrow_mut().push_comment(None);
            st.template.borrow_mut().pop_element();
            statements.push(b::stmt(b::call("$.css_props", vec![anchor.clone(), b::thunk(b::object(custom_css_props))])));
            let target = b::member(anchor.clone(), "lastChild");
            let c = if is_component_dynamic { dynamic_call(self, target) } else { build_call(self, target) };
            statements.push(b::stmt(c));
            statements.push(b::stmt(b::call("$.reset", vec![anchor.clone()])));
        } else {
            st.template.borrow_mut().push_comment(None);
            let c = if is_component_dynamic { dynamic_call(self, anchor.clone()) } else { build_call(self, anchor.clone()) };
            statements.push(self.add_svelte_meta(c, Some(el.start), "component", Some(("componentTag", el.name))));
        }

        memoizer.apply(self);
        let async_values = memoizer.async_values(self);
        let blockers = memoizer.blockers(self);
        if async_values.is_some() || blockers.is_some() {
            let mut params = vec![b::id("$$anchor")];
            params.extend(memoizer.async_ids());
            statements = vec![b::stmt(b::call("$.async", vec![Some(anchor.clone()), blockers, async_values, Some(b::arrow(params, b::block(statements)))]))];
            if st.is_standalone {
                statements.push(b::stmt(b::call("$.next", ())));
            }
        }
        if statements.len() > 1 { b::block(statements) } else { statements.pop().unwrap_or_else(b::empty) }
    }
}
