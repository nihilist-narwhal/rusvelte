//! The attribute and directive visitors (`Attribute.js`, `SpreadAttribute.js`,
//! `BindDirective.js`, `OnDirective.js`, `UseDirective.js`, `TransitionDirective.js`,
//! `AnimateDirective.js`, `LetDirective.js`, `AttachTag.js`)

use std::rc::Rc;

use crate::analyze::nodes::P;
use crate::ast::{Attr, AttrValue, Node as TNode};
use crate::estree::builders as b;
use crate::estree::{Node, NodeKind};

use super::super::js;
use super::fragment::is_text_attribute;
use super::utils::{parse_directive_name, Memoize};
use super::{Client, State, Transform, TRANSITION_GLOBAL, TRANSITION_IN, TRANSITION_OUT};

/// `binding_properties[name]`: `(event, bidirectional)`
fn binding_event(name: &str) -> (Option<&'static str>, bool) {
    match name {
        "duration" => (Some("durationchange"), false),
        "videoHeight" | "videoWidth" | "devicePixelRatio" => (Some("resize"), false),
        "naturalWidth" | "naturalHeight" => (Some("load"), false),
        "fullscreenElement" => (Some("fullscreenchange"), false),
        "pointerLockElement" => (Some("pointerlockchange"), false),
        "visibilityState" => (Some("visibilitychange"), false),
        "indeterminate" => (Some("change"), true),
        "open" => (Some("toggle"), true),
        _ => (None, false),
    }
}

impl<'a, 's> Client<'a, 's> {
    /// `context.visit(attribute, state)`: the visitor's result (`OnDirective` and
    /// `SpreadAttribute` return expressions)
    pub fn visit_attr(&mut self, a: &'s Attr<'s>, st: &State) -> Option<Node> {
        self.tpl_path.push(P::Attr(a));
        let out = match a {
            Attr::Attribute { .. } => {
                if crate::analyze::utils::is_event_attribute(a) {
                    let parent_type = self.tpl_parent().map(|p| p.ty(self.an.ast)).unwrap_or("Fragment");
                    self.visit_event_attribute(a, st, parent_type);
                }
                None
            }
            Attr::Spread { expression, .. } => Some(self.visit_expr(expression, st)),
            Attr::Attach { .. } => {
                self.attach_tag(a, st);
                None
            }
            Attr::Directive { kind: "OnDirective", .. } => Some(self.on_directive(a, st)),
            Attr::Directive { kind: "BindDirective", .. } => {
                self.bind_directive(a, st);
                None
            }
            Attr::Directive { kind: "UseDirective", .. } => {
                self.use_directive(a, st);
                None
            }
            Attr::Directive { kind: "TransitionDirective", .. } => {
                self.transition_directive(a, st);
                None
            }
            Attr::Directive { kind: "AnimateDirective", .. } => {
                self.animate_directive(a, st);
                None
            }
            Attr::Directive { kind: "LetDirective", .. } => {
                self.let_directive(a, st);
                None
            }
            _ => None,
        };
        self.tpl_path.pop();
        out
    }

    fn attr_meta_key(&self, a: &'s Attr<'s>) -> u32 {
        self.meta_of_key(P::Attr(a).key())
    }

    /// Wrap a directive statement in `$.run_after_blockers(...)` when its expression is async
    fn after_blockers(&self, statement: Node, meta: u32) -> Node {
        if self.meta_is_async(meta) {
            return b::stmt(b::call("$.run_after_blockers", vec![self.meta_blockers_array(meta), b::thunk(b::block(vec![statement]))]));
        }
        statement
    }

    fn attach_tag(&mut self, a: &'s Attr<'s>, st: &State) {
        let Attr::Attach { expression, .. } = a else { return };
        let meta = self.attr_meta_key(a);
        let expression = self.build_expression(expression, meta, st);
        let statement = b::stmt(b::call("$.attach", vec![st.node.clone(), b::thunk(expression)]));
        let statement = self.after_blockers(statement, meta);
        st.init.borrow_mut().push(statement);
    }

    fn on_directive(&mut self, a: &'s Attr<'s>, st: &State) -> Node {
        let Attr::Directive { name, modifiers, expression, .. } = a else { unreachable!() };
        if expression.is_none() {
            self.needs_props = true;
        }
        let meta = self.attr_meta_key(a);
        let converted = expression.as_ref().map(|e| self.convert_expr(e));
        let mut handler = self.build_event_handler(converted.as_ref(), meta, st);
        for modifier in ["stopPropagation", "stopImmediatePropagation", "preventDefault", "self", "trusted", "once"] {
            if modifiers.contains(&modifier) {
                handler = b::call(format!("$.{modifier}").as_str(), vec![handler]);
            }
        }
        let capture = modifiers.contains(&"capture");
        let passive = if modifiers.contains(&"passive") {
            Some(true)
        } else if modifiers.contains(&"nonpassive") {
            Some(false)
        } else {
            None
        };
        self.build_event(name, handler, capture, passive, false, st)
    }

