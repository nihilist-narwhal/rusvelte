//! Ports of `svelte2tsx/nodes/slot.ts` (`SlotHandler`), `TemplateScope.ts` and
//! `handleScopeAndResolveForSlot.ts`: the types of the props `<slot>`s pass.

use std::collections::HashMap;

use indexmap::IndexMap;
use oxc_ast::ast::*;

use super::eswalk::{Ctx, EsHandler, EsWalker};
use crate::ast::{Expr, Pattern};
use crate::magic_string::MagicString;
use oxc_ast_visit::Visit;

/// A node's identity: its `(start, end)`
pub type NodeKey = (usize, usize);

struct TScope {
    parent: Option<usize>,
    /// name → owner type
    owners: HashMap<String, &'static str>,
    /// name → the node that declares it
    inits: HashMap<String, NodeKey>,
}

pub struct SlotHandler<'s> {
    htmlx: &'s str,
    pub slots: IndexMap<String, IndexMap<String, String>>,
    resolved: HashMap<NodeKey, String>,
    resolved_expression: HashMap<NodeKey, String>,
    scopes: Vec<TScope>,
    pub current: usize,
}

/// An identifier a pattern declares: name and identity
pub type PatternId = (String, NodeKey);

impl<'s> SlotHandler<'s> {
    pub fn new(htmlx: &'s str) -> Self {
        SlotHandler {
            htmlx,
            slots: IndexMap::new(),
            resolved: HashMap::new(),
            resolved_expression: HashMap::new(),
            scopes: vec![TScope { parent: None, owners: HashMap::new(), inits: HashMap::new() }],
            current: 0,
        }
    }

    // --- TemplateScope -------------------------------------------------------------------

    pub fn scope_child(&mut self) {
        self.scopes.push(TScope { parent: Some(self.current), owners: HashMap::new(), inits: HashMap::new() });
        self.current = self.scopes.len() - 1;
    }

    pub fn scope_parent(&mut self) {
        self.current = self.scopes[self.current].parent.unwrap_or(0);
    }

    fn add(&mut self, name: &str, init: NodeKey, owner: &'static str) {
        let s = &mut self.scopes[self.current];
        s.inits.insert(name.to_string(), init);
        s.owners.insert(name.to_string(), owner);
    }

