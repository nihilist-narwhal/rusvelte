//! The server transform's JS visitors (`global_visitors` in `transform-server.js`)

use crate::analyze::scope::{DeclKind, Kind};
use crate::estree::builders as b;
use crate::estree::{Node, NodeKind};

use super::super::js::{self, Ancestors, PathNode};
use super::{Server, State};

impl<'a, 's> Server<'a, 's> {
    /// `context.visit(node, state)` on a JS node from a template visitor, or on a tree of
    /// its own: a walk with no JS ancestors
    pub fn visit_js(&mut self, node: &Node, st: &State) -> Node {
        self.visit(node, &Ancestors::ROOT, st)
    }

    /// `context.visit(node, state)` on a JS node: the universal `set_scope` visitor, then the
    /// node's visitor
    pub fn visit(&mut self, node: &Node, ancestors: &Ancestors, st: &State) -> Node {
        let scoped;
        let st = match node.origin.and_then(|o| self.scope_of_key(o)) {
            Some(scope) if scope != st.scope => {
                scoped = State { scope, ..st.clone() };
                &scoped
            }
            _ => st,
        };
        match &node.kind {
            NodeKind::Identifier(_) => self.identifier(node, ancestors, st),
            NodeKind::MemberExpression(_) => self.member_expression(node, ancestors, st),
            NodeKind::UpdateExpression(_) => self.update_expression(node, ancestors, st),
            NodeKind::ExpressionStatement(_) => self.expression_statement(node, ancestors, st),
            NodeKind::LabeledStatement(_) => self.labeled_statement(node, ancestors, st),
            NodeKind::Program(_) => self.program(node, ancestors, st),
            NodeKind::AwaitExpression(_) => self.await_expression(node, ancestors, st),
            NodeKind::PropertyDefinition(_) => self.property_definition(node, ancestors, st),
            NodeKind::CallExpression(_) => self.call_expression(node, ancestors, st),
            NodeKind::AssignmentExpression(_) => self.assignment_expression(node, ancestors, st),
            NodeKind::VariableDeclaration(_) => self.variable_declaration(node, ancestors, st),
            NodeKind::ClassBody(_) => self.class_body(node, ancestors, st),
            _ => self.next(node, ancestors, st),
        }
    }

    /// `context.next()`: the node with its children visited
    pub fn next(&mut self, node: &Node, ancestors: &Ancestors, st: &State) -> Node {
        let ancestors = ancestors.push(node);
        node.map_children(&mut |c| self.visit(c, &ancestors, st))
    }

    /// `context.visit(child)` from the visitor of `node`
    pub fn visit_in(&mut self, node: &Node, child: &Node, ancestors: &Ancestors, st: &State) -> Node {
        self.visit(child, &ancestors.push(node), st)
    }

    /// An identifier of the instance AST, with its location (comments are placed by it)
    pub fn id_node(&self, id: crate::analyze::scope::Id) -> Node {
        let mut n = b::id(id.name);
        if let Some((start, end)) = id.span {
            n.span = Some(crate::estree::Span::new(start, end));
            if id.has_loc() {
                n.loc = Some(self.conv.location(oxc_span::Span::new(start, end)));
            }
        }
        n.origin = Some(id.key);
        n
    }

    fn identifier(&mut self, node: &Node, ancestors: &Ancestors, st: &State) -> Node {
        let parent = ancestors.parent();
        let is_ref = match js::path_at(&self.tpl_path, ancestors, 1) {
            Some(PathNode::Js(_)) | None => js::is_reference(node, parent),
            // a template node as the parent: a template expression
            Some(PathNode::Tpl(_)) => true,
        };
        if !is_ref {
            return node.clone();
        }
        let name = js::ident(node).unwrap();
        if name == "$$props" {
            return b::id("$$sanitized_props");
        }
        if name.starts_with("$$derived_array") {
            return b::call(node.clone(), ());
        }
        self.build_getter(node, st)
    }

    /// `build_getter(node, state)`
    pub fn build_getter(&self, node: &Node, st: &State) -> Node {
        let name = js::ident(node).unwrap();
        let Some(bid) = self.get(st.scope, name) else { return node.clone() };
        let binding = self.binding(bid);
        if node.origin.is_some() && node.origin == Some(binding.node.key) {
            return node.clone();
        }
        if binding.kind == Kind::StoreSub {
            let store_id = b::id(&name[1..]);
            return b::call(
                "$.store_get",
                vec![
                    b::assignment("??=", b::id("$$store_subs"), b::object(vec![])),
                    b::literal(name),
                    self.build_getter(&store_id, st),
                ],
            );
        }
        if binding.kind == Kind::Derived {
            return if binding.declaration_kind == DeclKind::Var { b::maybe_call(node.clone(), ()) } else { b::call(node.clone(), ()) };
        }
        node.clone()
    }