    fn use_directive(&mut self, a: &'s Attr<'s>, st: &State) {
        let Attr::Directive { name, expression, .. } = a else { return };
        let mut params = vec![b::id("$$node")];
        if expression.is_some() {
            params.push(b::id("$$action_arg"));
        }
        let action = self.visit_js(&parse_directive_name(name), st);
        let mut args = vec![st.node.clone(), b::arrow(params.clone(), b::maybe_call(action, params))];
        if let Some(e) = expression {
            let v = self.visit_expr(e, st);
            args.push(b::thunk(v));
        }
        let statement = b::stmt(b::call("$.action", args));
        let meta = self.attr_meta_key(a);
        let statement = self.after_blockers(statement, meta);
        st.init.borrow_mut().push(statement);
    }

    fn transition_directive(&mut self, a: &'s Attr<'s>, st: &State) {
        let Attr::Directive { name, expression, modifiers, intro_outro, .. } = a else { return };
        let mut flags = if modifiers.contains(&"global") { TRANSITION_GLOBAL } else { 0 };
        let (intro, outro) = intro_outro.unwrap_or((false, false));
        if intro {
            flags |= TRANSITION_IN;
        }
        if outro {
            flags |= TRANSITION_OUT;
        }
        let f = self.visit_js(&parse_directive_name(name), st);
        let mut args = vec![b::literal(flags as f64), st.node.clone(), b::thunk(f)];
        if let Some(e) = expression {
            let v = self.visit_expr(e, st);
            args.push(b::thunk(v));
        }
        let statement = b::stmt(b::call("$.transition", args));
        let meta = self.attr_meta_key(a);
        let statement = self.after_blockers(statement, meta);
        st.after_update.borrow_mut().push(statement);
    }

    fn animate_directive(&mut self, a: &'s Attr<'s>, st: &State) {
        let Attr::Directive { name, expression, .. } = a else { return };
        let expression = match expression {
            Some(e) => {
                let v = self.visit_expr(e, st);
                b::thunk(v)
            }
            None => b::null(),
        };
        let f = self.visit_js(&parse_directive_name(name), st);
        let statement = b::stmt(b::call("$.animation", vec![st.node.clone(), b::thunk(f), expression]));
        let meta = self.attr_meta_key(a);
        let statement = self.after_blockers(statement, meta);
        st.after_update.borrow_mut().push(statement);
    }

