//! `calculate_blockers` (`phases/2-analyze/index.js`): splits the instance script into
//! `instance_body` (hoisted imports, synchronous statements, async groups) and gives each
//! binding that depends on top-level `await` a blocker, `$$promises[i]`.
//!
//! A blocker is an object in the JS and is compared by identity (a set of blockers can hold
//! two `$$promises[1]`), so [`Blocker`] carries an id besides the index.

use oxc_ast::AstKind;
use oxc_ast::ast::*;
use rustc_hash::FxHashSet;

use super::nodes::{self, P};
use super::scope::{self, BindingId, Id, Kind, ScopeId};
use super::Analyzer;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Blocker {
    /// `$$promises[index]`
    pub index: u32,
    /// object identity
    pub id: u32,
}

/// An entry of `instance_body.sync`
#[derive(Debug, Clone, Copy)]
pub enum SyncItem<'s> {
    /// a statement as it is (a variable declaration with a single declarator included)
    Node(P<'s>),
    /// `b.declaration(kind, [declarator])`: one declarator of a declaration with several
    Declarator { declaration: &'s VariableDeclaration<'s>, declarator: P<'s> },
}

#[derive(Debug, Clone)]
pub struct AsyncGroup<'s> {
    /// statements or variable declarators
    pub nodes: Vec<P<'s>>,
    pub has_await: bool,
}

#[derive(Debug, Clone, Default)]
pub struct InstanceBody<'s> {
    pub sync: Vec<SyncItem<'s>>,
    pub r#async: Vec<AsyncGroup<'s>>,
    pub declarations: Vec<Id<'s>>,
    pub hoisted: Vec<P<'s>>,
}

/// `has_await_expression(node)`: an `await` outside nested functions
pub(crate) fn has_await_expression<'s>(an: &Analyzer<'s>, p: P<'s>) -> bool {
    match p {
        P::Js(AstKind::AwaitExpression(_)) => true,
        P::Js(AstKind::Function(_) | AstKind::ArrowFunctionExpression(_)) => false,
        _ => nodes::children(p, an.ast).into_iter().any(|c| has_await_expression(an, c)),
    }
}

struct Tracer<'a, 's> {
    an: &'a Analyzer<'s>,
    path: Vec<P<'s>>,
}

impl<'a, 's> Tracer<'a, 's> {
    fn scope_of(&self, p: P<'s>, scope: ScopeId) -> ScopeId {
        self.an.sc.map.get(&p.key()).copied().unwrap_or(scope)
    }

    fn reference(&self, p: P<'s>, scope: ScopeId) -> Option<BindingId> {
        let id = scope::ident(p)?;
        let parent = *self.path.last()?;
        if !scope::is_reference(p, parent) {
            return None;
        }
        self.an.get(scope, id.name)
    }

    /// `touch(expression, scope, touched, seen)`
    fn touch(&mut self, p: P<'s>, scope: ScopeId, touched: &mut Vec<BindingId>, seen: &mut FxHashSet<usize>) {
        if !seen.insert(p.key()) {
            return;
        }
        let saved = std::mem::take(&mut self.path);
        self.touch_walk(p, scope, touched, seen);
        self.path = saved;
    }

    fn touch_walk(&mut self, p: P<'s>, scope: ScopeId, touched: &mut Vec<BindingId>, seen: &mut FxHashSet<usize>) {
        let scope = self.scope_of(p, scope);
        match p {
            P::Js(AstKind::ImportDeclaration(_)) => return,
            _ if scope::ident(p).is_some() => {
                if let Some(b) = self.reference(p, scope) {
                    if !touched.contains(&b) {
                        touched.push(b);
                    }
                    for &(value, s) in &self.an.binding(b).assignments.clone() {
                        self.touch(value, s, touched, seen);
                    }
                }
                return;
            }
            _ => {}
        }
        self.path.push(p);
        for c in nodes::children(p, self.an.ast) {
            self.touch_walk(c, scope, touched, seen);
        }
        self.path.pop();
    }

    /// `trace_references(node, reads, writes, scope)`
    fn trace(&mut self, p: P<'s>, reads: &mut Vec<BindingId>, writes: &mut Vec<BindingId>, scope: ScopeId, same_set: bool) {
        let mut writes_seen = FxHashSet::default();
        let mut reads_seen = FxHashSet::default();
        let saved = std::mem::take(&mut self.path);
        self.trace_walk(p, reads, writes, scope, same_set, &mut writes_seen, &mut reads_seen);
        self.path = saved;
    }

