//! The analysis walk: the `_` visitor of `phases/2-analyze/index.js` and the visitors in
//! `phases/2-analyze/visitors/*`.

use oxc_ast::AstKind;
use oxc_ast::ast::*;

use super::nodes::{self, P, Res};
use super::scope::{self, BindingId, DeclKind, Id, Kind, get_rune, ident, is_member, is_reference, object};
use super::utils::{self, chunks, is_event_attribute, is_expression_value, text_value, value_expression};
use super::{Analyzer, AstType, ReactiveStatement, State, StateField, warnings as w};
use crate::ast::{Attr, AttrValue, Chunk, Expr, Node, NodeId, Pattern};
use crate::errors as e;

const IGNORE_CODES_EXTRA: &[&str] = &[];

impl<'s> Analyzer<'s> {
    // -----------------------------------------------------------------------------------
    // walking

    pub fn visit(&mut self, p: P<'s>, state: &State<'s>) -> Res {
        let ignores = self.collect_ignores(p);
        let pushed = !ignores.is_empty();
        if pushed {
            self.push_ignore(ignores);
        }
        if let Some(&top) = self.ignore_stack.last() {
            self.ignore_map.insert(p.key(), top);
        } else if !self.ignore_map.is_empty() {
            self.ignore_map.remove(&p.key());
        }

        let mut st = *state;
        if let Some(&s) = self.sc.map.get(&p.key()) {
            st.scope = s;
        }
        self.dispatch(p, &st)?;

        if pushed {
            self.pop_ignore();
        }
        Ok(())
    }

    pub fn next(&mut self, p: P<'s>, state: &State<'s>) -> Res {
        self.path.push(p);
        match p {
            P::SlotFragment(i) => {
                let n = self.slot_fragments[i as usize].1.len();
                for k in 0..n {
                    let child = self.slot_fragments[i as usize].1[k];
                    self.child_index = k;
                    self.visit(P::Node(child), state)?;
                }
            }
            P::Fragment(f) if self.emptied_fragment == Some(f) => {}
            P::Fragment(f) => {
                let nodes = &self.ast.fragments[f].nodes;
                for (k, &child) in nodes.iter().enumerate() {
                    self.child_index = k;
                    self.visit(P::Node(child), state)?;
                }
            }
            _ => nodes::each_child(p, self.ast, self, &mut |me: &mut Self, c| me.visit(c, state))?,
        }
        self.path.pop();
        Ok(())
    }

    /// `context.visit(child, state)` from the visitor of `parent`
    pub fn visit_child(&mut self, parent: P<'s>, child: P<'s>, state: &State<'s>) -> Res {
        self.path.push(parent);
        self.visit(child, state)?;
        self.path.pop();
        Ok(())
    }

