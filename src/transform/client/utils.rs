//! `client/visitors/shared/utils.js`: the `Memoizer`, template chunks, `bind:this`,
//! `build_expression` and friends

use crate::analyze::blockers::Blocker;
use crate::analyze::nodes::P;
use crate::analyze::scope::{DeclKind, Kind};
use crate::ast::Expr;
use crate::estree::builders as b;
use crate::estree::{LiteralValue, Node, NodeKind};

use super::super::js::{self, PathNode};
use super::{blocker_expression, Client, State};

/// An entry of the memoizer
#[derive(Clone)]
struct Memo {
    id: Node,
    expression: Node,
    meta: u32,
}

/// `Memoizer`: extracts complex expressions from templates as `$0`, `$1`, ...
#[derive(Default, Clone)]
pub struct Memoizer {
    sync: Vec<Memo>,
    r#async: Vec<Memo>,
    blockers: Vec<Blocker>,
}

impl Memoizer {
    /// `add(expression, metadata, memoize_if_state)`: the expression or its placeholder id,
    /// and whether it was memoized
    pub fn add(&mut self, c: &mut Client, expression: Node, meta: u32, memoize_if_state: bool) -> (Node, bool) {
        self.check_blockers(c, meta);
        let m = &c.an.metas[meta as usize];
        let should_memoize = m.has_call || m.has_await || (memoize_if_state && m.has_state);
        let has_await = m.has_await;
        if !should_memoize {
            return (expression, false);
        }
        let id = c.memo_id();
        let memo = Memo { id: id.clone(), expression, meta };
        if has_await {
            self.r#async.push(memo);
        } else {
            self.sync.push(memo);
        }
        (id, true)
    }

    /// `check_blockers(metadata)`
    pub fn check_blockers(&mut self, c: &Client, meta: u32) {
        for &r in &c.an.metas[meta as usize].references {
            if let Some(bl) = c.binding(r).blocker {
                if !self.blockers.contains(&bl) {
                    self.blockers.push(bl);
                }
            }
        }
    }