    fn let_directive(&mut self, a: &'s Attr<'s>, st: &State) {
        let Attr::Directive { name, expression, .. } = a else { return };
        let converted = expression.as_ref().map(|e| self.convert_expr(e));
        match &converted {
            Some(e) if !e.is("Identifier") => {
                let name_id = self.generate(st.scope, name);
                let mut ids: Vec<Node> = Vec::new();
                collect_let_ids(e, &mut ids);
                for id in &ids {
                    let n = name_id.clone();
                    st.transform.borrow_mut().insert(
                        js::ident(id).unwrap().to_string(),
                        Transform::read(Rc::new(move |_, node| b::member(b::call("$.get", vec![b::id(n.as_str())]), node.clone()))),
                    );
                }
                let pattern = match &e.kind {
                    NodeKind::ObjectExpression(o) => b::object_pattern(o.properties.clone()),
                    NodeKind::ArrayExpression(arr) => b::array_pattern(arr.elements.clone()),
                    _ => e.clone(),
                };
                let props: Vec<Node> = ids.iter().map(|i| b::init(js::ident(i).unwrap(), i.clone())).collect();
                st.let_directives.borrow_mut().push(b::r#const(
                    b::id(name_id.as_str()),
                    b::call(
                        "$.derived",
                        vec![b::thunk(b::block(vec![b::r#let(pattern, b::member(b::id("$$slotProps"), *name)), b::r#return(b::object(props))]))],
                    ),
                ));
            }
            _ => {
                let id_name = match &converted {
                    Some(e) => js::ident(e).unwrap().to_string(),
                    None => name.to_string(),
                };
                st.transform.borrow_mut().insert(id_name.clone(), Transform::read(Rc::new(|_, node| b::call("$.get", vec![node.clone()]))));
                let d = self.create_derived(b::member(b::id("$$slotProps"), *name), None);
                st.let_directives.borrow_mut().push(b::r#const(b::id(id_name.as_str()), d));
            }
        }
    }

    fn bind_directive(&mut self, a: &'s Attr<'s>, st: &State) {
        let Attr::Directive { name, expression: Some(binding_expr), name_loc, .. } = a else { return };
        let raw = self.convert_expr(binding_expr);
        let expression = self.visit_js(&raw, st);
        let (event, bidirectional) = binding_event(name);
        let len = self.tpl_path.len();
        let parent = if len >= 2 { self.tpl_path[len - 2] } else { self.tpl_path[0] };
        let parent_node = match parent {
            P::Node(p) => Some(p),
            _ => None,
        };
        let parent_el = parent_node.and_then(|p| match &self.ast().nodes[p] {
            TNode::Element(el) => Some(el),
            _ => None,
        });

        let (get, set): (Node, Option<Node>);
        if let NodeKind::SequenceExpression(s) = &expression.kind {
            get = s.expressions[0].clone();
            set = s.expressions.get(1).cloned();
        } else {
            if self.dev
                && self.an.runes
                && expression.is("MemberExpression")
                && (*name != "this"
                    || self.tpl_path.iter().any(|p| matches!(p.ty(self.an.ast), "IfBlock" | "EachBlock" | "AwaitBlock" | "KeyBlock")))
                && !self.is_ignored_key(P::Attr(a).key(), "binding_property_non_reactive")
            {
                self.validate_binding(st, a, binding_expr, &expression);
            }
            let mut raw_assignment = b::assignment("=", raw.clone(), b::id("$$value"));
            // the assignment inherits the binding's ignores
            raw_assignment.origin = Some(P::Attr(a).key());
            let assignment = self.visit_js(&raw_assignment, st);
            if self.dev {
                let mut get_id = b::id("get");
                get_id.loc = Some(self.conv.svelte_location(oxc_span::Span::new(name_loc.start as u32, name_loc.end as u32)));
                let mut set_id = b::id("set");
                set_id.loc = get_id.loc;
                get = b::r#function(get_id, vec![], b::block(vec![b::r#return(expression.clone())]));
                set = Some(b::r#function(set_id, vec![b::id("$$value")], b::block(vec![b::stmt(assignment)])));
            } else {
                get = b::thunk(expression.clone());
                let s = b::unthunk(b::arrow(vec![b::id("$$value")], assignment));
                // `get === set`: both are the binding's own identifier
                let same = js::ident(&get).is_some()
                    && js::ident(&get) == js::ident(&s)
                    && get.origin.is_some()
                    && get.origin == s.origin
                    && get.origin == raw.origin;
                set = if same { None } else { Some(s) };
            }
        }

        let node = st.node.clone();
        let set_or_get = || set.clone().unwrap_or_else(|| get.clone());
        let call = if let Some(event) = event {
            b::call(
                "$.bind_property",
                vec![Some(b::literal(*name)), Some(b::literal(event)), Some(node.clone()), Some(set_or_get()), if bidirectional { Some(get.clone()) } else { None }],
            )
        } else {
            let gs = |callee: &str| b::call(callee, vec![Some(node.clone()), Some(get.clone()), set.clone()]);
            match *name {
                "online" => b::call("$.bind_online", vec![set_or_get()]),
                "scrollX" | "scrollY" => b::call(
                    "$.bind_window_scroll",
                    vec![Some(b::literal(if *name == "scrollX" { "x" } else { "y" })), Some(get.clone()), set.clone()],
                ),
                "innerWidth" | "innerHeight" | "outerWidth" | "outerHeight" => b::call("$.bind_window_size", vec![b::literal(*name), set_or_get()]),
                "activeElement" => b::call("$.bind_active_element", vec![set_or_get()]),
                "muted" => gs("$.bind_muted"),
                "paused" => gs("$.bind_paused"),
                "volume" => gs("$.bind_volume"),
                "playbackRate" => gs("$.bind_playback_rate"),
                "currentTime" => gs("$.bind_current_time"),
                "buffered" => b::call("$.bind_buffered", vec![node.clone(), set_or_get()]),
                "played" => b::call("$.bind_played", vec![node.clone(), set_or_get()]),
                "seekable" => b::call("$.bind_seekable", vec![node.clone(), set_or_get()]),
                "seeking" => b::call("$.bind_seeking", vec![node.clone(), set_or_get()]),
                "ended" => b::call("$.bind_ended", vec![node.clone(), set_or_get()]),
                "readyState" => b::call("$.bind_ready_state", vec![node.clone(), set_or_get()]),
                "contentRect" | "contentBoxSize" | "borderBoxSize" | "devicePixelContentBoxSize" => {
                    b::call("$.bind_resize_observer", vec![node.clone(), b::literal(*name), set_or_get()])
                }
                "clientWidth" | "clientHeight" | "offsetWidth" | "offsetHeight" => b::call("$.bind_element_size", vec![node.clone(), b::literal(*name), set_or_get()]),
                "value" => {
                    if parent_el.is_some_and(|el| el.kind == "RegularElement" && el.name == "select") {
                        gs("$.bind_select_value")
                    } else {
                        gs("$.bind_value")
                    }
                }
                "files" => gs("$.bind_files"),
                "this" => self.build_bind_this(&raw, node.clone(), st),
                "textContent" | "innerHTML" | "innerText" => {
                    b::call("$.bind_content_editable", vec![Some(b::literal(*name)), Some(node.clone()), Some(get.clone()), set.clone()])
                }
                "checked" => gs("$.bind_checked"),
                "focused" => b::call("$.bind_focused", vec![node.clone(), set_or_get()]),
                "group" => {
                    let bm = self.an.bind_meta.get(&P::Attr(a).key()).cloned().unwrap_or_default();
                    let ast = self.ast();
                    let indexes: Vec<Node> = bm
                        .parent_each_blocks
                        .iter()
                        .map(|&e| {
                            let keyed = self.an.node_meta.get(&e).is_some_and(|m| m.keyed);
                            let has_index = matches!(&ast.nodes[e], TNode::EachBlock { index: Some(_), .. });
                            let idx = b::id(self.an.sc.each_index.get(&e).cloned().unwrap_or_default().as_str());
                            if keyed && has_index { b::call("$.get", vec![idx]) } else { idx }
                        })
                        .collect();
                    let mut group_getter = get.clone();
                    if let Some(el) = parent_el.filter(|el| el.kind == "RegularElement") {
                        let value = el.attributes.iter().find_map(|x| match x {
                            Attr::Attribute { name: "value", value, .. } if !is_text_attribute(value) && !matches!(value, AttrValue::True) => Some(value),
                            _ => None,
                        });
                        if let Some(value) = value {
                            let mut none = None;
                            // `build_attribute_value(value, context)` with the BindDirective's context
                            let (v, _) = self.build_attribute_value(value, st, Memoize::None, &mut none);
                            group_getter = b::thunk(b::block(vec![b::stmt(v), b::r#return(expression.clone())]));
                        }
                    }
                    let group_name = bm.binding_group_name.clone().unwrap_or_default();
                    b::call("$.bind_group", vec![b::id(group_name.as_str()), b::array(indexes), node.clone(), group_getter, set_or_get()])
                }
                _ => b::call("$.noop", ()),
            }
        };

        let defer = *name != "this"
            && parent_el.is_some_and(|el| el.kind == "RegularElement" && el.attributes.iter().any(|x| matches!(x, Attr::Directive { kind: "UseDirective", .. })));
        let mut statement = if defer { b::stmt(b::call("$.effect", vec![b::thunk(call)])) } else { b::stmt(call) };
        let meta = self.attr_meta_key(a);
        statement = self.after_blockers(statement, meta);
        if *name == "this" || defer {
            st.init.borrow_mut().push(statement);
        } else {
            st.after_update.borrow_mut().push(statement);
        }
    }
}

/// The identifiers a `let:x={{ y, z }}` destructures (`scope.get_bindings(node)` order)
fn collect_let_ids(e: &Node, out: &mut Vec<Node>) {
    // `extract_identifiers_from_destructuring`, which (unlike the scope's own pattern walk)
    // skips defaults (`AssignmentExpression`) and array rest elements (`SpreadElement`)
    match &e.kind {
        NodeKind::Identifier(_) => out.push(e.clone()),
        NodeKind::ObjectExpression(o) => {
            for p in &o.properties {
                match &p.kind {
                    NodeKind::Property(p) => collect_let_ids(&p.value, out),
                    NodeKind::SpreadElement(s) | NodeKind::RestElement(s) => collect_let_ids(&s.argument, out),
                    _ => {}
                }
            }
        }
        NodeKind::ArrayExpression(a) => {
            for el in a.elements.iter().flatten() {
                collect_let_ids(el, out);
            }
        }
        _ => {}
    }
}
