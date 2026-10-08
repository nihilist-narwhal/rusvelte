//! `RegularElement.js`, `shared/element.js` and `shared/events.js`

use std::cell::RefCell;
use std::rc::Rc;

use crate::analyze::nodes::P;
use super::super::js::PathNode;
use crate::ast::{Attr, AttrValue, Chunk, Node as TNode, NodeId};
use crate::estree::builders as b;
use crate::estree::{LiteralValue, Node, NodeKind};

use super::fragment::{is_text_attribute, Initial};
use super::template::{escape_html, Template};
use super::utils::{ChunkRef, Memoize};
use super::{shared, Client, Memoizer, State, TEMPLATE_FRAGMENT};

impl<'a, 's> Client<'a, 's> {
    /// `build_attribute_value(value, context, memoize)`
    pub fn build_attribute_value(&mut self, value: &'s AttrValue<'s>, st: &State, how: Memoize, local: &mut Option<Memoizer>) -> (Node, bool) {
        if let Some(n) = self.textarea_value_owner(value) {
            return self.build_textarea_value(n, st, how, local);
        }
        let chunks: &'s [Chunk<'s>] = match value {
            AttrValue::True => return (b::r#true(), false),
            AttrValue::Expression(c) => std::slice::from_ref(&**c),
            AttrValue::Sequence(chunks) => chunks,
        };
        if chunks.len() == 1 {
            match &chunks[0] {
                Chunk::Text { data, .. } => return (b::literal(&**data), false),
                Chunk::Expression { expression, .. } => {
                    let meta = self.meta_of_key(P::Chunk(&chunks[0]).key());
                    let e = self.build_expression(expression, meta, st);
                    let has_state = self.an.metas[meta as usize].has_state || self.meta_is_async(meta);
                    let v = self.memoize(how, e, meta, st, local);
                    return (v, has_state);
                }
            }
        }
        let values: Vec<ChunkRef<'s>> = chunks
            .iter()
            .map(|c| match c {
                Chunk::Text { data, .. } => ChunkRef::Text(data),
                Chunk::Expression { expression, .. } => ChunkRef::Expr(expression, P::Chunk(c).key()),
            })
            .collect();
        self.build_template_chunk(&values, &[], st, how, local)
    }

    /// The `<textarea>` whose children make up this (synthetic) `value` attribute
    fn textarea_value_owner(&self, value: &AttrValue) -> Option<NodeId> {
        let is_synthetic = super::fragment::TEXTAREA_VALUE.with(|a| match a {
            Attr::Attribute { value: v, .. } => std::ptr::eq(v as *const AttrValue, value as *const AttrValue),
            _ => false,
        });
        if !is_synthetic {
            return None;
        }
        self.path.iter().rev().find_map(|p| match p {
            PathNode::Tpl(P::Node(n)) if self.an.textarea_values.contains(n) => Some(*n),
            _ => None,
        })
    }