    #[allow(clippy::too_many_arguments)]
    fn trace_walk(
        &mut self,
        p: P<'s>,
        reads: &mut Vec<BindingId>,
        writes: &mut Vec<BindingId>,
        scope: ScopeId,
        same_set: bool,
        writes_seen: &mut FxHashSet<usize>,
        reads_seen: &mut FxHashSet<usize>,
    ) {
        let scope = self.scope_of(p, scope);
        let update = |this: &Self, target: P<'s>, writes: &mut Vec<BindingId>, reads: &mut Vec<BindingId>| {
            let mut targets = scope::Nodes::new();
            scope::unwrap_pattern(target, &mut targets);
            for pattern in targets {
                let Some(node) = scope::object(pattern) else { return };
                let Some(b) = this.an.get(scope, node.name) else { return };
                let set = if same_set { &mut *reads } else { &mut *writes };
                if !set.contains(&b) {
                    set.push(b);
                }
            }
        };
        match p {
            P::Js(AstKind::AssignmentExpression(a)) => {
                update(self, nodes::target(&a.left), writes, reads);
                return;
            }
            P::Js(AstKind::UpdateExpression(u)) => {
                update(self, nodes::simple_target(&u.argument), writes, reads);
                return;
            }
            P::Js(AstKind::CallExpression(_)) => {
                if scope::get_rune(&self.an.sc, Some(p), scope) == Some("$effect") {
                    return;
                }
                let target = if same_set { &mut *reads } else { &mut *writes };
                self.touch(p, scope, target, writes_seen);
                return;
            }
            P::Js(AstKind::ReturnStatement(r)) => {
                if let Some(arg) = &r.argument {
                    self.touch(nodes::expr(arg), scope, reads, reads_seen);
                }
                return;
            }
            P::Js(AstKind::Function(_) | AstKind::ArrowFunctionExpression(_)) => return,
            _ if scope::ident(p).is_some() => {
                if let Some(b) = self.reference(p, scope) {
                    if !reads.contains(&b) {
                        reads.push(b);
                    }
                }
                return;
            }
            _ => {}
        }
        self.path.push(p);
        for c in nodes::children(p, self.an.ast) {
            self.trace_walk(c, reads, writes, scope, same_set, writes_seen, reads_seen);
        }
        self.path.pop();
    }
}

