//! `server/visitors/shared/element.js` and `build_attribute_value` from `shared/utils.js`

use crate::analyze::nodes::P;
use crate::analyze::utils as u;
use crate::ast::{Attr, AttrValue, Chunk, Expr, Node as TNode, NodeId};
use crate::estree::builders as b;
use crate::estree::{LiteralValue, Node, NodeKind};

use super::utils::{escape_html, evaluated_string, template_from, PromiseOptimiser};
use super::{Server, State};

const ELEMENT_IS_NAMESPACED: f64 = 1.0;
const ELEMENT_PRESERVE_ATTRIBUTE_CASE: f64 = 2.0;
const ELEMENT_IS_INPUT: f64 = 4.0;

fn whitespace_insensitive(name: &str) -> bool {
    name == "class" || name == "style"
}

/// Bindings that have no server counterpart (`binding_properties[name].omit_in_ssr`)
fn omit_in_ssr(name: &str) -> bool {
    matches!(
        name,
        "currentTime" | "duration" | "focused" | "paused" | "buffered" | "seekable" | "played" | "volume" | "muted"
            | "playbackRate" | "seeking" | "ended" | "readyState" | "videoHeight" | "videoWidth" | "naturalWidth"
            | "naturalHeight" | "activeElement" | "fullscreenElement" | "pointerLockElement" | "visibilityState"
            | "innerWidth" | "innerHeight" | "outerWidth" | "outerHeight" | "scrollX" | "scrollY" | "online"
            | "devicePixelRatio" | "clientWidth" | "clientHeight" | "offsetWidth" | "offsetHeight" | "contentRect"
            | "contentBoxSize" | "borderBoxSize" | "devicePixelContentBoxSize" | "indeterminate" | "this" | "files"
    )
}

/// An attribute of `build_element_attributes`' list
enum ElAttr<'s> {
    Attr(&'s Attr<'s>),
    /// `class={...}` that needs clsx: the attribute with its expression wrapped in `$.clsx`
    Clsx(&'s Attr<'s>),
    /// `{ type: 'transformed', name, expression }`
    Transformed(String, Node),
    /// a `bind:` directive (in `prepare_element_spread_object`)
    Bind(&'s Attr<'s>),
}

/// `/^\r?\n/`
fn starts_with_newline(s: &str) -> bool {
    s.starts_with('\n') || s.starts_with("\r\n")
}

impl<'a, 's> Server<'a, 's> {
    /// `context.visit(expression)` from an element visitor (the element is on the path)
    pub fn visit_expr_here(&mut self, e: &'s Expr<'s>, st: &State) -> Node {
        let node = self.convert_expr(e);
        self.visit_js(&node, st)
    }

    fn chunk_meta(&self, c: &Chunk<'s>) -> u32 {
        self.an.meta_of.get(&P::Chunk(c).key()).copied().unwrap_or(0)
    }

    fn attr_meta(&self, a: &Attr<'s>) -> u32 {
        self.an.meta_of.get(&P::Attr(a).key()).copied().unwrap_or(0)
    }