    fn get_owner(&self, mut scope: Option<usize>, name: &str) -> Option<&'static str> {
        while let Some(s) = scope {
            if let Some(o) = self.scopes[s].owners.get(name) {
                return Some(o);
            }
            scope = self.scopes[s].parent;
        }
        None
    }

    fn get_init(&self, mut scope: Option<usize>, name: &str) -> Option<NodeKey> {
        while let Some(s) = scope {
            if let Some(i) = self.scopes[s].inits.get(name) {
                return Some(*i);
            }
            scope = self.scopes[s].parent;
        }
        None
    }

    // --- resolving -----------------------------------------------------------------------

    /// `handleScopeAndResolveForSlot`: an `{#each}` context or `{:then}`/`{:catch}` value
    pub fn handle_scope_and_resolve(&mut self, def: &Pattern, def_range: (usize, usize), init: &Expr, owner: &'static str) {
        match def {
            Pattern::Ident { name, start, end, .. } => {
                let key = (*start, *end);
                self.add(name, key, owner);
                if !self.resolved.contains_key(&key) {
                    if let Some(r) = self.resolve_expression_str(name, self.current, init) {
                        self.resolved.insert(key, r);
                    }
                }
            }
            Pattern::Destructure { assign, .. } => {
                let mut ids = Vec::new();
                if let Expression::AssignmentExpression(a) = assign.inner() {
                    target_identifiers(&a.left, &mut ids);
                }
                for (name, key) in &ids {
                    self.add(name, *key, owner);
                }
                let destructuring = &self.htmlx[def_range.0..def_range.1];
                for (name, key) in &ids {
                    if let Some(r) = self.resolve_expression_str(name, self.current, init) {
                        self.resolved.insert(*key, format!("(({destructuring}) => {name})({r})"));
                    }
                }
            }
        }
    }

    /// `getResolveExpressionStr`
    fn resolve_expression_str(&mut self, name: &str, scope: usize, init: &Expr) -> Option<String> {
        match self.get_owner(Some(scope), name) {
            Some("CatchBlock") => Some("__sveltets_2_any({})".into()),
            Some("ThenBlock") => {
                let parent = self.scopes[scope].parent;
                Some(format!("__sveltets_2_unwrapPromiseLike({})", self.resolve_expression(init, parent)))
            }
            Some("EachBlock") => {
                let parent = self.scopes[scope].parent;
                Some(format!("__sveltets_2_unwrapArr({})", self.resolve_expression(init, parent)))
            }
            _ => None,
        }
    }

    /// `handleScopeAndResolveLetVarForSlot` for each `let:` of a component (`handleComponentLet`)
    pub fn handle_let(&mut self, let_start: usize, let_end: usize, let_name: &str, expression: Option<&Expr>, component_type: &str, slot_name: &str) {
        let resolved_for_let = format!("__sveltets_2_instanceOf({component_type}).$$slot_def['{slot_name}'].{let_name}");
        match expression {
            None => {
                let key = (let_start, let_end);
                self.add(let_name, key, "InlineComponent");
                self.resolved.entry(key).or_insert(resolved_for_let);
            }
            Some(e) => {
                if let Expr::Ident { name, start, end, .. } = e {
                    let key = (*start, *end);
                    self.add(name, key, "InlineComponent");
                    self.resolved.entry(key).or_insert(resolved_for_let.clone());
                    return;
                }
                let Expr::Js(js) = e else { return };
                let inner = js.inner();
                if let Expression::Identifier(id) = inner {
                    let key = (id.span.start as usize, id.span.end as usize);
                    self.add(&id.name, key, "InlineComponent");
                    self.resolved.entry(key).or_insert(resolved_for_let.clone());
                    return;
                }
                let mut ids = Vec::new();
                match inner {
                    Expression::ObjectExpression(o) => {
                        for p in &o.properties {
                            if let ObjectPropertyKind::ObjectProperty(p) = p {
                                expression_identifiers(&p.value, &mut ids);
                            }
                        }
                    }
                    Expression::ArrayExpression(a) => {
                        for el in &a.elements {
                            if let Some(e) = el.as_expression() {
                                expression_identifiers(e, &mut ids);
                            }
                        }
                    }
                    _ => return,
                }
                for (name, key) in &ids {
                    self.add(name, *key, "InlineComponent");
                }
                let destructuring = &self.htmlx[e.start()..e.end()];
                for (name, key) in &ids {
                    self.resolved.insert(*key, format!("(({destructuring}) => {name})({resolved_for_let})"));
                }
            }
        }
    }

    /// `resolveExpression`: the expression with the template scope's names replaced by
    /// what they resolve to
    fn resolve_expression(&mut self, expression: &Expr, scope: Option<usize>) -> String {
        let key = (expression.start(), expression.end());
        if let Some(r) = self.resolved_expression.get(&key) {
            return r.clone();
        }
        let mut collector = IdCollector::default();
        match expression {
            Expr::Js(js) => {
                let mut w = EsWalker::new(&mut collector);
                w.visit_expression(js.inner());
            }
            Expr::Ident { name, start, end, .. } => collector.identifier(name, *start, *end, &Ctx::Other),
            Expr::Literal { .. } => {}
        }
        let overwrite = |this: &Self, name: &str| -> String {
            match this.get_init(scope, name) {
                Some(init) => this.resolved.get(&init).cloned().unwrap_or_else(|| "undefined".into()),
                None => name.to_string(),
            }
        };
        let mut ms = MagicString::new(self.htmlx);
        for (name, _, end) in &collector.shorthands {
            let _ = ms.append_left(*end, &format!(":{}", overwrite(self, name)));
        }
        for (name, start, end) in &collector.identifiers {
            let _ = ms.overwrite(*start, *end, &overwrite(self, name), false);
        }
        let resolved = ms.slice(key.0, key.1).unwrap_or_default();
        self.resolved_expression.insert(key, resolved.clone());
        resolved
    }

    /// `handleSlot`
    pub fn handle_slot(&mut self, name: String, attrs: Vec<SlotAttr>) {
        let mut attributes = IndexMap::new();
        for a in attrs {
            match a {
                SlotAttr::Spread(raw_name) => {
                    let name = match self.get_init(Some(self.current), &raw_name) {
                        Some(init) => self.resolved.get(&init).cloned().unwrap_or_else(|| "undefined".into()),
                        None => raw_name,
                    };
                    attributes.insert(format!("__spread__{name}"), name);
                }
                SlotAttr::String(name, value) => {
                    attributes.insert(name, value);
                }
                SlotAttr::Shorthand(name, ident) => {
                    let value = self.get_init(Some(self.current), &ident).and_then(|init| self.resolved.get(&init).cloned()).unwrap_or(ident);
                    attributes.insert(name, value);
                }
                SlotAttr::Expression(name, e) => {
                    let value = self.resolve_expression(e, Some(self.current));
                    attributes.insert(name, value);
                }
            }
        }
        self.slots.insert(name, attributes);
    }
}

