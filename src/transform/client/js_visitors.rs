//! The client transform's JS visitors (`visitors/*.js` for ESTree nodes)

use std::rc::Rc;

use crate::analyze::scope::{DeclKind, Kind};
use crate::estree::builders as b;
use crate::estree::{Node, NodeKind};

use super::super::js::{self, PathNode};
use super::{call_fn, copy_transform, get_value, get_value_fn, Client, State, Transform};

impl<'a, 's> Client<'a, 's> {
    /// `context.visit(node, state)` on a JS node: the universal `set_scope` visitor, then the
    /// node's visitor
    pub fn visit_js(&mut self, node: &Node, st: &State) -> Node {
        let scoped;
        let st = match node.origin.and_then(|o| self.scope_of_key(o)) {
            Some(scope) if scope != st.scope => {
                scoped = self.with_scope(scope, st);
                &scoped
            }
            _ => st,
        };
        match &node.kind {
            NodeKind::Identifier(_) => self.identifier(node, st),
            NodeKind::MemberExpression(_) => self.member_expression(node, st),
            NodeKind::UpdateExpression(_) => self.update_expression(node, st),
            NodeKind::AssignmentExpression(_) => self.assignment_expression(node, st),
            NodeKind::CallExpression(_) => self.call_expression(node, st),
            NodeKind::ClassBody(_) => self.class_body(node, st),
            NodeKind::LabeledStatement(_) => self.labeled_statement(node, st),
            NodeKind::VariableDeclaration(_) => self.variable_declaration(node, st),
            NodeKind::Program(_) => self.program(node, st),
            NodeKind::ExpressionStatement(_) => self.expression_statement(node, st),
            NodeKind::ExportNamedDeclaration(_) => self.export_named_declaration(node, st),
            NodeKind::BlockStatement(_) => self.block_statement(node, st),
            NodeKind::BreakStatement(_) => self.break_statement(node, st),
            NodeKind::AwaitExpression(_) => self.await_expression(node, st),
            NodeKind::BinaryExpression(_) => self.binary_expression(node, st),
            NodeKind::ArrowFunctionExpression(_) | NodeKind::FunctionExpression(_) => self.visit_function(node, st),
            NodeKind::FunctionDeclaration(_) => {
                let state = State { in_constructor: false, in_derived: false, ..st.clone() };
                self.next(node, &state)
            }
            NodeKind::ForOfStatement(_) => self.for_of_statement(node, st),
            _ => self.next(node, st),
        }
    }

    /// `context.next(state)`: the node with its children visited
    pub fn next(&mut self, node: &Node, st: &State) -> Node {
        self.path.push(PathNode::Js(node));
        let out = node.map_children(&mut |c| self.visit_js(c, st));
        self.path.pop();
        out
    }

    /// `context.visit(child)` from the visitor of `node`
    pub fn visit_in(&mut self, node: &Node, child: &Node, st: &State) -> Node {
        self.path.push(PathNode::Js(node));
        let out = self.visit_js(child, st);
        self.path.pop();
        out
    }

    fn parent_js(&self) -> Option<&Node> {
        self.path.last().and_then(|p| p.js())
    }