    /// `build_attribute_value(value, context, transform, trim_whitespace, is_component)`
    pub fn build_attribute_value(
        &mut self,
        value: &'s AttrValue<'s>,
        st: &State,
        opt: &mut PromiseOptimiser,
        trim_whitespace: bool,
        is_component: bool,
        textarea_newline: bool,
    ) -> Node {
        let chunks: &[Chunk<'s>] = match value {
            AttrValue::True => return b::r#true(),
            AttrValue::Expression(c) => std::slice::from_ref(c.as_ref()),
            AttrValue::Sequence(c) => c,
        };
        if chunks.len() == 1 {
            let chunk = &chunks[0];
            return match chunk {
                Chunk::Text { data, .. } => {
                    let mut data = data.to_string();
                    if textarea_newline {
                        data = format!("\n{data}");
                    }
                    let data = if trim_whitespace { strict_ws_collapse(&data).trim().to_string() } else { data };
                    b::literal(if is_component { data } else { escape_html(&data, true) }.as_str())
                }
                Chunk::Expression { expression, .. } => {
                    let e = self.visit_expr_here(expression, st);
                    let meta = self.chunk_meta(chunk);
                    opt.transform(self, e, meta)
                }
            };
        }
        let mut quasis: Vec<(String, bool)> = vec![(String::new(), false)];
        let mut expressions = Vec::new();
        let len = chunks.len();
        for (i, chunk) in chunks.iter().enumerate() {
            match chunk {
                Chunk::Text { data, .. } => {
                    let mut data = data.to_string();
                    if i == 0 && textarea_newline {
                        data = format!("\n{data}");
                    }
                    quasis.last_mut().unwrap().0.push_str(&if trim_whitespace { strict_ws_collapse(&data) } else { data });
                }
                Chunk::Expression { expression, .. } => {
                    let evaluated = self.evaluate_expr(expression, st.scope);
                    if evaluated.is_known {
                        quasis.last_mut().unwrap().0.push_str(&evaluated_string(&evaluated.value));
                    } else {
                        let e = self.visit_expr_here(expression, st);
                        let meta = self.chunk_meta(chunk);
                        let e = opt.transform(self, e, meta);
                        expressions.push(if evaluated.is_string && evaluated.is_defined { e } else { b::call("$.stringify", vec![e]) });
                        quasis.push((String::new(), i + 1 == len));
                    }
                }
            }
        }
        if expressions.is_empty() {
            b::literal(quasis[0].0.as_str())
        } else {
            template_from(quasis, expressions)
        }
    }

    /// `get_attribute_name(element, attribute)`
    fn attribute_name(&self, n: NodeId, name: &str) -> String {
        let m = self.an.node_meta.get(&n).cloned().unwrap_or_default();
        if !m.svg && !m.mathml {
            name.to_lowercase()
        } else {
            name.to_string()
        }
    }

