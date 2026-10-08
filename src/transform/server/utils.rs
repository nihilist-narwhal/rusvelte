//! `server/visitors/shared/utils.js`

use crate::estree::builders as b;
use crate::estree::{LiteralValue, Node, NodeKind};

use super::super::js;
use super::template::Child;
use super::{Server, State};

pub const BLOCK_OPEN: &str = "<!--[-->";
pub const BLOCK_OPEN_ELSE: &str = "<!--[!-->";
pub const BLOCK_CLOSE: &str = "<!--]-->";
pub const EMPTY_COMMENT: &str = "<!---->";

/// `escape_html(value, is_attr)`
pub fn escape_html(s: &str, is_attr: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '"' if is_attr => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
    out
}

/// A `TemplateElement` with `cooked` (raw is filled in from it later)
fn quasi(cooked: String, tail: bool) -> (String, bool) {
    (cooked, tail)
}

/// `b.template(quasis, expressions)` where each quasi's raw is `sanitize_template_string(cooked)`
pub fn template_from(quasis: Vec<(String, bool)>, expressions: Vec<Node>) -> Node {
    b::template(quasis.into_iter().map(|(cooked, tail)| b::quasi_with(&cooked, tail)).collect(), expressions)
}

/// The value of an evaluated expression as `(value ?? '') + ''` gives it
pub fn evaluated_string(v: &Option<crate::analyze::evaluate::Val>) -> String {
    use crate::analyze::evaluate::Val;
    match v {
        None | Some(Val::Undefined) | Some(Val::Null) => String::new(),
        Some(v) => v.to_js_string().unwrap_or_default(),
    }
}