    /// The `type` of `context.path.at(-i)`
    pub fn path_type(&self, i: usize) -> Option<&'static str> {
        let len = self.path.len();
        if i > len {
            return None;
        }
        Some(self.path[len - i].ty(self.an.ast))
    }

    fn identifier(&mut self, node: &Node, st: &State) -> Node {
        let parent = self.parent_js();
        let is_ref = match self.path.last() {
            Some(PathNode::Js(_)) | None => js::is_reference(node, parent),
            Some(PathNode::Tpl(_)) => true,
        };
        if !is_ref {
            return node.clone();
        }
        let name = js::ident(node).unwrap();
        if name == "$$props" {
            return b::id("$$sanitized_props");
        }
        // reading a member of a rest prop can use `$$props` directly
        let binding = self.get(st.scope, name);
        if self.an.runes {
            if let Some(bid) = binding {
                let bnd = self.binding(bid);
                if node.origin.is_none_or(|o| o != bnd.node.key) && bnd.kind == Kind::RestProp {
                    let grand_parent = self.path_type(2);
                    if let Some(NodeKind::MemberExpression(m)) = parent.map(|p| &p.kind) {
                        if !m.computed && grand_parent != Some("AssignmentExpression") && grand_parent != Some("UpdateExpression") {
                            let key = js::ident(&m.property).unwrap_or("").to_string();
                            let excluded = self.an.exclude_props.get(&bnd.node.key).is_some_and(|e| e.contains(&key));
                            if !excluded {
                                return b::id("$$props");
                            }
                        }
                    }
                }
            }
        }
        self.build_getter(node, st)
    }

    fn state_field(&self, st: &State, name: &str) -> Option<usize> {
        self.an.state_fields[st.state_fields as usize].iter().position(|f| f.name == name)
    }

    fn member_expression(&mut self, node: &Node, st: &State) -> Node {
        let NodeKind::MemberExpression(m) = &node.kind else { unreachable!() };
        if let NodeKind::PrivateIdentifier(p) = &m.property.kind {
            let name = format!("#{}", p.name);
            if let Some(i) = self.state_field(st, &name) {
                let field = &self.an.state_fields[st.state_fields as usize][i];
                return if st.in_constructor && matches!(field.rune, "$state.raw" | "$state") {
                    b::member(node.clone(), "v")
                } else {
                    b::call("$.get", vec![node.clone()])
                };
            }
        }
        self.next(node, st)
    }

    fn update_expression(&mut self, node: &Node, st: &State) -> Node {
        let NodeKind::UpdateExpression(u) = &node.kind else { unreachable!() };
        let argument = &*u.argument;
        if let NodeKind::MemberExpression(m) = &argument.kind {
            if let NodeKind::PrivateIdentifier(p) = &m.property.kind {
                if self.state_field(st, &format!("#{}", p.name)).is_some() {
                    let f = if u.prefix { "$.update_pre" } else { "$.update" };
                    let mut args = vec![argument.clone()];
                    if u.operator.as_str() == "--" {
                        args.push(b::literal(-1.0));
                    }
                    return b::call(f, args);
                }
            }
        }
        let left = js::object(argument).cloned();
        let transformers = left.as_ref().and_then(|l| st.transform.borrow().get(js::ident(l).unwrap()).cloned());
        let left_is_argument = matches!(argument.kind, NodeKind::Identifier(_));
        if left_is_argument {
            if let Some(update) = transformers.as_ref().and_then(|t| t.update.clone()) {
                return update(self, node);
            }
        }
        let mut update = self.next(node, st);
        if let (Some(left), Some(mutate)) = (&left, transformers.as_ref().and_then(|t| t.mutate.clone())) {
            update = mutate(self, left, update);
        }
        self.validate_mutation(node, st, update)
    }

    fn assignment_expression(&mut self, node: &Node, st: &State) -> Node {
        let expression = match self.visit_assignment_expression(node, st) {
            Some(e) => e,
            None => self.next(node, st),
        };
        self.validate_mutation(node, st, expression)
    }

    /// `visit_assignment_expression(node, context, build_assignment)`
    fn visit_assignment_expression(&mut self, node: &Node, st: &State) -> Option<Node> {
        let NodeKind::AssignmentExpression(a) = &node.kind else { unreachable!() };
        if matches!(a.left.kind, NodeKind::ArrayPattern(_) | NodeKind::ObjectPattern(_) | NodeKind::RestElement(_)) {
            let value = self.visit_in(node, &a.right, st);
            let should_cache = !matches!(value.kind, NodeKind::Identifier(_));
            let rhs = if should_cache { b::id("$$value") } else { value.clone() };
            let (inserts, paths) = js::extract_paths(&a.left, rhs.clone());
            let names: Vec<String> = inserts.iter().map(|_| self.generate(st.scope, "$$array")).collect();
            let mut changed = false;
            let mut assignments = Vec::new();
            for path in paths {
                let mut value = path.expression;
                js::rename_placeholders(&mut value, &names);
                let assignment = self.build_assignment("=", &path.node, &value, node, false, st);
                if assignment.is_some() {
                    changed = true;
                }
                assignments.push(match assignment {
                    Some(a) => a,
                    None => {
                        let left = self.visit_in(node, &path.node, st);
                        let right = self.visit_in(node, &value, st);
                        b::assignment("=", left, right)
                    }
                });
            }
            if !changed {
                return None;
            }
            let is_standalone = self.path_type(1).is_some_and(|t| t.ends_with("Statement"));
            if !inserts.is_empty() || should_cache {
                let mut statements: Vec<Node> = inserts
                    .into_iter()
                    .enumerate()
                    .map(|(i, (_, mut v))| {
                        js::rename_placeholders(&mut v, &names);
                        b::var(b::id(names[i].as_str()), v)
                    })
                    .collect();
                statements.extend(assignments.iter().cloned().map(b::stmt));
                if !is_standalone {
                    statements.push(b::r#return(rhs.clone()));
                }
                let is_async = js::is_expression_async(&value) || assignments.iter().any(js::is_expression_async);
                let iife = b::arrow_with(vec![rhs], b::block(statements), is_async);
                let call = b::call(iife, vec![value]);
                return Some(if is_async { b::r#await(call) } else { call });
            }
            let mut expressions = assignments;
            if !is_standalone {
                expressions.push(rhs);
            }
            return Some(b::sequence(expressions));
        }
        let operator = a.operator.as_str();
        self.build_assignment(operator, &a.left, &a.right, node, true, st)
    }

    /// The client's `build_assignment(operator, left, right, context)`. `direct` is set when
    /// `left` is `node.left` (not a destructured part).
    fn build_assignment(&mut self, operator: &str, left: &Node, right: &Node, node: &Node, direct: bool, st: &State) -> Option<Node> {
        if self.an.runes {
            if let NodeKind::MemberExpression(m) = &left.kind {
                let name = js::get_name(&m.property);
                let field = name.as_deref().and_then(|n| self.state_field(st, n));
                if let (Some(name), Some(fi)) = (name, field) {
                    let (is_assignment, node_key, rune_ty, key) = {
                        let f = &self.an.state_fields[st.state_fields as usize][fi];
                        (f.is_assignment, f.node_key, f.rune, f.key.clone())
                    };
                    // state declaration in a class constructor
                    if is_assignment && direct && node.origin == Some(node_key) {
                        let rune = js::get_rune(Some(right), &self.an.sc, st.scope);
                        if let Some(rune) = rune {
                            let child_state = State { in_constructor: rune != "$derived" && rune != "$derived.by", ..st.clone() };
                            let mut value = self.visit_in(node, right, &child_state);
                            if self.dev {
                                let class_name = self.enclosing_class_name();
                                value = b::call("$.tag", vec![value, b::literal(format!("{}.{}", class_name, name))]);
                            }
                            return Some(b::assignment(operator, b::member(b::this(), b::private_id(key.as_str())), value));
                        }
                    }
                    // assignment to a private state field
                    if let NodeKind::PrivateIdentifier(_) = &m.property.kind {
                        let logical = matches!(operator, "||=" | "&&=" | "??=");
                        let to_visit = if logical { right.clone() } else { js::build_assignment_value(operator, left.clone(), right.clone()) };
                        let value = self.visit_in(node, &to_visit, st);
                        let needs_proxy = rune_ty == "$state" && is_non_coercive_operator(operator) && self.should_proxy(&value, st.scope);
                        let assignment = b::call("$.set", vec![Some(left.clone()), Some(value), if needs_proxy { Some(b::r#true()) } else { None }]);
                        return Some(if !logical {
                            assignment
                        } else {
                            let l = self.visit_in(node, left, st);
                            b::logical(&operator[..operator.len() - 1], l, assignment)
                        });
                    }
                }
            }
        }

        let mut object = left;
        while let NodeKind::MemberExpression(m) = &object.kind {
            object = &m.object;
        }
        let name = js::ident(object)?.to_string();
        let bid = self.get(st.scope, &name)?;
        let transform = st.transform.borrow().get(name.as_str()).cloned();

        // reassignment
        if std::ptr::eq(object, left) {
            if let Some(assign) = transform.as_ref().and_then(|t| t.assign.clone()) {
                let is_primitive = self.path_type(1) == Some("BindDirective") && self.path_type(2) == Some("RegularElement");
                let value_node = js::build_assignment_value(operator, left.clone(), right.clone());
                let value = self.visit_in(node, &value_node, st);
                let kind = self.binding(bid).kind;
                let proxy = !is_primitive
                    && !matches!(kind, Kind::Prop | Kind::BindableProp | Kind::RawState | Kind::Derived | Kind::StoreSub)
                    && self.an.runes
                    && self.should_proxy(right, st.scope)
                    && is_non_coercive_operator(operator);
                return Some(assign(self, object, value, proxy));
            }
        }

        // mutation
        if let Some(mutate) = transform.as_ref().and_then(|t| t.mutate.clone()) {
            let l = self.visit_in(node, left, st);
            let r = self.visit_in(node, right, st);
            let mut mutation = mutate(self, object, b::assignment(operator, l, r));
            let indirect = self.an.legacy_indirect_bindings.get(&bid).cloned().unwrap_or_default();
            if !indirect.is_empty() {
                let mut stmts = Vec::new();
                for ib in indirect {
                    let id = b::id(self.binding(ib).node.name);
                    stmts.push(b::stmt(self.build_getter(&id, st)));
                }
                mutation = b::sequence(vec![mutation, b::call("$.invalidate_inner_signals", vec![b::arrow(vec![], b::block(stmts))])]);
            }
            return Some(mutation);
        }

        // `(object.items ??= []).push(value)` and the like: `$.assign(...)` in dev
        let mut should_transform = self.dev
            && self.path_type(1) != Some("ExpressionStatement")
            && is_non_coercive_operator(operator)
            && !self.evaluate(right, st.scope).is_primitive;

        // `onclick={() => (...)}`
        if self.path_type(1) == Some("ArrowFunctionExpression") && matches!(self.path_type(2), Some("RegularElement" | "SvelteElement")) {
            let len = self.path.len();
            if let (PathNode::Tpl(crate::analyze::nodes::P::Node(el)), Some(arrow)) = (self.path[len - 2], self.path[len - 1].js()) {
                if let crate::ast::Node::Element(e) = &self.ast().nodes[el] {
                    let arrow_origin = arrow.origin;
                    let found = e.attributes.iter().any(|a| {
                        crate::analyze::utils::is_event_attribute(a)
                            && crate::analyze::utils::value_expression(match a {
                                crate::ast::Attr::Attribute { value, .. } => value,
                                _ => return false,
                            })
                            .is_some_and(|ex| match ex {
                                crate::ast::Expr::Js(j) => arrow_origin == Some(crate::analyze::nodes::template_expr(ex).key()) || {
                                    let _ = j;
                                    false
                                },
                                _ => false,
                            })
                    });
                    if found {
                        should_transform = false;
                    }
                }
            }
        }

        let p1 = self.path_type(1);
        let p2 = self.path_type(2);
        let p3 = self.path_type(3);
        if matches!(p1, Some("BindDirective" | "Component" | "SvelteComponent"))
            || (p1 == Some("ArrowFunctionExpression")
                && (p2 == Some("BindDirective")
                    || (p2 == Some("Component") && p3 == Some("Fragment"))
                    || (p2 == Some("SequenceExpression") && matches!(p3, Some("Component" | "SvelteComponent" | "BindDirective")))))
        {
            should_transform = false;
        }

        if let NodeKind::MemberExpression(m) = &left.kind {
            if should_transform {
                let needs_lazy_getter = operator != "=";
                let needs_async = needs_lazy_getter && js::is_expression_async(right);
                let property = if m.computed { (*m.property).clone() } else { b::literal(js::ident(&m.property).unwrap_or("")) };
                let loc = self.locate_node(left.start().unwrap_or(0) as usize);
                let mut e = b::call(
                    if needs_async { "$.assign_async" } else { "$.assign" },
                    vec![
                        (*m.object).clone(),
                        property,
                        b::literal(operator),
                        if needs_lazy_getter { b::arrow_with(vec![], right.clone(), needs_async) } else { right.clone() },
                        b::literal(loc.as_str()),
                    ],
                );
                if needs_async {
                    e = b::r#await(e);
                }
                return Some(self.visit_in(node, &e, st));
            }
        }
        None
    }

    /// The name of the class around the current node (`[class]` if anonymous)
    fn enclosing_class_name(&self) -> String {
        for p in self.path.iter().rev() {
            if let Some(n) = p.js() {
                match &n.kind {
                    NodeKind::ClassDeclaration(c) | NodeKind::ClassExpression(c) => {
                        return c.id.as_deref().and_then(js::ident).unwrap_or("[class]").to_string();
                    }
                    _ => {}
                }
            }
        }
        "[class]".into()
    }

    /// `validate_mutation(node, context, expression)`
    pub fn validate_mutation(&mut self, node: &Node, st: &State, expression: Node) -> Node {
        let left = match &node.kind {
            NodeKind::AssignmentExpression(a) => &*a.left,
            NodeKind::UpdateExpression(u) => &*u.argument,
            _ => return expression,
        };
        if !self.dev || !left.is("MemberExpression") || self.is_ignored(node, "ownership_invalid_mutation") {
            return expression;
        }
        let Some(name) = js::object(left).and_then(js::ident).map(str::to_string) else { return expression };
        let Some(bid) = self.get(st.scope, &name) else { return expression };
        let binding_kind = self.binding(bid).kind;
        if !matches!(binding_kind, Kind::Prop | Kind::BindableProp) {
            return expression;
        }
        self.needs_mutation_validation = true;
        let mut path: Vec<Node> = Vec::new();
        let mut l = left;
        while let NodeKind::MemberExpression(m) = &l.kind {
            match &m.property.kind {
                NodeKind::Literal(_) => path.insert(0, (*m.property).clone()),
                NodeKind::Identifier(i) => {
                    let transform = st.transform.borrow().get(i.name.as_str()).cloned();
                    if m.computed {
                        let v = match transform {
                            Some(t) => (t.read)(self, &m.property),
                            None => (*m.property).clone(),
                        };
                        path.insert(0, v);
                    } else {
                        path.insert(0, b::literal(i.name.as_str()));
                    }
                }
                _ => return expression,
            }
            l = &m.object;
        }
        path.insert(0, b::literal(name.as_str()));
        let (line, column) = self.locate(l.start().unwrap_or(0) as usize);
        let alias = self.binding(bid).prop_alias;
        b::call(
            "$$ownership_validator.mutation",
            vec![
                match alias {
                    Some(a) => b::literal(a),
                    None => b::null(),
                },
                b::array(path),
                expression,
                b::literal(line as f64),
                b::literal(column as f64),
            ],
        )
    }

    fn call_expression(&mut self, node: &Node, st: &State) -> Node {
        let NodeKind::CallExpression(c) = &node.kind else { unreachable!() };
        let rune = js::get_rune(Some(node), &self.an.sc, st.scope);
        let first = || c.arguments.first();
        match rune {
            Some("$host") => return b::id("$$props.$$host"),
            Some("$effect.tracking") => return b::call("$.effect_tracking", ()),
            Some(r @ ("$state" | "$state.raw")) => {
                let mut value = None;
                if let Some(arg) = first() {
                    let mut v = self.visit_in(node, arg, st);
                    if r == "$state" && self.should_proxy(arg, st.scope) {
                        v = b::call("$.proxy", vec![v]);
                    }
                    value = Some(v);
                }
                let mut callee = b::id("$.state");
                callee.loc = c.callee.loc;
                return b::call(callee, vec![value]);
            }
            Some(r @ ("$derived" | "$derived.by")) => {
                let arg = first().cloned().unwrap_or_else(b::void0);
                let f = self.visit_in(node, &arg, st);
                return b::call("$.derived", vec![if r == "$derived" { b::thunk(f) } else { f }]);
            }
            Some("$state.eager") => {
                let arg = first().cloned().unwrap_or_else(b::void0);
                let v = self.visit_in(node, &arg, st);
                return b::call("$.eager", vec![b::thunk(v)]);
            }
            Some("$state.snapshot") => {
                let arg = first().cloned().unwrap_or_else(b::void0);
                let v = self.visit_in(node, &arg, st);
                let ignored = self.is_ignored(node, "state_snapshot_uncloneable");
                return b::call("$.snapshot", vec![Some(v), if ignored { Some(b::r#true()) } else { None }]);
            }
            Some(r @ ("$effect" | "$effect.pre")) => {
                let callee = if r == "$effect" { "$.user_effect" } else { "$.user_pre_effect" };
                let arg = first().cloned().unwrap_or_else(b::void0);
                let func = self.visit_in(node, &arg, st);
                let mut expr = b::call(callee, vec![func]);
                if let NodeKind::CallExpression(ce) = &mut expr.kind {
                    ce.callee.loc = c.callee.loc;
                }
                return expr;
            }
            Some("$effect.root") => {
                let args: Vec<Node> = c.arguments.iter().map(|a| self.visit_in(node, a, st)).collect();
                return b::call("$.effect_root", args);
            }
            Some("$effect.pending") => return b::call("$.eager", vec![b::thunk(b::call("$.pending", ()))]),
            Some(r @ ("$inspect" | "$inspect().with")) => return self.transform_inspect_rune(r, node, st),
            _ => {}
        }

        if self.dev {
            if let NodeKind::MemberExpression(m) = &c.callee.kind {
                if js::ident(&m.object) == Some("console")
                    && self.get(st.scope, "console").is_none()
                    && matches!(js::ident(&m.property), Some("debug" | "dir" | "error" | "group" | "groupCollapsed" | "info" | "log" | "trace" | "warn"))
                    && c.arguments.iter().any(|a| a.is("SpreadElement") || self.evaluate(a, st.scope).has_unknown)
                {
                    let mut args = vec![b::literal(js::ident(&m.property).unwrap())];
                    for a in &c.arguments {
                        args.push(self.visit_in(node, a, st));
                    }
                    return b::call((*c.callee).clone(), vec![b::spread(b::call("$.log_if_contains_state", args))]);
                }
            }
        }
        self.next(node, st)
    }

    fn transform_inspect_rune(&mut self, rune: &str, node: &Node, st: &State) -> Node {
        if !self.dev {
            return b::empty();
        }
        let NodeKind::CallExpression(c) = &node.kind else { unreachable!() };
        let call = if rune == "$inspect" {
            node
        } else {
            match &c.callee.kind {
                NodeKind::MemberExpression(m) => &*m.object,
                _ => node,
            }
        };
        let NodeKind::CallExpression(inner) = &call.kind else { unreachable!() };
        let args: Vec<Node> = inner.arguments.iter().map(|a| self.visit_in(node, a, st)).collect();
        let inspector = if rune == "$inspect" { b::id("console.log") } else { self.visit_in(node, &c.arguments[0], st) };
        let id = b::id("$$args");
        let f = b::arrow(vec![b::rest(id.clone())], b::call(inspector, vec![b::spread(id)]));
        b::call("$.inspect", vec![Some(b::thunk(b::array(args))), Some(f), if rune == "$inspect" { Some(b::r#true()) } else { None }])
    }

    fn class_body(&mut self, node: &Node, st: &State) -> Node {
        let NodeKind::ClassBody(body) = &node.kind else { unreachable!() };
        let Some(&fields_idx) = node.origin.and_then(|o| self.an.classes.get(&o)) else {
            return self.next(node, st);
        };
        let fields: Vec<_> = self.an.state_fields[fields_idx as usize].iter().map(|f| (f.name.clone(), f.is_assignment, f.rune, f.key.clone(), f.node_key, f.value_key)).collect();
        let child_state = State { state_fields: fields_idx, ..st.clone() };
        let mut out = Vec::new();

        for (name, is_assignment, rune, key, _, _) in &fields {
            if name.starts_with('#') {
                continue;
            }
            // backing fields for state declared in the constructor
            if *is_assignment {
                let member = b::member(b::this(), b::private_id(key.as_str()));
                let should_proxy = *rune == "$state";
                let k = b::key(name);
                out.push(b::prop_def(b::private_id(key.as_str()), None));
                out.push(b::method("get", k.clone(), vec![], vec![b::r#return(b::call("$.get", vec![member.clone()]))]));
                out.push(b::method(
                    "set",
                    k,
                    vec![b::id("value")],
                    vec![b::stmt(b::call("$.set", vec![Some(member), Some(b::id("value")), if should_proxy { Some(b::r#true()) } else { None }]))],
                ));
            }
        }

        let class_name = match self.parent_js().map(|p| &p.kind) {
            Some(NodeKind::ClassDeclaration(c) | NodeKind::ClassExpression(c)) => c.id.as_deref().and_then(js::ident).unwrap_or("[class]").to_string(),
            _ => "[class]".to_string(),
        };

        self.path.push(PathNode::Js(node));
        for definition in &body.body {
            let NodeKind::PropertyDefinition(d) = &definition.kind else {
                let v = self.visit_js(definition, &child_state);
                out.push(v);
                continue;
            };
            let name = js::get_name(&d.key);
            let field = name.as_ref().and_then(|n| fields.iter().find(|f| &f.0 == n));
            let Some((name, _, rune, key, node_key, value_key)) = field.cloned() else {
                let v = self.visit_js(definition, &child_state);
                out.push(v);
                continue;
            };
            if name.starts_with('#') {
                let mut value = d.value.as_deref().map(|v| {
                    self.path.push(PathNode::Js(definition));
                    let r = self.visit_js(v, &child_state);
                    self.path.pop();
                    r
                });
                if self.dev && definition.origin == Some(node_key) {
                    value = Some(b::call("$.tag", vec![value.unwrap_or_else(b::void0), b::literal(format!("{}.{}", class_name, name))]));
                }
                out.push(b::prop_def((*d.key).clone(), value));
            } else if definition.origin == Some(node_key) {
                let value = self.js_node_by_key(value_key).unwrap_or_else(b::void0);
                self.path.push(PathNode::Js(definition));
                let mut call = self.visit_js(&value, &child_state);
                self.path.pop();
                if self.dev {
                    call = b::call("$.tag", vec![call, b::literal(format!("{}.{}", class_name, name))]);
                }
                let member = b::member(b::this(), b::private_id(key.as_str()));
                let should_proxy = rune == "$state";
                out.push(b::prop_def(b::private_id(key.as_str()), call));
                out.push(b::method("get", (*d.key).clone(), vec![], vec![b::r#return(b::call("$.get", vec![member.clone()]))]));
                out.push(b::method(
                    "set",
                    (*d.key).clone(),
                    vec![b::id("value")],
                    vec![b::stmt(b::call("$.set", vec![Some(member), Some(b::id("value")), if should_proxy { Some(b::r#true()) } else { None }]))],
                ));
            }
        }
        self.path.pop();
        let mut result = node.clone();
        if let NodeKind::ClassBody(b) = &mut result.kind {
            b.body = out;
        }
        result
    }

    /// The converted node of a script node by its key
    pub fn js_node_by_key(&self, key: usize) -> Option<Node> {
        // SAFETY: see `js_node`
        self.js_nodes.get(&key).map(|&n| unsafe { (*n).clone() })
    }

    fn labeled_statement(&mut self, node: &Node, st: &State) -> Node {
        let NodeKind::LabeledStatement(l) = &node.kind else { unreachable!() };
        if self.an.runes || self.path.len() > 1 || js::ident(&l.label) != Some("$") {
            return self.next(node, st);
        }
        let start = node.span.map_or(0, |s| s.start) as usize;
        let Some(rs) = self.an.reactive_statements.iter().position(|r| r.node_start == start) else {
            return node.clone();
        };
        let mut serialized_body = self.visit_in(node, &l.body, st);
        if !serialized_body.is("BlockStatement") {
            serialized_body = b::block(vec![serialized_body]);
        }
        let NodeKind::BlockStatement(body) = serialized_body.kind else { unreachable!() };
        let mut sequence = Vec::new();
        let deps = self.an.reactive_statements[rs].dependencies.clone();
        for bid in deps {
            let binding = self.binding(bid);
            if binding.kind == Kind::Normal && binding.declaration_kind != DeclKind::Import {
                continue;
            }
            let name = binding.node.name;
            let kind = binding.kind;
            let mut serialized = self.build_getter(&b::id(name), st);
            if name == "$$props" || name == "$$restProps" || kind == Kind::BindableProp {
                serialized = b::call("$.deep_read_state", vec![serialized]);
            }
            sequence.push(serialized);
        }
        let deps_thunk = if sequence.is_empty() { b::thunk(b::block(vec![])) } else { b::thunk(b::sequence(sequence)) };
        let statement = b::stmt(b::call("$.legacy_pre_effect", vec![deps_thunk, b::thunk(b::block(body.body))]));
        self.legacy_reactive_statements.push((start as u32, statement));
        b::empty()
    }

    fn program(&mut self, node: &Node, st: &State) -> Node {
        if !self.an.runes {
            st.transform.borrow_mut().insert(
                "$$props".into(),
                Transform::read(Rc::new(|_, node| {
                    let mut n = node.clone();
                    if let NodeKind::Identifier(i) = &mut n.kind {
                        i.name = "$$sanitized_props".into();
                    }
                    n
                })),
            );
            let decls: Vec<(String, crate::analyze::scope::BindingId)> =
                self.an.sc.scope(st.scope).declarations.iter().map(|(n, b)| (n.to_string(), *b)).collect();
            for (name, bid) in decls {
                let binding = self.binding(bid);
                if binding.declaration_kind == DeclKind::Import && binding.mutated {
                    let is_instance_import = match (binding.initial, &self.an.root.instance) {
                        (Some(init), Some(script)) => init
                            .span(self.an.ast)
                            .is_some_and(|(s, e)| s > script.content.start && e < script.content.end),
                        _ => false,
                    };
                    if is_instance_import {
                        let id = b::id(format!("$$_import_{name}"));
                        let read_id = id.clone();
                        let mutate_id = id.clone();
                        st.transform.borrow_mut().insert(
                            name.clone(),
                            Transform {
                                read: Rc::new(move |_, _| b::call(read_id.clone(), ())),
                                assign: None,
                                mutate: Some(Rc::new(move |_, _, mutation| b::call(mutate_id.clone(), vec![mutation]))),
                                update: None,
                            },
                        );
                        self.legacy_reactive_imports.push(b::var(id, b::call("$.reactive_import", vec![b::thunk(b::id(name.as_str()))])));
                    }
                }
            }
        }

        let decls: Vec<(String, crate::analyze::scope::BindingId)> =
            self.an.sc.scope(st.scope).declarations.iter().map(|(n, b)| (n.to_string(), *b)).collect();
        for (name, bid) in decls {
            let kind = self.binding(bid).kind;
            if kind == Kind::StoreSub {
                let store_name = name[1..].to_string();
                let program_state = st.clone();
                let get_store = {
                    let store_name = store_name.clone();
                    let program_state = program_state.clone();
                    Rc::new(move |c: &mut Client| -> Node {
                        if let Some(n) = c.store_cache.get(&store_name) {
                            return n.clone();
                        }
                        let n = c.build_getter(&b::id(store_name.as_str()), &program_state);
                        c.store_cache.insert(store_name.clone(), n.clone());
                        n
                    })
                };
                let gs_assign = get_store.clone();
                let gs_mutate = get_store.clone();
                let update_state = program_state.clone();
                let update_store = store_name.clone();
                st.transform.borrow_mut().insert(
                    name.clone(),
                    Transform {
                        read: call_fn(),
                        assign: Some(Rc::new(move |c, _, value, _| {
                            let s = gs_assign(c);
                            b::call("$.store_set", vec![s, value])
                        })),
                        mutate: Some(Rc::new(move |c, node, mutation| {
                            let untracked = b::call("$.untrack", vec![node.clone()]);
                            fn replace(n: &Node, untracked: &Node) -> Node {
                                if let NodeKind::MemberExpression(m) = &n.kind {
                                    let mut out = n.clone();
                                    if let NodeKind::MemberExpression(om) = &mut out.kind {
                                        om.object = Box::new(replace(&m.object, untracked));
                                    }
                                    return out;
                                }
                                untracked.clone()
                            }
                            let store = gs_mutate(c);
                            let m = match mutation.kind {
                                NodeKind::AssignmentExpression(a) => {
                                    b::assignment(a.operator.as_str(), replace(&a.left, &untracked), *a.right)
                                }
                                NodeKind::UpdateExpression(u) => b::update_with(u.operator.as_str(), replace(&u.argument, &untracked), u.prefix),
                                kind => Node::new(kind),
                            };
                            b::call("$.store_mutate", vec![store, m, untracked])
                        })),
                        update: Some(Rc::new(move |c, node| {
                            let NodeKind::UpdateExpression(u) = &node.kind else { return node.clone() };
                            let getter = c.build_getter(&b::id(update_store.as_str()), &update_state);
                            b::call(
                                if u.prefix { "$.update_pre_store" } else { "$.update_store" },
                                vec![Some(getter), Some(b::call((*u.argument).clone(), ())), if u.operator.as_str() == "--" { Some(b::literal(-1.0)) } else { None }],
                            )
                        })),
                    },
                );
            }

            if matches!(kind, Kind::Prop | Kind::BindableProp) {
                if self.is_prop_source(bid) {
                    let bindable = kind == Kind::BindableProp;
                    st.transform.borrow_mut().insert(
                        name.clone(),
                        Transform {
                            read: call_fn(),
                            assign: Some(Rc::new(|_, node, value, _| b::call(node.clone(), vec![value]))),
                            mutate: Some(Rc::new(move |_, node, value| {
                                if bindable {
                                    // only necessary for interop with legacy parent bindings
                                    return b::call(node.clone(), vec![value, b::r#true()]);
                                }
                                value
                            })),
                            update: Some(Rc::new(|_, node| {
                                let NodeKind::UpdateExpression(u) = &node.kind else { return node.clone() };
                                b::call(
                                    if u.prefix { "$.update_pre_prop" } else { "$.update_prop" },
                                    vec![Some((*u.argument).clone()), if u.operator.as_str() == "--" { Some(b::literal(-1.0)) } else { None }],
                                )
                            })),
                        },
                    );
                } else if let Some(alias) = self.binding(bid).prop_alias {
                    let key = b::key(alias);
                    let computed = key.is("Literal");
                    st.transform
                        .borrow_mut()
                        .insert(name.clone(), Transform::read(Rc::new(move |_, _| b::member_with(b::id("$$props"), key.clone(), computed, false))));
                } else {
                    st.transform.borrow_mut().insert(name.clone(), Transform::read(Rc::new(|_, node| b::member(b::id("$$props"), node.clone()))));
                }
            }
        }

        self.add_state_transformers(st);

        if st.is_instance {
            self.path.push(PathNode::Js(node));
            let body = self.instance_body(st);
            self.path.pop();
            let mut out = node.map_children(&mut |c| c.clone());
            if let NodeKind::Program(p) = &mut out.kind {
                p.body = body;
            }
            return out;
        }
        self.next(node, st)
    }

    /// `add_state_transformers(context)`
    pub fn add_state_transformers(&mut self, st: &State) {
        let decls: Vec<(String, crate::analyze::scope::BindingId)> =
            self.an.sc.scope(st.scope).declarations.iter().map(|(n, b)| (n.to_string(), *b)).collect();
        for (name, bid) in decls {
            let binding = self.binding(bid);
            if self.is_state_source(bid) || matches!(binding.kind, Kind::Derived | Kind::LegacyReactive) {
                let is_var = binding.declaration_kind == DeclKind::Var;
                let scope = st.scope;
                let runes = self.an.runes;
                st.transform.borrow_mut().insert(
                    name,
                    Transform {
                        read: if is_var { Rc::new(|_, node| b::call("$.safe_get", vec![node.clone()])) } else { get_value_fn() },
                        assign: Some(Rc::new(move |c, node, value, proxy| {
                            let mut call = b::call("$.set", vec![Some(node.clone()), Some(value), if proxy { Some(b::r#true()) } else { None }]);
                            let store = format!("${}", js::ident(node).unwrap_or(""));
                            if c.get(scope, &store).is_some_and(|b| c.binding(b).kind == Kind::StoreSub) {
                                call = b::call("$.store_unsub", vec![call, b::literal(store.as_str()), b::id("$$stores")]);
                            }
                            call
                        })),
                        mutate: Some(Rc::new(move |_, node, mutation| {
                            if runes {
                                return mutation;
                            }
                            b::call("$.mutate", vec![node.clone(), mutation])
                        })),
                        update: Some(Rc::new(|_, node| {
                            let NodeKind::UpdateExpression(u) = &node.kind else { return node.clone() };
                            b::call(
                                if u.prefix { "$.update_pre" } else { "$.update" },
                                vec![Some((*u.argument).clone()), if u.operator.as_str() == "--" { Some(b::literal(-1.0)) } else { None }],
                            )
                        })),
                    },
                );
            }
        }
    }

    /// `transform_body(analysis.instance_body, b.id('$.run'), visit)`, with `self.path`
    /// already holding the Program
    fn instance_body(&mut self, st: &State) -> Vec<Node> {
        use crate::analyze::blockers::SyncItem;
        let body = self.an.instance_body.clone();
        let mut statements = Vec::new();
        for item in &body.sync {
            let node = match item {
                SyncItem::Node(p) => self.js_node(*p),
                SyncItem::Declarator { declaration, declarator } => {
                    let kind = match declaration.kind {
                        oxc_ast::ast::VariableDeclarationKind::Var => "var",
                        oxc_ast::ast::VariableDeclarationKind::Let => "let",
                        oxc_ast::ast::VariableDeclarationKind::Const => "const",
                        oxc_ast::ast::VariableDeclarationKind::Using => "using",
                        oxc_ast::ast::VariableDeclarationKind::AwaitUsing => "await using",
                    };
                    self.js_node(*declarator).map(|d| b::declaration(kind, vec![d]))
                }
            };
            if let Some(node) = node {
                statements.push(self.visit_js(&node, st));
            }
        }
        if !body.declarations.is_empty() {
            statements.push(b::declaration("var", body.declarations.iter().map(|id| b::declarator(self.id_node(*id), None)).collect()));
        }
        if !body.r#async.is_empty() {
            let mut thunks = Vec::new();
            for entry in &body.r#async {
                let mut entry_statements = Vec::new();
                for p in &entry.nodes {
                    let Some(node) = self.js_node(*p) else { continue };
                    entry_statements.extend(self.transform_async_node(&node, st));
                }
                let thunk = if entry_statements.is_empty() {
                    b::thunk_with(b::void0(), false)
                } else if entry_statements.len() == 1 && entry_statements[0].is("ExpressionStatement") {
                    let NodeKind::ExpressionStatement(e) = entry_statements.pop().unwrap().kind else { unreachable!() };
                    b::thunk_with(*e.expression, entry.has_await)
                } else {
                    b::thunk_with(b::block(entry_statements), entry.has_await)
                };
                thunks.push(thunk);
            }
            statements.push(b::var(b::id("$$promises"), b::call("$.run", vec![b::array(thunks)])));
        }
        statements
    }

    fn transform_async_node(&mut self, node: &Node, st: &State) -> Vec<Node> {
        match &node.kind {
            NodeKind::VariableDeclarator(d) => {
                let var = b::var((*d.id).clone(), d.init.as_deref().cloned());
                let visited = self.visit_js(&var, st);
                match visited.kind {
                    NodeKind::VariableDeclaration(v) => v
                        .declarations
                        .into_iter()
                        .map(|decl| {
                            let NodeKind::VariableDeclarator(d) = decl.kind else { unreachable!() };
                            if let Some(name) = js::ident(&d.id) {
                                if name.starts_with("$$d") || name.starts_with("$$array") {
                                    return b::var(*d.id, d.init.map(|i| *i));
                                }
                            }
                            b::stmt(b::assignment("=", *d.id, d.init.map_or_else(b::void0, |i| *i)))
                        })
                        .collect(),
                    _ => vec![],
                }
            }
            NodeKind::ClassDeclaration(c) => {
                let id = c.id.as_deref().cloned().unwrap_or_else(|| b::id("_"));
                let mut expr = node.clone();
                if let NodeKind::ClassDeclaration(c) = expr.kind {
                    expr.kind = NodeKind::ClassExpression(c);
                }
                vec![b::stmt(b::assignment("=", id, expr))]
            }
            NodeKind::ExpressionStatement(e) => {
                let expression = self.visit_js(&e.expression, st);
                match &expression.kind {
                    NodeKind::EmptyStatement => vec![],
                    NodeKind::AwaitExpression(_) => vec![b::stmt(expression)],
                    _ => vec![b::stmt(b::unary("void", expression))],
                }
            }
            _ => {
                let statement = self.visit_js(node, st);
                if statement.is("EmptyStatement") {
                    vec![]
                } else {
                    vec![statement]
                }
            }
        }
    }

    fn expression_statement(&mut self, node: &Node, st: &State) -> Node {
        let NodeKind::ExpressionStatement(e) = &node.kind else { unreachable!() };
        if e.expression.is("CallExpression") && js::get_rune(Some(&e.expression), &self.an.sc, st.scope) == Some("$inspect.trace") {
            return b::empty();
        }
        self.next(node, st)
    }

    fn export_named_declaration(&mut self, node: &Node, st: &State) -> Node {
        let NodeKind::ExportNamedDeclaration(e) = &node.kind else { unreachable!() };
        if st.is_instance {
            if let Some(d) = &e.declaration {
                return self.visit_in(node, d, st);
            }
            return b::empty();
        }
        self.next(node, st)
    }

    fn block_statement(&mut self, node: &Node, st: &State) -> Node {
        self.add_state_transformers(st);
        let tracing = self.an.scope_tracing.get(&st.scope).cloned();
        if let Some(tracing) = tracing {
            let NodeKind::BlockStatement(bl) = &node.kind else { unreachable!() };
            let is_async = match self.parent_js().map(|p| &p.kind) {
                Some(NodeKind::ArrowFunctionExpression(a)) => a.is_async,
                Some(NodeKind::FunctionExpression(f) | NodeKind::FunctionDeclaration(f)) => f.is_async,
                _ => false,
            };
            let tracing = match tracing {
                crate::analyze::Tracing::Expression(key) => b::thunk(self.js_node_by_key(key).unwrap_or_else(b::void0)),
                crate::analyze::Tracing::Label(l) => b::thunk(b::literal(l.as_str())),
            };
            let body: Vec<Node> = bl.body.iter().map(|n| self.visit_in(node, n, st)).collect();
            let call = b::call("$.trace", vec![tracing, b::thunk_with(b::block(body), is_async)]);
            return b::block(vec![b::r#return(if is_async { b::r#await(call) } else { call })]);
        }
        self.next(node, st)
    }

    fn break_statement(&mut self, node: &Node, _st: &State) -> Node {
        let NodeKind::BreakStatement(j) = &node.kind else { unreachable!() };
        if self.an.runes || j.label.as_deref().and_then(js::ident) != Some("$") {
            return node.clone();
        }
        let in_reactive_statement = self
            .path
            .get(1)
            .and_then(|p| p.js())
            .is_some_and(|p| matches!(&p.kind, NodeKind::LabeledStatement(l) if js::ident(&l.label) == Some("$")));
        if in_reactive_statement {
            return b::r#return(None);
        }
        node.clone()
    }

    fn await_expression(&mut self, node: &Node, st: &State) -> Node {
        let NodeKind::AwaitExpression(a) = &node.kind else { unreachable!() };
        let argument = self.visit_in(node, &a.argument, st);
        if node.origin.is_some_and(|o| self.an.pickled_awaits.contains(&o)) {
            return js::save(argument);
        }
        if self.dev && !self.is_ignored(node, "await_reactivity_loss") {
            let mut n = node.clone();
            if let NodeKind::AwaitExpression(aw) = &mut n.kind {
                aw.argument = Box::new(b::call("$.track_reactivity_loss", vec![argument]));
            }
            return b::call(n, ());
        }
        let mut out = node.clone();
        if let NodeKind::AwaitExpression(aw) = &mut out.kind {
            aw.argument = Box::new(argument);
        }
        out
    }

    fn binary_expression(&mut self, node: &Node, st: &State) -> Node {
        let NodeKind::BinaryExpression(e) = &node.kind else { unreachable!() };
        if self.dev {
            let op = e.operator.as_str();
            if op == "===" || op == "!==" {
                let l = self.visit_in(node, &e.left, st);
                let r = self.visit_in(node, &e.right, st);
                return b::call("$.strict_equals", vec![Some(l), Some(r), if op == "!==" { Some(b::r#false()) } else { None }]);
            }
            if op == "==" || op == "!=" {
                let l = self.visit_in(node, &e.left, st);
                let r = self.visit_in(node, &e.right, st);
                return b::call("$.equals", vec![Some(l), Some(r), if op == "!=" { Some(b::r#false()) } else { None }]);
            }
        }
        self.next(node, st)
    }

    fn visit_function(&mut self, node: &Node, st: &State) -> Node {
        let mut state = State { in_constructor: false, in_derived: false, ..st.clone() };
        if node.is("FunctionExpression") {
            state.in_constructor = matches!(self.parent_js().map(|p| &p.kind), Some(NodeKind::MethodDefinition(m)) if m.kind == crate::estree::MethodKind::Constructor);
        }
        self.next(node, &state)
    }

    fn for_of_statement(&mut self, node: &Node, st: &State) -> Node {
        let NodeKind::ForOfStatement(f) = &node.kind else { unreachable!() };
        if f.is_await && self.dev && !self.is_ignored(node, "await_reactivity_loss") && self.options.experimental_async {
            let left = self.visit_in(node, &f.left, st);
            let argument = self.visit_in(node, &f.right, st);
            let body = self.visit_in(node, &f.body, st);
            let right = b::call("$.for_await_track_reactivity_loss", vec![argument]);
            return b::for_of_with(left, right, body, true);
        }
        self.next(node, st)
    }

    fn variable_declaration(&mut self, node: &Node, st: &State) -> Node {
        let NodeKind::VariableDeclaration(v) = &node.kind else { unreachable!() };
        let mut declarations: Vec<Node> = Vec::new();
        if self.an.runes {
            for declarator in &v.declarations {
                let NodeKind::VariableDeclarator(d) = &declarator.kind else { continue };
                let init = d.init.as_deref();
                let rune = js::get_rune(init, &self.an.sc, st.scope);
                if rune.is_none()
                    || matches!(rune, Some("$effect.tracking" | "$effect.root" | "$inspect" | "$inspect.trace" | "$state.snapshot" | "$state.eager" | "$host"))
                {
                    declarations.push(self.visit_in(node, declarator, st));
                    continue;
                }
                let rune = rune.unwrap();
                if rune == "$props.id" {
                    continue;
                }
                if rune == "$props" {
                    let mut seen: Vec<String> = vec!["$$slots".into(), "$$events".into(), "$$legacy".into()];
                    if self.an.custom_element {
                        seen.push("$$host".into());
                    }
                    if let NodeKind::Identifier(i) = &d.id.kind {
                        let exclude_id = self.an.sc.unique("rest_excludes");
                        self.hoisted.push(b::var(
                            b::id(exclude_id.as_str()),
                            b::new("Set", vec![b::array(seen.iter().map(|n| b::literal(n.as_str())).collect::<Vec<_>>())]),
                        ));
                        let mut args = vec![b::id("$$props"), b::id(exclude_id.as_str())];
                        if self.dev {
                            args.push(b::literal(i.name.as_str()));
                        }
                        declarations.push(b::declarator((*d.id).clone(), b::call("$.rest_props", args)));
                    } else if let NodeKind::ObjectPattern(o) = &d.id.kind {
                        for property in &o.properties {
                            if let NodeKind::Property(p) = &property.kind {
                                let name = match &p.key.kind {
                                    NodeKind::Identifier(i) => i.name.to_string(),
                                    _ => js::get_name(&p.key).unwrap_or_default(),
                                };
                                seen.push(name.clone());
                                let id = match &p.value.kind {
                                    NodeKind::AssignmentPattern(a) => (*a.left).clone(),
                                    _ => (*p.value).clone(),
                                };
                                let Some(bid) = js::ident(&id).and_then(|n| self.get(st.scope, n)) else { continue };
                                let mut initial = match self.binding(bid).initial {
                                    Some(i) => self.js_node(i).map(|n| self.visit_in(node, &n, st)),
                                    None => None,
                                };
                                if let Some(i) = &initial {
                                    if self.binding(bid).kind == Kind::BindableProp && self.should_proxy(i, st.scope) {
                                        let mut v = b::call("$.proxy", vec![i.clone()]);
                                        if self.dev {
                                            v = b::call("$.tag_proxy", vec![v, b::literal(js::ident(&id).unwrap())]);
                                        }
                                        initial = Some(v);
                                    }
                                }
                                if self.is_prop_source(bid) {
                                    let source = self.get_prop_source(bid, &name, initial);
                                    declarations.push(b::declarator(id, source));
                                }
                            } else if let NodeKind::RestElement(r) = &property.kind {
                                let exclude_id = self.an.sc.unique("rest_excludes");
                                self.hoisted.push(b::var(
                                    b::id(exclude_id.as_str()),
                                    b::new("Set", vec![b::array(seen.iter().map(|n| b::literal(n.as_str())).collect::<Vec<_>>())]),
                                ));
                                let mut args = vec![b::id("$$props"), b::id(exclude_id.as_str())];
                                if self.dev {
                                    args.push(b::literal(js::ident(&r.argument).unwrap_or("")));
                                }
                                declarations.push(b::declarator((*r.argument).clone(), b::call("$.rest_props", args)));
                            }
                        }
                    }
                    continue;
                }

                let init = init.unwrap();
                let NodeKind::CallExpression(call) = &init.kind else { continue };
                let value = call.arguments.first().cloned().unwrap_or_else(b::void0);

                if rune == "$state" || rune == "$state.raw" {
                    let callee_loc = call.callee.loc;
                    let create_state_declarator = |this: &mut Self, id_name: &str, mut value: Node| -> Node {
                        let Some(bid) = this.get(st.scope, id_name) else { return value };
                        let is_state = this.is_state_source(bid);
                        let is_proxy = this.should_proxy(&value, st.scope);
                        if rune == "$state" && is_proxy {
                            value = b::call("$.proxy", vec![value]);
                            if this.dev && !is_state {
                                value = b::call("$.tag_proxy", vec![value, b::literal(id_name)]);
                            }
                        }
                        if is_state {
                            let mut callee = b::id("$.state");
                            callee.loc = callee_loc;
                            value = b::call(callee, vec![value]);
                            if this.dev {
                                value = b::call("$.tag", vec![value, b::literal(id_name)]);
                            }
                        }
                        value
                    };
                    if let NodeKind::Identifier(i) = &d.id.kind {
                        let expression = self.visit_in(node, &value, st);
                        let v = create_state_declarator(self, i.name.as_str(), expression);
                        declarations.push(b::declarator((*d.id).clone(), v));
                    } else {
                        let tmp = b::id(self.generate(st.scope, "tmp"));
                        let (inserts, paths) = js::extract_paths(&d.id, tmp.clone());
                        let v = self.visit_in(node, &value, st);
                        declarations.push(b::declarator(tmp, v));
                        let mut names = Vec::new();
                        let is_array = d.id.is("ArrayPattern");
                        for (_, value) in inserts {
                            let name = self.generate(st.scope, "$$array");
                            names.push(name.clone());
                            st.transform.borrow_mut().insert(name.clone(), Transform::read(get_value_fn()));
                            let mut value = value;
                            js::rename_placeholders(&mut value, &names);
                            let expression = self.visit_in(node, &b::thunk(value), st);
                            let mut c = b::call("$.derived", vec![expression]);
                            if self.dev {
                                c = b::call("$.tag", vec![c, b::literal(format!("[$state {}]", if is_array { "iterable" } else { "object" }))]);
                            }
                            declarations.push(b::declarator(b::id(name.as_str()), c));
                        }
                        for path in paths {
                            let mut e = path.expression;
                            js::rename_placeholders(&mut e, &names);
                            let value = self.visit_in(node, &e, st);
                            let pname = js::ident(&path.node).unwrap_or("").to_string();
                            let bid = self.get(st.scope, &pname);
                            let is_state = bid.is_some_and(|b| matches!(self.binding(b).kind, Kind::State | Kind::RawState));
                            let v = if is_state { create_state_declarator(self, &pname, value) } else { value };
                            declarations.push(b::declarator(path.node, v));
                        }
                    }
                    continue;
                }

                if rune == "$derived" || rune == "$derived.by" {
                    let meta = init.origin.and_then(|o| self.an.async_deriveds.iter().find(|(k, _)| *k == o).map(|(_, m)| *m));
                    let is_async = meta.is_some();
                    if let NodeKind::Identifier(i) = &d.id.kind {
                        let mut expression = self.visit_in(node, &value, st);
                        if is_async {
                            let location = if self.dev && !self.is_ignored(init, "await_waterfall") {
                                Some(self.locate_node(init.start().unwrap_or(0) as usize))
                            } else {
                                None
                            };
                            let call = b::call(
                                "$.async_derived",
                                vec![
                                    Some(self.async_thunk(expression, meta.unwrap())),
                                    if self.dev { Some(b::literal(i.name.as_str())) } else { None },
                                    location.map(|l| b::literal(l.as_str())),
                                ],
                            );
                            declarations.push(b::declarator((*d.id).clone(), b::r#await(call)));
                        } else {
                            if rune == "$derived" {
                                expression = b::thunk(expression);
                            }
                            let mut call = b::call("$.derived", vec![expression]);
                            if self.dev {
                                call = b::call("$.tag", vec![call, b::literal(i.name.as_str())]);
                            }
                            declarations.push(b::declarator((*d.id).clone(), call));
                        }
                    } else {
                        let expression = self.visit_in(node, &value, st);
                        let mut rhs = value.clone();
                        let is_array = d.id.is("ArrayPattern");
                        if rune != "$derived" || !call.arguments.first().is_some_and(|a| a.is("Identifier")) {
                            let id = b::id(self.generate(st.scope, "$$d"));
                            let mut c = b::call("$.derived", vec![if rune == "$derived" { b::thunk(expression.clone()) } else { expression.clone() }]);
                            rhs = b::call("$.get", vec![id.clone()]);
                            if is_async {
                                let location = if self.dev && !self.is_ignored(init, "await_waterfall") {
                                    Some(self.locate_node(init.start().unwrap_or(0) as usize))
                                } else {
                                    None
                                };
                                c = b::call(
                                    "$.async_derived",
                                    vec![
                                        Some(self.async_thunk(expression, meta.unwrap())),
                                        if self.dev { Some(b::literal(format!("[$derived {}]", if is_array { "iterable" } else { "object" }))) } else { None },
                                        location.map(|l| b::literal(l.as_str())),
                                    ],
                                );
                                c = b::r#await(c);
                            }
                            declarations.push(b::declarator(id, c));
                        }
                        let (inserts, paths) = js::extract_paths(&d.id, rhs);
                        let mut names = Vec::new();
                        for (_, value) in inserts {
                            let name = self.generate(st.scope, "$$array");
                            names.push(name.clone());
                            st.transform.borrow_mut().insert(name.clone(), Transform::read(get_value_fn()));
                            let mut value = value;
                            js::rename_placeholders(&mut value, &names);
                            let expression = self.visit_in(node, &b::thunk(value), st);
                            let mut c = b::call("$.derived", vec![expression]);
                            if self.dev {
                                c = b::call("$.tag", vec![c, b::literal(format!("[$derived {}]", if is_array { "iterable" } else { "object" }))]);
                            }
                            declarations.push(b::declarator(b::id(name.as_str()), c));
                        }
                        for path in paths {
                            let mut e = path.expression;
                            js::rename_placeholders(&mut e, &names);
                            let expression = self.visit_in(node, &e, st);
                            let call = b::call("$.derived", vec![b::thunk(expression)]);
                            let name = js::ident(&path.node).unwrap_or("").to_string();
                            declarations.push(b::declarator(path.node, if self.dev { b::call("$.tag", vec![call, b::literal(name.as_str())]) } else { call }));
                        }
                    }
                    continue;
                }
            }
        } else {
            for declarator in &v.declarations {
                let NodeKind::VariableDeclarator(d) = &declarator.kind else { continue };
                let bindings: Vec<_> = js::extract_identifiers(&d.id).into_iter().filter_map(|id| self.get(st.scope, js::ident(id).unwrap())).collect();
                let has_state = bindings.iter().any(|&b| self.binding(b).kind == Kind::State);
                let has_props = bindings.iter().any(|&b| self.binding(b).kind == Kind::BindableProp);
                if !has_state && !has_props {
                    declarations.push(self.visit_in(node, declarator, st));
                    continue;
                }
                if has_props {
                    if !d.id.is("Identifier") {
                        let tmp = b::id(self.generate(st.scope, "tmp"));
                        let (inserts, paths) = js::extract_paths(&d.id, tmp.clone());
                        let init = d.init.as_deref().cloned().unwrap_or_else(b::void0);
                        let init = self.visit_in(node, &init, st);
                        declarations.push(b::declarator(tmp, init));
                        let mut names = Vec::new();
                        for (_, value) in inserts {
                            let name = self.generate(st.scope, "$$array");
                            names.push(name.clone());
                            st.transform.borrow_mut().insert(name.clone(), Transform::read(get_value_fn()));
                            let mut value = value;
                            js::rename_placeholders(&mut value, &names);
                            let expression = self.visit_in(node, &b::thunk(value), st);
                            declarations.push(b::declarator(b::id(name.as_str()), b::call("$.derived", vec![expression])));
                        }
                        for path in paths {
                            let name = js::ident(&path.node).unwrap_or("").to_string();
                            let bid = self.get(st.scope, &name);
                            let mut e = path.expression;
                            js::rename_placeholders(&mut e, &names);
                            let value = self.visit_in(node, &e, st);
                            let v = match bid {
                                Some(bid) if self.binding(bid).kind == Kind::BindableProp => {
                                    let key = self.binding(bid).prop_alias.map(str::to_string).unwrap_or_else(|| name.clone());
                                    self.get_prop_source(bid, &key, Some(value))
                                }
                                _ => value,
                            };
                            declarations.push(b::declarator(path.node, v));
                        }
                        continue;
                    }
                    let name = js::ident(&d.id).unwrap().to_string();
                    let bid = self.get(st.scope, &name).unwrap();
                    let key = self.binding(bid).prop_alias.map(str::to_string).unwrap_or_else(|| name.clone());
                    let init = d.init.as_deref().map(|i| self.visit_in(node, i, st));
                    let source = self.get_prop_source(bid, &key, init);
                    declarations.push(b::declarator((*d.id).clone(), source));
                    continue;
                }
                let value = d.init.as_deref().map(|i| self.visit_in(node, i, st));
                let decls = self.create_state_declarators(node, &d.id, st, value);
                declarations.extend(decls);
            }
        }
        if declarations.is_empty() {
            return b::empty();
        }
        let mut out = node.map_children(&mut |c| c.clone());
        if let NodeKind::VariableDeclaration(v) = &mut out.kind {
            v.declarations = declarations;
        }
        out
    }

    /// `create_state_declarators(declarator, context, value)` (legacy mode)
    fn create_state_declarators(&mut self, node: &Node, id: &Node, st: &State, value: Option<Node>) -> Vec<Node> {
        let immutable = self.immutable;
        let dev = self.dev;
        let mutable_source = move |value: Option<Node>, name: &str| -> Node {
            let call = b::call("$.mutable_source", vec![value, if immutable { Some(b::r#true()) } else { None }]);
            if dev { b::call("$.tag", vec![call, b::literal(name)]) } else { call }
        };
        if let NodeKind::Identifier(i) = &id.kind {
            return vec![b::declarator(id.clone(), mutable_source(value, i.name.as_str()))];
        }
        let tmp = b::id(self.generate(st.scope, "tmp"));
        let (inserts, paths) = js::extract_paths(id, tmp.clone());
        let mut out = vec![b::declarator(tmp, value)];
        let mut names = Vec::new();
        for (_, value) in inserts {
            let name = self.generate(st.scope, "$$array");
            names.push(name.clone());
            st.transform.borrow_mut().insert(name.clone(), Transform::read(get_value_fn()));
            let mut value = value;
            js::rename_placeholders(&mut value, &names);
            let expression = self.visit_in(node, &b::thunk(value), st);
            out.push(b::declarator(b::id(name.as_str()), b::call("$.derived", vec![expression])));
        }
        for path in paths {
            let mut e = path.expression;
            js::rename_placeholders(&mut e, &names);
            let value = self.visit_in(node, &e, st);
            let name = js::ident(&path.node).unwrap_or("").to_string();
            let is_state = self.get(st.scope, &name).is_some_and(|b| self.binding(b).kind == Kind::State);
            out.push(b::declarator(path.node, if is_state { mutable_source(Some(value), &name) } else { value }));
        }
        out
    }
}

/// `is_non_coercive_operator(operator)`
fn is_non_coercive_operator(operator: &str) -> bool {
    matches!(operator, "=" | "||=" | "&&=" | "??=")
}