    /// `build_element_attributes(node, context, transform)`: pushes the attributes to
    /// `state.template`, returns the content some attributes replace the children with
    pub fn build_element_attributes(&mut self, n: NodeId, st: &State, opt: &mut PromiseOptimiser) -> Option<Node> {
        let ast = self.ast();
        let TNode::Element(el) = &ast.nodes[n] else { return None };
        let is_regular = el.kind == "RegularElement";
        let mut attributes: Vec<ElAttr<'s>> = Vec::new();
        let mut class_directives: Vec<&'s Attr<'s>> = Vec::new();
        let mut style_directives: Vec<&'s Attr<'s>> = Vec::new();
        let mut content = None;
        let mut has_spread = false;
        let mut events_to_capture: Vec<&'static str> = Vec::new();
        let mut add_event = |e: &'static str, events: &mut Vec<&'static str>| {
            if !events.contains(&e) {
                events.push(e);
            }
        };

        let all_attributes = self.element_attributes(n);
        for &a in &all_attributes {
            match a {
                Attr::Attribute { name, value, .. } => {
                    if *name == "value" {
                        if el.name == "textarea" {
                            let newline = matches!(value, AttrValue::Sequence(c) if matches!(c.first(), Some(Chunk::Text { data, .. }) if starts_with_newline(data)));
                            let v = self.build_attribute_value(value, st, opt, false, false, newline);
                            content = Some(b::call("$.escape", vec![v]));
                        } else if el.name != "select" {
                            attributes.push(ElAttr::Attr(a));
                        }
                    } else if u::is_event_attribute(a) {
                        if (*name == "onload" || *name == "onerror") && u::is_load_error_element(el.name) {
                            add_event(if *name == "onload" { "onload" } else { "onerror" }, &mut events_to_capture);
                        }
                    } else if is_regular && el.name == "input" && (*name == "defaultValue" || *name == "defaultChecked") {
                        attributes.push(ElAttr::Attr(a));
                        has_spread = true;
                    } else if *name != "defaultValue" && *name != "defaultChecked" {
                        let needs_clsx = *name == "class" && self.an.attr_meta.get(&P::Attr(a).key()).is_some_and(|m| m.needs_clsx);
                        attributes.push(if needs_clsx { ElAttr::Clsx(a) } else { ElAttr::Attr(a) });
                    }
                }
                Attr::Directive { kind: "BindDirective", name, expression, .. } => {
                    if *name == "value" && el.name == "select" {
                        continue;
                    }
                    if *name == "value"
                        && attributes.iter().any(|x| matches!(x, ElAttr::Attr(Attr::Attribute { name: "type", value, .. }) if u::text_value(value) == Some("file")))
                    {
                        continue;
                    }
                    if *name == "this" || omit_in_ssr(name) {
                        continue;
                    }
                    let Some(expression) = expression else { continue };
                    let mut e = self.visit_expr_here(expression, st);
                    if let NodeKind::SequenceExpression(s) = &e.kind {
                        e = b::call(s.expressions[0].clone(), ());
                    }
                    let meta = self.attr_meta(a);
                    let e = opt.transform(self, e, meta);
                    let is_sequence = matches!(crate::analyze::nodes::template_expr(expression), P::Js(oxc_ast::AstKind::SequenceExpression(_)));
                    if *name == "innerHTML" {
                        content = Some(e);
                    } else if u::is_content_editable_binding(name) || (*name == "value" && el.name == "textarea") {
                        content = Some(b::call("$.escape", vec![e]));
                    } else if *name == "group" && !is_sequence {
                        let value_attribute = all_attributes.iter().copied().find(|x| matches!(x, Attr::Attribute { name: "value", .. }));
                        let Some(Attr::Attribute { value: va, .. }) = value_attribute else { continue };
                        let is_checkbox = el.attributes.iter().any(|x| matches!(x, Attr::Attribute { name: "type", value, .. } if u::text_value(value) == Some("checkbox")));
                        let v = self.build_attribute_value(va, st, opt, false, false, false);
                        let expression = if is_checkbox { b::call(b::member(e, "includes"), vec![v]) } else { b::binary("===", e, v) };
                        attributes.push(ElAttr::Transformed("checked".into(), expression));
                    } else {
                        attributes.push(ElAttr::Transformed(self.attribute_name(n, name), e));
                    }
                }
                Attr::Spread { .. } => {
                    attributes.push(ElAttr::Attr(a));
                    has_spread = true;
                    if u::is_load_error_element(el.name) {
                        add_event("onload", &mut events_to_capture);
                        add_event("onerror", &mut events_to_capture);
                    }
                }
                Attr::Directive { kind: "UseDirective", .. } => {
                    if u::is_load_error_element(el.name) {
                        add_event("onload", &mut events_to_capture);
                        add_event("onerror", &mut events_to_capture);
                    }
                }
                Attr::Directive { kind: "ClassDirective", .. } => class_directives.push(a),
                Attr::StyleDirective { .. } => style_directives.push(a),
                Attr::Directive { kind: "LetDirective", .. } => {}
                _ => {
                    // `context.visit(attribute)`: visited for its side effects, the result is dropped
                    self.visit_attribute_discarding(a, st);
                }
            }
        }

        let scoped = self.scoped.contains(&n);
        if has_spread {
            let args = self.prepare_element_spread(n, &attributes, &style_directives, &class_directives, st, opt);
            st.template.borrow_mut().push(b::call("$.attributes", args));
        } else {
            let css_hash = scoped.then(|| self.css_hash.clone());
            for attribute in attributes {
                let (a, clsx) = match attribute {
                    ElAttr::Transformed(name, expression) => {
                        let mut args = vec![Some(b::literal(name.as_str())), Some(expression)];
                        if u::is_boolean_attribute(&name) {
                            args.push(Some(b::r#true()));
                        }
                        st.template.borrow_mut().push(b::call("$.attr", args));
                        continue;
                    }
                    ElAttr::Attr(a) => (a, false),
                    ElAttr::Clsx(a) => (a, true),
                    ElAttr::Bind(_) => continue,
                };
                let Attr::Attribute { name: raw_name, value, .. } = a else { continue };
                let name = self.attribute_name(n, raw_name);
                let can_use_literal = (name != "class" || class_directives.is_empty()) && (name != "style" || style_directives.is_empty());
                let is_text = matches!(value, AttrValue::Sequence(c) if c.len() == 1 && matches!(c[0], Chunk::Text { .. }));
                if can_use_literal && (matches!(value, AttrValue::True) || is_text) {
                    let lit = self.build_attribute_value(value, st, opt, whitespace_insensitive(&name), false, false);
                    // `true`, or the (escaped) string
                    let mut literal_value: Option<String> = match &lit.kind {
                        NodeKind::Literal(l) => match &l.value {
                            LiteralValue::String(s) => Some(s.to_string()),
                            _ => None,
                        },
                        _ => None,
                    };
                    if name == "class" {
                        if let Some(h) = &css_hash {
                            let current = literal_value.clone().unwrap_or_else(|| "true".into());
                            literal_value = Some(format!("{current} {h}").trim().to_string());
                        }
                    }
                    let truthy = literal_value.as_ref().is_none_or(|v| !v.is_empty());
                    if name != "class" || truthy {
                        let shown = literal_value.unwrap_or_default();
                        st.template.borrow_mut().push(b::literal(format!(" {name}=\"{shown}\"").as_str()));
                    }
                    continue;
                }
                let mut value_node = if clsx {
                    // `{ ...attribute, value: { ...value, expression: b.call('$.clsx', expression) } }`
                    self.build_clsx_value(value, st, opt)
                } else {
                    self.build_attribute_value(value, st, opt, whitespace_insensitive(&name), false, false)
                };
                if can_use_literal {
                    if let NodeKind::Literal(l) = &mut value_node.kind {
                        if let LiteralValue::String(s) = &l.value {
                            let mut s = s.to_string();
                            if name == "class" {
                                if let Some(h) = &css_hash {
                                    s = format!("{s} {h}").trim().to_string();
                                }
                            }
                            st.template.borrow_mut().push(b::literal(format!(" {name}=\"{}\"", escape_html(&s, true)).as_str()));
                            continue;
                        }
                    }
                }
                if name == "class" {
                    let node = self.build_attr_class(&class_directives, value_node, css_hash.as_deref(), st, opt);
                    st.template.borrow_mut().push(node);
                } else if name == "style" {
                    let node = self.build_attr_style(&style_directives, value_node, st, opt);
                    st.template.borrow_mut().push(node);
                } else {
                    let mut args = vec![Some(b::literal(name.as_str())), Some(value_node)];
                    if u::is_boolean_attribute(&name) {
                        args.push(Some(b::r#true()));
                    }
                    st.template.borrow_mut().push(b::call("$.attr", args));
                }
            }
        }

        for event in events_to_capture {
            st.template.borrow_mut().push(b::literal(format!(" {event}=\"this.__e=event\"").as_str()));
        }
        content
    }

    /// The value of a `class={...}` that needs clsx: the single expression tag, transformed
    fn build_clsx_value(&mut self, value: &'s AttrValue<'s>, st: &State, opt: &mut PromiseOptimiser) -> Node {
        let chunk = match value {
            AttrValue::Expression(c) => c.as_ref(),
            AttrValue::Sequence(c) if c.len() == 1 => &c[0],
            _ => return self.build_attribute_value(value, st, opt, true, false, false),
        };
        let Chunk::Expression { expression, .. } = chunk else {
            return self.build_attribute_value(value, st, opt, true, false, false);
        };
        let e = self.convert_expr(expression);
        let wrapped = b::call("$.clsx", vec![e]);
        let visited = self.visit_js(&wrapped, st);
        let meta = self.chunk_meta(chunk);
        opt.transform(self, visited, meta)
    }

    /// `context.visit(attribute)` for attributes the element doesn't output (handlers,
    /// transitions, ...): the expressions are visited and the result dropped
    fn visit_attribute_discarding(&mut self, a: &'s Attr<'s>, st: &State) {
        match a {
            Attr::Directive { expression: Some(e), .. } | Attr::Attach { expression: e, .. } => {
                self.path.push(super::super::js::PathNode::Tpl(P::Attr(a)));
                let _ = self.visit_expr_here(e, st);
                self.path.pop();
            }
            _ => {}
        }
    }

    /// `prepare_element_spread(element, attributes, style_directives, class_directives, context, transform)`
    fn prepare_element_spread(
        &mut self,
        n: NodeId,
        attributes: &[ElAttr<'s>],
        style_directives: &[&'s Attr<'s>],
        class_directives: &[&'s Attr<'s>],
        st: &State,
        opt: &mut PromiseOptimiser,
    ) -> Vec<Option<Node>> {
        let ast = self.ast();
        let TNode::Element(el) = &ast.nodes[n] else { return vec![] };
        let mut classes = None;
        let mut styles = None;
        let mut flags = 0.0;
        if !class_directives.is_empty() {
            let mut props = Vec::new();
            for d in class_directives {
                let Attr::Directive { name, expression, .. } = d else { continue };
                let shorthand = matches!(expression.as_ref().map(crate::analyze::nodes::template_expr), Some(p) if crate::analyze::scope::ident(p).is_some_and(|i| i.name == *name));
                let value = if shorthand {
                    b::id(*name)
                } else {
                    let e = self.visit_expr_here(expression.as_ref().unwrap(), st);
                    let meta = self.attr_meta(d);
                    opt.transform(self, e, meta)
                };
                props.push(b::init(name, value));
            }
            classes = Some(b::object(props));
        }
        if !style_directives.is_empty() {
            let mut props = Vec::new();
            for d in style_directives {
                let Attr::StyleDirective { name, value, .. } = d else { continue };
                let v = if matches!(value, AttrValue::True) { b::id(*name) } else { self.build_attribute_value(value, st, opt, true, false, false) };
                props.push(b::init(name, v));
            }
            styles = Some(b::object(props));
        }
        let m = self.an.node_meta.get(&n).cloned().unwrap_or_default();
        if m.svg || m.mathml {
            flags = ELEMENT_IS_NAMESPACED + ELEMENT_PRESERVE_ATTRIBUTE_CASE;
        } else if el.kind == "RegularElement" && (el.name.contains('-') || el.attributes.iter().any(|a| matches!(a, Attr::Attribute { name: "is", .. }))) {
            flags = ELEMENT_PRESERVE_ATTRIBUTE_CASE;
        } else if el.kind == "RegularElement" && el.name == "input" {
            flags = ELEMENT_IS_INPUT;
        }
        let object = self.build_spread_object(n, attributes, st, opt);
        let css_hash = (self.scoped.contains(&n) && !self.css_hash.is_empty()).then(|| b::literal(self.css_hash.as_str()));
        vec![Some(object), css_hash, classes, styles, (flags != 0.0).then(|| b::literal(flags))]
    }

    /// `prepare_element_spread_object(element, context, transform)`
    pub fn prepare_element_spread_object(&mut self, n: NodeId, st: &State, opt: &mut PromiseOptimiser) -> Vec<Option<Node>> {
        let ast = self.ast();
        let TNode::Element(el) = &ast.nodes[n] else { return vec![] };
        let mut attributes = Vec::new();
        let mut class_directives = Vec::new();
        let mut style_directives = Vec::new();
        let _ = &el.attributes;
        for a in self.element_attributes(n) {
            match a {
                Attr::Attribute { .. } | Attr::Spread { .. } => attributes.push(ElAttr::Attr(a)),
                Attr::Directive { kind: "BindDirective", .. } => attributes.push(ElAttr::Bind(a)),
                Attr::Directive { kind: "ClassDirective", .. } => class_directives.push(a),
                Attr::StyleDirective { .. } => style_directives.push(a),
                _ => {}
            }
        }
        self.prepare_element_spread(n, &attributes, &style_directives, &class_directives, st, opt)
    }

    /// `build_spread_object(element, attributes, context, transform)`
    fn build_spread_object(&mut self, n: NodeId, attributes: &[ElAttr<'s>], st: &State, opt: &mut PromiseOptimiser) -> Node {
        let ast = self.ast();
        let TNode::Element(el) = &ast.nodes[n] else { return b::object(vec![]) };
        let is_select = el.kind == "RegularElement" && el.name == "select";
        let mut props = Vec::new();
        for attribute in attributes {
            match attribute {
                ElAttr::Transformed(name, e) => props.push(b::prop("init", b::key(name), e.clone())),
                ElAttr::Attr(a @ Attr::Attribute { name, value, .. }) | ElAttr::Clsx(a @ Attr::Attribute { name, value, .. }) => {
                    let mut name = self.attribute_name(n, name);
                    if is_select && name == "defaultvalue" {
                        name = "defaultValue".into();
                    }
                    let v = if matches!(attribute, ElAttr::Clsx(_)) {
                        self.build_clsx_value(value, st, opt)
                    } else {
                        self.build_attribute_value(value, st, opt, whitespace_insensitive(&name), false, false)
                    };
                    let _ = a;
                    props.push(b::prop("init", b::key(&name), v));
                }
                ElAttr::Bind(Attr::Directive { name, expression: Some(expression), .. }) => {
                    let name = self.attribute_name(n, name);
                    let e = self.visit_expr_here(expression, st);
                    let value = match &e.kind {
                        NodeKind::SequenceExpression(s) => b::call(s.expressions[0].clone(), ()),
                        _ => e,
                    };
                    props.push(b::prop("init", b::key(&name), value));
                }
                ElAttr::Attr(a @ Attr::Spread { expression, .. }) => {
                    self.path.push(super::super::js::PathNode::Tpl(P::Attr(a)));
                    let e = self.visit_expr_here(expression, st);
                    self.path.pop();
                    let meta = self.attr_meta(a);
                    props.push(b::spread(opt.transform(self, e, meta)));
                }
                _ => {}
            }
        }
        b::object(props)
    }

    /// `build_attr_class(class_directives, expression, context, hash, transform)`
    fn build_attr_class(&mut self, class_directives: &[&'s Attr<'s>], mut expression: Node, hash: Option<&str>, st: &State, opt: &mut PromiseOptimiser) -> Node {
        let mut directives = None;
        if !class_directives.is_empty() {
            let mut props = Vec::new();
            for d in class_directives {
                let Attr::Directive { name, expression: Some(e), .. } = d else { continue };
                let v = self.visit_expr_here(e, st);
                let meta = self.attr_meta(d);
                props.push(b::prop("init", b::literal(*name), opt.transform(self, v, meta)));
            }
            directives = Some(b::object(props));
        }
        let mut css_hash = None;
        if let Some(h) = hash {
            match &mut expression.kind {
                NodeKind::Literal(l) if matches!(l.value, LiteralValue::String(_)) => {
                    if let LiteralValue::String(s) = &l.value {
                        let v = format!("{s} {h}").trim().to_string();
                        l.value = LiteralValue::String(v.as_str().into());
                        l.raw = None;
                    }
                }
                _ => css_hash = Some(b::literal(h)),
            }
        }
        b::call("$.attr_class", vec![Some(expression), css_hash, directives])
    }

    /// `build_attr_style(style_directives, expression, context, transform)`
    fn build_attr_style(&mut self, style_directives: &[&'s Attr<'s>], expression: Node, st: &State, opt: &mut PromiseOptimiser) -> Node {
        let mut directives = None;
        if !style_directives.is_empty() {
            let mut normal = Vec::new();
            let mut important = Vec::new();
            for d in style_directives {
                let Attr::StyleDirective { name, value, modifiers, .. } = d else { continue };
                let e = if matches!(value, AttrValue::True) { b::id(*name) } else { self.build_attribute_value(value, st, opt, true, false, false) };
                let name = if name.starts_with("--") { name.to_string() } else { name.to_lowercase() };
                let property = b::init(&name, e);
                if modifiers.contains(&"important") {
                    important.push(property);
                } else {
                    normal.push(property);
                }
            }
            directives = Some(if !important.is_empty() { b::array(vec![b::object(normal), b::object(important)]) } else { b::object(normal) });
        }
        b::call("$.attr_style", vec![Some(expression), directives])
    }
}

/// `.replace(regex_whitespaces_strict, ' ')`: runs of `[ \t\n\r\f]` become one space
fn strict_ws_collapse(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_ws = false;
    for c in s.chars() {
        if matches!(c, ' ' | '\t' | '\n' | '\r' | '\u{c}') {
            if !in_ws {
                out.push(' ');
            }
            in_ws = true;
        } else {
            out.push(c);
            in_ws = false;
        }
    }
    out
}