impl<'a, 's> Server<'a, 's> {
    /// `process_children(nodes, { visit, state })`
    pub fn process_children(&mut self, nodes: &[Child<'s>], st: &State) {
        let mut sequence: Vec<&Child<'s>> = Vec::new();
        for node in nodes {
            if let Child::Node(n) = node {
                if let crate::ast::Node::ExpressionTag { expression, .. } = &self.ast().nodes[*n] {
                    let meta = self.meta_of_node(*n);
                    if self.meta_is_async(meta) {
                        self.flush_sequence(&mut sequence, st);
                        let expression = self.visit_template_expr_here(expression, st);
                        let has_await = self.an.metas[meta as usize].has_await;
                        let mut call = b::call("$$renderer.push", vec![b::thunk_with(b::call("$.escape", vec![expression]), has_await)]);
                        let blockers = self.meta_blockers_array(meta);
                        if !array_is_empty(&blockers) {
                            call = b::call("$$renderer.async", vec![blockers, b::arrow(vec![b::id("$$renderer")], call)]);
                        }
                        st.template.borrow_mut().push(b::stmt(call));
                        continue;
                    }
                }
            }
            if node.is_text_like(self.ast()) {
                sequence.push(node);
            } else {
                self.flush_sequence(&mut sequence, st);
                self.visit_child_node(node, st);
            }
        }
        self.flush_sequence(&mut sequence, st);
    }

    fn flush_sequence(&mut self, sequence: &mut Vec<&Child<'s>>, st: &State) {
        if sequence.is_empty() {
            return;
        }
        let mut quasis = vec![quasi(String::new(), false)];
        let mut expressions = Vec::new();
        let len = sequence.len();
        for (i, node) in sequence.iter().enumerate() {
            match node.text_data(self.ast()) {
                Some((data, is_comment)) => {
                    let last = quasis.last_mut().unwrap();
                    if is_comment {
                        last.0.push_str(&format!("<!--{data}-->"));
                    } else {
                        last.0.push_str(&escape_html(&data, false));
                    }
                }
                None => {
                    let Child::Node(n) = node else { continue };
                    let crate::ast::Node::ExpressionTag { expression, .. } = &self.ast().nodes[*n] else { continue };
                    let evaluated = self.evaluate_expr(expression, st.scope);
                    if evaluated.is_known {
                        quasis.last_mut().unwrap().0.push_str(&escape_html(&evaluated_string(&evaluated.value), false));
                    } else {
                        let e = self.visit_template_expr_here(expression, st);
                        expressions.push(b::call("$.escape", vec![e]));
                        quasis.push(quasi(String::new(), i + 1 == len));
                    }
                }
            }
        }
        st.template.borrow_mut().push(template_from(quasis, expressions));
        sequence.clear();
    }
}

fn array_is_empty(n: &Node) -> bool {
    matches!(&n.kind, NodeKind::ArrayExpression(a) if a.elements.is_empty())
}

/// `build_template(template)`: the template parts as `$$renderer.push(...)` statements
pub fn build_template(template: Vec<Node>) -> Vec<Node> {
    let mut strings: Vec<String> = Vec::new();
    let mut expressions: Vec<Node> = Vec::new();
    let mut statements = Vec::new();
    let flush = |strings: &mut Vec<String>, expressions: &mut Vec<Node>, statements: &mut Vec<Node>| {
        let n = strings.len();
        let quasis = strings.drain(..).enumerate().map(|(i, s)| (s, i == n - 1)).collect();
        statements.push(b::stmt(b::call(b::id("$$renderer.push"), vec![template_from(quasis, std::mem::take(expressions))])));
    };
    for node in template {
        if js::is_statement(&node) {
            if !strings.is_empty() {
                flush(&mut strings, &mut expressions, &mut statements);
            }
            statements.push(node);
        } else {
            if strings.is_empty() {
                strings.push(String::new());
            }
            if !matches!(node.kind, NodeKind::Literal(_) | NodeKind::TemplateLiteral(_)) {
                expressions.push(node);
                strings.push(String::new());
                continue;
            }
            match node.kind {
                NodeKind::Literal(l) => {
                    let s = match l.value {
                        LiteralValue::String(s) => s.to_string(),
                        LiteralValue::Number(n) => crate::analyze::evaluate::number_to_string(n),
                        LiteralValue::Boolean(b) => b.to_string(),
                        LiteralValue::Null => "null".into(),
                        _ => String::new(),
                    };
                    strings.last_mut().unwrap().push_str(&s);
                }
                NodeKind::TemplateLiteral(t) => {
                    let mut quasis = t.quasis.into_iter().map(|q| match q.kind {
                        NodeKind::TemplateElement(e) => e.cooked.map(|c| c.to_string()).unwrap_or_default(),
                        _ => String::new(),
                    });
                    if let Some(first) = quasis.next() {
                        strings.last_mut().unwrap().push_str(&first);
                    }
                    strings.extend(quasis);
                    expressions.extend(t.expressions);
                }
                _ => unreachable!(),
            }
        }
    }
    if !strings.is_empty() {
        flush(&mut strings, &mut expressions, &mut statements);
    }
    statements
}

/// `create_child_block(statements, blockers, has_await)`
pub fn create_child_block(statements: Vec<Node>, blockers: Node, has_await: bool) -> Vec<Node> {
    if array_is_empty(&blockers) && !has_await {
        return statements;
    }
    let f = b::arrow_with(vec![b::id("$$renderer")], b::block(statements), has_await);
    if !array_is_empty(&blockers) {
        vec![b::stmt(b::call("$$renderer.async_block", vec![blockers, f]))]
    } else {
        vec![b::stmt(b::call("$$renderer.child_block", vec![f]))]
    }
}

/// `PromiseOptimiser`
#[derive(Default)]
pub struct PromiseOptimiser {
    pub expressions: Vec<Node>,
    pub has_await: bool,
    /// blocker objects (and their expressions), deduplicated by identity
    blockers: Vec<(crate::analyze::blockers::Blocker, Node)>,
    /// `(expression) => expression` in place of `optimiser.transform`
    pub identity: bool,
}

impl PromiseOptimiser {
    /// A transform that returns expressions unchanged
    pub fn identity() -> Self {
        PromiseOptimiser { identity: true, ..Default::default() }
    }

