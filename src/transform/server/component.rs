//! `server/visitors/shared/component.js` (`build_inline_component`) and the `Component` visitor

use crate::analyze::nodes::P;
use crate::ast::{Attr, Node as TNode, NodeId};
use crate::estree::builders as b;
use crate::estree::{Node, NodeKind};

use super::super::js::PathNode;
use super::template::Parent;
use super::utils::{PromiseOptimiser, BLOCK_CLOSE, BLOCK_OPEN, BLOCK_OPEN_ELSE, EMPTY_COMMENT};
use super::{shared, Server, State};

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

impl<'a, 's> Server<'a, 's> {
    /// `context.visit(b.member_id(node.name))`
    pub fn component_expression(&mut self, name: &str, st: &State) -> Node {
        let id = b::member_id(name);
        self.visit_js(&id, st)
    }

    /// `build_inline_component(node, expression, context)`
    pub fn build_inline_component(&mut self, n: NodeId, expression: Node, st: &State) {
        let ast = self.ast();
        let TNode::Element(el) = &ast.nodes[n] else { return };
        let mut props_and_spreads: Vec<PropOrSpread> = Vec::new();
        let mut delayed: Vec<Node> = Vec::new();
        let mut custom_css_props = Vec::new();
        let mut lets: Vec<(&'s str, Vec<&'s Attr<'s>>)> = vec![("default", Vec::new())];
        let scopes = self.an.sc.component_scopes.get(&n).cloned().unwrap_or_default();
        let scope_for = |name: &str| scopes.iter().find(|(k, _)| *k == name).map(|(_, s)| *s);
        let child_state = State { scope: scope_for("default").unwrap_or(st.scope), ..st.clone() };
        let slot_scope_applies_to_itself = el.attributes.iter().any(|a| matches!(a, Attr::Attribute { name: "slot", .. }));
        let mut has_children_prop = false;
        let mut opt = PromiseOptimiser::default();

        for a in &el.attributes {
            match a {
                Attr::Directive { kind: "LetDirective", .. } => {
                    if !slot_scope_applies_to_itself {
                        lets[0].1.push(a);
                    }
                }
                Attr::Spread { expression, .. } => {
                    self.path.push(PathNode::Tpl(P::Attr(a)));
                    let e = self.visit_expr_here(expression, st);
                    self.path.pop();
                    let meta = self.an.meta_of.get(&P::Attr(a).key()).copied().unwrap_or(0);
                    props_and_spreads.push(PropOrSpread::Spread(opt.transform(self, e, meta)));
                }
                Attr::Attribute { name, value, .. } => {
                    let v = self.build_attribute_value(value, st, &mut opt, false, true, false);
                    if name.starts_with("--") {
                        custom_css_props.push(b::init(name, v));
                        continue;
                    }
                    if *name == "children" {
                        has_children_prop = true;
                    }
                    push_prop(&mut props_and_spreads, b::prop("init", b::key(name), v));
                }
                Attr::Directive { kind: "BindDirective", name, expression: Some(expression), .. } => {
                    let meta = self.an.meta_of.get(&P::Attr(a).key()).copied().unwrap_or(0);
                    opt.check_blockers(self, meta);
                    if *name == "this" {
                        continue;
                    }
                    let converted = self.convert_expr(expression);
                    if let NodeKind::SequenceExpression(_) = &converted.kind {
                        let visited = self.visit_js(&converted, st);
                        let NodeKind::SequenceExpression(seq) = visited.kind else { continue };
                        let mut it = seq.expressions.into_iter();
                        let (get, set) = (it.next().unwrap(), it.next().unwrap());
                        let get_id = b::id(self.an.sc.generate(st.scope, "bind_get").as_str());
                        let set_id = b::id(self.an.sc.generate(st.scope, "bind_set").as_str());
                        st.init.borrow_mut().push(b::var(get_id.clone(), get));
                        st.init.borrow_mut().push(b::var(set_id.clone(), set));
                        push_prop(&mut props_and_spreads, b::get(name, vec![b::r#return(b::call(get_id, ()))]));
                        push_prop(&mut props_and_spreads, b::set(name, vec![b::stmt(b::call(set_id, vec![b::id("$$value")]))]));
                    } else {
                        let getter_value = self.visit_js(&converted, st);
                        delayed.push(b::get(name, vec![b::r#return(getter_value)]));
                        let assignment = b::assignment("=", converted, b::id("$$value"));
                        let visited = self.visit_js(&assignment, st);
                        delayed.push(b::set(name, vec![b::stmt(visited), b::stmt(b::assignment("=", b::id("$$settled"), b::r#false()))]));
                    }
                }
                Attr::Attach { .. } => {
                    let meta = self.an.meta_of.get(&P::Attr(a).key()).copied().unwrap_or(0);
                    opt.check_blockers(self, meta);
                }
                _ => {}
            }
        }
        for prop in delayed {
            push_prop(&mut props_and_spreads, prop);
        }

        let snippet_declarations = shared();
        let mut serialized_slots = Vec::new();
        let mut children: Vec<(&'s str, Vec<NodeId>)> = Vec::new();
        for &child in &ast.fragments[el.fragment].nodes {
            if let TNode::SnippetBlock { expression, .. } = &ast.nodes[child] {
                let snippet_state = State { init: snippet_declarations.clone(), ..st.clone() };
                self.visit_node(child, &snippet_state);
                let id = self.convert_expr(expression);
                push_prop(&mut props_and_spreads, b::prop("init", id.clone(), id.clone()));
                let name = super::super::js::ident(&id).unwrap_or("").to_string();
                serialized_slots.push(b::init(if name == "children" { "default" } else { name.as_str() }, b::r#true()));
                continue;
            }
            let mut slot_name: &'s str = "default";
            if let TNode::Element(child_el) = &ast.nodes[child] {
                if matches!(child_el.kind, "SvelteElement" | "RegularElement" | "SvelteFragment" | "Component" | "SvelteComponent" | "SvelteSelf" | "SlotElement") {
                    let slot = child_el.attributes.iter().find_map(|x| match x {
                        Attr::Attribute { name: "slot", value, .. } => Some(value),
                        _ => None,
                    });
                    if let Some(value) = slot {
                        if let Some(text) = crate::analyze::utils::text_value(value) {
                            slot_name = text;
                        }
                        let let_directives: Vec<&'s Attr<'s>> = child_el.attributes.iter().filter(|x| matches!(x, Attr::Directive { kind: "LetDirective", .. })).collect();
                        match lets.iter_mut().find(|(k, _)| *k == slot_name) {
                            Some(entry) => entry.1 = let_directives,
                            None => lets.push((slot_name, let_directives)),
                        }
                    } else if child_el.kind == "SvelteFragment" {
                        lets[0].1.extend(child_el.attributes.iter().filter(|x| matches!(x, Attr::Directive { kind: "LetDirective", .. })));
                    }
                }
            }
            match children.iter_mut().find(|(k, _)| *k == slot_name) {
                Some(entry) => entry.1.push(child),
                None => children.push((slot_name, vec![child])),
            }
        }

        for (slot_name, nodes) in &children {
            let slot_state = if *slot_name == "default" {
                child_state.clone()
            } else {
                State { scope: scope_for(slot_name).unwrap_or(st.scope), ..st.clone() }
            };
            self.path.push(PathNode::Tpl(P::Node(n)));
            let block = self.fragment_nodes(el.fragment, nodes, Parent::Node(n), &slot_state);
            self.path.pop();
            let NodeKind::BlockStatement(body) = block.kind else { continue };
            if body.body.is_empty() {
                continue;
            }
            let mut params = vec![b::id("$$renderer")];
            let slot_lets = lets.iter().find(|(k, _)| k == slot_name).map(|(_, l)| l.clone()).unwrap_or_default();
            if !slot_lets.is_empty() {
                let props = slot_lets
                    .iter()
                    .filter_map(|d| match d {
                        Attr::Directive { name, expression, .. } => Some(match expression {
                            None => b::init(name, b::id(*name)),
                            Some(e) => {
                                let converted = self.convert_expr(e);
                                let value = match &converted.kind {
                                    NodeKind::ObjectExpression(o) => b::object_pattern(o.properties.clone()),
                                    NodeKind::ArrayExpression(a) => Node::new(NodeKind::ArrayPattern(a.clone())),
                                    _ => converted,
                                };
                                b::init(name, value)
                            }
                        }),
                        _ => None,
                    })
                    .collect();
                params.push(b::object_pattern(props));
            }
            let slot_fn = b::arrow(params, b::block(body.body));
            if *slot_name == "default" && !has_children_prop {
                let svelte_fragment_with_lets = nodes.iter().any(|&c| {
                    matches!(&ast.nodes[c], TNode::Element(e) if e.kind == "SvelteFragment" && e.attributes.iter().any(|x| matches!(x, Attr::Directive { kind: "LetDirective", .. })))
                });
                if lets[0].1.is_empty() && !svelte_fragment_with_lets {
                    let value = if self.dev { b::call("$.prevent_snippet_stringification", vec![slot_fn]) } else { slot_fn };
                    push_prop(&mut props_and_spreads, b::prop("init", b::id("children"), value));
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
            push_prop(&mut props_and_spreads, b::prop("init", b::id("$$slots"), b::object(serialized_slots)));
        }

        let props_expression = if props_and_spreads.is_empty() || (props_and_spreads.len() == 1 && matches!(props_and_spreads[0], PropOrSpread::Props(_))) {
            match props_and_spreads.pop() {
                Some(PropOrSpread::Props(p)) => b::object(p),
                _ => b::object(vec![]),
            }
        } else {
            b::call(
                "$.spread_props",
                vec![b::array(
                    props_and_spreads
                        .into_iter()
                        .map(|p| match p {
                            PropOrSpread::Props(p) => b::object(p),
                            PropOrSpread::Spread(e) => e,
                        })
                        .collect::<Vec<_>>(),
                )],
            )
        };

        let dynamic = el.kind == "SvelteComponent" || (el.kind == "Component" && self.an.node_meta.get(&n).is_some_and(|m| m.dynamic));
        let mut statement = b::stmt(b::call(expression.clone(), vec![b::id("$$renderer"), props_expression]));
        if dynamic {
            statement = b::r#if(
                expression,
                b::block(vec![
                    b::stmt(b::call("$$renderer.push", vec![b::literal(BLOCK_OPEN)])),
                    statement,
                    b::stmt(b::call("$$renderer.push", vec![b::literal(BLOCK_CLOSE)])),
                ]),
                Some(b::block(vec![
                    b::stmt(b::call("$$renderer.push", vec![b::literal(BLOCK_OPEN_ELSE)])),
                    b::stmt(b::call("$$renderer.push", vec![b::literal(BLOCK_CLOSE)])),
                ])),
            );
        }
        let snippet_declarations = std::mem::take(&mut *snippet_declarations.borrow_mut());
        if !snippet_declarations.is_empty() {
            let mut body = snippet_declarations;
            body.push(statement);
            statement = b::block(body);
        }
        let has_css_props = !custom_css_props.is_empty();
        if has_css_props {
            let mut args = vec![
                Some(b::id("$$renderer")),
                Some(b::literal(st.namespace != "svg")),
                Some(b::object(custom_css_props)),
                Some(b::thunk(b::block(vec![statement]))),
            ];
            if dynamic {
                args.push(Some(b::r#true()));
            }
            statement = b::stmt(b::call("$.css_props", args));
        }
        if el.kind != "SvelteSelf" {
            let meta = self.meta_of_node(n);
            opt.check_blockers(self, meta);
        }
        st.template.borrow_mut().extend(opt.render_block(vec![statement]));
        if !dynamic && !opt.is_async() && !st.is_standalone && !has_css_props {
            st.template.borrow_mut().push(b::literal(EMPTY_COMMENT));
        }
    }
}