    fn collect_ignores(&mut self, p: P<'s>) -> Vec<&'s str> {
        let mut ignores = Vec::new();
        if !self.has_comments {
            return ignores;
        }
        let parent = self.path.last().copied();
        let is_text_or_comment = matches!(
            p,
            P::Node(n) if matches!(self.ast.nodes[n], Node::Text { .. } | Node::Comment { .. })
        );
        match (parent, p) {
            (Some(parent @ (P::Fragment(_) | P::SlotFragment(_))), P::Node(n)) if !is_text_or_comment => {
                let siblings: &[NodeId] = match parent {
                    P::Fragment(f) => &self.ast.fragments[f].nodes,
                    P::SlotFragment(i) => &self.slot_fragments[i as usize].1,
                    _ => unreachable!(),
                };
                // `parent.nodes.indexOf(node)`: `next` records the index
                let idx = if siblings.get(self.child_index) == Some(&n) {
                    self.child_index
                } else {
                    let Some(idx) = siblings.iter().position(|&s| s == n) else { return ignores };
                    idx
                };
                let mut comments = Vec::new();
                for &prev in siblings[..idx].iter().rev() {
                    match &self.ast.nodes[prev] {
                        Node::Comment { start, data, .. } => comments.push((*start + 4, *data)),
                        Node::Text { .. } => {}
                        _ => break,
                    }
                }
                for (offset, data) in comments {
                    let found = self.extract_svelte_ignore(offset, data);
                    ignores.extend(found);
                }
            }
            (Some(P::Fragment(_) | P::SlotFragment(_)), _) => {}
            _ => {
                if let Some(comments) = self.leading_comments.get(&p.key()).cloned() {
                    for (start, value) in comments {
                        let found = self.extract_svelte_ignore(start + 2, value);
                        ignores.extend(found);
                    }
                }
            }
        }
        ignores
    }

    /// `extract_svelte_ignore(offset, text, runes)`
    pub fn extract_svelte_ignore(&mut self, offset: usize, text: &'s str) -> Vec<&'s str> {
        let mut ignores = Vec::new();
        // /^\s*svelte-ignore\s/
        let trimmed = text.trim_start_matches(utils::is_js_whitespace);
        let Some(rest) = trimmed.strip_prefix("svelte-ignore") else { return ignores };
        let Some(ws) = rest.chars().next().filter(|c| utils::is_js_whitespace(*c)) else { return ignores };
        let length = text.len() - rest.len() + ws.len_utf8();
        let offset = offset + length;
        let body = &text[length..];
        let codes = crate::warning_codes::CODES;

        let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'$' || b == b'-';
        let bytes = body.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if !is_word(bytes[i]) {
                i += 1;
                continue;
            }
            let start = i;
            while i < bytes.len() && is_word(bytes[i]) {
                i += 1;
            }
            let code = &body[start..i];
            let comma = bytes.get(i) == Some(&b',');
            if self.runes {
                if codes.contains(&code) {
                    ignores.push(code);
                } else {
                    let replacement = legacy_replacement(code);
                    let (s, e) = (offset + start, offset + start + code.len());
                    match codes.iter().find(|c| **c == replacement) {
                        Some(r) => self.warn_range(s, e, w::legacy_code(code, r)),
                        None => {
                            let suggestion = utils::fuzzymatch(code, codes);
                            self.warn_range(s, e, w::unknown_code(code, suggestion.as_deref()));
                        }
                    }
                }
                if !comma {
                    break;
                }
                i += 1;
            } else {
                ignores.push(code);
                if !codes.contains(&code) {
                    let replacement = legacy_replacement(code);
                    if let Some(r) = codes.iter().find(|c| **c == replacement) {
                        ignores.push(r);
                    }
                }
            }
        }
        let _ = IGNORE_CODES_EXTRA;
        ignores
    }

    fn meta(&mut self, m: Option<u32>) -> Option<&mut super::ExprMeta> {
        m.map(|m| &mut self.metas[m as usize])
    }

    // -----------------------------------------------------------------------------------
    // dispatch

    fn dispatch(&mut self, p: P<'s>, st: &State<'s>) -> Res {
        match p {
            P::Js(k) => self.visit_js(p, k, st),
            P::Node(n) => self.visit_node(p, n, st),
            P::Attr(a) => self.visit_attr(p, a, st),
            P::TextareaValue(_) => self.next(p, st),
            P::Chunk(Chunk::Text { start, data, .. }) => {
                self.text(p, *start, data);
                Ok(())
            }
            P::Chunk(Chunk::Expression { .. }) => {
                let meta = self.new_meta();
                self.next(p, &State { expression: Some(meta), ..*st })
            }
            P::TplExpr(Expr::Ident { .. }) | P::PatIdent(_) => self.identifier(p, st),
            P::TplExpr(Expr::Literal { value, start, end, .. }) => {
                if has_bidi(value) {
                    self.warn_range(*start, *end, w::bidirectional_control_characters());
                }
                Ok(())
            }
            P::Empty => Ok(()),
            _ => self.next(p, st),
        }
    }

    fn visit_js(&mut self, p: P<'s>, k: AstKind<'s>, st: &State<'s>) -> Res {
        use AstKind as K;
        match k {
            K::IdentifierReference(_) | K::BindingIdentifier(_) | K::IdentifierName(_) | K::LabelIdentifier(_) => {
                self.identifier(p, st)
            }
            K::StringLiteral(s) => {
                if has_bidi(s.value.as_str()) {
                    self.warn(Some(p), w::bidirectional_control_characters());
                }
                Ok(())
            }
            K::TemplateElement(t) => {
                if has_bidi(t.value.cooked.as_ref().map_or("", |c| c.as_str())) {
                    self.warn(Some(p), w::bidirectional_control_characters());
                }
                Ok(())
            }
            K::Function(f) => {
                if !f.is_expression() && self.runes {
                    if let Some(id) = &f.id {
                        self.validate_identifier_name_of(st.scope, id.name.as_str())?;
                    }
                }
                self.visit_function(p, st)
            }
            K::ArrowFunctionExpression(_) => self.visit_function(p, st),
            K::AssignmentExpression(a) => {
                let left = nodes::target(&a.left);
                self.validate_assignment(p, left, st)?;
                if let Some(rs) = st.reactive_statement {
                    let id = if is_member(left) { object(left) } else { ident(left).or(Some(dummy_id())) };
                    if id.is_some() {
                        for id in scope::extract_identifiers(left) {
                            if let Some(b) = self.get(st.scope, id.name) {
                                let rs = &mut self.reactive_statements[rs as usize];
                                if !rs.assignments.contains(&b) {
                                    rs.assignments.push(b);
                                }
                            }
                        }
                    }
                }
                self.next(p, st)
            }
            K::UpdateExpression(u) => {
                let arg = nodes::simple_target(&u.argument);
                self.validate_assignment(p, arg, st)?;
                if let Some(rs) = st.reactive_statement {
                    let id = if is_member(arg) { object(arg) } else { ident(arg) };
                    if let Some(id) = id {
                        if let Some(b) = self.get(st.scope, id.name) {
                            let rs = &mut self.reactive_statements[rs as usize];
                            if !rs.assignments.contains(&b) {
                                rs.assignments.push(b);
                            }
                        }
                    }
                }
                self.next(p, st)
            }
            K::AwaitExpression(_) => {
                let tla = st.ast_type == AstType::Instance && st.function_depth == 1;
                let mut suspend = tla;
                if let Some(m) = self.meta(st.expression) {
                    m.has_await = true;
                    suspend = true;
                }
                if suspend {
                    // `experimental.async` is off
                    return Err(e::experimental_async(self.loc(p)));
                }
                self.next(p, st)
            }
            K::CallExpression(c) => self.call_expression(p, c, st),
            K::StaticMemberExpression(_) | K::ComputedMemberExpression(_) | K::PrivateFieldExpression(_) => {
                let (obj, prop) = match k {
                    K::StaticMemberExpression(m) => (nodes::expr(&m.object), P::Js(K::IdentifierName(&m.property))),
                    K::ComputedMemberExpression(m) => (nodes::expr(&m.object), nodes::expr(&m.expression)),
                    K::PrivateFieldExpression(m) => (nodes::expr(&m.object), P::Js(K::PrivateIdentifier(&m.field))),
                    _ => unreachable!(),
                };
                if let (Some(o), Some(pr)) = (ident(obj), ident(prop)) {
                    if let Some(b) = self.get(st.scope, o.name) {
                        if self.binding(b).kind == Kind::RestProp && pr.name.starts_with("$$") {
                            return Err(e::props_illegal_name(self.loc(prop)));
                        }
                    }
                }
                self.next(p, st)
            }
            K::NewExpression(n) => {
                if matches!(nodes::strip(&n.callee), Expression::ClassExpression(_))
                    && self.sc.scope(st.scope).function_depth > 0
                {
                    self.warn(Some(p), w::perf_avoid_inline_class());
                }
                self.next(p, st)
            }
            K::Class(c) if !c.is_expression() => {
                if self.runes {
                    if let Some(id) = &c.id {
                        self.validate_identifier_name_of(st.scope, id.name.as_str())?;
                    }
                }
                let allowed_depth = if st.ast_type == AstType::Module { 0 } else { 1 };
                if self.sc.scope(st.scope).function_depth > allowed_depth {
                    self.warn(Some(p), w::perf_avoid_nested_class());
                }
                self.next(p, st)
            }
            K::ClassBody(b) => self.class_body(p, b, st),
            K::PropertyDefinition(d) => {
                if let Some(name) = get_name(nodes::property_key(&d.key)) {
                    let field = self.state_fields[st.state_fields as usize].iter().find(|f| f.name == name);
                    if let Some(field) = field {
                        if field.node_key != p.key() && d.value.is_some() && (d.span.start as usize) < field.node_start {
                            return Err(e::state_field_invalid_assignment(self.loc(p)));
                        }
                    }
                }
                self.next(p, st)
            }
            K::ExportDefaultDeclaration(_) => Err(e::module_illegal_default_export(self.loc(p))),
            K::ExportDeclaration(_) | K::ExportNamedDeclaration(_) | K::ExportFromDeclaration(_) => {
                self.export_named_declaration(p, k, st)
            }
            K::ExportSpecifier(s) => {
                let local = module_export_name_str(&s.local);
                if st.ast_type == AstType::Instance {
                    if self.runes {
                        if let Some(b) = self.get(st.scope, local) {
                            self.sc.binding_mut(b).reassigned = true;
                        }
                    }
                } else {
                    self.validate_export(p, st.scope, local)?;
                }
                Ok(())
            }
            K::ExpressionStatement(s) => {
                self.legacy_component_creation(&s.expression, st);
                self.next(p, st)
            }
            K::ImportDeclaration(i) => {
                if self.runes {
                    let source = i.source.value.as_str();
                    if source.starts_with("svelte/internal") {
                        return Err(e::import_svelte_internal_forbidden(self.loc(p)));
                    }
                    if source == "svelte" {
                        for s in i.specifiers.iter().flatten() {
                            if let ImportDeclarationSpecifier::ImportSpecifier(s) = s {
                                if s.import_kind.is_type() {
                                    continue;
                                }
                                if let ModuleExportName::IdentifierName(n) = &s.imported {
                                    if n.name == "beforeUpdate" || n.name == "afterUpdate" {
                                        return Err(e::runes_mode_invalid_import(
                                            self.loc(P::Js(K::ImportSpecifier(s))),
                                            n.name.as_str(),
                                        ));
                                    }
                                }
                            }
                        }
                    }
                }
                Ok(())
            }
            K::LabeledStatement(l) => self.labeled_statement(p, l, st),
            K::VariableDeclarator(d) => self.variable_declarator(p, d, st),
            K::SpreadElement(_) | K::TaggedTemplateExpression(_) => self.next(p, st),
            _ => self.next(p, st),
        }
    }

    fn validate_identifier_name_of(&self, scope: super::ScopeId, name: &str) -> Res {
        if let Some(b) = self.get(scope, name) {
            scope::validate_identifier_name(self.binding(b), None)?;
        }
        Ok(())
    }

    fn visit_function(&mut self, p: P<'s>, st: &State<'s>) -> Res {
        let depth = self.sc.scope(st.scope).function_depth.max(st.function_depth) + 1;
        self.next(p, &State { function_depth: depth, expression: None, ..*st })
    }

    // -----------------------------------------------------------------------------------
    // Identifier

    fn identifier(&mut self, p: P<'s>, st: &State<'s>) -> Res {
        let Some(node) = ident(p) else { return Ok(()) };
        let mut i = self.path.len();
        let Some(&first_parent) = self.path.last() else { return Ok(()) };
        if !is_reference(p, first_parent) {
            return Ok(());
        }
        i -= 1;
        let mut parent = first_parent;

        if node.name == "arguments"
            && !self.path.iter().any(|n| matches!(n, P::Js(AstKind::Function(_))))
        {
            return Err(e::invalid_arguments_usage(node.err_loc()));
        }

        if node.name == "$$slots" {
            self.uses_slots = true;
        }

        if self.runes
            && utils::is_rune(node.name).is_some()
            && self.get(st.scope, node.name).is_none()
            && self.get(st.scope, &node.name[1..]).is_none_or(|b| self.binding(b).kind != Kind::StoreSub)
        {
            let mut current = p;
            let mut name = node.name.to_string();
            while is_member(parent) {
                if let P::Js(AstKind::ComputedMemberExpression(_)) = parent {
                    return Err(e::rune_invalid_computed_property(self.loc(parent)));
                }
                let prop = match parent {
                    P::Js(AstKind::StaticMemberExpression(m)) => m.property.name.as_str(),
                    P::Js(AstKind::PrivateFieldExpression(m)) => m.field.name.as_str(),
                    _ => "",
                };
                name.push('.');
                name.push_str(prop);
                current = parent;
                i = i.wrapping_sub(1);
                parent = match self.path.get(i) {
                    Some(&p) => p,
                    None => P::Empty,
                };
                if utils::is_rune(&name).is_none() {
                    if name == "$effect.active" {
                        return Err(e::rune_renamed(self.loc(parent), "$effect.active", "$effect.tracking"));
                    }
                    if name == "$state.frozen" {
                        return Err(e::rune_renamed(self.loc(parent), "$state.frozen", "$state.raw"));
                    }
                    if name == "$state.is" {
                        return Err(e::rune_removed(self.loc(parent), "$state.is"));
                    }
                    return Err(e::rune_invalid_name(self.loc(parent), &name));
                }
            }
            if !matches!(parent, P::Js(AstKind::CallExpression(_))) {
                return Err(e::rune_missing_parentheses(self.loc(current)));
            }
        }

        let Some(b) = self.get(st.scope, node.name) else { return Ok(()) };

        if let Some(m) = st.expression {
            let meta = &mut self.metas[m as usize];
            if meta.track_deps && !meta.dependencies.contains(&b) {
                meta.dependencies.push(b);
            }
        }

        let binding = self.binding(b);
        if self.runes
            && node.key != binding.node.key
            && st.function_depth == self.sc.scope(binding.scope).function_depth
            && (match binding.kind {
                Kind::State => binding.reassigned || self.initial_is_unproxied_call(binding.initial, st.scope),
                Kind::RawState | Kind::Derived | Kind::Prop | Kind::RestProp => true,
                _ => false,
            })
            && !matches!(first_parent, P::Js(AstKind::AssignmentExpression(a)) if p.is(nodes::target(&a.left)))
            && !matches!(first_parent, P::Js(AstKind::UpdateExpression(_)))
        {
            let mut ty = "closure";
            let mut i = self.path.len();
            while i > 0 {
                i -= 1;
                let parent = self.path[i];
                if matches!(parent, P::Js(AstKind::Function(_) | AstKind::ArrowFunctionExpression(_))) {
                    break;
                }
                if let P::Js(AstKind::CallExpression(c)) = parent {
                    let child = self.path.get(i + 1).copied();
                    if child.is_some_and(|child| c.arguments.iter().any(|a| nodes::argument(a).is(child))) {
                        let rune = get_rune(&self.sc, Some(parent), st.scope);
                        if rune == Some("$state") || rune == Some("$state.raw") {
                            ty = "derived";
                            break;
                        }
                    }
                }
            }
            self.warn(Some(p), w::state_referenced_locally(node.name, ty));
        }

        let binding = self.binding(b);
        if st.reactive_statement.is_some() && binding.scope == self.module_scope && binding.reassigned {
            self.warn(Some(p), w::reactive_declaration_module_script_dependency());
        }
        Ok(())
    }

    /// `binding.initial?.type === 'CallExpression' && arguments.length === 1 && ... && !should_proxy(...)`
    fn initial_is_unproxied_call(&self, initial: Option<P<'s>>, scope: super::ScopeId) -> bool {
        let Some(P::Js(AstKind::CallExpression(c))) = initial else { return false };
        if c.arguments.len() != 1 {
            return false;
        }
        match &c.arguments[0] {
            Argument::SpreadElement(_) => false,
            a => !self.should_proxy(Some(nodes::expr(a.as_expression().unwrap())), Some(scope)),
        }
    }

    pub fn should_proxy(&self, node: Option<P<'s>>, scope: Option<super::ScopeId>) -> bool {
        use AstKind as K;
        let Some(node) = node else { return false };
        match node {
            P::Js(
                K::BooleanLiteral(_)
                | K::NullLiteral(_)
                | K::NumericLiteral(_)
                | K::StringLiteral(_)
                | K::BigIntLiteral(_)
                | K::RegExpLiteral(_)
                | K::TemplateLiteral(_)
                | K::ArrowFunctionExpression(_)
                | K::UnaryExpression(_)
                | K::BinaryExpression(_)
                | K::PrivateInExpression(_),
            ) => return false,
            P::Js(K::Function(f)) if f.is_expression() => return false,
            P::TplExpr(Expr::Literal { .. }) => return false,
            _ => {}
        }
        if let Some(id) = ident(node) {
            if id.name == "undefined" {
                return false;
            }
            if let Some(scope) = scope {
                if let Some(b) = self.get(scope, id.name) {
                    let b = self.binding(b);
                    if !b.reassigned {
                        if let Some(init) = b.initial {
                            let excluded = match init {
                                P::Js(K::Function(f)) => !f.is_expression(),
                                P::Js(K::Class(c)) => !c.is_expression(),
                                P::Js(K::ImportDeclaration(_)) => true,
                                P::Node(n) => matches!(self.ast.nodes[n], Node::EachBlock { .. } | Node::SnippetBlock { .. }),
                                _ => false,
                            };
                            if !excluded {
                                return self.should_proxy(Some(init), None);
                            }
                        }
                    }
                }
            }
        }
        true
    }

    // -----------------------------------------------------------------------------------
    // CallExpression

    fn call_expression(&mut self, p: P<'s>, c: &'s CallExpression<'s>, st: &State<'s>) -> Res {
        let parent = self.path.last().copied().unwrap_or(P::Empty);
        let rune = get_rune(&self.sc, Some(p), st.scope);
        let loc = self.loc(p);

        if let Some(rune) = rune {
            if rune != "$inspect" {
                for a in &c.arguments {
                    if matches!(a, Argument::SpreadElement(_)) {
                        return Err(e::rune_invalid_spread(loc, rune));
                    }
                }
            }
        }
        let nargs = c.arguments.len();

        match rune {
            None => {}
            Some("$bindable") => {
                if nargs > 1 {
                    return Err(e::rune_invalid_arguments_length(loc, "$bindable", "zero or one arguments"));
                }
                let n = self.path.len();
                let at = |i: usize| if n >= i { Some(self.path[n - i]) } else { None };
                let ok = self.ty(parent) == "AssignmentPattern"
                    && at(3).is_some_and(|p| self.ty(p) == "ObjectPattern")
                    && at(4).is_some_and(|p| self.ty(p) == "VariableDeclarator")
                    && match at(4) {
                        Some(P::Js(AstKind::VariableDeclarator(d))) => {
                            get_rune(&self.sc, d.init.as_ref().map(nodes::expr), st.scope) == Some("$props")
                        }
                        _ => false,
                    };
                if !ok {
                    return Err(e::bindable_invalid_location(loc));
                }
            }
            Some("$host") => {
                if nargs > 0 {
                    return Err(e::rune_invalid_arguments(loc, "$host"));
                } else if st.ast_type == AstType::Module || !self.custom_element {
                    return Err(e::host_invalid_placement(loc));
                }
            }
            Some("$props") => {
                if self.has_props_rune {
                    return Err(e::props_duplicate(loc, "$props"));
                }
                self.has_props_rune = true;
                if self.ty(parent) != "VariableDeclarator"
                    || st.ast_type != AstType::Instance
                    || st.scope != self.instance_scope
                {
                    return Err(e::props_invalid_placement(loc));
                }
                if nargs > 0 {
                    return Err(e::rune_invalid_arguments(loc, "$props"));
                }
            }
            Some("$props.id") => {
                let n = self.path.len();
                let grand_parent = if n >= 2 { Some(self.path[n - 2]) } else { None };
                if self.props_id.is_some() {
                    return Err(e::props_duplicate(loc, "$props.id"));
                }
                let id = match parent {
                    P::Js(AstKind::VariableDeclarator(d)) => match &d.id {
                        BindingPattern::BindingIdentifier(i) => ident(P::Js(AstKind::BindingIdentifier(i))),
                        _ => None,
                    },
                    _ => None,
                };
                if id.is_none()
                    || st.ast_type != AstType::Instance
                    || st.scope != self.instance_scope
                    || !grand_parent.is_some_and(|g| self.ty(g) == "VariableDeclaration")
                {
                    return Err(e::props_id_invalid_placement(loc));
                }
                if nargs > 0 {
                    return Err(e::rune_invalid_arguments(loc, "$props.id"));
                }
                self.props_id = id;
            }
            Some(r @ ("$state" | "$state.raw" | "$derived" | "$derived.by")) => {
                let valid = self.is_variable_declaration(parent)
                    || matches!(parent, P::Js(AstKind::PropertyDefinition(d)) if !d.r#static && !d.computed)
                    || self.is_class_property_assignment_at_constructor_root(parent);
                if !valid {
                    return Err(e::state_invalid_placement(loc, r));
                }
                if (r == "$derived" || r == "$derived.by") && nargs != 1 {
                    return Err(e::rune_invalid_arguments_length(loc, r, "exactly one argument"));
                } else if nargs > 1 {
                    return Err(e::rune_invalid_arguments_length(loc, r, "zero or one arguments"));
                }
            }
            Some(r @ ("$effect" | "$effect.pre")) => {
                if self.ty(parent) != "ExpressionStatement" {
                    return Err(e::effect_invalid_placement(loc));
                }
                if nargs != 1 {
                    return Err(e::rune_invalid_arguments_length(loc, r, "exactly one argument"));
                }
            }
            Some("$effect.tracking") => {
                if nargs != 0 {
                    return Err(e::rune_invalid_arguments(loc, "$effect.tracking"));
                }
            }
            Some(r @ ("$effect.root" | "$inspect().with" | "$state.eager" | "$state.snapshot")) => {
                if nargs != 1 {
                    return Err(e::rune_invalid_arguments_length(loc, r, "exactly one argument"));
                }
            }
            Some("$effect.pending") => {}
            Some("$inspect") => {
                if nargs < 1 {
                    return Err(e::rune_invalid_arguments_length(loc, "$inspect", "one or more arguments"));
                }
            }
            Some("$inspect.trace") => {
                if nargs > 1 {
                    return Err(e::rune_invalid_arguments_length(loc, "$inspect.trace", "zero or one arguments"));
                }
                let n = self.path.len();
                let grand_parent = if n >= 2 { Some(self.path[n - 2]) } else { None };
                let func = if n >= 3 { Some(self.path[n - 3]) } else { None };
                let is_fn = matches!(func, Some(P::Js(AstKind::Function(_) | AstKind::ArrowFunctionExpression(_))));
                let first_is_parent = match grand_parent {
                    Some(P::Js(AstKind::BlockStatement(b))) => b.body.first().is_some_and(|s| nodes::statement(s).is(parent)),
                    Some(P::Js(AstKind::FunctionBody(b))) => {
                        b.statements.first().is_some_and(|s| nodes::statement(s).is(parent))
                    }
                    _ => false,
                };
                if self.ty(parent) != "ExpressionStatement"
                    || !grand_parent.is_some_and(|g| self.ty(g) == "BlockStatement")
                    || !is_fn
                    || !first_is_parent
                {
                    return Err(e::inspect_trace_invalid_placement(loc));
                }
                if let Some(P::Js(AstKind::Function(f))) = func {
                    if f.generator {
                        return Err(e::inspect_trace_generator(loc));
                    }
                }
            }
            Some(_) => {}
        }

        if rune == Some("$derived") {
            let meta = self.new_meta();
            let depth = st.function_depth + 1;
            self.next(
                p,
                &State { function_depth: depth, derived_function_depth: depth as i32, expression: Some(meta), ..*st },
            )?;
            if st.in_declaration_tag && self.metas[meta as usize].has_await {
                if let Some(m) = self.meta(st.expression) {
                    m.has_await = true;
                }
            }
        } else if rune == Some("$inspect") {
            self.next(p, &State { function_depth: st.function_depth + 1, ..*st })?;
        } else {
            self.next(p, st)?;
        }
        Ok(())
    }

    fn is_variable_declaration(&self, parent: P<'s>) -> bool {
        let n = self.path.len();
        self.ty(parent) == "VariableDeclarator" && !(n >= 3 && self.ty(self.path[n - 3]) == "ConstTag")
    }

    fn is_class_property_assignment_at_constructor_root(&self, parent: P<'s>) -> bool {
        let P::Js(AstKind::AssignmentExpression(a)) = parent else { return false };
        if a.operator != AssignmentOperator::Assign {
            return false;
        }
        let ok = match &a.left {
            AssignmentTarget::StaticMemberExpression(m) => matches!(nodes::strip(&m.object), Expression::ThisExpression(_)),
            AssignmentTarget::PrivateFieldExpression(m) => matches!(nodes::strip(&m.object), Expression::ThisExpression(_)),
            AssignmentTarget::ComputedMemberExpression(m) => {
                matches!(nodes::strip(&m.object), Expression::ThisExpression(_))
                    && matches!(
                        nodes::strip(&m.expression),
                        Expression::StringLiteral(_)
                            | Expression::NumericLiteral(_)
                            | Expression::BooleanLiteral(_)
                            | Expression::NullLiteral(_)
                            | Expression::BigIntLiteral(_)
                            | Expression::RegExpLiteral(_)
                    )
            }
            _ => false,
        };
        if !ok {
            return false;
        }
        // MethodDefinition (-5) -> FunctionExpression (-4) -> BlockStatement (-3) -> ExpressionStatement (-2) -> AssignmentExpression (-1)
        let n = self.path.len();
        if n < 5 {
            return false;
        }
        matches!(self.path[n - 5], P::Js(AstKind::MethodDefinition(m)) if m.kind == MethodDefinitionKind::Constructor)
    }

    // -----------------------------------------------------------------------------------
    // assignments

    fn validate_assignment(&mut self, node: P<'s>, argument: P<'s>, st: &State<'s>) -> Res {
        let is_binding = matches!(node, P::Attr(Attr::Directive { kind: "BindDirective", .. }));
        self.validate_no_const_assignment(node, argument, st.scope, is_binding)?;

        if let Some(id) = ident(argument) {
            let b = self.get(st.scope, id.name);
            if self.runes {
                if let (Some(props_id), Some(b)) = (self.props_id, b) {
                    if self.binding(b).node.key == props_id.key {
                        return Err(e::constant_assignment(self.loc(node), "$props.id()"));
                    }
                }
                if b.is_some_and(|b| self.binding(b).kind == Kind::Each) {
                    return Err(e::each_item_invalid_assignment(self.loc(node)));
                }
            }
            if b.is_some_and(|b| self.binding(b).kind == Kind::Snippet) {
                return Err(e::snippet_parameter_assignment(self.loc(node)));
            }
        }

        let this_member = match argument {
            P::Js(AstKind::StaticMemberExpression(m)) if matches!(nodes::strip(&m.object), Expression::ThisExpression(_)) => {
                Some(Some(m.property.name.to_string()))
            }
            P::Js(AstKind::PrivateFieldExpression(m)) if matches!(nodes::strip(&m.object), Expression::ThisExpression(_)) => {
                Some(Some(format!("#{}", m.field.name)))
            }
            P::Js(AstKind::ComputedMemberExpression(m)) if matches!(nodes::strip(&m.object), Expression::ThisExpression(_)) => {
                Some(get_name(nodes::expr(&m.expression)).filter(|_| is_literal(nodes::expr(&m.expression))))
            }
            _ => None,
        };
        if let Some(Some(name)) = this_member {
            let field = self.state_fields[st.state_fields as usize].iter().find(|f| f.name == name);
            if let Some(field) = field {
                if field.is_assignment && field.node_key != node.key() {
                    let field_start = field.node_start;
                    let mut i = self.path.len();
                    while i > 0 {
                        i -= 1;
                        let parent = self.path[i];
                        if matches!(parent, P::Js(AstKind::Function(_) | AstKind::ArrowFunctionExpression(_))) {
                            let grandparent = if i >= 1 { Some(self.path[i - 1]) } else { None };
                            if let Some(P::Js(AstKind::MethodDefinition(m))) = grandparent {
                                if m.kind == MethodDefinitionKind::Constructor
                                    && node.start(self.ast).is_some_and(|s| s < field_start)
                                {
                                    return Err(e::state_field_invalid_assignment(self.loc(node)));
                                }
                            }
                            break;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn validate_no_const_assignment(&self, node: P<'s>, argument: P<'s>, scope: super::ScopeId, is_binding: bool) -> Res {
        use AstKind as K;
        match argument {
            P::Js(K::ArrayPattern(a)) => {
                for el in a.elements.iter().flatten() {
                    self.validate_no_const_assignment(node, nodes::binding(el), scope, is_binding)?;
                }
                if let Some(r) = &a.rest {
                    self.validate_no_const_assignment(node, P::Js(K::BindingRestElement(r)), scope, is_binding)?;
                }
            }
            P::Js(K::ArrayAssignmentTarget(a)) => {
                for el in a.elements.iter().flatten() {
                    self.validate_no_const_assignment(node, nodes::target_maybe_default(el), scope, is_binding)?;
                }
            }
            P::Js(K::ObjectPattern(o)) => {
                for prop in &o.properties {
                    self.validate_no_const_assignment(node, nodes::binding(&prop.value), scope, is_binding)?;
                }
            }
            P::Js(K::ObjectAssignmentTarget(o)) => {
                for prop in &o.properties {
                    let value = match prop {
                        AssignmentTargetProperty::AssignmentTargetPropertyIdentifier(x) => {
                            if x.init.is_some() {
                                P::AtpiDefault(x)
                            } else {
                                P::Js(K::IdentifierReference(&x.binding))
                            }
                        }
                        AssignmentTargetProperty::AssignmentTargetPropertyProperty(x) => nodes::target_maybe_default(&x.binding),
                    };
                    self.validate_no_const_assignment(node, value, scope, is_binding)?;
                }
            }
            _ => {
                if let Some(id) = ident(argument) {
                    if let Some(b) = self.get(scope, id.name) {
                        let b = self.binding(b);
                        if b.declaration_kind == DeclKind::Import || (b.declaration_kind == DeclKind::Const && b.kind != Kind::Each) {
                            let thing = if b.declaration_kind == DeclKind::Import { "import" } else { "constant" };
                            return Err(if is_binding {
                                e::constant_binding(self.loc(node), thing)
                            } else {
                                e::constant_assignment(self.loc(node), thing)
                            });
                        }
                    }
                }
            }
        }
        Ok(())
    }

    // -----------------------------------------------------------------------------------
    // classes

    fn class_body(&mut self, p: P<'s>, body: &'s ClassBody<'s>, st: &State<'s>) -> Res {
        if !self.runes {
            return self.next(p, st);
        }
        let mut state_fields: Vec<StateField> = Vec::new();
        let mut fields: Vec<(String, Vec<&'static str>)> = Vec::new();
        let mut constructor: Option<&'s MethodDefinition<'s>> = None;

        let mut handle = |an: &Self,
                          state_fields: &mut Vec<StateField>,
                          fields: &Vec<(String, Vec<&'static str>)>,
                          node: P<'s>,
                          key: P<'s>,
                          value: Option<P<'s>>,
                          is_static: bool|
         -> Res {
            let Some(name) = get_name(key) else { return Ok(()) };
            let rune = get_rune(&an.sc, value, st.scope);
            if let Some(rune) = rune {
                if utils::is_state_creation_rune(rune) {
                    if state_fields.iter().any(|f| f.name == name) {
                        return Err(e::state_field_duplicate(an.loc(node), &name));
                    }
                    let is_assignment = matches!(node, P::Js(AstKind::AssignmentExpression(_)));
                    let key = format!("{}{}", if is_assignment || !is_static { "" } else { "@" }, name);
                    if let Some((_, field)) = fields.iter().find(|(k, _)| *k == key) {
                        if !(field.len() == 1 && field[0] == "prop") {
                            return Err(e::duplicate_class_field(an.loc(node), &key));
                        }
                    }
                    let node_start = node.start(an.ast).unwrap_or(0);
                    match state_fields.iter_mut().find(|f| f.name == name) {
                        Some(f) => {
                            f.node_key = node.key();
                            f.node_start = node_start;
                            f.is_assignment = is_assignment;
                        }
                        None => state_fields.push(StateField { name, node_key: node.key(), node_start, is_assignment }),
                    }
                }
            }
            Ok(())
        };

        for child in &body.body {
            match child {
                ClassElement::PropertyDefinition(d) if !d.computed && !d.r#static => {
                    if d.declare || d.r#type != PropertyDefinitionType::PropertyDefinition {
                        continue;
                    }
                    let cp = P::Js(AstKind::PropertyDefinition(d));
                    let key = nodes::property_key(&d.key);
                    handle(self, &mut state_fields, &fields, cp, key, d.value.as_ref().map(nodes::expr), false)?;
                    let key = get_name(key).unwrap_or_else(|| "null".into());
                    if !fields.iter().any(|(k, _)| *k == key) {
                        fields.push((key, vec![if d.value.is_some() { "assigned_prop" } else { "prop" }]));
                        continue;
                    }
                    return Err(e::duplicate_class_field(self.loc(cp), &key));
                }
                ClassElement::MethodDefinition(m) => {
                    if m.r#type != MethodDefinitionType::MethodDefinition {
                        continue;
                    }
                    let cp = P::Js(AstKind::MethodDefinition(m));
                    if m.kind == MethodDefinitionKind::Constructor {
                        constructor = Some(m);
                    } else if !m.computed {
                        let kind = match m.kind {
                            MethodDefinitionKind::Method => "method",
                            MethodDefinitionKind::Get => "get",
                            MethodDefinitionKind::Set => "set",
                            MethodDefinitionKind::Constructor => "constructor",
                        };
                        let key = format!(
                            "{}{}",
                            if m.r#static { "@" } else { "" },
                            get_name(nodes::property_key(&m.key)).unwrap_or_else(|| "null".into())
                        );
                        let Some(idx) = fields.iter().position(|(k, _)| *k == key) else {
                            fields.push((key, vec![kind]));
                            continue;
                        };
                        let field = &mut fields[idx].1;
                        if field.contains(&kind) || field.contains(&"prop") || field.contains(&"assigned_prop") {
                            return Err(e::duplicate_class_field(self.loc(cp), &key));
                        }
                        if kind == "get" {
                            if field.len() == 1 && field[0] == "set" {
                                field.push("get");
                                continue;
                            }
                        } else if kind == "set" {
                            if field.len() == 1 && field[0] == "get" {
                                field.push("set");
                                continue;
                            }
                        } else {
                            field.push(kind);
                            continue;
                        }
                        return Err(e::duplicate_class_field(self.loc(cp), &key));
                    }
                }
                _ => {}
            }
        }

        if let Some(ctor) = constructor {
            if let Some(body) = &ctor.value.body {
                for statement in &body.statements {
                    let Statement::ExpressionStatement(es) = statement else { continue };
                    let Expression::AssignmentExpression(a) = nodes::strip(&es.expression) else { continue };
                    let (key, ok) = match &a.left {
                        AssignmentTarget::StaticMemberExpression(m) => (
                            P::Js(AstKind::IdentifierName(&m.property)),
                            matches!(nodes::strip(&m.object), Expression::ThisExpression(_)),
                        ),
                        AssignmentTarget::PrivateFieldExpression(m) => (
                            P::Js(AstKind::PrivateIdentifier(&m.field)),
                            matches!(nodes::strip(&m.object), Expression::ThisExpression(_)),
                        ),
                        AssignmentTarget::ComputedMemberExpression(m) => (
                            nodes::expr(&m.expression),
                            matches!(nodes::strip(&m.object), Expression::ThisExpression(_))
                                && is_literal(nodes::expr(&m.expression)),
                        ),
                        _ => continue,
                    };
                    if !ok {
                        continue;
                    }
                    let ap = P::Js(AstKind::AssignmentExpression(a));
                    handle(self, &mut state_fields, &fields, ap, key, Some(nodes::expr(&a.right)), false)?;
                }
            }
        }

        self.state_fields.push(state_fields);
        let idx = (self.state_fields.len() - 1) as u32;
        self.next(p, &State { state_fields: idx, ..*st })
    }

    // -----------------------------------------------------------------------------------
    // module-level statements

    fn export_named_declaration(&mut self, p: P<'s>, k: AstKind<'s>, st: &State<'s>) -> Res {
        self.next(p, st)?;
        let specifiers: &[ExportSpecifier] = match k {
            AstKind::ExportNamedDeclaration(d) => &d.specifiers,
            AstKind::ExportFromDeclaration(d) => &d.specifiers,
            _ => &[],
        };
        if specifiers.iter().any(|s| !s.export_kind.is_type() && module_export_name_str(&s.exported) == "default") {
            return Err(e::module_illegal_default_export(self.loc(p)));
        }
        if let AstKind::ExportDeclaration(d) = k {
            if let Declaration::VariableDeclaration(v) = &d.declaration {
                for declarator in &v.declarations {
                    for id in scope::extract_identifiers(nodes::binding(&declarator.id)) {
                        let Some(b) = self.get(st.scope, id.name) else { continue };
                        let b = self.binding(b);
                        if b.kind == Kind::Derived {
                            return Err(e::derived_invalid_export(self.loc(p)));
                        }
                        if matches!(b.kind, Kind::State | Kind::RawState) && b.reassigned {
                            return Err(e::state_invalid_export(self.loc(p)));
                        }
                    }
                }
                if self.runes && st.ast_type == AstType::Instance && v.kind == VariableDeclarationKind::Let {
                    return Err(e::legacy_export_invalid(self.loc(p)));
                }
            }
        }
        Ok(())
    }

    fn validate_export(&self, node: P<'s>, scope: super::ScopeId, name: &str) -> Res {
        let Some(b) = self.get(scope, name) else { return Ok(()) };
        let b = self.binding(b);
        if b.kind == Kind::Derived {
            return Err(e::derived_invalid_export(self.loc(node)));
        }
        if matches!(b.kind, Kind::State | Kind::RawState) && b.reassigned {
            return Err(e::state_invalid_export(self.loc(node)));
        }
        Ok(())
    }

    fn legacy_component_creation(&mut self, expression: &'s Expression<'s>, st: &State<'s>) {
        let Expression::NewExpression(n) = nodes::strip(expression) else { return };
        let Expression::Identifier(callee) = nodes::strip(&n.callee) else { return };
        if n.arguments.len() != 1 {
            return;
        }
        let Some(Expression::ObjectExpression(o)) = n.arguments[0].as_expression().map(nodes::strip) else { return };
        let has_target = o.properties.iter().any(|p| {
            matches!(p, ObjectPropertyKind::ObjectProperty(p) if matches!(&p.key, PropertyKey::StaticIdentifier(k) if k.name == "target"))
        });
        if !has_target {
            return;
        }
        let Some(b) = self.get(st.scope, callee.name.as_str()) else { return };
        let binding = self.binding(b);
        if binding.kind == Kind::Normal && binding.declaration_kind == DeclKind::Import {
            if let Some(P::Js(AstKind::ImportDeclaration(i))) = binding.initial {
                let name = binding.node.name;
                let is_default = i.specifiers.iter().flatten().any(|s| {
                    matches!(s, ImportDeclarationSpecifier::ImportDefaultSpecifier(d) if d.local.name == name)
                });
                if is_default {
                    let np = nodes::expr(expression);
                    self.warn(Some(np), w::legacy_component_creation());
                }
            }
        }
    }

    fn labeled_statement(&mut self, p: P<'s>, l: &'s LabeledStatement<'s>, st: &State<'s>) -> Res {
        if l.label.name == "$" {
            let parent_is_program = matches!(self.path.last(), Some(P::Js(AstKind::Program(_))));
            if st.ast_type == AstType::Instance && parent_is_program {
                if self.runes {
                    return Err(e::legacy_reactive_statement_invalid(self.loc(p)));
                }
                self.reactive_statements.push(ReactiveStatement {
                    assignments: Vec::new(),
                    dependencies: Vec::new(),
                    node_start: l.span.start as usize,
                    node_end: l.span.end as usize,
                });
                let rs = (self.reactive_statements.len() - 1) as u32;
                let depth = self.sc.scope(st.scope).function_depth + 1;
                self.next(p, &State { reactive_statement: Some(rs), function_depth: depth, ..*st })?;

                // every referenced binding becomes a dependency, unless it's on the left of `=`
                let names: Vec<(&'s str, Vec<u32>)> =
                    self.sc.scope(st.scope).references.iter().map(|(k, v)| (*k, v.clone())).collect();
                for (name, refs) in names {
                    let Some(b) = self.get(st.scope, name) else { continue };
                    for r in refs {
                        let node = self.sc.refs[r as usize].node;
                        let path = self.sc.ref_path(r);
                        let mut left_key = node.key;
                        let mut i = path.len() as isize - 1;
                        let mut parent = if i >= 0 { Some(path[i as usize]) } else { None };
                        while let Some(pp) = parent.filter(|pp| is_member(*pp)) {
                            left_key = pp.key();
                            i -= 1;
                            parent = if i >= 0 { Some(path[i as usize]) } else { None };
                        }
                        if let Some(P::Js(AstKind::AssignmentExpression(a))) = parent {
                            if a.operator == AssignmentOperator::Assign && nodes::target(&a.left).key() == left_key {
                                continue;
                            }
                        }
                        self.reactive_statements[rs as usize].dependencies.push(b);
                        break;
                    }
                }

                if let Statement::ExpressionStatement(es) = &l.body {
                    if let Expression::AssignmentExpression(a) = nodes::strip(&es.expression) {
                        let left = nodes::target(&a.left);
                        let mut ids = scope::extract_identifiers(left);
                        if is_member(left) {
                            if let Some(id) = object(left) {
                                ids = smallvec::smallvec![id];
                            }
                        }
                        for id in ids {
                            if let Some(b) = self.get(st.scope, id.name) {
                                if self.binding(b).kind == Kind::LegacyReactive {
                                    let deps = self.reactive_statements[rs as usize].dependencies.clone();
                                    self.sc.binding_mut(b).legacy_dependencies = deps;
                                }
                            }
                        }
                    }
                }
            } else if !self.runes {
                self.warn(Some(p), w::reactive_declaration_invalid_placement());
            }
        }
        self.next(p, st)
    }

    fn variable_declarator(&mut self, p: P<'s>, d: &'s VariableDeclarator<'s>, st: &State<'s>) -> Res {
        let id_p = nodes::binding(&d.id);
        // ensure_no_module_import_conflict
        for id in scope::extract_identifiers(id_p) {
            if st.ast_type == AstType::Instance
                && st.scope == self.instance_scope
                && self.get(self.module_scope, id.name).is_some_and(|b| self.binding(b).declaration_kind == DeclKind::Import)
            {
                return Err(e::declaration_duplicate_module_import(self.loc(id_p)));
            }
        }

        let init = d.init.as_ref().map(nodes::expr);
        if self.runes {
            let rune = get_rune(&self.sc, init, st.scope);
            let paths = extract_paths(id_p);
            for (path, _) in &paths {
                if let Some(id) = ident(*path) {
                    self.validate_identifier_name_of(st.scope, id.name)?;
                }
            }
            if let Some(rune @ ("$state" | "$state.raw" | "$derived" | "$derived.by" | "$props")) = rune {
                for (path, is_rest) in &paths {
                    let Some(id) = ident(*path) else { continue };
                    let Some(b) = self.get(st.scope, id.name) else { continue };
                    self.sc.binding_mut(b).kind = match rune {
                        "$state" => Kind::State,
                        "$state.raw" => Kind::RawState,
                        "$derived" | "$derived.by" => Kind::Derived,
                        _ if *is_rest => Kind::RestProp,
                        _ => Kind::Prop,
                    };
                }
            }

            if rune == Some("$props") {
                let is_object = matches!(d.id, BindingPattern::ObjectPattern(_));
                let is_ident = matches!(d.id, BindingPattern::BindingIdentifier(_));
                if !is_object && !is_ident {
                    return Err(e::props_invalid_identifier(self.loc(p)));
                }
                if self.custom_element && !self.custom_element_props {
                    match &d.id {
                        BindingPattern::BindingIdentifier(_) => self.warn(Some(id_p), w::custom_element_props_identifier()),
                        BindingPattern::ObjectPattern(o) => {
                            if let Some(r) = &o.rest {
                                self.warn(Some(P::Js(AstKind::BindingRestElement(r))), w::custom_element_props_identifier());
                            }
                        }
                        _ => {}
                    }
                }
                match &d.id {
                    BindingPattern::BindingIdentifier(i) => {
                        if let Some(b) = self.get(st.scope, i.name.as_str()) {
                            let b = self.sc.binding_mut(b);
                            b.initial = None;
                            b.kind = Kind::RestProp;
                        }
                    }
                    BindingPattern::ObjectPattern(o) => {
                        for property in &o.properties {
                            let pp = P::Js(AstKind::BindingProperty(property));
                            if property.computed {
                                return Err(e::props_invalid_pattern(self.loc(pp)));
                            }
                            if let PropertyKey::StaticIdentifier(k) = &property.key {
                                if k.name.starts_with("$$") {
                                    return Err(e::props_illegal_name(self.loc(pp)));
                                }
                            }
                            let (value, initial) = match &property.value {
                                BindingPattern::AssignmentPattern(a) => (nodes::binding(&a.left), Some(&a.right)),
                                v => (nodes::binding(v), None),
                            };
                            let Some(value_id) = ident(value) else {
                                return Err(e::props_invalid_pattern(self.loc(pp)));
                            };
                            let alias: &'s str = match &property.key {
                                PropertyKey::StaticIdentifier(k) => k.name.as_str(),
                                k => self.alloc.alloc_str(&get_name(nodes::property_key(k)).unwrap_or_default()),
                            };
                            let Some(b) = self.get(st.scope, value_id.name) else { continue };
                            let mut new_initial = initial.map(nodes::expr);
                            let mut bindable = false;
                            if let Some(Expression::CallExpression(c)) = initial.map(nodes::strip) {
                                if matches!(nodes::strip(&c.callee), Expression::Identifier(i) if i.name == "$bindable") {
                                    new_initial = c.arguments.first().and_then(|a| a.as_expression()).map(nodes::expr);
                                    if matches!(c.arguments.first(), Some(Argument::SpreadElement(s)) if true || s.span.start == 0) {
                                        new_initial = c.arguments.first().map(nodes::argument);
                                    }
                                    bindable = true;
                                }
                            }
                            let b = self.sc.binding_mut(b);
                            b.prop_alias = Some(alias);
                            b.initial = new_initial;
                            if bindable {
                                b.kind = Kind::BindableProp;
                            }
                        }
                    }
                    _ => {}
                }
            }
        } else if let Some(Expression::CallExpression(c)) = d.init.as_ref().map(nodes::strip) {
            if let Expression::Identifier(callee) = nodes::strip(&c.callee) {
                let name = callee.name.as_str();
                if (name == "$state" || name == "$derived" || name == "$props")
                    && self.get(st.scope, name).is_none_or(|b| self.binding(b).kind != Kind::StoreSub)
                {
                    return Err(e::rune_invalid_usage(self.loc(nodes::expr(d.init.as_ref().unwrap())), name));
                }
            }
        }

        if get_rune(&self.sc, init, st.scope) == Some("$props") {
            self.visit_child(p, id_p, &State { function_depth: st.function_depth + 1, ..*st })?;
            self.visit_child(p, init.unwrap(), st)?;
            Ok(())
        } else {
            self.next(p, st)
        }
    }
}

/// `extract_paths(param)`: the identifiers (and member expressions) of a pattern, with `is_rest`
fn extract_paths<'s>(p: P<'s>) -> smallvec::SmallVec<[(P<'s>, bool); 4]> {
    use AstKind as K;
    let mut out = smallvec::SmallVec::new();
    fn go<'s>(p: P<'s>, out: &mut smallvec::SmallVec<[(P<'s>, bool); 4]>) {
        use AstKind as K;
        match p {
            _ if ident(p).is_some() || is_member(p) => out.push((p, false)),
            P::Js(K::ObjectPattern(o)) => {
                for prop in &o.properties {
                    go(nodes::binding(&prop.value), out);
                }
                if let Some(r) = &o.rest {
                    let arg = nodes::binding(&r.argument);
                    if ident(arg).is_some() { out.push((arg, true)) } else { go(arg, out) }
                }
            }
            P::Js(K::ArrayPattern(a)) => {
                for el in a.elements.iter().flatten() {
                    go(nodes::binding(el), out);
                }
                if let Some(r) = &a.rest {
                    let arg = nodes::binding(&r.argument);
                    if ident(arg).is_some() { out.push((arg, true)) } else { go(arg, out) }
                }
            }
            P::Js(K::AssignmentPattern(a)) => go(nodes::binding(&a.left), out),
            _ => {}
        }
    }
    let _ = K::Program;
    go(p, &mut out);
    out
}

fn dummy_id<'s>() -> Id<'s> {
    Id { name: "", span: None, key: 0 }
}

pub fn has_bidi(s: &str) -> bool {
    let b = s.as_bytes();
    b.contains(&0xE2)
        && b.windows(3).any(|w| {
            w[0] == 0xE2 && ((w[1] == 0x80 && (0xAA..=0xAE).contains(&w[2])) || (w[1] == 0x81 && (0xA6..=0xA9).contains(&w[2])))
        })
}

fn is_bidi(c: char) -> bool {
    matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

fn legacy_replacement(code: &str) -> String {
    match code {
        "non-top-level-reactive-declaration" => "reactive_declaration_invalid_placement".into(),
        "module-script-reactive-declaration" => "reactive_declaration_module_script".into(),
        "empty-block" => "block_empty".into(),
        "avoid-is" => "attribute_avoid_is".into(),
        "invalid-html-attribute" => "attribute_invalid_property_name".into(),
        "a11y-structure" => "a11y_figcaption_parent".into(),
        "illegal-attribute-character" => "attribute_illegal_colon".into(),
        "invalid-rest-eachblock-binding" => "bind_invalid_each_rest".into(),
        "unused-export-let" => "export_let_unused".into(),
        _ => code.replace('-', "_"),
    }
}

/// `get_name(node)` from `phases/nodes.js`
pub fn get_name(p: P) -> Option<String> {
    use AstKind as K;
    Some(match p {
        P::Js(K::StringLiteral(s)) => s.value.to_string(),
        P::Js(K::NumericLiteral(n)) => js_number(n.value),
        P::Js(K::BooleanLiteral(b)) => b.value.to_string(),
        P::Js(K::NullLiteral(_)) => "null".into(),
        P::Js(K::BigIntLiteral(b)) => b.raw.as_ref().map_or_else(String::new, |r| r.trim_end_matches('n').to_string()),
        P::Js(K::RegExpLiteral(r)) => format!("/{}/{}", r.regex.pattern.text, r.regex.flags),
        P::Js(K::PrivateIdentifier(i)) => format!("#{}", i.name),
        _ => ident(p)?.name.to_string(),
    })
}

fn is_literal(p: P) -> bool {
    use AstKind as K;
    matches!(
        p,
        P::Js(
            K::StringLiteral(_)
                | K::NumericLiteral(_)
                | K::BooleanLiteral(_)
                | K::NullLiteral(_)
                | K::BigIntLiteral(_)
                | K::RegExpLiteral(_)
        )
    )
}

/// `String(number)`
pub fn js_number(v: f64) -> String {
    if v.is_nan() {
        "NaN".into()
    } else if v.is_infinite() {
        if v > 0.0 { "Infinity".into() } else { "-Infinity".into() }
    } else if v == v.trunc() && v.abs() < 1e21 {
        format!("{}", v as i128)
    } else {
        format!("{v}")
    }
}

fn module_export_name_str<'s>(n: &'s ModuleExportName<'s>) -> &'s str {
    match n {
        ModuleExportName::IdentifierName(i) => i.name.as_str(),
        ModuleExportName::IdentifierReference(i) => i.name.as_str(),
        ModuleExportName::StringLiteral(s) => s.value.as_str(),
    }
}

// ---------------------------------------------------------------------------------------
// template visitors

impl<'s> Analyzer<'s> {
    fn visit_node(&mut self, p: P<'s>, n: NodeId, st: &State<'s>) -> Res {
        let ast = self.ast;
        match &ast.nodes[n] {
            Node::Text { start, data, .. } => {
                let parent_is_fragment = matches!(self.path.last(), Some(P::Fragment(_) | P::SlotFragment(_)));
                if parent_is_fragment && utils::has_non_whitespace(data) {
                    if let Some(pe) = st.parent_element {
                        if let Some(message) = utils::is_tag_valid_with_parent("#text", pe) {
                            return Err(e::node_invalid_placement(self.loc(p), &message));
                        }
                    }
                }
                self.text(p, *start, data);
                Ok(())
            }
            Node::Comment { .. } => Ok(()),
            Node::ExpressionTag { .. } => {
                let parent = self.path.last().copied();
                let in_template = matches!(parent, Some(P::Fragment(_) | P::SlotFragment(_)));
                if in_template {
                    if let Some(pe) = st.parent_element {
                        if let Some(message) = utils::is_tag_valid_with_parent("#text", pe) {
                            return Err(e::node_invalid_placement(self.loc(p), &message));
                        }
                    }
                }
                let meta = self.new_meta();
                self.next(p, &State { expression: Some(meta), ..*st })
            }
            Node::HtmlTag { .. } => {
                if self.runes {
                    self.validate_opening_tag(n, '@')?;
                }
                let meta = self.new_meta();
                self.next(p, &State { expression: Some(meta), ..*st })
            }
            Node::DebugTag { .. } => {
                if self.runes {
                    self.validate_opening_tag(n, '@')?;
                }
                self.next(p, st)
            }
            Node::ConstTag { id, init, .. } => self.const_tag(p, n, id, init, st),
            Node::DeclarationTag { declaration, .. } => self.declaration_tag(p, n, declaration, st),
            Node::RenderTag { expression, .. } => self.render_tag(p, n, expression, st),
            Node::IfBlock { elseif, test, consequent, alternate, .. } => {
                self.validate_block_not_empty(Some(*consequent));
                self.validate_block_not_empty(*alternate);
                if self.runes {
                    self.validate_opening_tag(n, if *elseif { ':' } else { '#' })?;
                }
                let meta = self.new_meta();
                self.visit_child(p, nodes::template_expr(test), &State { expression: Some(meta), ..*st })?;
                self.visit_child(p, P::Fragment(*consequent), st)?;
                if let Some(a) = alternate {
                    self.visit_child(p, P::Fragment(*a), st)?;
                }
                Ok(())
            }
            Node::EachBlock { .. } => self.each_block(p, n, st),
            Node::AwaitBlock { expression, value, error, pending, then, catch, .. } => {
                self.validate_block_not_empty(*pending);
                self.validate_block_not_empty(*then);
                self.validate_block_not_empty(*catch);
                if self.runes {
                    self.validate_opening_tag(n, '#')?;
                    for (pattern, keyword) in [(value, "then"), (error, "catch")] {
                        let Some(pattern) = pattern else { continue };
                        let start = pattern.start();
                        let window_start = utf16_back(self.source, start, 10);
                        if let Some(true) = match_then_catch(&self.source[window_start..start], keyword) {
                            return Err(e::block_unexpected_character((window_start, start), ":"));
                        }
                    }
                }
                let meta = self.new_meta();
                self.visit_child(p, nodes::template_expr(expression), &State { expression: Some(meta), ..*st })?;
                for f in [pending, then, catch].into_iter().flatten() {
                    self.visit_child(p, P::Fragment(*f), st)?;
                }
                Ok(())
            }
            Node::KeyBlock { expression, fragment, .. } => {
                self.validate_block_not_empty(Some(*fragment));
                if self.runes {
                    self.validate_opening_tag(n, '#')?;
                }
                let meta = self.new_meta();
                self.visit_child(p, nodes::template_expr(expression), &State { expression: Some(meta), ..*st })?;
                self.visit_child(p, P::Fragment(*fragment), st)
            }
            Node::SnippetBlock { .. } => self.snippet_block(p, n, st),
            Node::Element(el) => match el.kind {
                "RegularElement" => self.regular_element(p, n, st),
                "SvelteElement" => self.svelte_element(p, n, st),
                "Component" => self.visit_component(p, n, st),
                "SvelteComponent" => {
                    if self.runes {
                        self.warn(Some(p), w::svelte_component_deprecated());
                    }
                    if let Some(e) = &el.expression {
                        let meta = self.new_meta();
                        self.visit_child(p, nodes::template_expr(e), &State { expression: Some(meta), ..*st })?;
                    }
                    self.visit_component(p, n, st)
                }
                "SvelteSelf" => {
                    let valid = self.path.iter().any(|q| {
                        matches!(self.ty(*q), "IfBlock" | "EachBlock" | "Component" | "SnippetBlock")
                    });
                    if !valid {
                        return Err(e::svelte_self_invalid_placement(self.loc(p)));
                    }
                    if self.runes {
                        let basename = self.filename.rsplit(['/', '\\']).next().unwrap_or("").to_string();
                        let name = self.name.clone();
                        self.warn(Some(p), w::svelte_self_deprecated(&name, &basename));
                    }
                    self.visit_component(p, n, st)
                }
                "SvelteFragment" => {
                    let parent = self.path.len().checked_sub(2).map(|i| self.path[i]);
                    if !parent.is_some_and(|q| matches!(self.ty(q), "Component" | "SvelteComponent")) {
                        return Err(e::svelte_fragment_invalid_placement(self.loc(p)));
                    }
                    for a in &el.attributes {
                        match a {
                            Attr::Attribute { name: "slot", .. } => self.validate_slot_attribute(a, false, st)?,
                            Attr::Attribute { .. } | Attr::Directive { kind: "LetDirective", .. } => {}
                            _ => return Err(e::svelte_fragment_invalid_attribute(self.loc(P::Attr(a)))),
                        }
                    }
                    self.next(p, &State { parent_element: None, ..*st })
                }
                "SvelteHead" => {
                    if let Some(a) = el.attributes.first() {
                        return Err(e::svelte_head_illegal_attribute(self.loc(P::Attr(a))));
                    }
                    self.next(p, st)
                }
                "SvelteBody" | "SvelteDocument" | "SvelteWindow" => {
                    self.disallow_children(n)?;
                    for a in &el.attributes {
                        if matches!(a, Attr::Attribute { .. }) && is_event_attribute(a) {
                            self.check_global_event_reference(a, st);
                        } else if matches!(a, Attr::Attribute { .. } | Attr::Spread { .. }) {
                            let loc = self.loc(P::Attr(a));
                            return Err(match el.kind {
                                "SvelteBody" => e::svelte_body_illegal_attribute(loc),
                                "SvelteDocument" => e::illegal_element_attribute(loc, "svelte:document"),
                                _ => e::illegal_element_attribute(loc, "svelte:window"),
                            });
                        }
                    }
                    self.next(p, st)
                }
                "SvelteBoundary" => {
                    for a in &el.attributes {
                        let loc = self.loc(P::Attr(a));
                        let valid = matches!(a, Attr::Attribute { name: "onerror" | "failed" | "pending", .. });
                        if !valid {
                            return Err(e::svelte_boundary_invalid_attribute(loc));
                        }
                        if let Attr::Attribute { value, .. } = a {
                            let bad = match value {
                                AttrValue::True => true,
                                AttrValue::Sequence(c) => c.len() != 1 || !matches!(c[0], Chunk::Expression { .. }),
                                AttrValue::Expression(_) => false,
                            };
                            if bad {
                                return Err(e::svelte_boundary_invalid_attribute_value(loc));
                            }
                        }
                    }
                    self.next(p, st)
                }
                "SlotElement" => {
                    if self.runes && !self.custom_element {
                        self.warn(Some(p), w::slot_element_deprecated());
                    }
                    let mut name: &'s str = "default";
                    for a in &el.attributes {
                        match a {
                            Attr::Attribute { name: attr_name, value, .. } => {
                                if *attr_name == "name" {
                                    let Some(text) = text_value(value) else {
                                        return Err(e::slot_element_invalid_name(self.loc(P::Attr(a))));
                                    };
                                    name = text;
                                    if name == "default" {
                                        return Err(e::slot_element_invalid_name_default(self.loc(P::Attr(a))));
                                    }
                                }
                            }
                            Attr::Spread { .. } | Attr::Directive { kind: "LetDirective", .. } => {}
                            _ => return Err(e::slot_element_invalid_attribute(self.loc(P::Attr(a)))),
                        }
                    }
                    match self.slot_names.iter_mut().find(|(k, _)| *k == name) {
                        Some(entry) => entry.1 = n,
                        None => self.slot_names.push((name, n)),
                    }
                    self.next(p, st)
                }
                "TitleElement" => {
                    if let Some(a) = el.attributes.first() {
                        return Err(e::title_illegal_attribute(self.loc(P::Attr(a))));
                    }
                    for &child in &ast.fragments[el.fragment].nodes {
                        if !matches!(ast.nodes[child], Node::Text { .. } | Node::ExpressionTag { .. }) {
                            return Err(e::title_invalid_content(self.node_loc(child)));
                        }
                    }
                    self.next(p, st)
                }
                _ => self.next(p, st),
            },
        }
    }

    fn text(&mut self, p: P<'s>, start: usize, data: &'s str) {
        let parent = self.path.last().copied();
        let parent_is_fragment = matches!(parent, Some(P::Fragment(_) | P::SlotFragment(_)));
        // node_invalid_placement is an error, checked by the caller for Text nodes
        if !has_bidi(data) {
            return;
        }
        let mut i = 0;
        let bytes = data.as_bytes();
        while i < data.len() {
            let c = data[i..].chars().next().unwrap();
            if !is_bidi(c) {
                i += c.len_utf8();
                continue;
            }
            let run_start = i;
            while i < data.len() {
                let c = data[i..].chars().next().unwrap();
                if !is_bidi(c) {
                    break;
                }
                i += c.len_utf8();
            }
            let _ = bytes;
            let mut is_ignored = false;
            if let (true, Some(parent), P::Node(me)) = (parent_is_fragment, parent, p) {
                let siblings: Vec<NodeId> = self.fragment_nodes(parent).to_vec();
                for child in siblings {
                    if child == me {
                        break;
                    }
                    if let Node::Comment { start, data, .. } = &self.ast.nodes[child] {
                        let found = self.extract_svelte_ignore(start + 4, data);
                        is_ignored |= found.contains(&"bidirectional_control_characters");
                    }
                }
            }
            if !is_ignored {
                self.warn_range(start + run_start, start + i, w::bidirectional_control_characters());
            }
        }
    }

    /// `validate_opening_tag(node, state, expected)`
    fn validate_opening_tag(&self, n: NodeId, expected: char) -> Res {
        let start = self.ast.nodes[n].start();
        if self.source[start + 1..].chars().next() != Some(expected) {
            let end = utf16_forward(self.source, start, 5);
            return Err(e::block_unexpected_character((start, end), &expected.to_string()));
        }
        Ok(())
    }

    fn validate_block_not_empty(&mut self, f: Option<usize>) {
        let Some(f) = f else { return };
        let nodes = &self.ast.fragments[f].nodes;
        if nodes.len() == 1 {
            if let Node::Text { raw, .. } = &self.ast.nodes[nodes[0]] {
                if utils::js_trim(raw).is_empty() {
                    let first = nodes[0];
                    self.warn(Some(P::Node(first)), w::block_empty());
                }
            }
        }
    }

    fn disallow_children(&self, n: NodeId) -> Res {
        let el = self.element(n).unwrap();
        let nodes = &self.ast.fragments[el.fragment].nodes;
        if let (Some(&first), Some(&last)) = (nodes.first(), nodes.last()) {
            let start = self.ast.nodes[first].start();
            let end = self.ast.nodes[last].end().unwrap_or(usize::MAX);
            return Err(e::svelte_meta_invalid_content((start, end), el.name));
        }
        Ok(())
    }

    fn const_tag(&mut self, p: P<'s>, n: NodeId, id: &'s Pattern<'s>, init: &'s Expr<'s>, st: &State<'s>) -> Res {
        if self.runes {
            self.validate_opening_tag(n, '@')?;
        }
        let len = self.path.len();
        let parent = self.path.last().copied();
        let grand_parent = if len >= 2 { Some(self.path[len - 2]) } else { None };
        let gp_ty = grand_parent.map(|g| self.ty(g));
        let ok = matches!(parent, Some(P::Fragment(_) | P::SlotFragment(_)))
            && (matches!(
                gp_ty,
                Some(
                    "IfBlock"
                        | "SvelteFragment"
                        | "Component"
                        | "SvelteComponent"
                        | "EachBlock"
                        | "AwaitBlock"
                        | "SnippetBlock"
                        | "SvelteBoundary"
                        | "KeyBlock"
                )
            ) || (matches!(gp_ty, Some("RegularElement" | "SvelteElement"))
                && grand_parent
                    .and_then(|g| g.node())
                    .and_then(|g| self.element(g))
                    .is_some_and(|el| el.attributes.iter().any(|a| matches!(a, Attr::Attribute { name: "slot", .. })))));
        if !ok {
            return Err(e::const_tag_invalid_placement(self.loc(p)));
        }
        self.visit_child(p, nodes::pattern_p(id), st)?;
        let meta = self.new_meta();
        let depth = st.function_depth + 1;
        self.visit_child(
            p,
            nodes::template_expr(init),
            &State { expression: Some(meta), function_depth: depth, derived_function_depth: depth as i32, ..*st },
        )
    }

    fn declaration_tag(&mut self, p: P<'s>, _n: NodeId, declaration: &'s crate::ast::Declaration<'s>, st: &State<'s>) -> Res {
        if !self.runes && !self.maybe_runes {
            return Err(e::declaration_tag_no_legacy_mode(self.loc(p)));
        }
        let crate::ast::Declaration::Js(stmt) = declaration else { return Ok(()) };
        let decl = nodes::statement(&stmt.stmt);
        let is_top_level = self.path.len() == 1 && matches!(self.path[0], P::Fragment(_));
        if is_top_level {
            if let Statement::VariableDeclaration(v) = &stmt.stmt {
                for d in &v.declarations {
                    for id in scope::extract_identifiers(nodes::binding(&d.id)) {
                        if self.sc.scope(self.instance_scope).declarations.contains_key(id.name) {
                            return Err(e::declaration_duplicate(id.err_loc(), id.name));
                        }
                    }
                }
            }
        }
        let meta = self.new_meta();
        let depth = self.sc.scope(st.scope).function_depth;
        self.visit_child(
            p,
            decl,
            &State { in_declaration_tag: true, function_depth: depth, expression: Some(meta), ..*st },
        )
    }

    fn render_tag(&mut self, p: P<'s>, n: NodeId, expression: &'s Expr<'s>, st: &State<'s>) -> Res {
        self.validate_opening_tag(n, '@')?;
        self.save_path(n);
        let mut call = nodes::template_expr(expression);
        if let P::Js(AstKind::ChainExpression(c)) = call {
            call = nodes::chain_element(&c.expression);
        }
        let P::Js(AstKind::CallExpression(c)) = call else { return Ok(()) };
        let callee = nodes::expr(&c.callee);
        let binding = ident(callee).and_then(|i| self.get(st.scope, i.name));
        let resolved = ident(callee).is_some() && self.is_resolved_snippet(binding);
        let mut snippets = Vec::new();
        if let Some(b) = binding {
            if let Some(P::Node(s)) = self.binding(b).initial {
                if matches!(self.ast.nodes[s], Node::SnippetBlock { .. }) {
                    snippets.push(s);
                }
            }
        }
        self.renderer_snippets.insert(n, snippets);
        self.snippet_renderers.push((n, resolved));
        self.uses_render_tags = true;

        for a in &c.arguments {
            if let Argument::SpreadElement(s) = a {
                return Err(e::render_tag_invalid_spread_argument(self.loc(P::Js(AstKind::SpreadElement(s)))));
            }
        }
        if let P::Js(AstKind::StaticMemberExpression(m)) = callee {
            if matches!(m.property.name.as_str(), "bind" | "apply" | "call") {
                return Err(e::render_tag_invalid_call_expression(self.loc(p)));
            }
        }

        let meta = self.new_meta();
        self.visit_child(p, callee, &State { expression: Some(meta), ..*st })?;
        for a in &c.arguments {
            let meta = self.new_meta();
            self.visit_child(p, nodes::argument(a), &State { expression: Some(meta), ..*st })?;
        }
        Ok(())
    }

    /// `is_resolved_snippet(binding)`
    fn is_resolved_snippet(&self, binding: Option<BindingId>) -> bool {
        let Some(b) = binding else { return true };
        let b = self.binding(b);
        b.declaration_kind == DeclKind::Import
            || matches!(b.kind, Kind::Prop | Kind::RestProp | Kind::BindableProp)
            || matches!(b.initial, Some(P::Node(n)) if matches!(self.ast.nodes[n], Node::SnippetBlock { .. }))
    }

    fn each_block(&mut self, p: P<'s>, n: NodeId, st: &State<'s>) -> Res {
        let Node::EachBlock { expression, context, body, fallback, index, key, .. } = &self.ast.nodes[n] else {
            unreachable!()
        };
        self.validate_opening_tag(n, '#')?;
        self.validate_block_not_empty(Some(*body));
        self.validate_block_not_empty(*fallback);

        if let Some(Pattern::Ident { name, .. }) = context {
            if name == "$state" || name == "$derived" {
                return Err(e::state_invalid_placement(self.loc(p), name));
            }
        }
        let keyed = key.as_ref().is_some_and(|k| {
            let kp = nodes::template_expr(k);
            ident(kp).is_none_or(|i| index.as_deref() != Some(i.name))
        });
        if keyed && context.is_none() {
            return Err(e::each_key_without_as(self.loc(nodes::template_expr(key.as_ref().unwrap()))));
        }

        let meta = self.new_meta();
        self.metas[meta as usize].track_deps = !self.runes;
        let parent_scope = self.sc.scope(st.scope).parent.unwrap_or(st.scope);
        self.visit_child(
            p,
            nodes::template_expr(expression),
            &State { expression: Some(meta), scope: parent_scope, ..*st },
        )?;
        self.visit_child(p, P::Fragment(*body), st)?;
        if let Some(k) = key {
            self.visit_child(p, nodes::template_expr(k), st)?;
        }
        if let Some(f) = fallback {
            self.visit_child(p, P::Fragment(*f), st)?;
        }

        if !self.runes {
            let mutated = context.as_ref().is_some_and(|c| {
                scope::extract_identifiers(nodes::pattern_p(c))
                    .iter()
                    .any(|id| self.get(st.scope, id.name).is_some_and(|b| self.binding(b).mutated))
            });
            let mut transitive: Vec<BindingId> = Vec::new();
            for b in self.metas[meta as usize].dependencies.clone() {
                if self.binding(b).declaration_kind != DeclKind::Function {
                    self.collect_transitive_dependencies(b, &mut transitive);
                }
            }
            if mutated {
                for b in transitive {
                    let b = self.sc.binding_mut(b);
                    if b.kind == Kind::Normal && matches!(b.declaration_kind, DeclKind::Const | DeclKind::Let | DeclKind::Var) {
                        b.kind = Kind::State;
                    }
                }
            }
        }
        Ok(())
    }

    fn collect_transitive_dependencies(&self, b: BindingId, out: &mut Vec<BindingId>) {
        if out.contains(&b) {
            return;
        }
        out.push(b);
        if self.binding(b).kind == Kind::LegacyReactive {
            for dep in self.binding(b).legacy_dependencies.clone() {
                self.collect_transitive_dependencies(dep, out);
            }
        }
    }

    fn snippet_block(&mut self, p: P<'s>, n: NodeId, st: &State<'s>) -> Res {
        let Node::SnippetBlock { expression, parameters, body, .. } = &self.ast.nodes[n] else { unreachable!() };
        self.snippets.push(n);
        self.validate_block_not_empty(Some(*body));
        if self.runes {
            self.validate_opening_tag(n, '#')?;
        }
        if let Some(arrow) = parameters {
            if let Expression::ArrowFunctionExpression(a) = &arrow.expr {
                if let Some(rest) = &a.params.rest {
                    return Err(e::snippet_invalid_rest_parameter(self.loc(P::Js(AstKind::BindingRestElement(&rest.rest)))));
                }
            }
        }
        self.next(p, &State { parent_element: None, ..*st })?;

        let name = ident(nodes::template_expr(expression)).map_or("", |i| i.name);
        let is_top_level = self.path.len() == 1 && matches!(self.path[0], P::Fragment(_));
        if is_top_level {
            if self.sc.scope(self.instance_scope).declarations.contains_key(name) {
                return Err(e::declaration_duplicate(self.loc(nodes::template_expr(expression)), name));
            }
            let mut visited = Vec::new();
            if self.can_hoist_snippet(st.scope, &mut visited) {
                if let Some(b) = self.get(st.scope, name) {
                    let module = self.module_scope;
                    let node_name = self.binding(b).node.name;
                    self.sc.scopes[module as usize].declarations.insert(node_name, b);
                }
            }
        }

        let len = self.path.len();
        let Some(parent) = len.checked_sub(2).map(|i| self.path[i]) else { return Ok(()) };
        let parent_el = parent.node().and_then(|q| self.element(q));
        if let Some(el) = parent_el.filter(|el| el.kind == "Component") {
            let shadows = el.attributes.iter().any(|a| match a {
                Attr::Attribute { name: an, .. } | Attr::Directive { kind: "BindDirective", name: an, .. } => *an == name,
                _ => false,
            });
            if shadows {
                return Err(e::snippet_shadowing_prop(self.loc(p), name));
            }
        }
        if name != "children" {
            return Ok(());
        }
        if let Some(el) = parent_el.filter(|el| matches!(el.kind, "Component" | "SvelteComponent" | "SvelteSelf")) {
            let conflict = self.ast.fragments[el.fragment].nodes.iter().any(|&c| match &self.ast.nodes[c] {
                Node::SnippetBlock { .. } | Node::Comment { .. } => false,
                Node::Text { data, .. } => !utils::js_trim(data).is_empty(),
                _ => true,
            });
            if conflict {
                return Err(e::snippet_conflict(self.loc(p)));
            }
        }
        Ok(())
    }

    fn can_hoist_snippet(&self, scope: super::ScopeId, visited: &mut Vec<BindingId>) -> bool {
        let depth = self.sc.scope(scope).function_depth;
        for &name in self.sc.scope(scope).references.keys() {
            let Some(b) = self.get(scope, name) else { continue };
            let binding = self.binding(b);
            let bdepth = self.sc.scope(binding.scope).function_depth;
            if bdepth == 0 || bdepth >= depth {
                continue;
            }
            if let Some(P::Node(s)) = binding.initial {
                if matches!(self.ast.nodes[s], Node::SnippetBlock { .. }) {
                    if visited.contains(&b) {
                        continue;
                    }
                    visited.push(b);
                    if let Some(&snippet_scope) = self.sc.map.get(&P::Node(s).key()) {
                        if self.can_hoist_snippet(snippet_scope, visited) {
                            continue;
                        }
                    }
                }
            }
            return false;
        }
        true
    }

    // -----------------------------------------------------------------------------------
    // elements

    fn regular_element(&mut self, p: P<'s>, n: NodeId, st: &State<'s>) -> Res {
        let el = self.element(n).unwrap();
        self.validate_element(n, st)?;
        super::a11y::check_element(self, n)?;

        self.save_path(n);
        self.elements.push(n);

        let frag_nodes = &self.ast.fragments[el.fragment].nodes;
        let mut textarea_moved = false;
        if el.name == "textarea" && !frag_nodes.is_empty() {
            for a in &el.attributes {
                if matches!(a, Attr::Attribute { name: "value", .. }) {
                    return Err(e::textarea_invalid_content(self.loc(p)));
                }
            }
            if frag_nodes.len() > 1 || !matches!(self.ast.nodes[frag_nodes[0]], Node::Text { .. }) {
                textarea_moved = true;
                self.textarea_values.insert(n);
            }
        }

        if let Some(b) = self.get(st.scope, el.name) {
            let b = self.binding(b);
            if b.declaration_kind == DeclKind::Import && b.references.is_empty() {
                self.warn(Some(p), w::component_name_lowercase(el.name));
            }
        }

        if let Some(parent_element) = st.parent_element {
            let mut past_parent = false;
            let mut only_warn = false;
            let mut ancestors: Vec<&str> = vec![parent_element];
            for i in (0..self.path.len()).rev() {
                let ancestor = self.path[i];
                let ty = self.ty(ancestor);
                if matches!(ty, "IfBlock" | "EachBlock" | "AwaitBlock" | "KeyBlock") {
                    only_warn = true;
                }
                if !past_parent {
                    if ty == "RegularElement" && ancestor.node().and_then(|a| self.element(a)).is_some_and(|a| a.name == parent_element) {
                        if let Some(message) = utils::is_tag_valid_with_parent(el.name, parent_element) {
                            if only_warn {
                                self.warn(Some(p), w::node_invalid_placement_ssr(&message));
                            } else {
                                return Err(e::node_invalid_placement(self.loc(p), &message));
                            }
                        }
                        past_parent = true;
                    }
                } else if ty == "RegularElement" {
                    ancestors.push(self.element(ancestor.node().unwrap()).unwrap().name);
                    if let Some(message) = utils::is_tag_valid_with_ancestor(el.name, &ancestors) {
                        if only_warn {
                            self.warn(Some(p), w::node_invalid_placement_ssr(&message));
                        } else {
                            return Err(e::node_invalid_placement(self.loc(p), &message));
                        }
                    }
                } else if matches!(ty, "Component" | "SvelteComponent" | "SvelteElement" | "SvelteSelf" | "SnippetBlock") {
                    break;
                }
            }
        }

        // strip any namespace from the beginning of the name
        let node_name = strip_namespaces(el.name);
        if let Some(end) = el.end {
            if end >= 2 && self.source.as_bytes()[end - 2] == b'/' && !utils::is_void(&node_name) && !utils::is_svg(&node_name) && !utils::is_mathml(&node_name) {
                self.warn(Some(p), w::element_invalid_self_closing_tag(el.name));
            }
        }

        let state = State { parent_element: Some(el.name), ..*st };
        if textarea_moved {
            // the dynamic children become a `value` attribute
            self.path.push(p);
            for a in &el.attributes {
                self.visit(P::Attr(a), &state)?;
            }
            self.visit(P::TextareaValue(n), &state)?;
            let prev = self.emptied_fragment.replace(el.fragment);
            self.visit(P::Fragment(el.fragment), &state)?;
            self.emptied_fragment = prev;
            self.path.pop();
            Ok(())
        } else {
            self.next(p, &state)
        }
    }

    fn svelte_element(&mut self, p: P<'s>, n: NodeId, st: &State<'s>) -> Res {
        let el = self.element(n).unwrap();
        self.validate_element(n, st)?;
        super::a11y::check_element(self, n)?;
        self.save_path(n);
        self.elements.push(n);

        if let Some(tag) = &el.tag {
            let meta = self.new_meta();
            self.visit_child(p, nodes::template_expr(tag), &State { expression: Some(meta), ..*st })?;
        }
        for a in &el.attributes {
            self.visit_child(p, P::Attr(a), st)?;
        }
        self.visit_child(p, P::Fragment(el.fragment), &State { parent_element: None, ..*st })
    }

    /// `validate_element` (shared/element.js)
    fn validate_element(&mut self, n: NodeId, st: &State<'s>) -> Res {
        const EVENT_MODIFIERS: &[&str] =
            &["preventDefault", "stopPropagation", "stopImmediatePropagation", "capture", "once", "passive", "nonpassive", "self", "trusted"];
        let el = self.element(n).unwrap();
        let mut has_animate_directive = false;
        let mut in_transition: Option<&'s Attr<'s>> = None;
        let mut out_transition: Option<&'s Attr<'s>> = None;

        for a in &el.attributes {
            let ap = P::Attr(a);
            match a {
                Attr::Attribute { name, value, .. } => {
                    let is_expression = is_expression_value(value);
                    if self.runes {
                        self.validate_attribute(a, n)?;
                        if is_expression {
                            if let Some(expr) = value_expression(value) {
                                self.disallow_unparenthesized_sequences(expr)?;
                            }
                        }
                    }
                    if illegal_attribute_character(name) {
                        return Err(e::attribute_invalid_name(self.loc(ap), name));
                    }
                    if name.starts_with("on") && name.len() > 2 {
                        if !is_expression {
                            return Err(e::attribute_invalid_event_handler(self.loc(ap)));
                        }
                        self.check_global_event_reference(a, st);
                    }
                    if *name == "slot" {
                        self.validate_slot_attribute(a, false, st)?;
                    }
                    if *name == "is" {
                        self.warn(Some(ap), w::attribute_avoid_is());
                    }
                    let correct = match *name {
                        "className" => Some("class"),
                        "htmlFor" => Some("for"),
                        _ => None,
                    };
                    if let Some(correct) = correct {
                        self.warn(Some(ap), w::attribute_invalid_property_name(name, correct));
                    }
                    self.validate_attribute_name(a);
                }
                Attr::Directive { kind: "AnimateDirective", .. } => {
                    let parent = self.path.len().checked_sub(2).map(|i| self.path[i]);
                    match parent.and_then(|q| q.node()).map(|q| &self.ast.nodes[q]) {
                        Some(Node::EachBlock { key, body, .. }) => {
                            if key.is_none() {
                                return Err(e::animation_missing_key(self.loc(ap)));
                            }
                            let count = self.ast.fragments[*body]
                                .nodes
                                .iter()
                                .filter(|&&c| match &self.ast.nodes[c] {
                                    Node::Comment { .. } | Node::ConstTag { .. } | Node::DeclarationTag { .. } => false,
                                    Node::Text { data, .. } => !utils::js_trim(data).is_empty(),
                                    _ => true,
                                })
                                .count();
                            if count > 1 {
                                return Err(e::animation_invalid_placement(self.loc(ap)));
                            }
                        }
                        _ => return Err(e::animation_invalid_placement(self.loc(ap))),
                    }
                    if has_animate_directive {
                        return Err(e::animation_duplicate(self.loc(ap)));
                    }
                    has_animate_directive = true;
                }
                Attr::Directive { kind: "TransitionDirective", intro_outro: Some((intro, outro)), .. } => {
                    let existing = if *intro && in_transition.is_some() {
                        in_transition
                    } else if *outro && out_transition.is_some() {
                        out_transition
                    } else {
                        None
                    };
                    if let Some(Attr::Directive { intro_outro: Some((ei, eo)), .. }) = existing {
                        let a_ = if *ei { if *eo { "transition" } else { "in" } } else { "out" };
                        let b_ = if *intro { if *outro { "transition" } else { "in" } } else { "out" };
                        if a_ == b_ {
                            return Err(e::transition_duplicate(self.loc(ap), a_));
                        } else {
                            return Err(e::transition_conflict(self.loc(ap), a_, b_));
                        }
                    }
                    if *intro {
                        in_transition = Some(a);
                    }
                    if *outro {
                        out_transition = Some(a);
                    }
                }
                Attr::Directive { kind: "OnDirective", modifiers, .. } => {
                    let mut has_passive = false;
                    let mut conflicting = "";
                    for m in modifiers {
                        if !EVENT_MODIFIERS.contains(m) {
                            let list = format!(
                                "{} or {}",
                                EVENT_MODIFIERS[..EVENT_MODIFIERS.len() - 1].join(", "),
                                EVENT_MODIFIERS[EVENT_MODIFIERS.len() - 1]
                            );
                            return Err(e::event_handler_invalid_modifier(self.loc(ap), &list));
                        }
                        if *m == "passive" {
                            has_passive = true;
                        } else if *m == "nonpassive" || *m == "preventDefault" {
                            conflicting = m;
                        }
                        if has_passive && !conflicting.is_empty() {
                            return Err(e::event_handler_invalid_modifier_combination(self.loc(ap), "passive", conflicting));
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn validate_attribute_name(&mut self, a: &'s Attr<'s>) {
        let name = super::attr_name(a);
        if name.contains(':') && !name.starts_with("xmlns:") && !name.starts_with("xlink:") && !name.starts_with("xml:") {
            self.warn(Some(P::Attr(a)), w::attribute_illegal_colon());
        }
    }

    /// `validate_attribute(attribute, parent)`
    fn validate_attribute(&mut self, a: &'s Attr<'s>, parent: NodeId) -> Res {
        let Attr::Attribute { value, end, .. } = a else { return Ok(()) };
        let el = self.element(parent).unwrap();
        if let AttrValue::Sequence(chunks) = value {
            if chunks.len() == 1
                && matches!(chunks[0], Chunk::Expression { .. })
                && (matches!(el.kind, "Component" | "SvelteComponent" | "SvelteSelf")
                    || (el.kind == "RegularElement" && self.is_custom_element_node(parent)))
            {
                self.warn(Some(P::Attr(a)), w::attribute_quoted());
            }
            if chunks.len() == 1 {
                return Ok(());
            }
            let last_end = match chunks.last() {
                Some(Chunk::Text { end, .. } | Chunk::Expression { end, .. }) => *end,
                None => usize::MAX,
            };
            if last_end == *end {
                return Err(e::attribute_unquoted_sequence(self.loc(P::Attr(a))));
            }
        }
        Ok(())
    }

    pub fn is_custom_element_node(&self, n: NodeId) -> bool {
        let Some(el) = self.element(n) else { return false };
        el.kind == "RegularElement"
            && (el.name.contains('-') || el.attributes.iter().any(|a| matches!(a, Attr::Attribute { name: "is", .. })))
    }

    fn disallow_unparenthesized_sequences(&self, expr: &'s Expr<'s>) -> Res {
        let ep = nodes::template_expr(expr);
        if let P::Js(AstKind::SequenceExpression(s)) = ep {
            let bytes = self.source.as_bytes();
            let mut i = s.span.start as usize;
            while i > 1 {
                i -= 1;
                match bytes[i] {
                    b'(' => break,
                    b'{' => return Err(e::attribute_invalid_sequence_expression(self.loc(ep))),
                    _ => {}
                }
            }
        }
        Ok(())
    }

    fn check_global_event_reference(&mut self, a: &'s Attr<'s>, st: &State<'s>) {
        let Attr::Attribute { name, value, .. } = a else { return };
        let Some(expr) = value_expression(value) else { return };
        if let Some(id) = ident(nodes::template_expr(expr)) {
            if id.name == *name && self.get(st.scope, id.name).is_none() {
                self.warn(Some(P::Attr(a)), w::attribute_global_event_reference(name));
            }
        }
    }

    fn validate_slot_attribute(&mut self, a: &'s Attr<'s>, is_component: bool, st: &State<'s>) -> Res {
        let len = self.path.len();
        let parent = len.checked_sub(2).map(|i| self.path[i]);
        let Attr::Attribute { value, .. } = a else { return Ok(()) };
        if let Some(P::Node(pn)) = parent {
            if matches!(self.ast.nodes[pn], Node::SnippetBlock { .. }) {
                if text_value(value).is_none() {
                    return Err(e::slot_attribute_invalid(self.loc(P::Attr(a))));
                }
                return Ok(());
            }
        }
        let mut owner: Option<NodeId> = None;
        for i in (0..len).rev() {
            let ancestor = self.path[i];
            if owner.is_none() {
                if let Some(an) = ancestor.node() {
                    if let Some(el) = self.element(an) {
                        if matches!(el.kind, "Component" | "SvelteComponent" | "SvelteSelf" | "SvelteElement")
                            || (el.kind == "RegularElement" && self.is_custom_element_node(an))
                        {
                            owner = Some(an);
                        }
                    }
                }
            }
        }
        match owner {
            Some(o) => {
                let el = self.element(o).unwrap();
                if matches!(el.kind, "Component" | "SvelteComponent" | "SvelteSelf") {
                    if parent.and_then(|q| q.node()) != Some(o) {
                        if !is_component {
                            return Err(e::slot_attribute_invalid_placement(self.loc(P::Attr(a))));
                        }
                    } else {
                        let Some(name) = text_value(value) else {
                            return Err(e::slot_attribute_invalid(self.loc(P::Attr(a))));
                        };
                        let slots = &mut self.component_slots[st.component_slots as usize];
                        if slots.contains(name) {
                            return Err(e::slot_attribute_duplicate(self.loc(P::Attr(a)), name, el.name));
                        }
                        slots.insert(name.to_string());
                        if name == "default" {
                            for &c in &self.ast.fragments[el.fragment].nodes {
                                match &self.ast.nodes[c] {
                                    Node::Text { data, .. } if only_whitespaces(data) => continue,
                                    Node::Element(child) if matches!(child.kind, "RegularElement" | "SvelteFragment") => {
                                        if child.attributes.iter().any(|a| matches!(a, Attr::Attribute { name: "slot", .. })) {
                                            continue;
                                        }
                                    }
                                    _ => {}
                                }
                                return Err(e::slot_default_duplicate(self.node_loc(c)));
                            }
                        }
                    }
                }
            }
            None => {
                if !is_component {
                    return Err(e::slot_attribute_invalid_placement(self.loc(P::Attr(a))));
                }
            }
        }
        Ok(())
    }

    // -----------------------------------------------------------------------------------
    // components

    fn visit_component(&mut self, p: P<'s>, n: NodeId, st: &State<'s>) -> Res {
        let el = self.element(n).unwrap();
        self.save_path(n);

        // which snippets this component might render
        let mut resolved = true;
        let mut snippets: Vec<NodeId> = Vec::new();
        for a in &el.attributes {
            match a {
                Attr::Spread { .. } | Attr::Directive { kind: "BindDirective", .. } => {
                    resolved = false;
                    continue;
                }
                Attr::Attribute { value, .. } if is_expression_value(value) => {
                    let expr = nodes::template_expr(value_expression(value).unwrap());
                    if let Some(id) = ident(expr) {
                        let b = self.get(st.scope, id.name);
                        resolved = resolved && self.is_resolved_snippet(b);
                        if let Some(P::Node(s)) = b.and_then(|b| self.binding(b).initial) {
                            if matches!(self.ast.nodes[s], Node::SnippetBlock { .. }) && !snippets.contains(&s) {
                                snippets.push(s);
                            }
                        }
                    } else if !matches!(
                        expr,
                        P::Js(
                            AstKind::StringLiteral(_)
                                | AstKind::NumericLiteral(_)
                                | AstKind::BooleanLiteral(_)
                                | AstKind::NullLiteral(_)
                                | AstKind::BigIntLiteral(_)
                                | AstKind::RegExpLiteral(_)
                        ) | P::TplExpr(Expr::Literal { .. })
                    ) {
                        resolved = false;
                    }
                }
                _ => {}
            }
        }
        if resolved {
            for &c in &self.ast.fragments[el.fragment].nodes {
                if matches!(self.ast.nodes[c], Node::SnippetBlock { .. }) && !snippets.contains(&c) {
                    snippets.push(c);
                }
            }
        }
        self.renderer_snippets.insert(n, snippets);
        self.snippet_renderers.push((n, resolved));

        for a in &el.attributes {
            let ap = P::Attr(a);
            if !matches!(
                a,
                Attr::Attribute { .. }
                    | Attr::Spread { .. }
                    | Attr::Attach { .. }
                    | Attr::Directive { kind: "LetDirective" | "OnDirective" | "BindDirective", .. }
            ) {
                return Err(e::component_invalid_directive(self.loc(ap)));
            }
            if let Attr::Directive { kind: "OnDirective", modifiers, .. } = a {
                if modifiers.len() > 1 || modifiers.iter().any(|m| *m != "once") {
                    return Err(e::event_handler_invalid_component_modifier(self.loc(ap)));
                }
            }
            if let Attr::Attribute { name, value, .. } = a {
                if self.runes {
                    self.validate_attribute(a, n)?;
                    if is_expression_value(value) {
                        self.disallow_unparenthesized_sequences(value_expression(value).unwrap())?;
                    }
                }
                self.validate_attribute_name(a);
                if *name == "slot" {
                    self.validate_slot_attribute(a, true, st)?;
                }
            }
            if let Attr::Attach { expression, .. } = a {
                self.disallow_unparenthesized_sequences(expression)?;
            }
        }

        let scopes = self.sc.component_scopes.get(&n).cloned().unwrap_or_default();
        let scope_of = |name: &str| scopes.iter().find(|(k, _)| *k == name).map(|(_, s)| *s);
        let default_scope = if scope::determine_slot(self.ast, n).is_some() {
            st.scope
        } else {
            scope_of("default").unwrap_or(st.scope)
        };
        for a in &el.attributes {
            let s = if matches!(a, Attr::Directive { kind: "LetDirective", .. }) { default_scope } else { st.scope };
            self.visit_child(p, P::Attr(a), &State { scope: s, ..*st })?;
        }

        let mut comments: Vec<NodeId> = Vec::new();
        let mut slots: Vec<(&'s str, Vec<NodeId>)> = vec![("default", Vec::new())];
        for &c in &self.ast.fragments[el.fragment].nodes {
            if matches!(self.ast.nodes[c], Node::Comment { .. }) {
                comments.push(c);
                continue;
            }
            let slot_name = scope::determine_slot(self.ast, c).unwrap_or("default");
            let idx = match slots.iter().position(|(k, _)| *k == slot_name) {
                Some(i) => i,
                None => {
                    slots.push((slot_name, Vec::new()));
                    slots.len() - 1
                }
            };
            slots[idx].1.extend(comments.iter().copied());
            slots[idx].1.push(c);
            if slot_name != "default" {
                comments.clear();
            }
        }
        // `for (const slot_name in nodes)`: integer-like keys come first
        let mut order: Vec<usize> = (0..slots.len()).collect();
        order.sort_by_key(|&i| match array_index(slots[i].0) {
            Some(v) => (0, v, 0),
            None => (1, 0, i),
        });

        self.component_slots.push(Default::default());
        let component_slots = (self.component_slots.len() - 1) as u32;
        for i in order {
            let (slot_name, nodes) = (slots[i].0, std::mem::take(&mut slots[i].1));
            let scope = scope_of(slot_name).unwrap_or(st.scope);
            self.slot_fragments.push((el.fragment, nodes));
            let frag = P::SlotFragment((self.slot_fragments.len() - 1) as u32);
            self.visit_child(p, frag, &State { scope, parent_element: None, component_slots, ..*st })?;
        }
        Ok(())
    }

    // -----------------------------------------------------------------------------------
    // attributes and directives

    fn visit_attr(&mut self, p: P<'s>, a: &'s Attr<'s>, st: &State<'s>) -> Res {
        let parent = self.path.last().copied();
        let parent_ty = parent.map(|q| self.ty(q));
        match a {
            Attr::Attribute { value, .. } => {
                self.next(p, st)?;
                if !matches!(value, AttrValue::True)
                    && is_event_attribute(a)
                    && matches!(parent_ty, Some("RegularElement" | "SvelteElement"))
                {
                    self.uses_event_attributes = true;
                }
                Ok(())
            }
            Attr::Spread { .. } => {
                let meta = self.new_meta();
                self.next(p, &State { expression: Some(meta), ..*st })
            }
            Attr::Attach { .. } => {
                let meta = self.new_meta();
                self.next(p, &State { expression: Some(meta), ..*st })?;
                if self.metas[meta as usize].has_await {
                    return Err(e::illegal_await_expression(self.loc(p)));
                }
                Ok(())
            }
            Attr::StyleDirective { modifiers, value, .. } => {
                if modifiers.len() > 1 || (modifiers.len() == 1 && modifiers[0] != "important") {
                    return Err(e::style_directive_invalid_modifier(self.loc(p)));
                }
                if !matches!(value, AttrValue::True) {
                    self.next(p, st)?;
                }
                Ok(())
            }
            Attr::Directive { kind, name, modifiers, expression, start, end, .. } => match *kind {
                "AnimateDirective" | "TransitionDirective" | "UseDirective" => {
                    let meta = self.new_meta();
                    self.next(p, &State { expression: Some(meta), ..*st })?;
                    if self.metas[meta as usize].has_await {
                        return Err(e::illegal_await_expression(self.loc(p)));
                    }
                    Ok(())
                }
                "ClassDirective" => {
                    let meta = self.new_meta();
                    self.next(p, &State { expression: Some(meta), ..*st })
                }
                "OnDirective" => {
                    if self.runes && matches!(parent_ty, Some("RegularElement" | "SvelteElement")) {
                        self.warn(Some(p), w::event_directive_deprecated(name));
                    }
                    if matches!(parent_ty, Some("RegularElement" | "SvelteElement")) && self.event_directive_node.is_none() {
                        self.event_directive_node = Some((*start, *end, name));
                    }
                    let _ = modifiers;
                    let meta = self.new_meta();
                    self.next(p, &State { expression: Some(meta), ..*st })
                }
                "LetDirective" => {
                    let parent_el = parent.and_then(|q| q.node()).and_then(|q| self.element(q));
                    let valid = parent_el.is_some_and(|el| {
                        matches!(
                            el.kind,
                            "Component" | "RegularElement" | "SlotElement" | "SvelteElement" | "SvelteComponent" | "SvelteSelf" | "SvelteFragment"
                        )
                    });
                    if !valid {
                        return Err(e::let_directive_invalid_placement(self.loc(p)));
                    }
                    let el = parent_el.unwrap();
                    let pn = parent.unwrap().node().unwrap();
                    if matches!(el.kind, "Component" | "SvelteComponent" | "SvelteSelf")
                        && scope::determine_slot(self.ast, pn).is_none()
                        && self.ast.fragments[el.fragment].nodes.iter().any(|&c| {
                            matches!(&self.ast.nodes[c], Node::SnippetBlock { expression, .. }
                                if ident(nodes::template_expr(expression)).is_some_and(|i| i.name == "children"))
                        })
                    {
                        let pattern = match expression {
                            None => name.to_string(),
                            Some(e) if ident(nodes::template_expr(e)).is_some_and(|i| i.name == *name) => name.to_string(),
                            Some(e) => format!("{}: {}", name, &self.source[e.start()..e.end()]),
                        };
                        return Err(e::let_directive_snippet_conflict(self.loc(p), &pattern));
                    }
                    Ok(())
                }
                "BindDirective" => self.bind_directive(p, a, st),
                _ => self.next(p, st),
            },
        }
    }

    fn bind_directive(&mut self, p: P<'s>, a: &'s Attr<'s>, st: &State<'s>) -> Res {
        let Attr::Directive { name, expression, .. } = a else { unreachable!() };
        let name: &'s str = name;
        let parent = self.path.last().copied();
        let parent_el = parent.and_then(|q| q.node()).and_then(|q| self.element(q));
        let loc = self.loc(p);

        if let Some(pel) = parent_el.filter(|el| {
            matches!(el.kind, "RegularElement" | "SvelteElement" | "SvelteWindow" | "SvelteDocument" | "SvelteBody")
        }) {
            let pname = pel.name;
            match utils::binding_property(name) {
                Some(property) => {
                    if let Some(valid) = property.valid_elements {
                        if !valid.contains(&pname) {
                            let list: Vec<String> = valid.iter().map(|v| format!("`<{v}>`")).collect();
                            return Err(e::bind_invalid_target(loc, name, &list.join(", ")));
                        }
                    }
                    if let Some(invalid) = property.invalid_elements {
                        if invalid.contains(&pname) {
                            let mut valid_bindings: Vec<&str> = utils::BINDING_PROPERTIES
                                .iter()
                                .filter(|b| {
                                    b.valid_elements.is_some_and(|v| v.contains(&pname))
                                        || (b.valid_elements.is_none() && !b.invalid_elements.is_some_and(|i| i.contains(&pname)))
                                })
                                .map(|b| b.name)
                                .collect();
                            valid_bindings.sort();
                            let explanation = format!("Possible bindings for <{}> are {}", pname, valid_bindings.join(", "));
                            return Err(e::bind_invalid_name(loc, name, Some(&explanation)));
                        }
                    }
                    if pname == "input" && name != "this" {
                        let ty = pel.attributes.iter().find(|a| matches!(a, Attr::Attribute { name: "type", .. }));
                        let type_text = match ty {
                            Some(Attr::Attribute { value, .. }) => text_value(value),
                            _ => None,
                        };
                        if let (Some(t @ Attr::Attribute { value, .. }), None) = (ty, type_text) {
                            if name != "value" || matches!(value, AttrValue::True) {
                                return Err(e::attribute_invalid_type(self.loc(P::Attr(t))));
                            }
                        } else {
                            if name == "checked" && type_text != Some("checkbox") {
                                let extra = if type_text == Some("radio") {
                                    " — for `<input type=\"radio\">`, use `bind:group`"
                                } else {
                                    ""
                                };
                                return Err(e::bind_invalid_target(loc, name, &format!("`<input type=\"checkbox\">`{extra}")));
                            }
                            if name == "files" && type_text != Some("file") {
                                return Err(e::bind_invalid_target(loc, name, "`<input type=\"file\">`"));
                            }
                        }
                    }
                    if pname == "select" && name != "this" {
                        let multiple = pel.attributes.iter().find(|a| match a {
                            Attr::Attribute { name: "multiple", value, .. } => {
                                text_value(value).is_none() && !matches!(value, AttrValue::True)
                            }
                            _ => false,
                        });
                        if let Some(m) = multiple {
                            return Err(e::attribute_invalid_multiple(self.loc(P::Attr(m))));
                        }
                    }
                    if name == "offsetWidth" && utils::is_svg(pname) {
                        return Err(e::bind_invalid_target(
                            loc,
                            name,
                            "non-`<svg>` elements. Use `bind:clientWidth` for `<svg>` instead",
                        ));
                    }
                    if utils::is_content_editable_binding(name) {
                        let ce = pel.attributes.iter().find(|a| matches!(a, Attr::Attribute { name: "contenteditable", .. }));
                        match ce {
                            None => return Err(e::attribute_contenteditable_missing(loc)),
                            Some(c @ Attr::Attribute { value, .. }) => {
                                if text_value(value).is_none() && !matches!(value, AttrValue::True) {
                                    return Err(e::attribute_contenteditable_dynamic(self.loc(P::Attr(c))));
                                }
                            }
                            _ => {}
                        }
                    }
                }
                None => {
                    static NAMES: std::sync::OnceLock<Vec<&'static str>> = std::sync::OnceLock::new();
                    let names = NAMES.get_or_init(|| utils::BINDING_PROPERTIES.iter().map(|b| b.name).collect());
                    if let Some(m) = utils::fuzzymatch(name, names) {
                        let property = utils::binding_property(&m).unwrap();
                        if property.valid_elements.is_none_or(|v| v.contains(&pname)) {
                            return Err(e::bind_invalid_name(loc, name, Some(&format!("Did you mean '{m}'?"))));
                        }
                    }
                    return Err(e::bind_invalid_name(loc, name, None));
                }
            }
        }

        let Some(expression) = expression else { return Ok(()) };
        let ep = nodes::template_expr(expression);

        if let P::Js(AstKind::SequenceExpression(seq)) = ep {
            if name == "group" {
                return Err(e::bind_group_invalid_expression(loc));
            }
            let bytes = self.source.as_bytes();
            let comments = self.leading_comment_range(expression, seq.span.start as usize);
            let mut i = seq.span.start as usize;
            loop {
                if i == 0 {
                    break;
                }
                i -= 1;
                if bytes[i] == b'{' {
                    break;
                }
                if bytes[i] == b'(' {
                    let in_comment = comments.is_some_and(|(s, e)| i <= e && i >= s);
                    if !in_comment {
                        return Err(e::bind_invalid_parens(loc, name));
                    }
                }
            }
            if seq.expressions.len() != 2 {
                return Err(e::bind_invalid_expression(loc));
            }
            let meta = self.new_meta();
            for x in &seq.expressions {
                let target = match nodes::strip(x) {
                    Expression::ArrowFunctionExpression(arrow) => match &arrow.body {
                        ArrowFunctionBody::FunctionBody(b) => P::Js(AstKind::FunctionBody(b)),
                        body => nodes::expr(body.as_expression().unwrap()),
                    },
                    _ => nodes::expr(x),
                };
                self.visit_child(p, target, &State { expression: Some(meta), ..*st })?;
            }
            if self.metas[meta as usize].has_await {
                return Err(e::illegal_await_expression(loc));
            }
            return Ok(());
        }

        self.validate_assignment(p, ep, st)?;

        let Some(left) = object(ep) else {
            return Err(e::bind_invalid_expression(loc));
        };
        let binding = self.get(st.scope, left.name);

        if ident(ep).is_some() && name != "this" {
            let ok = binding.is_some_and(|b| {
                let b = self.binding(b);
                matches!(
                    b.kind,
                    Kind::State | Kind::RawState | Kind::Prop | Kind::BindableProp | Kind::Each | Kind::StoreSub
                ) || b.updated()
            });
            if !ok {
                return Err(e::bind_invalid_value(self.loc(ep)));
            }
        }

        if name == "group" {
            if let Some(b) = binding {
                if self.binding(b).kind == Kind::Snippet {
                    return Err(e::bind_group_invalid_snippet_parameter(loc));
                }
            }
        }

        if let Some(b) = binding {
            let b = self.binding(b);
            if b.kind == Kind::Each && b.inside_rest {
                let node = b.node;
                self.warn_id(node, w::bind_invalid_each_rest(node.name));
            }
        }

        let meta = self.new_meta();
        self.next(p, &State { expression: Some(meta), ..*st })?;
        if self.metas[meta as usize].has_await {
            return Err(e::illegal_await_expression(loc));
        }
        Ok(())
    }
}

/// `node.name.replace(/[a-zA-Z-]*:/g, '')`
fn strip_namespaces(name: &str) -> std::borrow::Cow<'_, str> {
    if !name.contains(':') {
        return name.into();
    }
    let mut out = String::new();
    let mut run = String::new();
    for c in name.chars() {
        if c.is_ascii_alphabetic() || c == '-' {
            run.push(c);
        } else if c == ':' {
            run.clear();
        } else {
            out.push_str(&run);
            run.clear();
            out.push(c);
        }
    }
    out.push_str(&run);
    out.into()
}

/// `regex_illegal_attribute_character`: `/(^[0-9-.])|[\^$@%&#?!|()[\]{}^*+~;]/`
fn illegal_attribute_character(name: &str) -> bool {
    if name.as_bytes().first().is_some_and(|b| b.is_ascii_digit() || *b == b'-' || *b == b'.') {
        return true;
    }
    name.bytes().any(|b| b"^$@%&#?!|()[]{}*+~;".contains(&b))
}

/// `/^[ \t\n\r\f]+$/`
fn only_whitespaces(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0c))
}

/// JS array index keys (`for in` lists them first)
fn array_index(s: &str) -> Option<u64> {
    if s.is_empty() || (s.len() > 1 && s.starts_with('0')) || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse::<u64>().ok().filter(|v| *v < u32::MAX as u64)
}

/// The byte offset `units` UTF-16 units before `end` (clamped at 0)
pub fn utf16_back(source: &str, end: usize, units: usize) -> usize {
    let mut i = end;
    let mut n = 0;
    while n < units && i > 0 {
        let c = source[..i].chars().next_back().unwrap();
        n += c.len_utf16();
        if n > units {
            // `substring` splits a surrogate pair; keep the whole character out
            break;
        }
        i -= c.len_utf8();
    }
    i
}

/// The byte offset `units` UTF-16 units after `start`
pub fn utf16_forward(source: &str, start: usize, units: usize) -> usize {
    let mut i = start;
    let mut n = 0;
    while n < units {
        match source[i..].chars().next() {
            Some(c) => {
                n += c.len_utf16();
                i += c.len_utf8();
            }
            None => return i + (units - n),
        }
    }
    i
}

/// `/{(\s*):then\s+$/` (or `:catch`): `Some(true)` if it matches with non-empty whitespace
fn match_then_catch(s: &str, keyword: &str) -> Option<bool> {
    let trimmed = s.trim_end_matches(utils::is_js_whitespace);
    if trimmed.len() == s.len() {
        return None;
    }
    let before = trimmed.strip_suffix(keyword)?.strip_suffix(':')?;
    let ws_start = before.trim_end_matches(utils::is_js_whitespace);
    let ws = &before[ws_start.len()..];
    // `{` must come right before the whitespace; take the last `{`
    if !ws_start.ends_with('{') {
        return None;
    }
    Some(!ws.is_empty())
}

// ---------------------------------------------------------------------------------------
// before and after the walks

/// legacy mode: exported `let`/`var` become props
pub fn legacy_exports(an: &mut Analyzer) {
    let Some(program) = an.instance_program else { return };
    for s in nodes::children(program, an.ast) {
        match s {
            P::Js(AstKind::ExportDeclaration(d)) => {
                if let Declaration::VariableDeclaration(v) = &d.declaration {
                    if v.kind != VariableDeclarationKind::Const {
                        for declarator in &v.declarations {
                            for id in scope::extract_identifiers(nodes::binding(&declarator.id)) {
                                if let Some(b) = an.get(an.instance_scope, id.name) {
                                    an.sc.binding_mut(b).kind = Kind::BindableProp;
                                }
                            }
                        }
                    }
                }
            }
            P::Js(AstKind::ExportNamedDeclaration(d)) => {
                for s in &d.specifiers {
                    if s.export_kind.is_type() {
                        continue;
                    }
                    let (ModuleExportName::IdentifierReference(local), ModuleExportName::IdentifierName(exported)) =
                        (&s.local, &s.exported)
                    else {
                        continue;
                    };
                    if let Some(b) = an.get(an.instance_scope, local.name.as_str()) {
                        let binding = an.sc.binding_mut(b);
                        if matches!(binding.declaration_kind, DeclKind::Var | DeclKind::Let) {
                            binding.kind = Kind::BindableProp;
                            if exported.name != local.name {
                                binding.prop_alias = Some(exported.name.as_str());
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

/// legacy mode: reassigned/mutated bindings referenced in `$:` or the template become state,
/// and so do the dependencies of each blocks whose items are reassigned
pub fn legacy_state<'s>(an: &mut Analyzer<'s>) {
    let decls: Vec<BindingId> = an.sc.scope(an.instance_scope).declarations.values().copied().collect();
    for b in decls {
        if an.binding(b).kind != Kind::Normal {
            continue;
        }
        let refs = an.binding(b).references.clone();
        for r in refs {
            let node = an.sc.refs[r as usize].node;
            if node.key == an.binding(b).node.key {
                continue;
            }
            if an.binding(b).updated() {
                let path = an.sc.ref_path(r);
                let is_state = matches!(path.last(), Some(P::Attr(Attr::StyleDirective { .. })))
                    || path.iter().any(|q| matches!(q, P::Fragment(_) | P::SlotFragment(_)))
                    || matches!(path.get(1), Some(P::Js(AstKind::LabeledStatement(l))) if l.label.name == "$");
                if is_state {
                    an.sc.binding_mut(b).kind = Kind::State;
                }
            }
        }
    }

    // more legacy nonsense: if an `each` binding is reassigned/mutated, treat the
    // expression as being mutated as well
    let mut blocks = Vec::new();
    find_each_blocks(an, P::Fragment(an.root.fragment), &mut blocks);
    for n in blocks {
        let Some(&scope) = an.sc.map.get(&P::Node(n).key()) else { continue };
        let updated = an.sc.scope(scope).declarations.values().any(|&b| an.binding(b).updated());
        if !updated {
            continue;
        }
        let Node::EachBlock { expression, .. } = &an.ast.nodes[n] else { continue };
        let parent = an.sc.scope(scope).parent.unwrap();
        let mut path = vec![P::Node(n)];
        mark_each_expression(an, nodes::template_expr(expression), parent, &mut path);
    }
}

fn find_each_blocks(an: &Analyzer, p: P, out: &mut Vec<NodeId>) {
    if let P::Node(n) = p {
        if matches!(an.ast.nodes[n], Node::EachBlock { .. }) {
            out.push(n);
            return;
        }
    }
    let _ = nodes::each_child(p, an.ast, out, &mut |out: &mut Vec<NodeId>, c| {
        find_each_blocks(an, c, out);
        Ok(())
    });
}

fn mark_each_expression<'s>(an: &mut Analyzer<'s>, p: P<'s>, scope: super::ScopeId, path: &mut Vec<P<'s>>) {
    let scope = an.sc.map.get(&p.key()).copied().unwrap_or(scope);
    if let Some(id) = ident(p) {
        if let Some(&parent) = path.last() {
            if is_reference(p, parent) {
                if let Some(b) = an.get(scope, id.name) {
                    let b = an.sc.binding_mut(b);
                    if b.kind == Kind::Normal && b.declaration_kind != DeclKind::Import && b.declaration_kind != DeclKind::Function {
                        b.kind = Kind::State;
                        b.mutated = true;
                    }
                }
            }
        }
        return;
    }
    let children = nodes::children(p, an.ast);
    path.push(p);
    for c in children {
        mark_each_expression(an, c, scope, path);
    }
    path.pop();
}

/// runes mode: warn on non-state declarations that are reassigned and referenced in the template
pub fn non_reactive_updates(an: &mut Analyzer) {
    for scope in [an.module_scope, an.instance_scope] {
        let decls: Vec<(&str, BindingId)> = an.sc.scope(scope).declarations.iter().map(|(k, v)| (*k, *v)).collect();
        'outer: for (name, b) in decls {
            let binding = an.binding(b);
            if binding.kind != Kind::Normal || !binding.reassigned {
                continue;
            }
            'inner: for &r in &binding.references {
                let path = an.sc.ref_path(r);
                if !matches!(path.first(), Some(P::Fragment(_) | P::SlotFragment(_))) {
                    continue;
                }
                for i in 1..path.len() {
                    match path[i] {
                        P::Js(AstKind::Function(_) | AstKind::ArrowFunctionExpression(_)) => continue 'inner,
                        P::Attr(Attr::Directive { kind: "BindDirective", name: "this", .. }) => {
                            for j in (0..i).rev() {
                                if matches!(an.ty(path[j]), "IfBlock" | "EachBlock" | "AwaitBlock" | "KeyBlock") {
                                    let node = binding.node;
                                    an.warn_id(node, w::non_reactive_update(name));
                                    continue 'outer;
                                }
                            }
                            continue 'inner;
                        }
                        _ => {}
                    }
                }
                let node = binding.node;
                an.warn_id(node, w::non_reactive_update(name));
                continue 'outer;
            }
        }
    }
}

pub fn export_let_unused(an: &mut Analyzer) {
    let decls: Vec<(&str, BindingId)> = an.sc.scope(an.instance_scope).declarations.iter().map(|(k, v)| (*k, *v)).collect();
    for (name, b) in decls {
        let binding = an.binding(b);
        if !matches!(binding.kind, Kind::Prop | Kind::BindableProp) || binding.node.name == "$$props" {
            continue;
        }
        let used = binding.references.iter().any(|&r| {
            an.sc.refs[r as usize].node.key != binding.node.key
                && !matches!(an.sc.ref_path(r).last(), Some(P::Js(AstKind::ExportSpecifier(_))))
        });
        if !used && !an.sc.scope(an.instance_scope).declarations.contains_key(format!("${name}").as_str()) {
            let node = binding.node;
            an.warn_id(node, w::export_let_unused(name));
        }
    }
}

/// `order_reactive_statements`: only the cycle check matters for diagnostics
pub fn order_reactive_statements(an: &Analyzer) -> Res {
    let mut lookup: Vec<(&str, Vec<usize>)> = Vec::new();
    for (i, rs) in an.reactive_statements.iter().enumerate() {
        for &b in &rs.assignments {
            let name = an.binding(b).node.name;
            match lookup.iter_mut().find(|(k, _)| *k == name) {
                Some(entry) => entry.1.push(i),
                None => lookup.push((name, vec![i])),
            }
        }
    }
    let mut edges: Vec<(&str, &str)> = Vec::new();
    for rs in &an.reactive_statements {
        for &a in &rs.assignments {
            for &d in &rs.dependencies {
                if !rs.assignments.contains(&d) {
                    edges.push((an.binding(a).node.name, an.binding(d).node.name));
                }
            }
        }
    }
    if let Some(cycle) = check_graph_for_cycles(&edges) {
        if !cycle.is_empty() {
            let i = lookup.iter().find(|(k, _)| *k == cycle[0]).map(|(_, v)| v[0]).unwrap_or(0);
            let rs = &an.reactive_statements[i];
            return Err(e::reactive_declaration_cycle((rs.node_start, rs.node_end), &cycle.join(" → ")));
        }
    }
    Ok(())
}

fn check_graph_for_cycles<'a>(edges: &[(&'a str, &'a str)]) -> Option<Vec<&'a str>> {
    let mut graph: indexmap::IndexMap<&str, Vec<&str>> = indexmap::IndexMap::new();
    for &(u, v) in edges {
        graph.entry(u).or_default();
        graph.entry(v).or_default();
        graph.get_mut(u).unwrap().push(v);
    }
    let mut visited: Vec<&str> = Vec::new();
    let mut on_stack: indexmap::IndexSet<&str> = indexmap::IndexSet::new();
    let mut cycles: Vec<Vec<&str>> = Vec::new();
    fn visit<'a>(
        v: &'a str,
        graph: &indexmap::IndexMap<&'a str, Vec<&'a str>>,
        visited: &mut Vec<&'a str>,
        on_stack: &mut indexmap::IndexSet<&'a str>,
        cycles: &mut Vec<Vec<&'a str>>,
    ) {
        visited.push(v);
        on_stack.insert(v);
        if let Some(ws) = graph.get(v) {
            for &w in ws {
                if !visited.contains(&w) {
                    visit(w, graph, visited, on_stack, cycles);
                } else if on_stack.contains(w) {
                    let mut c: Vec<&str> = on_stack.iter().copied().collect();
                    c.push(w);
                    cycles.push(c);
                }
            }
        }
        on_stack.shift_remove(v);
    }
    let keys: Vec<&str> = graph.keys().copied().collect();
    for v in keys {
        if !visited.contains(&v) {
            visit(v, &graph, &mut visited, &mut on_stack, &mut cycles);
        }
    }
    cycles.into_iter().next()
}

/// `export { x }` in the module script must refer to something
pub fn module_exports(an: &Analyzer) -> Res {
    let Some(program) = an.module_program else { return Ok(()) };
    for s in nodes::children(program, an.ast) {
        let P::Js(AstKind::ExportNamedDeclaration(d)) = s else { continue };
        for spec in &d.specifiers {
            if spec.export_kind.is_type() {
                continue;
            }
            let name = match &spec.local {
                ModuleExportName::IdentifierReference(r) => r.name.as_str(),
                ModuleExportName::IdentifierName(r) => r.name.as_str(),
                ModuleExportName::StringLiteral(_) => continue,
            };
            let sp = P::Js(AstKind::ExportSpecifier(spec));
            if an.get(an.module_scope, name).is_none() {
                let is_snippet = an.snippets.iter().any(|&sn| match &an.ast.nodes[sn] {
                    Node::SnippetBlock { expression, .. } => ident(nodes::template_expr(expression)).is_some_and(|i| i.name == name),
                    _ => false,
                });
                if is_snippet {
                    return Err(e::snippet_invalid_export(an.loc(sp)));
                }
                return Err(e::export_undefined(an.loc(sp), name));
            }
        }
    }
    Ok(())
}