    fn member_expression(&mut self, node: &Node, ancestors: &Ancestors, st: &State) -> Node {
        let NodeKind::MemberExpression(m) = &node.kind else { unreachable!() };
        if self.an.runes {
            if let NodeKind::PrivateIdentifier(p) = &m.property.kind {
                if let Some(rune) = self.state_field_rune(st, &format!("#{}", p.name)) {
                    if matches!(rune, "$derived" | "$derived.by") {
                        return b::call(node.clone(), ());
                    }
                }
            }
        }
        self.next(node, ancestors, st)
    }

    /// `state.state_fields.get(name)?.type`
    fn state_field_rune(&self, st: &State, name: &str) -> Option<&'static str> {
        let idx = st.state_fields?;
        self.an.state_fields[idx as usize].iter().find(|f| f.name == name).map(|f| f.rune)
    }

    /// The `ClassBody` visitor
    fn class_body(&mut self, node: &Node, ancestors: &Ancestors, st: &State) -> Node {
        let NodeKind::ClassBody(body) = &node.kind else { unreachable!() };
        let Some(&idx) = node.origin.and_then(|o| self.an.classes.get(&o)) else {
            // in legacy mode, do nothing
            return self.next(node, ancestors, st);
        };
        let fields: Vec<(String, bool, &'static str, String, usize)> = self.an.state_fields[idx as usize]
            .iter()
            .map(|f| (f.name.clone(), f.is_assignment, f.rune, f.key.clone(), f.node_key))
            .collect();
        let child_state = State { state_fields: Some(idx), ..st.clone() };
        let mut out = Vec::new();

        // insert backing fields for stuff declared in the constructor
        for (name, is_assignment, rune, key, _) in &fields {
            if name.starts_with('#') {
                continue;
            }
            if *is_assignment && matches!(*rune, "$derived" | "$derived.by") {
                let member = b::member(b::this(), b::private_id(key.as_str()));
                out.push(b::prop_def(b::private_id(key.as_str()), None));
                out.push(b::method("get", b::key(name), vec![], vec![b::r#return(b::call(member.clone(), ()))]));
                out.push(b::method("set", b::key(name), vec![b::id("$$value")], vec![b::r#return(b::call(member, vec![b::id("$$value")]))]));
            }
        }

        // replace parts of the class body
        let ancestors = ancestors.push(node);
        for definition in &body.body {
            let NodeKind::PropertyDefinition(d) = &definition.kind else {
                out.push(self.visit(definition, &ancestors, &child_state));
                continue;
            };
            let name = js::get_name(&d.key);
            let field = name.as_ref().and_then(|n| fields.iter().find(|f| &f.0 == n));
            let Some((name, _, rune, key, node_key)) = field.cloned() else {
                out.push(self.visit(definition, &ancestors, &child_state));
                continue;
            };
            if name.starts_with('#') || rune == "$state" || rune == "$state.raw" {
                out.push(self.visit(definition, &ancestors, &child_state));
            } else if definition.origin == Some(node_key) {
                // $derived / $derived.by
                let member = b::member(b::this(), b::private_id(key.as_str()));
                let value = d.value.as_deref().cloned().unwrap_or_else(b::void0);
                let value = self.visit(&value, &ancestors, &child_state);
                out.push(b::prop_def(b::private_id(key.as_str()), value));
                out.push(b::method("get", (*d.key).clone(), vec![], vec![b::r#return(b::call(member.clone(), ()))]));
                out.push(b::method("set", b::key(&name), vec![b::id("$$value")], vec![b::r#return(b::call(member, vec![b::id("$$value")]))]));
            }
        }
        let mut result = node.clone();
        if let NodeKind::ClassBody(b) = &mut result.kind {
            b.body = out;
        }
        result
    }

    fn update_expression(&mut self, node: &Node, ancestors: &Ancestors, st: &State) -> Node {
        let NodeKind::UpdateExpression(u) = &node.kind else { unreachable!() };
        if let Some(name) = js::ident(&u.argument) {
            if let Some(bid) = self.get(st.scope, name) {
                let binding = self.binding(bid);
                let decrement = u.operator.as_str() == "--";
                if binding.kind == Kind::StoreSub {
                    let mut args = vec![
                        b::assignment("??=", b::id("$$store_subs"), b::object(vec![])),
                        b::literal(name),
                        b::id(&name[1..]),
                    ];
                    if decrement {
                        args.push(b::literal(-1.0));
                    }
                    return b::call(if u.prefix { "$.update_store_pre" } else { "$.update_store" }, args);
                }
                if binding.kind == Kind::Derived {
                    let mut args = vec![self.id_node(binding.node)];
                    if decrement {
                        args.push(b::literal(-1.0));
                    }
                    return b::call(if u.prefix { "$.update_derived_pre" } else { "$.update_derived" }, args);
                }
            }
        }
        self.next(node, ancestors, st)
    }

    fn expression_statement(&mut self, node: &Node, ancestors: &Ancestors, st: &State) -> Node {
        let NodeKind::ExpressionStatement(e) = &node.kind else { unreachable!() };
        let rune = js::get_rune(Some(&e.expression), &self.an.sc, st.scope);
        if matches!(rune, Some("$effect" | "$effect.pre" | "$effect.root" | "$inspect.trace")) {
            return b::empty();
        }
        self.next(node, ancestors, st)
    }

    fn labeled_statement(&mut self, node: &Node, ancestors: &Ancestors, st: &State) -> Node {
        let NodeKind::LabeledStatement(l) = &node.kind else { unreachable!() };
        if self.an.runes || self.tpl_path.len() + ancestors.len() > 1 || js::ident(&l.label) != Some("$") {
            return self.next(node, ancestors, st);
        }
        let body = self.visit_in(node, &l.body, ancestors, st);
        let start = node.span.map_or(0, |s| s.start);
        self.legacy_reactive_statements.push((start, b::labeled("$", body)));
        b::empty()
    }

    fn program(&mut self, node: &Node, ancestors: &Ancestors, st: &State) -> Node {
        if !st.is_instance {
            return self.next(node, ancestors, st);
        }
        let body = self.instance_body(&ancestors.push(node), st);
        let mut out = node.map_children(&mut |c| c.clone());
        if let NodeKind::Program(p) = &mut out.kind {
            p.body = body;
        }
        out
    }

    /// `transform_body(analysis.instance_body, b.id('$$renderer.run'), visit)`, with
    /// `ancestors` holding the Program
    pub fn instance_body(&mut self, ancestors: &Ancestors, st: &State) -> Vec<Node> {
        use crate::analyze::blockers::SyncItem;
        let body = self.an.instance_body.clone();
        let mut statements = Vec::new();
        for item in &body.sync {
            let node = match item {
                SyncItem::Node(p) => self.instance_nodes.get(&p.key()).cloned(),
                SyncItem::Declarator { declaration, declarator } => {
                    let kind = match declaration.kind {
                        oxc_ast::ast::VariableDeclarationKind::Var => "var",
                        oxc_ast::ast::VariableDeclarationKind::Let => "let",
                        oxc_ast::ast::VariableDeclarationKind::Const => "const",
                        oxc_ast::ast::VariableDeclarationKind::Using => "using",
                        oxc_ast::ast::VariableDeclarationKind::AwaitUsing => "await using",
                    };
                    self.instance_nodes.get(&declarator.key()).cloned().map(|d| b::declaration(kind, vec![d]))
                }
            };
            if let Some(node) = node {
                statements.push(self.visit(&node, ancestors, st));
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
                    let Some(node) = self.instance_nodes.get(&p.key()).cloned() else { continue };
                    entry_statements.extend(self.transform_async_node(&node, ancestors, st));
                }
                let thunk = if entry_statements.is_empty() {
                    b::thunk_with(b::void0(), false)
                } else if entry_statements.len() == 1 && matches!(entry_statements[0].kind, NodeKind::ExpressionStatement(_)) {
                    let NodeKind::ExpressionStatement(e) = entry_statements.pop().unwrap().kind else { unreachable!() };
                    b::thunk_with(*e.expression, entry.has_await)
                } else {
                    b::thunk_with(b::block(entry_statements), entry.has_await)
                };
                thunks.push(thunk);
            }
            statements.push(b::var(b::id("$$promises"), b::call("$$renderer.run", vec![b::array(thunks)])));
        }
        statements
    }

    fn transform_async_node(&mut self, node: &Node, ancestors: &Ancestors, st: &State) -> Vec<Node> {
        match &node.kind {
            NodeKind::VariableDeclarator(d) => {
                let var = b::var((*d.id).clone(), d.init.as_deref().cloned());
                let visited = self.visit(&var, ancestors, st);
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
                let visited = self.visit(&expr, ancestors, st);
                vec![b::stmt(b::assignment("=", id, visited))]
            }
            NodeKind::ExpressionStatement(e) => {
                let expression = self.visit(&e.expression, ancestors, st);
                match &expression.kind {
                    NodeKind::EmptyStatement => vec![],
                    NodeKind::AwaitExpression(_) => vec![b::stmt(expression)],
                    _ => vec![b::stmt(b::unary("void", expression))],
                }
            }
            _ => {
                let statement = self.visit(node, ancestors, st);
                if matches!(statement.kind, NodeKind::EmptyStatement) {
                    vec![]
                } else {
                    vec![statement]
                }
            }
        }
    }

    fn await_expression(&mut self, node: &Node, ancestors: &Ancestors, st: &State) -> Node {
        let NodeKind::AwaitExpression(a) = &node.kind else { unreachable!() };
        let argument = self.visit_in(node, &a.argument, ancestors, st);
        if node.origin.is_some_and(|o| self.an.pickled_awaits.contains(&o)) {
            return js::save(argument);
        }
        let ast = self.ast();
        for p in js::context_path(&self.tpl_path, ancestors) {
            let ty = p.ty(ast);
            if matches!(ty, "ArrowFunctionExpression" | "FunctionExpression" | "FunctionDeclaration") {
                break;
            }
            if let PathNode::Tpl(t) = p {
                if template_has_metadata(t) {
                    if ty != "ExpressionTag" && ty != "Fragment" {
                        return js::save(argument);
                    }
                    break;
                }
            }
        }
        let mut out = node.clone();
        if let NodeKind::AwaitExpression(a) = &mut out.kind {
            a.argument = Box::new(argument);
        }
        out
    }

    fn property_definition(&mut self, node: &Node, ancestors: &Ancestors, st: &State) -> Node {
        let NodeKind::PropertyDefinition(d) = &node.kind else { unreachable!() };
        if self.an.runes {
            if let Some(value) = d.value.as_deref() {
                if let NodeKind::CallExpression(c) = &value.kind {
                    let rune = js::get_rune(Some(value), &self.an.sc, st.scope);
                    if matches!(rune, Some("$state" | "$state.raw")) {
                        let new_value = c.arguments.first().map(|a| self.visit_in(node, a, ancestors, st));
                        let mut out = node.clone();
                        if let NodeKind::PropertyDefinition(d) = &mut out.kind {
                            d.value = new_value.map(Box::new);
                        }
                        return out;
                    }
                    if matches!(rune, Some("$derived" | "$derived.by")) {
                        let first = c.arguments.first().cloned().unwrap_or_else(b::void0);
                        let f = self.visit_in(node, &first, ancestors, st);
                        let new_value = if c.arguments.is_empty() {
                            None
                        } else {
                            Some(b::call("$.derived", vec![if rune == Some("$derived") { b::thunk(f) } else { f }]))
                        };
                        let mut out = node.clone();
                        if let NodeKind::PropertyDefinition(d) = &mut out.kind {
                            d.value = new_value.map(Box::new);
                        }
                        return out;
                    }
                }
            }
        }
        self.next(node, ancestors, st)
    }

    fn call_expression(&mut self, node: &Node, ancestors: &Ancestors, st: &State) -> Node {
        let NodeKind::CallExpression(c) = &node.kind else { unreachable!() };
        let rune = js::get_rune(Some(node), &self.an.sc, st.scope);
        match rune {
            Some("$host" | "$effect" | "$effect.pre" | "$inspect.trace") => return b::void0(),
            Some("$effect.tracking") => return b::r#false(),
            Some("$effect.root") => return b::arrow(vec![], b::block(vec![])),
            Some("$effect.pending") => return b::literal(0.0),
            Some("$state" | "$state.raw") => {
                return match c.arguments.first() {
                    Some(a) => self.visit_in(node, a, ancestors, st),
                    None => b::void0(),
                };
            }
            Some("$derived" | "$derived.by") => {
                let first = c.arguments.first().cloned().unwrap_or_else(b::void0);
                let f = self.visit_in(node, &first, ancestors, st);
                return b::call("$.derived", vec![if rune == Some("$derived") { b::thunk(f) } else { f }]);
            }
            Some("$state.eager") => {
                let first = c.arguments.first().cloned().unwrap_or_else(b::void0);
                return self.visit_in(node, &first, ancestors, st);
            }
            Some("$state.snapshot") => {
                let first = c.arguments.first().cloned().unwrap_or_else(b::void0);
                let arg = self.visit_in(node, &first, ancestors, st);
                let ignored = self.is_ignored(node, "state_snapshot_uncloneable");
                let mut args = vec![Some(arg)];
                if ignored {
                    args.push(Some(b::r#true()));
                }
                return b::call("$.snapshot", args);
            }
            Some("$inspect" | "$inspect().with") => {
                if !self.dev {
                    return b::empty();
                }
                let rune = rune.unwrap();
                let call = if rune == "$inspect" {
                    node
                } else {
                    match &c.callee.kind {
                        NodeKind::MemberExpression(m) => &m.object,
                        _ => node,
                    }
                };
                let NodeKind::CallExpression(inner) = &call.kind else { unreachable!() };
                let args: Vec<Node> = inner.arguments.iter().map(|a| self.visit_in(node, a, ancestors, st)).collect();
                if rune == "$inspect" {
                    let mut all = vec![b::literal("$inspect(")];
                    all.extend(args);
                    all.push(b::literal(")"));
                    return b::call("console.log", all);
                }
                let inspector = self.visit_in(node, &c.arguments[0], ancestors, st);
                let mut all = vec![b::literal("init")];
                all.extend(args);
                return b::call(inspector, all);
            }
            _ => {}
        }
        self.next(node, ancestors, st)
    }

    /// `is_ignored(node, code)`: a `svelte-ignore` comment covers the node (dev only)
    pub fn is_ignored(&self, node: &Node, code: &str) -> bool {
        if !self.dev {
            return false;
        }
        let Some(o) = node.origin else { return false };
        self.an.ignore_map.get(&o).is_some_and(|&i| self.an.ignore_sets[i as usize].contains(code))
    }

    fn assignment_expression(&mut self, node: &Node, ancestors: &Ancestors, st: &State) -> Node {
        match self.visit_assignment_expression(node, ancestors, st) {
            Some(n) => n,
            None => self.next(node, ancestors, st),
        }
    }

    /// `visit_assignment_expression(node, context, build_assignment)`
    fn visit_assignment_expression(&mut self, node: &Node, ancestors: &Ancestors, st: &State) -> Option<Node> {
        let NodeKind::AssignmentExpression(a) = &node.kind else { unreachable!() };
        if matches!(a.left.kind, NodeKind::ArrayPattern(_) | NodeKind::ObjectPattern(_) | NodeKind::RestElement(_)) {
            let value = self.visit_in(node, &a.right, ancestors, st);
            let should_cache = !matches!(value.kind, NodeKind::Identifier(_));
            let rhs = if should_cache { b::id("$$value") } else { value.clone() };
            let (inserts, paths) = js::extract_paths(&a.left, rhs.clone());
            let names: Vec<String> = inserts.iter().map(|_| self.an.sc.generate(st.scope, "$$array")).collect();
            let mut changed = false;
            let mut assignments = Vec::new();
            for path in paths {
                let mut value = path.expression;
                js::rename_placeholders(&mut value, &names);
                let assignment = self.build_assignment("=", &path.node, &value, node, ancestors, st);
                if assignment.is_some() {
                    changed = true;
                }
                assignments.push(assignment.unwrap_or_else(|| {
                    let left = self.visit_in(node, &path.node, ancestors, st);
                    let right = self.visit_in(node, &value, ancestors, st);
                    b::assignment("=", left, right)
                }));
            }
            if !changed {
                return None;
            }
            let is_standalone = ancestors.parent().is_some_and(|p| p.type_name().ends_with("Statement"));
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
        self.build_assignment(operator, &a.left, &a.right, node, ancestors, st)
    }

    /// The server's `build_assignment`
    fn build_assignment(&mut self, operator: &str, left: &Node, right: &Node, node: &Node, ancestors: &Ancestors, st: &State) -> Option<Node> {
        if self.an.runes {
            if let NodeKind::MemberExpression(m) = &left.kind {
                if m.object.is("ThisExpression") && !m.computed {
                    if let Some(r) = self.build_state_field_assignment(operator, left, right, node, ancestors, st) {
                        return Some(r);
                    }
                }
            }
        }
        let object = {
            let mut o = left;
            while let NodeKind::MemberExpression(m) = &o.kind {
                o = &m.object;
            }
            o
        };
        let name = js::ident(object)?;
        if is_store_name(name) {
            let store = &name[1..];
            self.get(st.scope, store)?;
            if std::ptr::eq(object, left) {
                let value = js::build_assignment_value(operator, left.clone(), right.clone());
                let value = self.visit_in(node, &value, ancestors, st);
                return Some(b::call("$.store_set", vec![b::id(store), value]));
            }
            let l = self.visit_in(node, left, ancestors, st);
            let r = self.visit_in(node, right, ancestors, st);
            return Some(b::call(
                "$.store_mutate",
                vec![
                    b::assignment("??=", b::id("$$store_subs"), b::object(vec![])),
                    b::literal(name),
                    b::id(store),
                    b::assignment(operator, l, r),
                ],
            ));
        }
        let bid = self.get(st.scope, name);
        if let Some(bid) = bid {
            if self.binding(bid).kind == Kind::Derived && std::ptr::eq(object, left) {
                let value = js::build_assignment_value(operator, left.clone(), right.clone());
                let value = self.visit_in(node, &value, ancestors, st);
                return Some(b::call(object.clone(), vec![value]));
            }
        }
        None
    }

    /// The state field cases of the server's `build_assignment` (`this.x = ...`)
    fn build_state_field_assignment(&mut self, operator: &str, left: &Node, right: &Node, node: &Node, ancestors: &Ancestors, st: &State) -> Option<Node> {
        let NodeKind::MemberExpression(m) = &left.kind else { return None };
        let name = js::get_name(&m.property)?;
        let idx = st.state_fields?;
        let field = self.an.state_fields[idx as usize].iter().find(|f| f.name == name)?;
        let (is_assignment, node_key, field_rune, key) = (field.is_assignment, field.node_key, field.rune, field.key.clone());
        let is_field_left = match &node.kind {
            NodeKind::AssignmentExpression(a) => std::ptr::eq(left, &*a.left),
            _ => false,
        };
        // special case — state declaration in class constructor
        if is_assignment && node.origin == Some(node_key) && is_field_left {
            let rune = js::get_rune(Some(right), &self.an.sc, st.scope)?;
            let key = if m.property.is("PrivateIdentifier") || rune == "$state" || rune == "$state.raw" {
                (*m.property).clone()
            } else {
                b::private_id(key.as_str())
            };
            let computed = key.is("Literal");
            let value = self.visit_in(node, right, ancestors, st);
            return Some(b::assignment(operator, b::member_with(b::this(), key, computed, false), value));
        } else if matches!(field_rune, "$derived" | "$derived.by") && m.property.is("PrivateIdentifier") {
            let value = js::build_assignment_value(operator, left.clone(), right.clone());
            let value = self.visit_in(node, &value, ancestors, st);
            return Some(b::call(b::member(b::this(), b::id(name.as_str())), vec![value]));
        }
        None
    }

    fn variable_declaration(&mut self, node: &Node, ancestors: &Ancestors, st: &State) -> Node {
        let NodeKind::VariableDeclaration(v) = &node.kind else { unreachable!() };
        let mut declarations = Vec::new();
        if self.an.runes {
            for declarator in &v.declarations {
                let NodeKind::VariableDeclarator(d) = &declarator.kind else { continue };
                let init = d.init.as_deref();
                let rune = js::get_rune(init, &self.an.sc, st.scope);
                if rune.is_none() || matches!(rune, Some("$effect.tracking" | "$inspect" | "$effect.root")) {
                    declarations.push(self.visit_in(node, declarator, ancestors, st));
                    continue;
                }
                let rune = rune.unwrap();
                if rune == "$props.id" {
                    continue;
                }
                if rune == "$props" {
                    let mut has_rest = false;
                    let mut id = self.remove_bindable(&d.id, &d.id, &mut has_rest, node, ancestors, st);
                    let slots_name = if self.an.uses_slots { b::id("$$slots_") } else { b::id("$$slots") };
                    match &mut id.kind {
                        NodeKind::ObjectPattern(o) if has_rest => {
                            let at = o.properties.len() - 1;
                            o.properties.insert(at, b::prop("init", b::id("$$events"), b::id("$$events")));
                            o.properties.insert(at, b::prop("init", b::id("$$slots"), slots_name));
                        }
                        NodeKind::Identifier(i) => {
                            let name = i.name.clone();
                            id = b::object_pattern(vec![
                                b::prop("init", b::id("$$slots"), slots_name),
                                b::prop("init", b::id("$$events"), b::id("$$events")),
                                b::rest(b::id(name.as_str())),
                            ]);
                        }
                        _ => {}
                    }
                    let id = self.visit_in(node, &id, ancestors, st);
                    declarations.push(b::declarator(id, b::id("$$props")));
                    continue;
                }
                let NodeKind::CallExpression(call) = &init.unwrap().kind else { continue };
                let value = match call.arguments.first() {
                    Some(a) => self.visit_in(node, a, ancestors, st),
                    None => b::void0(),
                };
                if rune == "$derived" || rune == "$derived.by" {
                    let is_async = rune == "$derived" && init.unwrap().origin.is_some_and(|o| self.an.async_deriveds.iter().any(|(k, _)| *k == o));
                    let init_expr = if is_async {
                        b::r#await(b::call("$.async_derived", vec![b::thunk_with(value, true)]))
                    } else {
                        b::call("$.derived", vec![if rune == "$derived" { b::thunk(value) } else { value }])
                    };
                    if matches!(d.id.kind, NodeKind::Identifier(_)) {
                        let id = self.visit_in(node, &d.id, ancestors, st);
                        declarations.push(b::declarator(id, init_expr));
                    } else {
                        let mut rhs = call.arguments[0].clone();
                        if rune == "$derived.by" || !matches!(call.arguments[0].kind, NodeKind::Identifier(_)) {
                            let id = b::id(self.an.sc.generate(st.scope, "$$d").as_str());
                            rhs = b::call(id.clone(), ());
                            declarations.push(b::declarator(id, init_expr));
                        }
                        let (inserts, paths) = js::extract_paths(&d.id, rhs);
                        let mut names = Vec::new();
                        for _ in &inserts {
                            names.push(self.an.sc.generate(st.scope, "$$derived_array"));
                        }
                        for (i, (_, value)) in inserts.into_iter().enumerate() {
                            let mut value = value;
                            js::rename_placeholders(&mut value, &names);
                            let thunk = b::thunk(value);
                            let expression = self.visit_in(node, &thunk, ancestors, st);
                            declarations.push(b::declarator(b::id(names[i].as_str()), b::call("$.derived", vec![expression])));
                        }
                        for path in paths {
                            let mut e = path.expression;
                            js::rename_placeholders(&mut e, &names);
                            let expression = self.visit_in(node, &e, ancestors, st);
                            declarations.push(b::declarator(path.node, b::call("$.derived", vec![b::thunk(expression)])));
                        }
                    }
                    continue;
                }
                if matches!(d.id.kind, NodeKind::Identifier(_)) {
                    declarations.push(b::declarator((*d.id).clone(), value));
                    continue;
                }
                declarations.extend(self.create_state_declarators(&d.id, st, Some(value)));
            }
        } else {
            for declarator in &v.declarations {
                let NodeKind::VariableDeclarator(d) = &declarator.kind else { continue };
                let bindings: Vec<_> = js::extract_identifiers(&d.id).into_iter().filter_map(|id| self.get(st.scope, js::ident(id).unwrap())).collect();
                let has_state = bindings.iter().any(|&b| self.binding(b).kind == Kind::State);
                let has_props = bindings.iter().any(|&b| self.binding(b).kind == Kind::BindableProp);
                if !has_state && !has_props {
                    declarations.push(self.visit_in(node, declarator, ancestors, st));
                    continue;
                }
                if has_props {
                    if !matches!(d.id.kind, NodeKind::Identifier(_)) {
                        let tmp = b::id(self.an.sc.generate(st.scope, "tmp").as_str());
                        let (inserts, paths) = js::extract_paths(&d.id, tmp.clone());
                        let init = d.init.as_deref().cloned().unwrap_or_else(b::void0);
                        let init = self.visit_in(node, &init, ancestors, st);
                        declarations.push(b::declarator(tmp, init));
                        let names: Vec<String> = inserts.iter().map(|_| self.an.sc.generate(st.scope, "$$array")).collect();
                        for (i, (_, mut value)) in inserts.into_iter().enumerate() {
                            js::rename_placeholders(&mut value, &names);
                            declarations.push(b::declarator(b::id(names[i].as_str()), value));
                        }
                        for path in paths {
                            let mut value = path.expression;
                            js::rename_placeholders(&mut value, &names);
                            let name = js::ident(&path.node).unwrap_or("").to_string();
                            let alias = self.get(st.scope, &name).and_then(|b| self.binding(b).prop_alias).map(str::to_string);
                            let prop = b::member_with(b::id("$$props"), b::literal(alias.as_deref().unwrap_or(&name)), true, false);
                            declarations.push(b::declarator(path.node, js::build_fallback(prop, value)));
                        }
                        continue;
                    }
                    let name = js::ident(&d.id).unwrap().to_string();
                    let alias = self.get(st.scope, &name).and_then(|b| self.binding(b).prop_alias).map(str::to_string);
                    let prop = b::member_with(b::id("$$props"), b::literal(alias.as_deref().unwrap_or(&name)), true, false);
                    let init = match d.init.as_deref() {
                        Some(i) => {
                            let default_value = self.visit_in(node, i, ancestors, st);
                            js::build_fallback(prop, default_value)
                        }
                        None => prop,
                    };
                    declarations.push(b::declarator((*d.id).clone(), init));
                    continue;
                }
                // `declarator.init && context.visit(declarator.init)`: no initializer stays none
                let value = d.init.as_deref().map(|i| self.visit_in(node, i, ancestors, st));
                declarations.extend(self.create_state_declarators(&d.id, st, value));
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

    /// The `walk(declarator.id, ...)` of `$props()`: `$bindable(x)` defaults become `x`
    fn remove_bindable(&mut self, pattern: &Node, root: &Node, has_rest: &mut bool, decl: &Node, ancestors: &Ancestors, st: &State) -> Node {
        match &pattern.kind {
            NodeKind::RestElement(_) => {
                // `context.path.at(-1) === declarator.id`
                if let NodeKind::ObjectPattern(o) = &root.kind {
                    if o.properties.iter().any(|p| std::ptr::eq(p, pattern)) {
                        *has_rest = true;
                    }
                }
                pattern.map_children(&mut |c| self.remove_bindable(c, root, has_rest, decl, ancestors, st))
            }
            NodeKind::AssignmentPattern(a) => {
                if let NodeKind::CallExpression(c) = &a.right.kind {
                    if js::get_rune(Some(&a.right), &self.an.sc, st.scope) == Some("$bindable") {
                        let right = match c.arguments.first() {
                            Some(arg) => self.visit_in(decl, arg, ancestors, st),
                            None => b::void0(),
                        };
                        return b::assignment_pattern((*a.left).clone(), right);
                    }
                }
                pattern.map_children(&mut |c| self.remove_bindable(c, root, has_rest, decl, ancestors, st))
            }
            _ => pattern.map_children(&mut |c| self.remove_bindable(c, root, has_rest, decl, ancestors, st)),
        }
    }

    /// `create_state_declarators(declarator, scope, value)`
    fn create_state_declarators(&mut self, id: &Node, st: &State, value: Option<Node>) -> Vec<Node> {
        if matches!(id.kind, NodeKind::Identifier(_)) {
            return vec![b::declarator(id.clone(), value)];
        }
        let tmp = b::id(self.an.sc.generate(st.scope, "tmp").as_str());
        let (inserts, paths) = js::extract_paths(id, tmp.clone());
        let mut out = vec![b::declarator(tmp, value)];
        let names: Vec<String> = inserts.iter().map(|_| self.an.sc.generate(st.scope, "$$array")).collect();
        for (i, (_, mut v)) in inserts.into_iter().enumerate() {
            js::rename_placeholders(&mut v, &names);
            out.push(b::declarator(b::id(names[i].as_str()), v));
        }
        for path in paths {
            let mut e = path.expression;
            js::rename_placeholders(&mut e, &names);
            out.push(b::declarator(path.node, e));
        }
        out
    }
}

fn is_store_name(name: &str) -> bool {
    let mut c = name.chars();
    c.next() == Some('$') && c.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
}

/// Template nodes have `metadata` (the JS tests `parent.metadata`)
fn template_has_metadata(p: crate::analyze::nodes::P) -> bool {
    use crate::analyze::nodes::P;
    matches!(p, P::Fragment(_) | P::SlotFragment(_) | P::Node(_) | P::Attr(_) | P::Chunk(_) | P::TextareaValue(_))
}