    /// `build_attribute_value` of the `value` made of a `<textarea>`'s children
    fn build_textarea_value(&mut self, n: NodeId, st: &State, how: Memoize, local: &mut Option<Memoizer>) -> (Node, bool) {
        let ast = self.ast();
        let TNode::Element(el) = &ast.nodes[n] else { return (b::r#true(), false) };
        let nodes = &ast.fragments[el.fragment].nodes;
        let mut texts = Vec::new();
        let mut values = Vec::new();
        for (i, &c) in nodes.iter().enumerate() {
            match &ast.nodes[c] {
                TNode::Text { data, .. } => {
                    let d = if i == 0 { data.strip_prefix("\r\n").or_else(|| data.strip_prefix('\n')).unwrap_or(data) } else { data };
                    texts.push(d.to_string());
                    values.push(ChunkRef::OwnedText(texts.len() - 1));
                }
                TNode::ExpressionTag { expression, .. } => values.push(ChunkRef::Expr(expression, P::Node(c).key())),
                _ => {}
            }
        }
        if values.len() == 1 {
            if let ChunkRef::Expr(expression, key) = values[0] {
                let meta = self.meta_of_key(key);
                let e = self.build_expression(expression, meta, st);
                let has_state = self.an.metas[meta as usize].has_state || self.meta_is_async(meta);
                let v = self.memoize(how, e, meta, st, local);
                return (v, has_state);
            }
        }
        self.build_template_chunk(&values, &texts, st, how, local)
    }

    /// `get_attribute_name(element, attribute)`
    pub fn get_attribute_name(&self, n: NodeId, name: &str) -> String {
        let m = self.an.node_meta.get(&n).cloned().unwrap_or_default();
        if !m.svg && !m.mathml {
            return crate::analyze::utils::normalize_attribute(name);
        }
        name.to_string()
    }

    /// `build_class_directives_object(class_directives, context, memoizer)`
    pub fn build_class_directives_object(&mut self, directives: &[&'s Attr<'s>], st: &State, local: Option<&mut Memoizer>) -> Node {
        let mut properties = Vec::new();
        let mut local = local;
        for &d in directives {
            let Attr::Directive { name, expression, .. } = d else { continue };
            let expression = match expression {
                Some(e) => self.visit_expr(e, st),
                None => {
                    let id = b::id(*name);
                    self.visit_js(&id, st)
                }
            };
            let meta = self.meta_of_key(P::Attr(d).key());
            let v = match local.as_deref_mut() {
                Some(m) => m.add(self, expression, meta, false).0,
                None => {
                    let mut m = std::mem::take(&mut *st.memoizer.borrow_mut());
                    let v = m.add(self, expression, meta, false).0;
                    *st.memoizer.borrow_mut() = m;
                    v
                }
            };
            properties.push(b::init(name, v));
        }
        b::object(properties)
    }

    /// `build_style_directives_object(style_directives, context, memoizer)`
    pub fn build_style_directives_object(&mut self, directives: &[&'s Attr<'s>], st: &State, local: Option<&mut Memoizer>) -> Node {
        let mut normal = Vec::new();
        let mut important = Vec::new();
        let mut local = local;
        for &d in directives {
            let Attr::StyleDirective { name, value, modifiers, .. } = d else { continue };
            let expression = match value {
                AttrValue::True => self.build_getter(&b::id(*name), st),
                v => {
                    let mut none = None;
                    self.build_attribute_value(v, st, Memoize::None, &mut none).0
                }
            };
            let meta = self.meta_of_key(P::Attr(d).key());
            let v = match local.as_deref_mut() {
                Some(m) => m.add(self, expression, meta, false).0,
                None => {
                    let mut m = std::mem::take(&mut *st.memoizer.borrow_mut());
                    let v = m.add(self, expression, meta, false).0;
                    *st.memoizer.borrow_mut() = m;
                    v
                }
            };
            let prop = b::init(name, v);
            if modifiers.contains(&"important") {
                important.push(prop);
            } else {
                normal.push(prop);
            }
        }
        if !important.is_empty() {
            b::array(vec![b::object(normal), b::object(important)])
        } else {
            b::object(normal)
        }
    }

    /// `build_attribute_effect(attributes, class_directives, style_directives, context, element, element_id, should_remove_defaults)`
    #[allow(clippy::too_many_arguments)]
    pub fn build_attribute_effect(
        &mut self,
        attributes: &[&'s Attr<'s>],
        class_directives: &[&'s Attr<'s>],
        style_directives: &[&'s Attr<'s>],
        st: &State,
        n: NodeId,
        element_id: &Node,
        should_remove_defaults: bool,
    ) {
        let ast = self.ast();
        let TNode::Element(el) = &ast.nodes[n] else { return };
        let mut values = Vec::new();
        let is_select = el.kind == "RegularElement" && el.name == "select";
        let mut memoizer = Some(Memoizer::default());
        for &a in attributes {
            match a {
                Attr::Attribute { name, value, .. } => {
                    let (value, _) = self.build_attribute_value(value, st, Memoize::Local, &mut memoizer);
                    if crate::analyze::utils::is_event_attribute(a) && matches!(value.kind, NodeKind::ArrowFunctionExpression(_) | NodeKind::FunctionExpression(_)) {
                        let id = self.generate(st.scope, "event_handler");
                        st.init.borrow_mut().push(b::var(b::id(id.as_str()), value));
                        values.push(b::init(name, b::id(id.as_str())));
                    } else {
                        let key = if is_select && crate::analyze::utils::normalize_attribute(name) == "defaultValue" { "defaultValue" } else { name };
                        values.push(b::init(key, value));
                    }
                }
                Attr::Spread { .. } => {
                    let value = self.visit_attr(a, st).unwrap_or_else(b::void0);
                    let meta = self.meta_of_key(P::Attr(a).key());
                    let value = memoizer.as_mut().unwrap().add(self, value, meta, false).0;
                    values.push(b::spread(value));
                }
                _ => {}
            }
        }
        if !class_directives.is_empty() {
            let obj = self.build_class_directives_object(class_directives, st, memoizer.as_mut());
            values.push(b::prop("init", b::array(vec![b::id("$.CLASS")]), obj));
        }
        if !style_directives.is_empty() {
            let obj = self.build_style_directives_object(style_directives, st, memoizer.as_mut());
            values.push(b::prop("init", b::array(vec![b::id("$.STYLE")]), obj));
        }
        let memoizer = memoizer.unwrap();
        let ids = memoizer.apply(self);
        let scoped = self.scoped.contains(&n) && !self.css_hash.is_empty();
        let ignored = self.is_ignored_key(P::Node(n).key(), "hydration_attribute_changed");
        st.init.borrow_mut().push(b::stmt(b::call(
            "$.attribute_effect",
            vec![
                Some(element_id.clone()),
                Some(b::arrow(ids, b::object(values))),
                memoizer.sync_values(),
                memoizer.async_values(self),
                memoizer.blockers(self),
                if scoped { Some(b::literal(self.css_hash.as_str())) } else { None },
                if should_remove_defaults { Some(b::r#true()) } else { None },
                if ignored { Some(b::r#true()) } else { None },
            ],
        )));
    }

    /// `build_set_class(element, node_id, attribute, class_directives, context, is_html)`
    pub fn build_set_class(&mut self, n: NodeId, node_id: &Node, attribute: &'s Attr<'s>, class_directives: &[&'s Attr<'s>], st: &State, is_html: bool) {
        let Attr::Attribute { value, .. } = attribute else { return };
        let needs_clsx = self.an.attr_meta.get(&P::Attr(attribute).key()).is_some_and(|m| m.needs_clsx);
        let mut none = None;
        let (mut value, mut has_state) = self.build_attribute_value(value, st, if needs_clsx { Memoize::StateClsx } else { Memoize::State }, &mut none);
        let mut previous_id = None;
        let mut prev = None;
        let mut next = None;
        if !class_directives.is_empty() {
            next = Some(self.build_class_directives_object(class_directives, st, None));
            has_state = has_state
                || class_directives.iter().any(|d| {
                    let m = self.meta_of_key(P::Attr(d).key());
                    self.an.metas[m as usize].has_state || self.meta_is_async(m)
                });
            if has_state {
                let id = b::id(self.generate(st.scope, "classes"));
                st.init.borrow_mut().push(b::declaration("let", vec![b::declarator(id.clone(), None)]));
                prev = Some(id.clone());
                previous_id = Some(id);
            } else {
                prev = Some(b::object(vec![]));
            }
        }
        let mut css_hash = None;
        if self.scoped.contains(&n) && !self.css_hash.is_empty() {
            match &value.kind {
                NodeKind::Literal(l) if matches!(&l.value, LiteralValue::String(s) if s.is_empty()) || matches!(l.value, LiteralValue::Null) => {
                    value = b::literal(self.css_hash.as_str());
                }
                NodeKind::Literal(l) if matches!(l.value, LiteralValue::String(_)) => {
                    let LiteralValue::String(s) = &l.value else { unreachable!() };
                    value = b::literal(format!("{} {}", escape_html(s, true), self.css_hash));
                }
                _ => css_hash = Some(b::literal(self.css_hash.as_str())),
            }
        }
        if css_hash.is_none() && next.is_some() {
            css_hash = Some(b::null());
        }
        let mut set_class = b::call(
            "$.set_class",
            vec![Some(node_id.clone()), Some(if is_html { b::literal(1.0) } else { b::literal(0.0) }), Some(value), css_hash, prev, next],
        );
        if let Some(id) = previous_id {
            set_class = b::assignment("=", id, set_class);
        }
        if has_state { st.update.borrow_mut() } else { st.init.borrow_mut() }.push(b::stmt(set_class));
    }

    /// `build_set_style(node_id, attribute, style_directives, context)`
    pub fn build_set_style(&mut self, node_id: &Node, attribute: &'s Attr<'s>, style_directives: &[&'s Attr<'s>], st: &State) {
        let Attr::Attribute { value, .. } = attribute else { return };
        let mut none = None;
        let (value, mut has_state) = self.build_attribute_value(value, st, Memoize::State, &mut none);
        let mut previous_id = None;
        let mut prev = None;
        let mut next = None;
        if !style_directives.is_empty() {
            next = Some(self.build_style_directives_object(style_directives, st, None));
            has_state = has_state
                || style_directives.iter().any(|d| {
                    let m = self.meta_of_key(P::Attr(d).key());
                    self.an.metas[m as usize].has_state || self.meta_is_async(m)
                });
            if has_state {
                let id = b::id(self.generate(st.scope, "styles"));
                st.init.borrow_mut().push(b::declaration("let", vec![b::declarator(id.clone(), None)]));
                prev = Some(id.clone());
                previous_id = Some(id);
            } else {
                prev = Some(b::object(vec![]));
            }
        }
        let mut set_style = b::call("$.set_style", vec![Some(node_id.clone()), Some(value), prev, next]);
        if let Some(id) = previous_id {
            set_style = b::assignment("=", id, set_style);
        }
        if has_state { st.update.borrow_mut() } else { st.init.borrow_mut() }.push(b::stmt(set_style));
    }

    /// `visit_event_attribute(node, context)`. `parent_type` is `context.path.at(-1).type`.
    pub fn visit_event_attribute(&mut self, a: &'s Attr<'s>, st: &State, parent_type: &str) {
        let Attr::Attribute { name, value, .. } = a else { return };
        let mut event_name = name[2..].to_string();
        let mut capture = false;
        if crate::analyze::utils::is_capture_event(&event_name) {
            event_name.truncate(event_name.len() - 7);
            capture = true;
        }
        let chunk: &'s Chunk<'s> = match value {
            AttrValue::Expression(c) => c,
            AttrValue::Sequence(chunks) => &chunks[0],
            AttrValue::True => return,
        };
        let Chunk::Expression { expression, .. } = chunk else { return };
        let meta = self.meta_of_key(P::Chunk(chunk).key());
        let converted = self.convert_expr(expression);
        let handler = self.build_event_handler(Some(&converted), meta, st);
        let delegated = self.an.attr_meta.get(&P::Attr(a).key()).is_some_and(|m| m.delegated);
        if delegated && !self.events.contains(&event_name) {
            self.events.push(event_name.clone());
        }
        let passive = if crate::analyze::utils::is_passive_event(&event_name) { Some(true) } else { None };
        let statement = b::stmt(self.build_event(&event_name, handler, capture, passive, delegated, st));
        if matches!(parent_type, "SvelteDocument" | "SvelteWindow" | "SvelteBody") {
            st.init.borrow_mut().push(statement);
        } else {
            st.after_update.borrow_mut().push(statement);
        }
    }

    /// `build_event(context, event_name, handler, capture, passive, delegated)`
    pub fn build_event(&mut self, event_name: &str, handler: Node, capture: bool, passive: Option<bool>, delegated: bool, st: &State) -> Node {
        let mut f = handler;
        if self.dev {
            if let NodeKind::ArrowFunctionExpression(a) = &f.kind {
                let name = self.generate(st.scope, event_name);
                let body = if a.body.is("BlockStatement") { (*a.body).clone() } else { b::block(vec![b::r#return((*a.body).clone())]) };
                f = b::function_with(b::id(name.as_str()), a.params.clone(), body, a.is_async);
            }
        }
        b::call(
            if delegated { "$.delegated" } else { "$.event" },
            vec![
                Some(b::literal(event_name)),
                Some(st.node.clone()),
                Some(f),
                if capture { Some(b::r#true()) } else { None },
                passive.map(b::literal),
            ],
        )
    }

    /// `build_event_handler(node, metadata, context)`. `node` is unvisited (`None` bubbles)
    pub fn build_event_handler(&mut self, node: Option<&Node>, meta: u32, st: &State) -> Node {
        let Some(node) = node else {
            return b::r#function(
                None,
                vec![b::id("$$arg")],
                b::block(vec![b::stmt(b::call("$.bubble_event.call", vec![b::this(), b::id("$$props"), b::id("$$arg")]))]),
            );
        };
        let mut handler = self.visit_js(node, st);
        if matches!(handler.kind, NodeKind::ArrowFunctionExpression(_) | NodeKind::FunctionExpression(_)) {
            return handler;
        }
        if let NodeKind::Identifier(i) = &handler.kind {
            let binding = self.get(st.scope, &i.name);
            if binding.is_some_and(|b| self.binding_is_function(b)) {
                return handler;
            }
            let is_import = binding.is_some_and(|b| self.binding(b).declaration_kind == crate::analyze::scope::DeclKind::Import);
            let has_blocker = binding.is_some_and(|b| self.binding(b).blocker.is_some());
            if !self.dev && !is_import && !has_blocker {
                return handler;
            }
        }
        if self.an.metas[meta as usize].has_call {
            let id = b::id(self.generate(st.scope, "event_handler"));
            st.init.borrow_mut().push(b::var(id.clone(), b::call("$.derived", vec![b::thunk(handler)])));
            handler = b::call("$.get", vec![id]);
        }
        let mut call = b::call(b::member_with(handler.clone(), b::id("apply"), false, true), vec![b::this(), b::id("$$args")]);
        if self.dev {
            let (line, column) = self.locate(node.start().unwrap_or(0) as usize);
            let remove_parens = matches!(&node.kind, NodeKind::CallExpression(c) if c.arguments.is_empty() && c.callee.is("Identifier"));
            call = b::call(
                "$.apply",
                vec![
                    Some(b::thunk(handler)),
                    Some(b::this()),
                    Some(b::id("$$args")),
                    Some(b::id(self.an.name.as_str())),
                    Some(b::array(vec![b::literal(line as f64), b::literal(column as f64)])),
                    if has_side_effects(node) { Some(b::r#true()) } else { None },
                    if remove_parens { Some(b::r#true()) } else { None },
                ],
            );
        }
        b::r#function(None, vec![b::rest(b::id("$$args"))], b::block(vec![b::stmt(call)]))
    }

    /// The `RegularElement` visitor (the element is on the path)
    pub fn regular_element(&mut self, n: NodeId, st: &State) {
        let ast = self.ast();
        let TNode::Element(el) = &ast.nodes[n] else { return };
        let is_html = st.namespace == "html" && el.name != "svg";
        let name = if is_html { el.name.to_lowercase() } else { el.name.to_string() };
        st.template.borrow_mut().push_element(&name, el.start, is_html);
        if name == "noscript" {
            st.template.borrow_mut().pop_element();
            return;
        }
        let is_custom_element = self.an.is_custom_element_node(n);
        {
            let mut t = st.template.borrow_mut();
            t.needs_import_node = t.needs_import_node || name == "video" || is_custom_element;
            t.contains_script_tag = t.contains_script_tag || name == "script";
        }

        let mut attributes: Vec<&'s Attr<'s>> = Vec::new();
        let mut class_directives: Vec<&'s Attr<'s>> = Vec::new();
        let mut style_directives: Vec<&'s Attr<'s>> = Vec::new();
        let mut other_directives: Vec<&'s Attr<'s>> = Vec::new();
        let lets = shared();
        let mut lookup: Vec<(&'s str, &'s Attr<'s>)> = Vec::new();
        let mut bindings: Vec<(&'s str, &'s Attr<'s>)> = Vec::new();
        let has_spread = self.an.node_meta.get(&n).is_some_and(|m| m.has_spread);
        let mut has_use = false;
        let mut should_remove_defaults = false;
        let all_attributes = self.element_attributes(n);

        for &a in &all_attributes {
            match a {
                Attr::Directive { kind: "AnimateDirective" | "OnDirective" | "TransitionDirective", .. } | Attr::Attach { .. } => other_directives.push(a),
                Attr::Attribute { name: an, value, .. } => {
                    if *an == "is" && st.namespace == "html" {
                        let mut none = None;
                        let (v, _) = self.build_attribute_value(value, st, Memoize::None, &mut none);
                        if let NodeKind::Literal(l) = &v.kind {
                            if let LiteralValue::String(s) = &l.value {
                                st.template.borrow_mut().set_prop("is", Some(s.to_string()));
                                continue;
                            }
                        }
                    }
                    attributes.push(a);
                    match lookup.iter_mut().find(|(k, _)| k == an) {
                        Some(e) => e.1 = a,
                        None => lookup.push((an, a)),
                    }
                }
                Attr::Directive { kind: "BindDirective", name: bn, .. } => {
                    match bindings.iter_mut().find(|(k, _)| k == bn) {
                        Some(e) => e.1 = a,
                        None => bindings.push((bn, a)),
                    }
                    other_directives.push(a);
                }
                Attr::Directive { kind: "ClassDirective", .. } => class_directives.push(a),
                Attr::Directive { kind: "LetDirective", .. } => {
                    let s = State { let_directives: lets.clone(), ..st.clone() };
                    self.visit_attr(a, &s);
                }
                Attr::Spread { .. } => attributes.push(a),
                Attr::StyleDirective { .. } => style_directives.push(a),
                Attr::Directive { kind: "UseDirective", .. } => {
                    has_use = true;
                    other_directives.push(a);
                }
                _ => {}
            }
        }
        let has_binding = |name: &str| bindings.iter().any(|(k, _)| *k == name);
        let lookup_get = |name: &str| lookup.iter().find(|(k, _)| *k == name).map(|(_, a)| *a);

        let element_state = State { init: shared(), after_update: shared(), ..st.clone() };
        for &a in &other_directives {
            if matches!(a, Attr::Directive { kind: "OnDirective", .. }) {
                let handler = self.visit_attr(a, st).unwrap_or_else(b::void0);
                if has_use {
                    element_state.init.borrow_mut().push(b::stmt(b::call("$.effect", vec![b::thunk(handler)])));
                } else {
                    element_state.after_update.borrow_mut().push(b::stmt(handler));
                }
            } else {
                self.visit_attr(a, &element_state);
            }
        }

        if name == "input" {
            let has_value_attribute = attributes
                .iter()
                .any(|a| matches!(a, Attr::Attribute { name, value, .. } if (*name == "value" || *name == "checked") && !is_text_attribute(value)));
            let has_default_value_attribute = attributes.iter().any(|a| matches!(a, Attr::Attribute { name, .. } if *name == "defaultValue" || *name == "defaultChecked"));
            if !has_default_value_attribute && (has_spread || has_binding("value") || has_binding("checked") || has_binding("group") || (!has_binding("group") && has_value_attribute)) {
                if has_spread {
                    should_remove_defaults = true;
                } else {
                    st.init.borrow_mut().push(b::stmt(b::call("$.remove_input_defaults", vec![st.node.clone()])));
                }
            }
        }
        if name == "textarea" {
            let attribute = lookup_get("value").or_else(|| lookup_get("checked"));
            let needs_content_reset = attribute.is_some_and(|a| matches!(a, Attr::Attribute { value, .. } if !is_text_attribute(value)));
            if has_spread || has_binding("value") || needs_content_reset {
                st.init.borrow_mut().push(b::stmt(b::call("$.remove_textarea_child", vec![st.node.clone()])));
            }
        }

        st.let_directives.borrow_mut().extend(lets.borrow().iter().cloned());
        let node_id = st.node.clone();
        let needs_special_value_handling = name == "option" || name == "select" || has_binding("group") || has_binding("checked");

        if has_spread {
            self.build_attribute_effect(&attributes, &class_directives, &style_directives, st, n, &node_id, should_remove_defaults);
        } else {
            for &a in &attributes {
                let Attr::Attribute { name: aname, value, .. } = a else { continue };
                if crate::analyze::utils::is_event_attribute(a) {
                    let parent_type = self.tpl_parent().map(|p| p.ty(self.an.ast)).unwrap_or("Fragment");
                    self.visit_event_attribute(a, st, parent_type);
                    continue;
                }
                if needs_special_value_handling && *aname == "value" {
                    continue;
                }
                let attr_name = self.get_attribute_name(n, aname);
                if el.name == "select" && attr_name == "defaultValue" {
                    continue;
                }
                if !is_custom_element
                    && !crate::analyze::utils::cannot_be_set_statically(aname)
                    && (attr_name != "value" || el.name != "textarea")
                    && (matches!(value, AttrValue::True) || is_text_attribute(value))
                    && (attr_name != "class" || class_directives.is_empty())
                    && (attr_name != "style" || style_directives.is_empty())
                {
                    let mut v: Option<String> = match value {
                        AttrValue::Sequence(c) => match &c[0] {
                            Chunk::Text { data, .. } => Some(data.to_string()),
                            _ => None,
                        },
                        _ => None,
                    };
                    if attr_name == "class" && self.scoped.contains(&n) && !self.css_hash.is_empty() {
                        v = Some(match v {
                            None => self.css_hash.clone(),
                            Some(s) if s.is_empty() => self.css_hash.clone(),
                            Some(s) => format!("{s} {}", self.css_hash),
                        });
                    }
                    // `value === true` sets an empty attribute; an empty class is skipped
                    let is_true = matches!(value, AttrValue::True) && v.is_none();
                    if attr_name != "class" || is_true || v.as_deref().is_some_and(|s| !s.is_empty()) {
                        st.template.borrow_mut().set_prop(aname, Some(if is_true { String::new() } else { v.unwrap_or_default() }));
                    }
                } else if attr_name == "autofocus" {
                    let mut none = None;
                    let (v, _) = self.build_attribute_value(value, st, Memoize::None, &mut none);
                    st.init.borrow_mut().push(b::stmt(b::call("$.autofocus", vec![node_id.clone(), v])));
                } else if attr_name == "class" {
                    self.build_set_class(n, &node_id, a, &class_directives, st, is_html);
                } else if attr_name == "style" {
                    self.build_set_style(&node_id, a, &style_directives, st);
                } else if is_custom_element {
                    self.build_custom_element_attribute_update_assignment(&node_id, a, st);
                } else {
                    let mut none = None;
                    let (v, has_state) = self.build_attribute_value(value, st, Memoize::State, &mut none);
                    let update = self.build_element_attribute_update(n, &node_id, &attr_name, v, &attributes);
                    if has_state { st.update.borrow_mut() } else { st.init.borrow_mut() }.push(b::stmt(update));
                }
            }
        }

        if crate::analyze::utils::is_load_error_element(&name) && (has_spread || has_use || lookup_get("onload").is_some() || lookup_get("onerror").is_some()) {
            st.after_update.borrow_mut().push(b::stmt(b::call("$.replay_events", vec![node_id.clone()])));
        }

        let namespace = self.determine_namespace_for_children(n);
        let mut bound_contenteditable = st.bound_contenteditable;
        if has_binding("innerHTML") || has_binding("innerText") || has_binding("textContent") {
            if let Some(Attr::Attribute { value, .. }) = lookup_get("contenteditable") {
                let is_true = matches!(value, AttrValue::True)
                    || (is_text_attribute(value) && matches!(value, AttrValue::Sequence(c) if matches!(&c[0], Chunk::Text { data, .. } if data == "true")));
                if is_true {
                    bound_contenteditable = true;
                }
            }
        }

        let frag_scope = self.scope_of_key(P::Fragment(el.fragment).key()).unwrap_or(st.scope);
        let state = State {
            namespace,
            bound_contenteditable,
            scope: frag_scope,
            transform: self.get_transform(frag_scope, st),
            preserve_whitespace: st.preserve_whitespace || name == "pre" || name == "textarea",
            ..st.clone()
        };
        // a `<textarea>`'s dynamic children were moved into its `value`
        let frag_nodes = if self.an.textarea_values.contains(&n) { vec![] } else { ast.fragments[el.fragment].nodes.clone() };
        let cleaned = self.clean_nodes(
            super::fragment::Parent::Node(n),
            &frag_nodes,
            state.namespace,
            state.scope,
            name == "script" || state.preserve_whitespace,
            self.options.preserve_comments,
        );
        let has_declarations = !ast.fragments[el.fragment].transparent;
        let child_state = State {
            init: shared(),
            update: shared(),
            after_update: shared(),
            snippets: shared(),
            consts: if has_declarations { shared() } else { state.consts.clone() },
            async_consts: if has_declarations { Rc::new(RefCell::new(None)) } else { state.async_consts.clone() },
            memoizer: if has_declarations { Rc::new(RefCell::new(Memoizer::default())) } else { state.memoizer.clone() },
            ..state.clone()
        };
        for &h in &cleaned.hoisted {
            self.visit_node(h, &child_state);
        }

        let trimmed = &cleaned.trimmed;
        let only_text = trimmed.iter().all(|c| matches!(c.ty(ast), "Text" | "ExpressionTag"));
        let use_text_content = only_text
            && trimmed.iter().all(|c| match c {
                super::fragment::Child::Node(cn) if matches!(ast.nodes[*cn], TNode::ExpressionTag { .. }) => {
                    let m = self.meta_of_node(*cn);
                    let md = &self.an.metas[m as usize];
                    !md.has_state && !md.has_await && !self.meta_has_blockers(m)
                }
                _ => true,
            })
            && trimmed.iter().any(|c| c.ty(ast) == "ExpressionTag");

        if use_text_content {
            let (values, texts) = super::fragment::chunk_refs(self, trimmed);
            let mut none = None;
            let (value, _) = self.build_template_chunk(&values, &texts, &child_state, Memoize::State, &mut none);
            let empty_string = matches!(&value.kind, NodeKind::Literal(l) if matches!(&l.value, LiteralValue::String(s) if s.is_empty()));
            if !empty_string {
                child_state.init.borrow_mut().push(b::stmt(b::assignment("=", b::member(st.node.clone(), "textContent"), value)));
            }
        } else if self.is_customizable_select(n) {
            let element_node = st.node.clone();
            st.template.borrow_mut().push_comment(None);
            let fragment_id = b::id(self.generate(st.scope, "fragment"));
            let anchor_id = b::id(self.generate(st.scope, "anchor"));
            let select_state = State { init: shared(), update: shared(), after_update: shared(), template: Rc::new(RefCell::new(Template::default())), ..state.clone() };
            self.process_children(trimmed, Initial::Call("$.first_child", fragment_id.clone()), false, &select_state);
            let template_name = self.transform_template(&select_state, &format!("{name}_content"), TEMPLATE_FRAGMENT);
            let mut body = vec![
                b::var(anchor_id.clone(), b::call("$.child", vec![element_node.clone()])),
                b::var(fragment_id.clone(), b::call(template_name, ())),
            ];
            body.extend(select_state.init.borrow().iter().cloned());
            if !select_state.update.borrow().is_empty() {
                let s = self.build_render_statement(&select_state);
                body.push(s);
            }
            body.extend(select_state.after_update.borrow().iter().cloned());
            body.push(b::stmt(b::call("$.append", vec![anchor_id, fragment_id])));
            child_state.init.borrow_mut().push(b::stmt(b::call("$.customizable_select", vec![element_node, b::arrow(vec![], b::block(body))])));
        } else {
            let mut arg = st.node.clone();
            let mut needs_reset = trimmed.iter().any(|c| c.ty(ast) != "Text" && !c.node_id().is_some_and(|cn| self.is_static_element(cn)));
            if name == "template" {
                needs_reset = true;
                child_state.init.borrow_mut().push(b::stmt(b::call("$.hydrate_template", vec![arg.clone()])));
                arg = b::member(arg, "content");
            }
            self.process_children(trimmed, Initial::Call("$.child", arg), true, &child_state);
            if needs_reset && !fold_reset_into_child(&mut child_state.init.borrow_mut(), &st.node) {
                child_state.init.borrow_mut().push(b::stmt(b::call("$.reset", vec![st.node.clone()])));
            }
        }

        let has_snippet = frag_nodes.iter().any(|&c| matches!(ast.nodes[c], TNode::SnippetBlock { .. }));
        if has_snippet || has_declarations {
            if let Some(ac) = child_state.async_consts.borrow().as_ref() {
                if !ac.thunks.is_empty() {
                    child_state.consts.borrow_mut().push(b::var(ac.id.clone(), b::call("$.run", vec![b::array(ac.thunks.clone())])));
                }
            }
            let mut block = Vec::new();
            block.extend(child_state.snippets.borrow().iter().cloned());
            block.extend(child_state.consts.borrow().iter().cloned());
            block.extend(child_state.init.borrow().iter().cloned());
            block.extend(element_state.init.borrow().iter().cloned());
            if !child_state.update.borrow().is_empty() {
                let s = self.build_render_statement(&child_state);
                block.push(s);
            } else {
                block.push(b::empty());
            }
            block.extend(child_state.after_update.borrow().iter().cloned());
            block.extend(element_state.after_update.borrow().iter().cloned());
            st.init.borrow_mut().push(b::block(block));
        } else if self.an.fragment_dynamic.contains(&el.fragment) {
            st.init.borrow_mut().extend(child_state.init.borrow().iter().cloned());
            st.init.borrow_mut().extend(element_state.init.borrow().iter().cloned());
            st.update.borrow_mut().extend(child_state.update.borrow().iter().cloned());
            st.after_update.borrow_mut().extend(child_state.after_update.borrow().iter().cloned());
            st.after_update.borrow_mut().extend(element_state.after_update.borrow().iter().cloned());
        } else {
            st.init.borrow_mut().extend(element_state.init.borrow().iter().cloned());
            st.after_update.borrow_mut().extend(element_state.after_update.borrow().iter().cloned());
        }

        if name == "selectedcontent" {
            st.init.borrow_mut().push(b::stmt(b::call(
                "$.selectedcontent",
                vec![st.node.clone(), b::arrow(vec![b::id("$$element")], b::assignment("=", st.node.clone(), b::id("$$element")))],
            )));
        }

        if lookup_get("dir").is_some() {
            let dir = b::member(node_id.clone(), "dir");
            st.update.borrow_mut().push(b::stmt(b::assignment("=", dir.clone(), dir)));
        }

        if !has_spread && needs_special_value_handling {
            if let Some(svn) = self.an.node_meta.get(&n).and_then(|m| m.synthetic_value_node) {
                self.build_element_special_value_attribute_synthetic(&name, &node_id, svn, st);
            } else {
                for &a in &attributes {
                    if matches!(a, Attr::Attribute { name: "value", .. }) {
                        self.build_element_special_value_attribute(&name, &node_id, a, st, false);
                        break;
                    }
                }
            }
        }

        if !has_spread && name == "select" {
            let default_value = attributes.iter().copied().find(|a| matches!(a, Attr::Attribute { name, .. } if self.get_attribute_name(n, name) == "defaultValue"));
            if let Some(Attr::Attribute { value, .. }) = default_value {
                let mut none = None;
                let (v, has_state) = self.build_attribute_value(value, st, Memoize::State, &mut none);
                if has_state { st.update.borrow_mut() } else { st.init.borrow_mut() }
                    .push(b::stmt(b::call("$.set_default_select_value", vec![node_id.clone(), v])));
            }
            let dynamic_value = matches!(lookup_get("value"), Some(Attr::Attribute { value, .. }) if !matches!(value, AttrValue::True) && !is_text_attribute(value));
            if default_value.is_some() || dynamic_value || has_binding("value") {
                st.init.borrow_mut().push(b::stmt(b::call("$.init_select", vec![node_id.clone()])));
            }
        }

        st.template.borrow_mut().pop_element();
    }

    /// `is_customizable_select_element(node)`
    pub fn is_customizable_select(&self, n: NodeId) -> bool {
        match &self.ast().nodes[n] {
            TNode::Element(el) => crate::analyze::visit::is_customizable_select_element(self.ast(), el),
            _ => false,
        }
    }

    /// `build_element_attribute_update(element, node_id, name, value, attributes)`
    fn build_element_attribute_update(&self, n: NodeId, node_id: &Node, name: &str, value: Node, attributes: &[&'s Attr<'s>]) -> Node {
        let TNode::Element(el) = &self.ast().nodes[n] else { return value };
        match name {
            "muted" => return b::assignment("=", b::member(node_id.clone(), b::id("muted")), value),
            "value" => return b::call("$.set_value", vec![node_id.clone(), value]),
            "checked" => return b::call("$.set_checked", vec![node_id.clone(), value]),
            "selected" => return b::call("$.set_selected", vec![node_id.clone(), value]),
            _ => {}
        }
        if name == "defaultValue"
            && (attributes.iter().any(|a| matches!(a, Attr::Attribute { name: "value", value, .. } if is_text_attribute(value)))
                || (el.name == "textarea" && !self.ast().fragments[el.fragment].nodes.is_empty() && !self.an.textarea_values.contains(&n)))
        {
            return b::call("$.set_default_value", vec![node_id.clone(), value]);
        }
        if name == "defaultChecked" && attributes.iter().any(|a| matches!(a, Attr::Attribute { name: "checked", value: AttrValue::True, .. })) {
            return b::call("$.set_default_checked", vec![node_id.clone(), value]);
        }
        if crate::analyze::utils::is_dom_property(name) {
            return b::assignment("=", b::member(node_id.clone(), name), value);
        }
        let ignored = self.is_ignored_key(P::Node(n).key(), "hydration_attribute_changed");
        b::call(
            if name.starts_with("xlink") { "$.set_xlink_attribute" } else { "$.set_attribute" },
            vec![Some(node_id.clone()), Some(b::literal(name)), Some(value), if ignored { Some(b::r#true()) } else { None }],
        )
    }

    /// `build_custom_element_attribute_update_assignment(node_id, attribute, context)`
    fn build_custom_element_attribute_update_assignment(&mut self, node_id: &Node, a: &'s Attr<'s>, st: &State) {
        let Attr::Attribute { name, value, .. } = a else { return };
        let mut memoizer = Some(Memoizer::default());
        let (v, has_state) = self.build_attribute_value(value, st, Memoize::Local, &mut memoizer);
        let call = b::call("$.set_custom_element_data", vec![node_id.clone(), b::literal(*name), v]);
        let memoizer = memoizer.unwrap();
        let update = if has_state {
            let ids = memoizer.apply(self);
            b::call(
                "$.template_effect",
                vec![Some(b::arrow(ids, call)), memoizer.sync_values(), memoizer.async_values(self), memoizer.blockers(self)],
            )
        } else {
            call
        };
        st.init.borrow_mut().push(b::stmt(update));
    }

    /// `build_element_special_value_attribute(element, node_id, attribute, context, synthetic)`
    fn build_element_special_value_attribute(&mut self, element: &str, node_id: &Node, a: &'s Attr<'s>, st: &State, synthetic: bool) {
        let Attr::Attribute { value, .. } = a else { return };
        let is_select_with_value = element == "select" && !matches!(value, AttrValue::True) && !is_text_attribute(value);
        let mut none = None;
        let (v, has_state) = self.build_attribute_value(value, st, Memoize::State, &mut none);
        self.finish_special_value(element, node_id, v, has_state, is_select_with_value, synthetic, st);
    }

    /// The same, for the synthetic `value` of an `<option>` with a single expression child
    fn build_element_special_value_attribute_synthetic(&mut self, element: &str, node_id: &Node, svn: NodeId, st: &State) {
        let TNode::ExpressionTag { expression, .. } = &self.ast().nodes[svn] else { return };
        let meta = self.meta_of_node(svn);
        let e = self.build_expression(expression, meta, st);
        let has_state = self.an.metas[meta as usize].has_state || self.meta_is_async(meta);
        let mut none = None;
        let v = self.memoize(Memoize::State, e, meta, st, &mut none);
        self.finish_special_value(element, node_id, v, has_state, false, true, st);
    }

    #[allow(clippy::too_many_arguments)]
    fn finish_special_value(&mut self, element: &str, node_id: &Node, value: Node, has_state: bool, is_select_with_value: bool, synthetic: bool, st: &State) {
        let evaluated = self.evaluate(&value, st.scope);
        let build_update = |value: Node| -> Node {
            let assignment = b::assignment("=", b::member(node_id.clone(), "__value"), value.clone());
            let set_value_assignment = b::assignment(
                "=",
                b::member(node_id.clone(), "value"),
                if evaluated.is_defined { assignment.clone() } else { b::logical("??", assignment.clone(), b::literal("")) },
            );
            b::stmt(if is_select_with_value {
                b::sequence(vec![set_value_assignment, b::call("$.select_option", vec![node_id.clone(), value])])
            } else if synthetic {
                assignment
            } else {
                set_value_assignment
            })
        };
        if has_state {
            let id = b::id(self.generate(st.scope, &format!("{}_value", super::super::js::ident(node_id).unwrap_or(""))));
            let init = if element == "option" { Some(b::object(vec![])) } else { None };
            st.init.borrow_mut().push(b::var(id.clone(), init));
            st.update.borrow_mut().push(b::r#if(b::binary("!==", id.clone(), b::assignment("=", id.clone(), value)), b::block(vec![build_update(id)]), None));
        } else {
            st.init.borrow_mut().push(build_update(value));
        }
    }
}

/// `fold_reset_into_child(init, node_id)`: `var x = $.child(p, true); $.reset(p);` →
/// `var x = $.only_child(p, true);`
fn fold_reset_into_child(init: &mut [Node], node_id: &Node) -> bool {
    let Some(node_name) = super::super::js::ident(node_id) else { return false };
    let Some(last) = init.last_mut() else { return false };
    let NodeKind::VariableDeclaration(v) = &mut last.kind else { return false };
    if v.declarations.len() != 1 {
        return false;
    }
    let NodeKind::VariableDeclarator(d) = &mut v.declarations[0].kind else { return false };
    let Some(call) = d.init.as_deref_mut() else { return false };
    let NodeKind::CallExpression(c) = &mut call.kind else { return false };
    if super::super::js::ident(&c.callee) != Some("$.child") {
        return false;
    }
    if c.arguments.first().and_then(super::super::js::ident) != Some(node_name) {
        return false;
    }
    c.callee = Box::new(b::id("$.only_child"));
    true
}

/// `has_side_effects(node)`
fn has_side_effects(node: &Node) -> bool {
    match &node.kind {
        NodeKind::CallExpression(_) | NodeKind::NewExpression(_) | NodeKind::AssignmentExpression(_) | NodeKind::UpdateExpression(_) => true,
        NodeKind::SequenceExpression(s) => s.expressions.iter().any(has_side_effects),
        _ => false,
    }
}