    /// `transform(expression, metadata)`
    pub fn transform(&mut self, s: &Server, expression: Node, meta: u32) -> Node {
        if self.identity {
            return expression;
        }
        self.check_blockers(s, meta);
        if s.an.metas[meta as usize].has_await {
            self.has_await = true;
            self.expressions.push(expression);
            return b::id(format!("$${}", self.expressions.len() - 1).as_str());
        }
        expression
    }

    pub fn check_blockers(&mut self, s: &Server, meta: u32) {
        for &r in &s.an.metas[meta as usize].references {
            if let Some(bl) = s.binding(r).blocker {
                if !self.blockers.iter().any(|(b, _)| *b == bl) {
                    self.blockers.push((bl, blocker_expression(s, bl)));
                }
            }
        }
    }

    fn apply(&self) -> Node {
        if self.expressions.is_empty() {
            return b::empty();
        }
        if self.expressions.len() == 1 {
            return b::r#const(b::id("$$0"), self.expressions[0].clone());
        }
        let promises = b::array(
            self.expressions
                .iter()
                .map(|e| match &e.kind {
                    NodeKind::AwaitExpression(a) if !b::has_await_expression(&a.argument) => (*a.argument).clone(),
                    _ => b::call(b::thunk_with(e.clone(), true), ()),
                })
                .collect::<Vec<_>>(),
        );
        b::r#const(
            b::array_pattern((0..self.expressions.len()).map(|i| b::id(format!("$${i}").as_str())).collect::<Vec<_>>()),
            js::save(b::call("Promise.all", vec![promises])),
        )
    }

    pub fn blockers(&self) -> Node {
        b::array(self.blockers.iter().map(|(_, e)| e.clone()).collect::<Vec<_>>())
    }

    pub fn is_async(&self) -> bool {
        !self.expressions.is_empty() || !self.blockers.is_empty()
    }

    pub fn render(&self, statements: Vec<Node>) -> Vec<Node> {
        if !self.is_async() {
            return statements;
        }
        let mut body = vec![self.apply()];
        body.extend(statements);
        let f = b::arrow_with(vec![b::id("$$renderer")], b::block(body), self.has_await);
        let blockers = self.blockers();
        if !array_is_empty(&blockers) {
            vec![b::stmt(b::call("$$renderer.async", vec![blockers, f]))]
        } else {
            vec![b::stmt(b::call("$$renderer.child", vec![f]))]
        }
    }

    pub fn render_block(&self, statements: Vec<Node>) -> Vec<Node> {
        if !self.is_async() {
            return statements;
        }
        let mut body = vec![self.apply()];
        body.extend(statements);
        create_child_block(body, self.blockers(), self.has_await)
    }
}

/// `$$promises[i]`, or `promises[i]` of an async `{@const}`
pub fn blocker_expression(s: &Server, bl: crate::analyze::blockers::Blocker) -> Node {
    let object = match bl.object {
        Some(o) => s.an.promise_ids[o as usize].as_str(),
        None => "$$promises",
    };
    b::member_with(b::id(object), b::literal(bl.index as f64), true, false)
}

/// `prepend_block_marker(block, marker)`
pub fn prepend_block_marker(block: &mut Node, marker: &str) {
    let NodeKind::BlockStatement(bl) = &mut block.kind else { return };
    if let Some(first) = bl.body.first_mut() {
        if let NodeKind::ExpressionStatement(es) = &mut first.kind {
            if let NodeKind::CallExpression(c) = &mut es.expression.kind {
                let is_push = matches!(&c.callee.kind, NodeKind::Identifier(i) if i.name == "$$renderer.push");
                if is_push && c.arguments.len() == 1 {
                    if let NodeKind::TemplateLiteral(t) = &mut c.arguments[0].kind {
                        if let NodeKind::TemplateElement(q) = &mut t.quasis[0].kind {
                            let cooked = q.cooked.as_ref().map(|c| c.to_string()).unwrap_or_default();
                            q.cooked = Some(format!("{marker}{cooked}").as_str().into());
                            q.raw = format!("{marker}{}", q.raw).as_str().into();
                            return;
                        }
                    }
                }
            }
        }
    }
    bl.body.insert(0, b::stmt(b::call(b::id("$$renderer.push"), vec![b::literal(marker)])));
}