/// `calculate_blockers(instance, analysis)`
pub(crate) fn calculate_blockers(an: &mut Analyzer<'_>) {
    let Some(program) = an.instance_program else { return };
    let instance_scope = an.instance_scope;
    let body = nodes::children(program, an.ast);

    let mut result = InstanceBody::default();
    let mut awaited = false;
    let mut sync_group: Vec<P> = Vec::new();
    let mut next_id = 0u32;
    let mut new_blocker = |index: u32| {
        next_id += 1;
        Blocker { index, id: next_id }
    };
    // (binding, blocker) assignments, applied in order
    let mut assigned: Vec<(BindingId, Blocker)> = Vec::new();
    // functions whose blockers are computed once statements are known: (declaration or declarator, function)
    let mut functions: Vec<(Id, P)> = Vec::new();

    for node in body {
        let mut node = node;
        match node {
            P::Js(AstKind::ImportDeclaration(_)) => {
                result.hoisted.push(node);
                continue;
            }
            P::Js(AstKind::ExportDefaultDeclaration(_) | AstKind::ExportAllDeclaration(_)) => continue,
            P::Js(AstKind::ExportNamedDeclaration(_)) => continue,
            P::Js(AstKind::ExportDeclaration(d)) => node = nodes::declaration(&d.declaration),
            P::Js(AstKind::ExportFromDeclaration(_)) => continue,
            _ => {}
        }

        let has_await = has_await_expression(an, node);
        awaited |= has_await;

        let mut tracer = Tracer { an, path: Vec::new() };
        match node {
            P::Js(AstKind::Function(f)) if !f.is_expression() => {
                result.sync.push(SyncItem::Node(node));
                if let Some(id) = &f.id {
                    functions.push((scope::ident(P::Js(AstKind::BindingIdentifier(id))).unwrap(), node));
                }
            }
            P::Js(AstKind::VariableDeclaration(decl)) => {
                for declarator in &decl.declarations {
                    let dp = P::Js(AstKind::VariableDeclarator(declarator));
                    let init = declarator.init.as_ref().map(nodes::expr);
                    if scope::get_rune(&an.sc, init, instance_scope) == Some("$props.id") {
                        continue;
                    }
                    let item = if decl.declarations.len() == 1 {
                        SyncItem::Node(node)
                    } else {
                        SyncItem::Declarator { declaration: decl, declarator: dp }
                    };
                    if matches!(init, Some(P::Js(AstKind::ArrowFunctionExpression(_) | AstKind::Function(_)))) {
                        result.sync.push(item);
                        if let Some(id) = scope::extract_identifiers(nodes::binding(&declarator.id)).first() {
                            functions.push((*id, init.unwrap()));
                        }
                    } else if !awaited {
                        result.sync.push(item);
                    } else {
                        let mut reads = Vec::new();
                        let mut writes = Vec::new();
                        tracer.trace(dp, &mut reads, &mut writes, instance_scope, false);
                        if has_await && !sync_group.is_empty() {
                            result.r#async.push(AsyncGroup { nodes: std::mem::take(&mut sync_group), has_await: false });
                        }
                        let blocker = new_blocker(result.r#async.len() as u32);
                        for &b in &writes {
                            assigned.push((b, blocker));
                        }
                        for id in scope::extract_identifiers(nodes::binding(&declarator.id)) {
                            result.declarations.push(id);
                            if let Some(b) = an.get(instance_scope, id.name) {
                                assigned.push((b, blocker));
                            }
                        }
                        if has_await {
                            result.r#async.push(AsyncGroup { nodes: vec![dp], has_await: true });
                        } else {
                            sync_group.push(dp);
                        }
                    }
                }
            }
            _ if awaited => {
                let mut reads = Vec::new();
                let mut writes = Vec::new();
                tracer.trace(node, &mut reads, &mut writes, instance_scope, false);
                if has_await && !sync_group.is_empty() {
                    result.r#async.push(AsyncGroup { nodes: std::mem::take(&mut sync_group), has_await: false });
                }
                let blocker = new_blocker(result.r#async.len() as u32);
                for &b in &writes {
                    assigned.push((b, blocker));
                }
                if let P::Js(AstKind::Class(c)) = node {
                    if let Some(id) = &c.id {
                        let id = scope::ident(P::Js(AstKind::BindingIdentifier(id))).unwrap();
                        result.declarations.push(id);
                        if let Some(b) = an.get(instance_scope, id.name) {
                            assigned.push((b, blocker));
                        }
                    }
                }
                if has_await {
                    result.r#async.push(AsyncGroup { nodes: vec![node], has_await: true });
                } else {
                    sync_group.push(node);
                }
            }
            _ => result.sync.push(SyncItem::Node(node)),
        }
    }

    if !awaited {
        an.instance_body = result;
        return;
    }
    if !sync_group.is_empty() {
        result.r#async.push(AsyncGroup { nodes: std::mem::take(&mut sync_group), has_await: false });
    }

    for (b, blocker) in assigned {
        an.sc.binding_mut(b).blocker = Some(blocker);
    }

    // store subscriptions wait on whatever blocks the store
    let names: Vec<(&str, BindingId)> =
        an.sc.scope(instance_scope).declarations.iter().map(|(name, &b)| (*name, b)).collect();
    for (name, b) in names {
        if an.binding(b).kind != Kind::StoreSub {
            continue;
        }
        let Some(store_blocker) = an.get(instance_scope, &name[1..]).and_then(|s| an.binding(s).blocker) else { continue };
        let current = an.binding(b).blocker;
        if current.is_none_or(|c| c.index < store_blocker.index) {
            an.sc.binding_mut(b).blocker = Some(store_blocker);
        }
    }

    for (id, init) in functions {
        let mut reads_writes = Vec::new();
        let fn_scope = an.sc.map.get(&init.key()).copied().unwrap_or(instance_scope);
        let mut tracer = Tracer { an, path: Vec::new() };
        let body = match init {
            P::Js(AstKind::Function(f)) => f.body.as_ref().map(|b| P::Js(AstKind::FunctionBody(b))),
            P::Js(AstKind::ArrowFunctionExpression(a)) => Some(match &a.body {
                ArrowFunctionBody::FunctionBody(b) => P::Js(AstKind::FunctionBody(b)),
                body => nodes::expr(body.as_expression().unwrap()),
            }),
            _ => None,
        };
        let Some(body) = body else { continue };
        if matches!(body, P::Js(AstKind::FunctionBody(_))) {
            let mut writes = Vec::new();
            tracer.trace(body, &mut reads_writes, &mut writes, fn_scope, true);
        } else {
            tracer.touch(body, fn_scope, &mut reads_writes, &mut FxHashSet::default());
        }
        let max = reads_writes.iter().filter_map(|&b| an.binding(b).blocker).map(|bl| bl.index as i64).max().unwrap_or(-1);
        if max == -1 {
            continue;
        }
        let blocker = new_blocker(max as u32);
        if let Some(b) = an.get(instance_scope, id.name) {
            an.sc.binding_mut(b).blocker = Some(blocker);
        }
    }

    an.instance_body = result;
}