/// A `<slot>` attribute, as `handleSlot` sees it
pub enum SlotAttr<'e, 'a> {
    /// `{...name}`
    Spread(String),
    /// a static value, as a JS expression string
    String(String, String),
    /// `{name}`
    Shorthand(String, String),
    Expression(String, &'e Expr<'a>),
}

/// The identifiers `resolveExpression` replaces
#[derive(Default)]
struct IdCollector {
    identifiers: Vec<(String, usize, usize)>,
    shorthands: Vec<(String, usize, usize)>,
    last_key: Option<(usize, usize)>,
}

impl EsHandler for IdCollector {
    fn identifier(&mut self, name: &str, start: usize, end: usize, ctx: &Ctx) {
        match ctx {
            Ctx::MemberProperty { .. } => {}
            Ctx::PropertyKey => self.last_key = Some((start, end)),
            Ctx::PropertyValue if self.last_key == Some((start, end)) => self.shorthands.push((name.to_string(), start, end)),
            _ => self.identifiers.push((name.to_string(), start, end)),
        }
    }
    fn scope_push(&mut self) {}
    fn scope_pop(&mut self) {}
    fn set_declaration(&mut self, _value: bool) {}
    fn await_expression(&mut self, _in_function: bool) {}
}

fn key(span: oxc_span::Span) -> NodeKey {
    (span.start as usize, span.end as usize)
}

/// periscopic's `extract_identifiers` on a destructuring (an assignment target here)
pub fn target_identifiers(t: &AssignmentTarget, out: &mut Vec<PatternId>) {
    match t {
        AssignmentTarget::AssignmentTargetIdentifier(id) => out.push((id.name.to_string(), key(id.span))),
        AssignmentTarget::ObjectAssignmentTarget(o) => {
            for p in &o.properties {
                match p {
                    AssignmentTargetProperty::AssignmentTargetPropertyIdentifier(p) => out.push((p.binding.name.to_string(), key(p.binding.span))),
                    AssignmentTargetProperty::AssignmentTargetPropertyProperty(p) => maybe_default_identifiers(&p.binding, out),
                }
            }
            if let Some(rest) = &o.rest {
                target_identifiers(&rest.target, out);
            }
        }
        AssignmentTarget::ArrayAssignmentTarget(a) => {
            for el in a.elements.iter().flatten() {
                maybe_default_identifiers(el, out);
            }
            if let Some(rest) = &a.rest {
                target_identifiers(&rest.target, out);
            }
        }
        _ => {
            if let Some(m) = t.as_member_expression() {
                let mut object = m.object();
                while let Some(m) = object.as_member_expression() {
                    object = m.object();
                }
                if let Expression::Identifier(id) = object {
                    out.push((id.name.to_string(), key(id.span)));
                }
            }
        }
    }
}

fn maybe_default_identifiers(t: &AssignmentTargetMaybeDefault, out: &mut Vec<PatternId>) {
    match t {
        AssignmentTargetMaybeDefault::AssignmentTargetWithDefault(d) => target_identifiers(&d.binding, out),
        other => {
            if let Some(t) = other.as_assignment_target() {
                target_identifiers(t, out);
            }
        }
    }
}

/// periscopic's `extract_identifiers` on an expression in a pattern's place (`let:x={{a, b}}`):
/// only identifiers and member expressions count
fn expression_identifiers(e: &Expression, out: &mut Vec<PatternId>) {
    match e {
        Expression::Identifier(id) => out.push((id.name.to_string(), key(id.span))),
        _ => {
            if let Some(m) = e.as_member_expression() {
                let mut object = m.object();
                while let Some(m) = object.as_member_expression() {
                    object = m.object();
                }
                if let Expression::Identifier(id) = object {
                    out.push((id.name.to_string(), key(id.span)));
                }
            }
        }
    }
}