    /// `apply()`: names the ids `$0`, `$1`, ... and returns them
    pub fn apply(&self, c: &mut Client) -> Vec<Node> {
        self.sync
            .iter()
            .chain(self.r#async.iter())
            .enumerate()
            .map(|(i, memo)| {
                let name = format!("${i}");
                c.memo_names.insert(memo.id.identifier_name().unwrap().to_string(), name);
                memo.id.clone()
            })
            .collect()
    }

    pub fn blockers(&self) -> Option<Node> {
        if self.blockers.is_empty() {
            None
        } else {
            Some(b::array(self.blockers.iter().map(|bl| blocker_expression(*bl)).collect::<Vec<_>>()))
        }
    }

    /// `deriveds(runes)`
    pub fn deriveds(&self, runes: bool) -> Vec<Node> {
        self.sync
            .iter()
            .map(|memo| b::r#let(memo.id.clone(), b::call(if runes { "$.derived" } else { "$.derived_safe_equal" }, vec![b::thunk(memo.expression.clone())])))
            .collect()
    }

    pub fn async_ids(&self) -> Vec<Node> {
        self.r#async.iter().map(|m| m.id.clone()).collect()
    }

    pub fn async_values(&self, c: &Client) -> Option<Node> {
        if self.r#async.is_empty() {
            return None;
        }
        Some(b::array(self.r#async.iter().map(|m| c.async_thunk(m.expression.clone(), m.meta)).collect::<Vec<_>>()))
    }

    pub fn sync_values(&self) -> Option<Node> {
        if self.sync.is_empty() {
            return None;
        }
        Some(b::array(self.sync.iter().map(|m| b::arrow(vec![], m.expression.clone())).collect::<Vec<_>>()))
    }
}

/// A `Text` or `ExpressionTag` of a template chunk: a fragment child or an attribute value part
#[derive(Clone, Copy)]
pub enum ChunkRef<'s> {
    /// a text node (its data, possibly trimmed by `clean_nodes`)
    Text(&'s str),
    /// owned trimmed text (from `clean_nodes`)
    OwnedText(usize),
    /// an ExpressionTag: its expression and the key of the node holding `metadata.expression`
    Expr(&'s Expr<'s>, usize),
}

/// How `build_template_chunk` memoizes values
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Memoize {
    /// `state.memoizer.add(value, metadata)`
    State,
    /// `(value) => value`
    None,
    /// add to the given local memoizer (by `Client::local_memoizer`)
    Local,
    /// `memoizer.add(value, metadata)`, wrapped with `$.clsx(...)` when the attribute needs it
    StateClsx,
    /// the component prop rules (`$.get(memoized)`), see `build_component`
    ComponentProp { wrap_in_derived: bool },
    /// SlotElement: `$.get(memoizer.add(...))` for calls and awaits
    SlotProp,
    /// custom css props: `$.get(memoized)` when memoized
    CssProp,
}

impl<'a, 's> Client<'a, 's> {
    /// Apply a memoize strategy to a value
    pub fn memoize(&mut self, how: Memoize, value: Node, meta: u32, st: &State, local: &mut Option<Memoizer>) -> Node {
        match how {
            Memoize::None => value,
            Memoize::State => {
                let mut m = std::mem::take(&mut *st.memoizer.borrow_mut());
                let (v, _) = m.add(self, value, meta, false);
                *st.memoizer.borrow_mut() = m;
                v
            }
            Memoize::StateClsx => {
                let mut m = std::mem::take(&mut *st.memoizer.borrow_mut());
                let (v, _) = m.add(self, b::call("$.clsx", vec![value]), meta, false);
                *st.memoizer.borrow_mut() = m;
                v
            }
            Memoize::Local => {
                let m = local.as_mut().expect("local memoizer");
                m.add(self, value, meta, false).0
            }
            Memoize::ComponentProp { wrap_in_derived } => {
                let m = local.as_mut().expect("local memoizer");
                let (memoized, did) = m.add(self, value.clone(), meta, wrap_in_derived);
                if did { b::call("$.get", vec![memoized]) } else { value }
            }
            Memoize::CssProp => {
                let m = local.as_mut().expect("local memoizer");
                let (memoized, did) = m.add(self, value.clone(), meta, false);
                if did { b::call("$.get", vec![memoized]) } else { value }
            }
            Memoize::SlotProp => {
                let md = &self.an.metas[meta as usize];
                if md.has_call || md.has_await {
                    let m = local.as_mut().expect("local memoizer");
                    let (memoized, _) = m.add(self, value, meta, false);
                    b::call("$.get", vec![memoized])
                } else {
                    value
                }
            }
        }
    }

    /// The ESTree form of a template expression
    pub fn convert_expr(&self, e: &Expr<'s>) -> Node {
        match e {
            Expr::Js(js) => self.conv.expression(js.effective_root()),
            Expr::Ident { name, start, end, .. } => {
                let mut id = b::id(name.as_str());
                id.span = Some(crate::estree::Span::new(*start as u32, *end as u32));
                id.loc = Some(self.conv.location(oxc_span::Span::new(*start as u32, *end as u32)));
                id.origin = Some(P::TplExpr(e).key());
                id
            }
            Expr::Literal { value, start, end, raw } => {
                let mut l = Node::new(NodeKind::Literal(crate::estree::Literal {
                    value: LiteralValue::String(value.as_str().into()),
                    raw: Some(raw.as_str().into()),
                }));
                l.span = Some(crate::estree::Span::new(*start as u32, *end as u32));
                l.loc = Some(self.conv.location(oxc_span::Span::new(*start as u32, *end as u32)));
                l.origin = Some(P::TplExpr(e).key());
                l
            }
        }
    }

    /// `context.visit(expression, state)` for a template expression (the owner is on the path)
    pub fn visit_expr(&mut self, e: &Expr<'s>, st: &State) -> Node {
        let node = self.convert_expr(e);
        self.visit_js(&node, st)
    }

    /// `build_expression(context, expression, metadata, state)`
    pub fn build_expression(&mut self, e: &Expr<'s>, meta: u32, st: &State) -> Node {
        let node = self.convert_expr(e);
        self.build_expression_node(&node, meta, st)
    }

    pub fn build_expression_node(&mut self, node: &Node, meta: u32, st: &State) -> Node {
        let value = self.visit_js(node, st);
        if self.an.runes || self.an.maybe_runes {
            return value;
        }
        let m = &self.an.metas[meta as usize];
        if !m.has_call && !m.has_member_expression && !m.has_assignment {
            return value;
        }
        let references = m.references.clone();
        let mut sequence = Vec::new();
        for bid in references {
            let binding = self.binding(bid);
            if binding.kind == Kind::Normal && binding.declaration_kind != DeclKind::Import {
                continue;
            }
            let name = binding.node.name;
            let deep = matches!(binding.kind, Kind::BindableProp | Kind::Template) || binding.declaration_kind == DeclKind::Import || name == "$$props" || name == "$$restProps";
            let mut getter = self.build_getter(&b::id(name), st);
            if deep {
                getter = b::call("$.deep_read_state", vec![getter]);
            }
            sequence.push(getter);
        }
        sequence.push(b::call("$.untrack", vec![b::thunk(value)]));
        b::sequence(sequence)
    }

    /// `build_template_chunk(values, context, state, memoize)`. `texts` holds the trimmed
    /// text of `ChunkRef::OwnedText` values.
    pub fn build_template_chunk(&mut self, values: &[ChunkRef<'s>], texts: &[String], st: &State, how: Memoize, local: &mut Option<Memoizer>) -> (Node, bool) {
        let mut expressions = Vec::new();
        // (cooked, tail) of each quasi; the last one is the current one
        let mut quasis: Vec<(String, bool)> = vec![(String::new(), false)];
        let mut has_state = false;
        let mut has_await = false;
        let len = values.len();
        for (i, v) in values.iter().enumerate() {
            match *v {
                ChunkRef::Text(data) => quasis.last_mut().unwrap().0.push_str(data),
                ChunkRef::OwnedText(t) => quasis.last_mut().unwrap().0.push_str(&texts[t]),
                ChunkRef::Expr(expr, owner) => {
                    let converted = self.convert_expr(expr);
                    if let NodeKind::Literal(l) = &converted.kind {
                        if !matches!(l.value, LiteralValue::Null) {
                            quasis.last_mut().unwrap().0.push_str(&literal_to_string(&l.value));
                        }
                        continue;
                    }
                    if js::ident(&converted) == Some("undefined") && self.get(st.scope, "undefined").is_none() {
                        continue;
                    }
                    let meta = self.meta_of_key(owner);
                    let built = self.build_expression_node(&converted, meta, st);
                    let mut value = self.memoize(how, built, meta, st, local);
                    let evaluated = self.evaluate(&value, st.scope);
                    has_await = has_await || self.an.metas[meta as usize].has_await || self.meta_has_blockers(meta);
                    has_state = has_state || has_await || (self.an.metas[meta as usize].has_state && !evaluated.is_known);
                    if len == 1 {
                        if evaluated.is_known {
                            value = b::literal(evaluated_string(&evaluated.value).as_str());
                        }
                        return (value, has_state);
                    }
                    if let NodeKind::LogicalExpression(le) = &mut value.kind {
                        if matches!(le.operator.as_str(), "??" | "||") && matches!(&le.right.kind, NodeKind::Literal(r) if matches!(r.value, LiteralValue::Null)) {
                            le.right = Box::new(b::literal(""));
                        }
                    }
                    if evaluated.is_known {
                        quasis.last_mut().unwrap().0.push_str(&evaluated_string(&evaluated.value));
                    } else {
                        if !evaluated.is_defined {
                            value = b::logical("??", value, b::literal(""));
                        }
                        expressions.push(value);
                        quasis.push((String::new(), i + 1 == len));
                    }
                }
            }
        }
        if expressions.is_empty() {
            return (b::literal(quasis.pop().unwrap().0.as_str()), has_state);
        }
        let elements: Vec<Node> = quasis.into_iter().map(|(c, tail)| b::quasi_with(&c, tail)).collect();
        (b::template(elements, expressions), has_state)
    }

    /// `build_render_statement(state)`
    pub fn build_render_statement(&mut self, st: &State) -> Node {
        let memoizer = st.memoizer.borrow().clone();
        let ids = memoizer.apply(self);
        let update = st.update.borrow().clone();
        let body = if update.len() == 1 && update[0].is("ExpressionStatement") {
            let NodeKind::ExpressionStatement(e) = update.into_iter().next().unwrap().kind else { unreachable!() };
            *e.expression
        } else {
            b::block(update)
        };
        b::stmt(b::call(
            "$.template_effect",
            vec![Some(b::arrow(ids, body)), memoizer.sync_values(), memoizer.async_values(self), memoizer.blockers()],
        ))
    }

    /// `build_bind_this(expression, value, context)`. `expression` is the (unvisited)
    /// binding expression; it is visited with the current path.
    pub fn build_bind_this(&mut self, expression: &Node, value: Node, st: &State) -> Node {
        let (getter, setter) = match &expression.kind {
            NodeKind::SequenceExpression(s) => (s.expressions.first().cloned(), s.expressions.get(1).cloned()),
            _ => (None, None),
        };
        let mut ids: Vec<Node> = Vec::new();
        let mut values: Vec<Node> = Vec::new();
        let mut seen: Vec<String> = Vec::new();
        let transform = super::copy_transform(&st.transform);

        // each block context variables are passed to the get/set functions, so that old values
        // can be nulled out on teardown
        fn walk_ids(n: &Node, parent: Option<&Node>, out: &mut Vec<(Node, bool)>) {
            if let NodeKind::Identifier(_) = &n.kind {
                out.push((n.clone(), js::is_reference(n, parent)));
                return;
            }
            n.for_each_child(&mut |c| walk_ids(c, Some(n), out));
        }
        let target = getter.clone().unwrap_or_else(|| expression.clone());
        let mut all = Vec::new();
        walk_ids(&target, None, &mut all);
        for (id, is_ref) in all {
            let name = js::ident(&id).unwrap().to_string();
            if seen.contains(&name) {
                continue;
            }
            seen.push(name.clone());
            if !is_ref {
                continue;
            }
            let Some(bid) = self.get(st.scope, &name) else { continue };
            if self.is_state_source(bid) || self.binding(bid).kind == Kind::Derived {
                continue;
            }
            let binding_scope = self.binding(bid).scope;
            if self.an.sc.each_scope.values().any(|&s| s == binding_scope) {
                ids.push(id.clone());
                let v = self.visit_js(&id, st);
                values.push(v);
                let existing = transform.borrow().get(name.as_str()).cloned();
                if let Some(mut t) = existing {
                    t.read = std::rc::Rc::new(|_, n| n.clone());
                    transform.borrow_mut().insert(name.clone(), t);
                }
            }
        }

        let child_state = State { transform, ..st.clone() };
        let mut get = self.visit_js(&target, &child_state);
        let set_target = match &setter {
            Some(s) => s.clone(),
            None => b::assignment("=", expression.clone(), b::id("$$value")),
        };
        let set = self.visit_js(&set_target, &child_state);

        // if a property is mutated, it might already be gone: make the member chain optional
        {
            let mut n = &mut get;
            while let NodeKind::MemberExpression(m) = &mut n.kind {
                m.optional = true;
                n = &mut m.object;
            }
        }

        let get = match get.kind {
            NodeKind::ArrowFunctionExpression(a) => b::arrow(ids.clone(), *a.body),
            NodeKind::FunctionExpression(f) => b::r#function(None, ids.clone(), *f.body),
            _ if getter.is_some() => get,
            _ => b::arrow(ids.clone(), get),
        };
        let set = match set.kind {
            NodeKind::ArrowFunctionExpression(a) => {
                let mut params = vec![a.params.into_iter().next().unwrap_or_else(|| b::id("_"))];
                params.extend(ids.clone());
                b::arrow(params, *a.body)
            }
            NodeKind::FunctionExpression(f) => {
                let mut params = vec![f.params.into_iter().next().unwrap_or_else(|| b::id("_"))];
                params.extend(ids.clone());
                b::r#function(None, params, *f.body)
            }
            _ if setter.is_some() => set,
            _ => {
                let mut params = vec![b::id("$$value")];
                params.extend(ids.clone());
                b::arrow(params, set)
            }
        };
        b::call(
            "$.bind_this",
            vec![Some(value), Some(set), Some(get), if values.is_empty() { None } else { Some(b::thunk(b::array(values))) }],
        )
    }

    /// `add_svelte_meta(expression, node, type, additional)`
    pub fn add_svelte_meta(&self, expression: Node, start: Option<usize>, ty: &str, additional: Option<(&str, &str)>) -> Node {
        if !self.dev {
            return b::stmt(expression);
        }
        let Some(start) = start else { return b::stmt(expression) };
        let (line, column) = self.locate(start);
        b::stmt(b::call(
            "$.add_svelte_meta",
            vec![
                Some(b::arrow(vec![], expression)),
                Some(b::literal(ty)),
                Some(b::id(self.an.name.as_str())),
                Some(b::literal(line as f64)),
                Some(b::literal(column as f64)),
                additional.map(|(k, v)| b::object(vec![b::init(k, b::literal(v))])),
            ],
        ))
    }

    /// `validate_binding(state, binding, expression)`
    pub fn validate_binding(&mut self, st: &State, a: &'s crate::ast::Attr<'s>, binding_expr: &Expr<'s>, expression: &Node) {
        let converted = self.convert_expr(binding_expr);
        if converted.is("SequenceExpression") {
            return;
        }
        if let Some(left) = js::object(&converted).and_then(js::ident) {
            if self.get(st.scope, left).is_some_and(|b| self.binding(b).kind == Kind::StoreSub) {
                return;
            }
        }
        let NodeKind::MemberExpression(m) = &expression.kind else { return };
        let (line, column) = self.locate(a.start());
        let obj = (*m.object).clone();
        let meta = self.meta_of_key(P::Attr(a).key());
        let blockers = self.meta_blockers_array(meta);
        let thunk_obj = if st.store_to_invalidate.is_some() {
            b::thunk(b::sequence(vec![b::call("$.mark_store_binding", ()), obj]))
        } else {
            b::thunk(obj)
        };
        let prop = if m.computed { (*m.property).clone() } else { b::literal(js::ident(&m.property).unwrap_or("")) };
        st.init.borrow_mut().push(b::stmt(b::call(
            "$.validate_binding",
            vec![
                b::literal(&self.an.source[a.start()..a.end()]),
                blockers,
                thunk_obj,
                b::thunk(prop),
                b::literal(line as f64),
                b::literal(column as f64),
            ],
        )));
    }
}

/// `(value ?? '') + ''` of an evaluated value
pub fn evaluated_string(v: &Option<crate::analyze::evaluate::Val>) -> String {
    use crate::analyze::evaluate::Val;
    match v {
        None | Some(Val::Undefined) | Some(Val::Null) => String::new(),
        Some(v) => v.to_js_string().unwrap_or_default(),
    }
}

/// `value + ''` for a literal's value
pub fn literal_to_string(v: &LiteralValue) -> String {
    match v {
        LiteralValue::String(s) => s.to_string(),
        LiteralValue::Number(n) => crate::analyze::evaluate::number_to_string(*n),
        LiteralValue::Boolean(b) => b.to_string(),
        LiteralValue::Null => "null".into(),
        LiteralValue::BigInt(b) => b.to_string(),
        LiteralValue::RegExp(r) => format!("/{}/{}", r.pattern, r.flags),
    }
}

/// `parse_directive_name(name)`: `a.b-c` → `a['b-c']`
pub fn parse_directive_name(name: &str) -> Node {
    let mut parts = name.split('.');
    let mut expression = b::id(parts.next().unwrap_or(""));
    for part in parts {
        if part.is_empty() {
            break;
        }
        let computed = !b::is_valid_identifier(part);
        expression = b::member_with(expression, if computed { b::literal(part) } else { b::id(part) }, computed, false);
    }
    expression
}
