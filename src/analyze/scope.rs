//! Port of `phases/scope.js`: scopes, bindings and `create_scopes`.

use oxc_ast::AstKind;
use oxc_ast::ast::*;
use rustc_hash::{FxHashMap, FxHashSet};

use super::nodes::{self, P, Res};
use super::utils::{is_reserved, is_rune};
use crate::ast::{Ast, Attr, Expr, Node, NodeId, Pattern};
use crate::errors as e;

pub type ScopeId = u32;
pub type BindingId = u32;
pub type FxIndexMap<K, V> = indexmap::IndexMap<K, V, std::hash::BuildHasherDefault<rustc_hash::FxHasher>>;

/// An identifier node: what Svelte's code holds as `binding.node`, `reference.node` etc.
#[derive(Clone, Copy, Debug)]
pub struct Id<'s> {
    pub name: &'s str,
    /// `None` for identifiers Svelte builds with `b.id()`
    pub span: Option<(u32, u32)>,
    /// identity
    pub key: usize,
}

impl<'s> Id<'s> {
    pub fn loc(&self) -> Option<(usize, usize)> {
        self.span.map(|(s, e)| (s as usize, e as usize))
    }
    pub fn err_loc(&self) -> crate::error::Loc {
        match self.span {
            Some((s, e)) => crate::error::Loc::Range(s as usize, e as usize),
            None => crate::error::Loc::None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Normal,
    Prop,
    BindableProp,
    RestProp,
    State,
    RawState,
    Derived,
    Each,
    Snippet,
    StoreSub,
    LegacyReactive,
    Template,
    Static,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeclKind {
    Var,
    Let,
    Const,
    Using,
    AwaitUsing,
    Function,
    Import,
    Param,
    RestParam,
    Synthetic,
}

#[derive(Debug)]
pub struct Binding<'s> {
    pub scope: ScopeId,
    pub node: Id<'s>,
    pub kind: Kind,
    pub declaration_kind: DeclKind,
    /// What the value was initialized with (an expression, a function/class/import
    /// declaration, an EachBlock or a SnippetBlock)
    pub initial: Option<P<'s>>,
    pub references: Vec<RefId>,
    pub legacy_dependencies: Vec<BindingId>,
    pub prop_alias: Option<&'s str>,
    pub inside_rest: bool,
    /// declared by a VariableDeclaration (`metadata.is_template_declaration`)
    pub is_template_declaration: bool,
    pub mutated: bool,
    pub reassigned: bool,
}

impl Binding<'_> {
    pub fn updated(&self) -> bool {
        self.mutated || self.reassigned
    }
}

pub type RefId = u32;

#[derive(Debug, Clone, Copy)]
pub struct Reference<'s> {
    pub node: Id<'s>,
    /// the last node of the path in `Scopes::path_tree` (`NO_PATH` if empty)
    pub path: u32,
}

pub const NO_PATH: u32 = u32::MAX;

#[derive(Debug)]
pub struct Scope<'s> {
    pub parent: Option<ScopeId>,
    pub porous: bool,
    pub function_depth: u32,
    pub declarations: DeclMap<'s>,
    /// Only kept for the scopes whose references the analysis looks at (the module scope,
    /// `$:` statements and snippets), see `track_references`
    pub references: FxIndexMap<&'s str, Vec<RefId>>,
    pub track_refs: bool,
    /// The parent to continue a lookup with: `parent`, skipping scopes that can never
    /// declare anything or record references (computed by `create_scopes`)
    pub lookup_parent: Option<ScopeId>,
}

impl Scope<'_> {
    #[inline]
    pub fn declared(&self, name: &str) -> Option<BindingId> {
        self.declarations.get(name).copied()
    }
}

/// An insertion-ordered map of declarations (a `Map` in the JS). Most scopes declare few
/// names, so this is a small vector, indexed once it grows.
#[derive(Debug, Default)]
pub struct DeclMap<'s> {
    entries: smallvec::SmallVec<[(&'s str, BindingId); 2]>,
    /// a cheap hash of each name, for scanning
    hashes: smallvec::SmallVec<[u32; 2]>,
    index: Option<Box<FxHashMap<&'s str, usize>>>,
}

/// A cheap hash of a name (length and first/last bytes), to skip most string comparisons
#[inline]
pub fn name_hash(name: &str) -> u32 {
    let b = name.as_bytes();
    match b.len() {
        0 => 0,
        n => (n as u32) << 16 | (b[0] as u32) << 8 | b[n - 1] as u32,
    }
}

impl<'s> DeclMap<'s> {
    pub fn get(&self, name: &str) -> Option<&BindingId> {
        self.get_hashed(name, name_hash(name))
    }

    #[inline]
    pub fn get_hashed(&self, name: &str, hash: u32) -> Option<&BindingId> {
        match &self.index {
            Some(index) => index.get(name).map(|&i| &self.entries[i].1),
            None => {
                for (i, &h) in self.hashes.iter().enumerate() {
                    if h == hash && self.entries[i].0 == name {
                        return Some(&self.entries[i].1);
                    }
                }
                None
            }
        }
    }

    pub fn contains_key(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    /// `map.set(name, binding)`: a new name goes last, an existing one keeps its position
    pub fn insert(&mut self, name: &'s str, b: BindingId) {
        let existing = match &self.index {
            Some(index) => index.get(name).copied(),
            None => self.entries.iter().position(|(k, _)| *k == name),
        };
        match existing {
            Some(i) => self.entries[i].1 = b,
            None => {
                self.entries.push((name, b));
                self.hashes.push(name_hash(name));
                let i = self.entries.len() - 1;
                match &mut self.index {
                    Some(index) => {
                        index.insert(name, i);
                    }
                    None if self.entries.len() > 8 => {
                        self.index = Some(Box::new(self.entries.iter().enumerate().map(|(i, (k, _))| (*k, i)).collect()));
                    }
                    None => {}
                }
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&&'s str, &BindingId)> {
        self.entries.iter().map(|(k, v)| (k, v))
    }

    pub fn values(&self) -> impl Iterator<Item = &BindingId> {
        self.entries.iter().map(|(_, v)| v)
    }
}

/// All scopes and bindings of a component (Svelte's `ScopeRoot` plus the per-AST `scopes` maps)
#[derive(Default)]
pub struct Scopes<'s> {
    pub scopes: Vec<Scope<'s>>,
    pub bindings: Vec<Binding<'s>>,
    pub refs: Vec<Reference<'s>>,
    /// Reference paths, stored as a tree: (node, index of its parent entry)
    pub path_tree: Vec<(P<'s>, u32)>,
    /// node → scope (`scopes.get(node)`)
    pub map: FxHashMap<usize, ScopeId>,
    pub conflicts: FxHashSet<&'s str>,
    next_synthetic: usize,
    /// `node.metadata.scopes` of components: slot name → scope (`default` first)
    pub component_scopes: FxHashMap<NodeId, Vec<(&'s str, ScopeId)>>,
}

impl<'s> Scopes<'s> {
    pub fn new_scope(&mut self, parent: Option<ScopeId>, porous: bool) -> ScopeId {
        let function_depth = match parent {
            Some(p) => self.scopes[p as usize].function_depth + if porous { 0 } else { 1 },
            None => 0,
        };
        self.scopes.push(Scope {
            parent,
            porous,
            function_depth,
            declarations: DeclMap::default(),
            references: FxIndexMap::default(),
            track_refs: parent.is_none(),
            lookup_parent: parent,
        });
        (self.scopes.len() - 1) as ScopeId
    }

    pub fn child(&mut self, scope: ScopeId, porous: bool) -> ScopeId {
        self.new_scope(Some(scope), porous)
    }

    pub fn synthetic_id(&mut self, name: &'s str) -> Id<'s> {
        self.next_synthetic += 1;
        Id { name, span: None, key: self.next_synthetic << 1 | 1 }
    }

    pub fn scope(&self, id: ScopeId) -> &Scope<'s> {
        &self.scopes[id as usize]
    }

    pub fn binding(&self, id: BindingId) -> &Binding<'s> {
        &self.bindings[id as usize]
    }

    pub fn binding_mut(&mut self, id: BindingId) -> &mut Binding<'s> {
        &mut self.bindings[id as usize]
    }

    pub fn get(&self, mut scope: ScopeId, name: &str) -> Option<BindingId> {
        let hash = name_hash(name);
        loop {
            let s = &self.scopes[scope as usize];
            if let Some(&b) = s.declarations.get_hashed(name, hash) {
                return Some(b);
            }
            scope = s.lookup_parent?;
        }
    }

    pub fn owner(&self, mut scope: ScopeId, name: &str) -> Option<ScopeId> {
        loop {
            let s = &self.scopes[scope as usize];
            if s.declared(name).is_some() {
                return Some(scope);
            }
            scope = s.lookup_parent?;
        }
    }

    /// Let lookups skip the scopes created after `from` (except `root`) that declare nothing
    fn compute_lookup_parents(&mut self, from: usize, root: ScopeId) {
        for s in from..self.scopes.len() {
            let parent = self.scopes[s].parent;
            self.scopes[s].lookup_parent = match parent {
                Some(p) if p != root && p as usize >= from => {
                    let ps = &self.scopes[p as usize];
                    if ps.declarations.is_empty() && !ps.track_refs { ps.lookup_parent } else { Some(p) }
                }
                other => other,
            };
        }
    }

    /// The `path` of a reference (ancestors, root first)
    pub fn ref_path(&self, r: RefId) -> Vec<P<'s>> {
        let mut path = Vec::new();
        let mut i = self.refs[r as usize].path;
        while i != NO_PATH {
            let (p, parent) = self.path_tree[i as usize];
            path.push(p);
            i = parent;
        }
        path.reverse();
        path
    }

    pub fn push_path(&mut self, p: P<'s>, parent: u32) -> u32 {
        self.path_tree.push((p, parent));
        (self.path_tree.len() - 1) as u32
    }

    pub fn declare(
        &mut self,
        scope: ScopeId,
        node: Id<'s>,
        kind: Kind,
        declaration_kind: DeclKind,
        initial: Option<P<'s>>,
    ) -> crate::error::Result<BindingId> {
        let s = &self.scopes[scope as usize];
        if let Some(parent) = s.parent {
            if declaration_kind == DeclKind::Var && s.porous {
                return self.declare(parent, node, kind, declaration_kind, None);
            }
            if declaration_kind == DeclKind::Import {
                return self.declare(parent, node, kind, declaration_kind, initial);
            }
        }

        if let Some(&existing) = s.declarations.get(node.name) {
            if self.bindings[existing as usize].declaration_kind != DeclKind::Var && declaration_kind != DeclKind::Var {
                return Err(e::declaration_duplicate(node.err_loc(), node.name));
            }
        }

        let binding = Binding {
            scope,
            node,
            kind,
            declaration_kind,
            initial,
            references: Vec::new(),
            legacy_dependencies: Vec::new(),
            prop_alias: None,
            inside_rest: false,
            is_template_declaration: false,
            mutated: false,
            reassigned: false,
        };
        validate_identifier_name(&binding, Some(s.function_depth))?;
        self.bindings.push(binding);
        let id = (self.bindings.len() - 1) as BindingId;
        self.scopes[scope as usize].declarations.insert(node.name, id);
        self.conflicts.insert(node.name);
        Ok(id)
    }

    /// `scope.reference(node, path)`, `path` being an entry of `path_tree`
    pub fn reference(&mut self, scope: ScopeId, node: Id<'s>, path: u32) {
        self.refs.push(Reference { node, path });
        let r = (self.refs.len() - 1) as RefId;
        self.add_reference(scope, node.name, r);
    }

    fn add_reference(&mut self, mut scope: ScopeId, name: &'s str, r: RefId) {
        let hash = name_hash(name);
        loop {
            let s = &mut self.scopes[scope as usize];
            if s.track_refs {
                s.references.entry(name).or_default().push(r);
            }
            if let Some(&b) = s.declarations.get_hashed(name, hash) {
                self.bindings[b as usize].references.push(r);
                return;
            }
            match s.lookup_parent {
                Some(p) => scope = p,
                None => {
                    self.conflicts.insert(name);
                    return;
                }
            }
        }
    }

    /// `scope.generate(name)` (only what's needed to compute the component name)
    pub fn generate(&mut self, mut scope: ScopeId, preferred_in: &str) -> String {
        while self.scopes[scope as usize].porous {
            scope = self.scopes[scope as usize].parent.unwrap();
        }
        let mut preferred = String::with_capacity(preferred_in.len());
        for c in preferred_name_chars(preferred_in) {
            preferred.push_str(c);
        }
        if preferred.as_bytes().first().is_some_and(|b| b.is_ascii_digit()) {
            preferred.replace_range(0..1, "_");
        }
        let s = &self.scopes[scope as usize];
        let taken = |name: &str| {
            s.references.contains_key(name) || s.declarations.contains_key(name) || self.conflicts.contains(name) || is_reserved(name)
        };
        let mut name = preferred.clone();
        let mut n = 1;
        while taken(&name) {
            name = format!("{preferred}_{n}");
            n += 1;
        }
        name
    }
}

/// `name.replace(/[^a-zA-Z0-9_$]/g, '_')`, per UTF-16 unit
fn preferred_name_chars(name: &str) -> impl Iterator<Item = &str> {
    name.char_indices().map(move |(i, c)| {
        if c.is_ascii_alphanumeric() || c == '_' || c == '$' {
            &name[i..i + 1]
        } else if c.len_utf16() == 2 {
            "__"
        } else {
            "_"
        }
    })
}

pub fn validate_identifier_name(binding: &Binding, function_depth: Option<u32>) -> Res {
    if !matches!(binding.declaration_kind, DeclKind::Synthetic | DeclKind::Param | DeclKind::RestParam)
        && function_depth.is_none_or(|d| d <= 1)
    {
        let node = binding.node;
        if node.name == "$" {
            return Err(e::dollar_binding_invalid(node.err_loc()));
        }
        if node.name.starts_with('$') {
            let type_import = matches!(binding.initial, Some(P::Js(AstKind::ImportDeclaration(i))) if i.import_kind.is_type());
            if !type_import {
                return Err(e::dollar_prefix_invalid(node.err_loc()));
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------
// identifiers and patterns

/// The identifier `p` is, if it is one
pub fn ident<'s>(p: P<'s>) -> Option<Id<'s>> {
    let span = |s: oxc_span::Span| Some((s.start, s.end));
    Some(match p {
        P::Js(AstKind::IdentifierReference(i)) => Id { name: i.name.as_str(), span: span(i.span), key: nodes::addr(&AstKind::IdentifierReference(i)) },
        P::Js(AstKind::BindingIdentifier(i)) => Id { name: i.name.as_str(), span: span(i.span), key: nodes::addr(&AstKind::BindingIdentifier(i)) },
        P::Js(AstKind::IdentifierName(i)) => Id { name: i.name.as_str(), span: span(i.span), key: nodes::addr(&AstKind::IdentifierName(i)) },
        P::Js(AstKind::LabelIdentifier(i)) => Id { name: i.name.as_str(), span: span(i.span), key: nodes::addr(&AstKind::LabelIdentifier(i)) },
        P::TplExpr(Expr::Ident { name, start, end, .. }) => {
            Id { name: name.as_str(), span: Some((*start as u32, *end as u32)), key: p.key() }
        }
        P::PatIdent(Pattern::Ident { name, start, end, .. }) => {
            Id { name: name.as_str(), span: Some((*start as u32, *end as u32)), key: p.key() }
        }
        _ => return None,
    })
}

pub fn is_member(p: P) -> bool {
    matches!(
        p,
        P::Js(AstKind::StaticMemberExpression(_) | AstKind::ComputedMemberExpression(_) | AstKind::PrivateFieldExpression(_))
    )
}

/// The `object` of a member expression
pub fn member_object<'s>(p: P<'s>) -> Option<P<'s>> {
    Some(match p {
        P::Js(AstKind::StaticMemberExpression(m)) => nodes::expr(&m.object),
        P::Js(AstKind::ComputedMemberExpression(m)) => nodes::expr(&m.object),
        P::Js(AstKind::PrivateFieldExpression(m)) => nodes::expr(&m.object),
        _ => return None,
    })
}

/// `object()`: the identifier at the root of a member expression chain
pub fn object<'s>(mut p: P<'s>) -> Option<Id<'s>> {
    while let Some(o) = member_object(p) {
        p = o;
    }
    ident(p)
}

pub type Nodes<'s> = smallvec::SmallVec<[P<'s>; 4]>;
pub type Ids<'s> = smallvec::SmallVec<[Id<'s>; 4]>;

/// `unwrap_pattern`: the identifiers and member expressions a pattern assigns to
pub fn unwrap_pattern<'s>(p: P<'s>, out: &mut Nodes<'s>) {
    use AstKind as K;
    match p {
        P::PatIdent(_) | P::TplExpr(Expr::Ident { .. }) => out.push(p),
        P::Js(K::IdentifierReference(_) | K::BindingIdentifier(_)) => out.push(p),
        _ if is_member(p) => out.push(p),
        P::Js(K::ObjectPattern(o)) => {
            for prop in &o.properties {
                unwrap_pattern(nodes::binding(&prop.value), out);
            }
            if let Some(r) = &o.rest {
                unwrap_pattern(nodes::binding(&r.argument), out);
            }
        }
        P::Js(K::ObjectAssignmentTarget(o)) => {
            for prop in &o.properties {
                match prop {
                    AssignmentTargetProperty::AssignmentTargetPropertyIdentifier(x) => {
                        out.push(P::Js(K::IdentifierReference(&x.binding)));
                    }
                    AssignmentTargetProperty::AssignmentTargetPropertyProperty(x) => {
                        unwrap_pattern(nodes::target_maybe_default(&x.binding), out)
                    }
                }
            }
            if let Some(r) = &o.rest {
                unwrap_pattern(nodes::target(&r.target), out);
            }
        }
        P::Js(K::ArrayPattern(a)) => {
            for el in a.elements.iter().flatten() {
                unwrap_pattern(nodes::binding(el), out);
            }
            if let Some(r) = &a.rest {
                unwrap_pattern(nodes::binding(&r.argument), out);
            }
        }
        P::Js(K::ArrayAssignmentTarget(a)) => {
            for el in a.elements.iter().flatten() {
                unwrap_pattern(nodes::target_maybe_default(el), out);
            }
            if let Some(r) = &a.rest {
                unwrap_pattern(nodes::target(&r.target), out);
            }
        }
        P::Js(K::BindingRestElement(r)) => unwrap_pattern(nodes::binding(&r.argument), out),
        P::Js(K::AssignmentTargetRest(r)) => unwrap_pattern(nodes::target(&r.target), out),
        P::Js(K::AssignmentPattern(a)) => unwrap_pattern(nodes::binding(&a.left), out),
        P::Js(K::AssignmentTargetWithDefault(a)) => unwrap_pattern(nodes::target(&a.binding), out),
        P::ParamDefault(param) => unwrap_pattern(nodes::binding(&param.pattern), out),
        P::AtpiDefault(x) => out.push(P::Js(K::IdentifierReference(&x.binding))),
        _ => {}
    }
}

pub fn extract_identifiers<'s>(p: P<'s>) -> Ids<'s> {
    if let Some(id) = ident(p) {
        let mut ids = Ids::new();
        ids.push(id);
        return ids;
    }
    let mut nodes = Nodes::new();
    unwrap_pattern(p, &mut nodes);
    nodes.into_iter().filter_map(ident).collect()
}

/// `extract_identifiers_from_destructuring` (for `let:` directives)
fn extract_identifiers_from_destructuring<'s>(p: P<'s>, out: &mut Vec<Id<'s>>) {
    match p {
        P::Js(AstKind::ObjectExpression(o)) => {
            for prop in &o.properties {
                match prop {
                    ObjectPropertyKind::ObjectProperty(prop) => extract_identifiers_from_destructuring(nodes::expr(&prop.value), out),
                    ObjectPropertyKind::SpreadProperty(s) => extract_identifiers_from_destructuring(nodes::expr(&s.argument), out),
                }
            }
        }
        P::Js(AstKind::ArrayExpression(a)) => {
            for el in &a.elements {
                match el {
                    ArrayExpressionElement::SpreadElement(s) => {
                        // `extract_identifiers_from_destructuring(element)` on a SpreadElement: no match
                        let _ = s;
                    }
                    ArrayExpressionElement::Elision(_) => {}
                    _ => extract_identifiers_from_destructuring(nodes::expr(el.as_expression().unwrap()), out),
                }
            }
        }
        _ => {
            if let Some(id) = ident(p) {
                out.push(id);
            }
        }
    }
}

/// acorn-typescript puts a binding's type annotation on the identifier, which then ends
/// where the annotation ends
pub fn with_type_annotation<'s>(
    id: Id<'s>,
    pattern: &BindingPattern,
    annotation: &Option<oxc_allocator::Box<TSTypeAnnotation>>,
) -> Id<'s> {
    match (pattern, annotation, id.span) {
        (BindingPattern::BindingIdentifier(b), Some(t), Some((start, _))) if b.span.start == start => {
            Id { span: Some((start, t.span.end)), ..id }
        }
        _ => id,
    }
}

/// `is_reference(node, parent)` from `is-reference`
pub fn is_reference(node: P, parent: P) -> bool {
    use AstKind as K;
    if is_member(node) {
        return match node {
            P::Js(K::StaticMemberExpression(m)) => is_reference(nodes::expr(&m.object), node),
            P::Js(K::PrivateFieldExpression(m)) => is_reference(nodes::expr(&m.object), node),
            _ => false,
        };
    }
    if ident(node).is_none() {
        return false;
    }
    match parent {
        P::Js(K::StaticMemberExpression(m)) => node.is(nodes::expr(&m.object)),
        P::Js(K::PrivateFieldExpression(m)) => node.is(nodes::expr(&m.object)),
        P::Js(K::ComputedMemberExpression(_)) => true,
        P::Js(K::MethodDefinition(m)) => m.computed,
        P::Js(K::ImportMeta(_) | K::NewTarget(_)) => false,
        P::Js(K::PropertyDefinition(d)) => d.computed || d.value.as_ref().is_some_and(|v| node.is(nodes::expr(v))),
        P::Js(K::AccessorProperty(d)) => d.computed || d.value.as_ref().is_some_and(|v| node.is(nodes::expr(v))),
        P::Js(K::ObjectProperty(p)) => p.computed || node.is(nodes::expr(&p.value)),
        P::Js(K::BindingProperty(p)) => p.computed || node.is(nodes::binding(&p.value)),
        P::Js(K::AssignmentTargetPropertyIdentifier(_)) => true,
        P::Js(K::AssignmentTargetPropertyProperty(p)) => p.computed || node.is(nodes::target_maybe_default(&p.binding)),
        P::Js(K::ExportSpecifier(s)) => node.is(nodes::module_export_name(&s.local)),
        P::Js(K::ImportSpecifier(s)) => node.is(P::Js(K::BindingIdentifier(&s.local))),
        P::Js(K::LabeledStatement(_) | K::BreakStatement(_) | K::ContinueStatement(_)) => false,
        _ => true,
    }
}

// ---------------------------------------------------------------------------------------
// runes

/// `get_global_keypath`
pub fn get_global_keypath(scopes: &Scopes, callee: P, scope: ScopeId) -> Option<String> {
    let mut n = callee;
    let mut joined = String::new();
    loop {
        match n {
            P::Js(AstKind::StaticMemberExpression(m)) => {
                joined.insert_str(0, m.property.name.as_str());
                joined.insert(0, '.');
                n = nodes::expr(&m.object);
            }
            P::Js(AstKind::ComputedMemberExpression(_)) => return None,
            P::Js(AstKind::PrivateFieldExpression(_)) => return None,
            _ => break,
        }
    }
    if let P::Js(AstKind::CallExpression(c)) = n {
        let callee = nodes::expr(&c.callee);
        if ident(callee).is_some() {
            joined.insert_str(0, "()");
            n = callee;
        }
    }
    let id = ident(n)?;
    if scopes.get(scope, id.name).is_some() {
        return None;
    }
    Some(format!("{}{}", id.name, joined))
}

/// `get_rune`: the rune a call expression calls, if any
pub fn get_rune(scopes: &Scopes, node: Option<P>, scope: ScopeId) -> Option<&'static str> {
    let P::Js(AstKind::CallExpression(c)) = node? else { return None };
    // fast path: the callee must start with `$`
    let mut root = nodes::expr(&c.callee);
    while let Some(o) = member_object(root).or(match root {
        P::Js(AstKind::CallExpression(c)) => Some(nodes::expr(&c.callee)),
        _ => None,
    }) {
        root = o;
    }
    if !ident(root).is_some_and(|i| i.name.starts_with('$')) {
        return None;
    }
    let keypath = get_global_keypath(scopes, nodes::expr(&c.callee), scope)?;
    is_rune(&keypath)
}

// ---------------------------------------------------------------------------------------
// create_scopes

pub struct ScopeBuilder<'s, 'x> {
    pub scopes: &'x mut Scopes<'s>,
    pub ast: &'s Ast<'s>,
    path: Vec<P<'s>>,
    /// `path_tree` entries of `path[..materialized]`
    path_entries: Vec<u32>,
    materialized: usize,
    references: Vec<(ScopeId, Id<'s>, u32)>,
    updates: Vec<(ScopeId, P<'s>)>,
    possible_implicit_declarations: Vec<Id<'s>>,
    allow_reactive_declarations: bool,
    top: ScopeId,
    pub has_await: bool,
    alloc: &'s oxc_allocator::Allocator,
}

pub struct Created {
    pub scope: ScopeId,
    pub has_await: bool,
}

/// `create_scopes(ast, root, allow_reactive_declarations, parent)`
pub fn create_scopes<'s>(
    scopes: &mut Scopes<'s>,
    ast: &'s Ast<'s>,
    alloc: &'s oxc_allocator::Allocator,
    root: Option<P<'s>>,
    allow_reactive_declarations: bool,
    parent: Option<ScopeId>,
) -> crate::error::Result<Created> {
    let scope = scopes.new_scope(parent, false);
    let first_scope = scopes.scopes.len();
    let mut b = ScopeBuilder {
        scopes,
        ast,
        path: Vec::new(),
        path_entries: Vec::new(),
        materialized: 0,
        references: Vec::new(),
        updates: Vec::new(),
        possible_implicit_declarations: Vec::new(),
        allow_reactive_declarations,
        top: scope,
        has_await: false,
        alloc,
    };
    if let Some(root) = root {
        b.scopes.map.insert(root.key(), scope);
        b.visit(root, scope)?;
    }

    b.scopes.compute_lookup_parents(first_scope, scope);

    for id in std::mem::take(&mut b.possible_implicit_declarations) {
        if b.scopes.get(scope, id.name).is_some() {
            continue;
        }
        b.scopes.declare(scope, id, Kind::LegacyReactive, DeclKind::Let, None)?;
    }

    let paths = std::mem::take(&mut b.path);
    drop(paths);
    for (s, node, path) in std::mem::take(&mut b.references) {
        b.scopes.refs.push(Reference { node, path });
        let r = (b.scopes.refs.len() - 1) as RefId;
        b.scopes.add_reference(s, node.name, r);
    }

    for (s, node) in std::mem::take(&mut b.updates) {
        let mut targets = Nodes::new();
        unwrap_pattern(node, &mut targets);
        for expression in targets {
            let Some(left) = object(expression) else { continue };
            let Some(binding) = b.scopes.get(s, left.name) else { continue };
            let binding = b.scopes.binding_mut(binding);
            if left.key != binding.node.key {
                if ident(expression).is_some() {
                    binding.reassigned = true;
                } else {
                    binding.mutated = true;
                }
            }
        }
    }

    Ok(Created { scope, has_await: b.has_await })
}

impl<'s> ScopeBuilder<'s, '_> {
    fn push(&mut self, p: P<'s>) {
        self.path.push(p);
    }

    fn pop(&mut self) {
        self.path.pop();
        if self.materialized > self.path.len() {
            self.materialized = self.path.len();
            self.path_entries.truncate(self.materialized);
        }
    }

    /// The `path_tree` entry for the current path (adding what's missing)
    fn current_path(&mut self) -> u32 {
        for i in self.materialized..self.path.len() {
            let parent = if i == 0 { NO_PATH } else { self.path_entries[i - 1] };
            let entry = self.scopes.push_path(self.path[i], parent);
            self.path_entries.push(entry);
        }
        self.materialized = self.path.len();
        self.path_entries.last().copied().unwrap_or(NO_PATH)
    }

    fn next(&mut self, p: P<'s>, scope: ScopeId) -> Res {
        self.push(p);
        nodes::each_child(p, self.ast, self, &mut |me: &mut Self, c| me.visit(c, scope))?;
        self.pop();
        Ok(())
    }

    /// `context.visit(child, state)` from the visitor of `parent`
    fn visit_child(&mut self, parent: P<'s>, child: P<'s>, scope: ScopeId) -> Res {
        self.push(parent);
        self.visit(child, scope)?;
        self.pop();
        Ok(())
    }

    fn reference_now(&mut self, scope: ScopeId, id: Id<'s>, extra: Option<P<'s>>) {
        let mut path = self.current_path();
        if let Some(x) = extra {
            path = self.scopes.push_path(x, path);
        }
        self.scopes.reference(scope, id, path);
    }

    fn add_params(&mut self, scope: ScopeId, params: &'s FormalParameters<'s>) -> Res {
        for (i, p) in params.items.iter().enumerate() {
            if i == 0 {
                if let BindingPattern::BindingIdentifier(id) = &p.pattern {
                    if id.name == "this" {
                        continue;
                    }
                }
            }
            for id in extract_identifiers(nodes::param(p)) {
                let id = with_type_annotation(id, &p.pattern, &p.type_annotation);
                self.scopes.declare(scope, id, Kind::Normal, DeclKind::Param, None)?;
            }
        }
        if let Some(rest) = &params.rest {
            for id in extract_identifiers(P::Js(AstKind::BindingRestElement(&rest.rest))) {
                self.scopes.declare(scope, id, Kind::Normal, DeclKind::RestParam, None)?;
            }
        }
        Ok(())
    }

    fn block_scope(&mut self, p: P<'s>, scope: ScopeId) -> Res {
        let s = self.scopes.child(scope, true);
        self.scopes.map.insert(p.key(), s);
        self.next(p, s)
    }

    fn name_id(&mut self, name: &'s str) -> Id<'s> {
        self.scopes.synthetic_id(name)
    }

    fn visit(&mut self, p: P<'s>, scope: ScopeId) -> Res {
        use AstKind as K;
        match p {
            P::Js(k) => match k {
                K::AwaitExpression(_) => {
                    let in_function = self.path.iter().any(|n| {
                        matches!(n, P::Js(K::ArrowFunctionExpression(_) | K::Function(_)))
                    });
                    if !in_function {
                        self.has_await = true;
                    }
                    self.next(p, scope)
                }
                K::IdentifierReference(_) | K::BindingIdentifier(_) | K::IdentifierName(_) | K::LabelIdentifier(_) => {
                    if let Some(&parent) = self.path.last() {
                        if is_reference(p, parent) {
                            let path = self.current_path();
                            self.references.push((scope, ident(p).unwrap(), path));
                        }
                    }
                    Ok(())
                }
                K::LabeledStatement(l) => {
                    if self.path.len() > 1 || !self.allow_reactive_declarations || l.label.name != "$" {
                        return self.next(p, scope);
                    }
                    let s = self.scopes.child(self.top, false);
                    self.scopes.scopes[s as usize].track_refs = true;
                    self.scopes.map.insert(p.key(), s);
                    if let Statement::ExpressionStatement(es) = &l.body {
                        if let Expression::AssignmentExpression(a) = nodes::strip(&es.expression) {
                            for id in extract_identifiers(nodes::target(&a.left)) {
                                if !id.name.starts_with('$') {
                                    self.possible_implicit_declarations.push(id);
                                }
                            }
                        }
                    }
                    self.next(p, s)
                }
                K::AssignmentExpression(a) => {
                    self.updates.push((scope, nodes::target(&a.left)));
                    self.next(p, scope)
                }
                K::UpdateExpression(u) => {
                    self.updates.push((scope, nodes::simple_target(&u.argument)));
                    self.next(p, scope)
                }
                K::ImportDeclaration(i) => {
                    if let Some(specs) = &i.specifiers {
                        for s in specs {
                            if nodes::is_type_specifier(s) {
                                continue;
                            }
                            let local = match s {
                                ImportDeclarationSpecifier::ImportSpecifier(x) => &x.local,
                                ImportDeclarationSpecifier::ImportDefaultSpecifier(x) => &x.local,
                                ImportDeclarationSpecifier::ImportNamespaceSpecifier(x) => &x.local,
                            };
                            let id = ident(P::Js(K::BindingIdentifier(local))).unwrap();
                            self.scopes.declare(scope, id, Kind::Normal, DeclKind::Import, Some(p))?;
                        }
                    }
                    Ok(())
                }
                K::Function(f) => {
                    if f.is_expression() {
                        let s = self.scopes.child(scope, true);
                        self.scopes.map.insert(p.key(), s);
                        if let Some(id) = &f.id {
                            let id = ident(P::Js(K::BindingIdentifier(id))).unwrap();
                            self.scopes.declare(s, id, Kind::Normal, DeclKind::Function, None)?;
                        }
                        self.add_params(s, &f.params)?;
                        self.next(p, s)
                    } else {
                        if let Some(id) = &f.id {
                            let id = ident(P::Js(K::BindingIdentifier(id))).unwrap();
                            self.scopes.declare(scope, id, Kind::Normal, DeclKind::Function, Some(p))?;
                        }
                        let s = self.scopes.child(scope, true);
                        self.scopes.map.insert(p.key(), s);
                        self.add_params(s, &f.params)?;
                        self.next(p, s)
                    }
                }
                K::ArrowFunctionExpression(a) => {
                    let s = self.scopes.child(scope, true);
                    self.scopes.map.insert(p.key(), s);
                    self.add_params(s, &a.params)?;
                    self.next(p, s)
                }
                K::ForStatement(_) | K::ForInStatement(_) | K::ForOfStatement(_) | K::SwitchStatement(_) => {
                    self.block_scope(p, scope)
                }
                K::BlockStatement(_) | K::FunctionBody(_) => {
                    let parent_is_function =
                        matches!(self.path.last(), Some(P::Js(K::Function(_) | K::ArrowFunctionExpression(_))));
                    if parent_is_function {
                        let s = self.scopes.child(scope, false);
                        self.scopes.map.insert(p.key(), s);
                        self.next(p, s)
                    } else {
                        self.block_scope(p, scope)
                    }
                }
                K::Class(c) if !c.is_expression() => {
                    if let Some(id) = &c.id {
                        let id = ident(P::Js(K::BindingIdentifier(id))).unwrap();
                        self.scopes.declare(scope, id, Kind::Normal, DeclKind::Let, Some(p))?;
                    }
                    self.next(p, scope)
                }
                K::VariableDeclaration(d) => {
                    let kind = match d.kind {
                        VariableDeclarationKind::Var => DeclKind::Var,
                        VariableDeclarationKind::Let => DeclKind::Let,
                        VariableDeclarationKind::Const => DeclKind::Const,
                        VariableDeclarationKind::Using => DeclKind::Using,
                        VariableDeclarationKind::AwaitUsing => DeclKind::AwaitUsing,
                    };
                    for declarator in &d.declarations {
                        let init = declarator.init.as_ref().map(nodes::expr);
                        for id in extract_identifiers(nodes::binding(&declarator.id)) {
                            let id = with_type_annotation(id, &declarator.id, &declarator.type_annotation);
                            let b = self.scopes.declare(scope, id, Kind::Normal, kind, init)?;
                            self.scopes.binding_mut(b).is_template_declaration = true;
                        }
                    }
                    self.next(p, scope)
                }
                K::CatchClause(c) => {
                    if let Some(param) = &c.param {
                        let s = self.scopes.child(scope, true);
                        self.scopes.map.insert(p.key(), s);
                        for id in extract_identifiers(nodes::binding(&param.pattern)) {
                            let id = with_type_annotation(id, &param.pattern, &param.type_annotation);
                            self.scopes.declare(s, id, Kind::Normal, DeclKind::Let, None)?;
                        }
                        self.next(p, s)
                    } else {
                        self.next(p, scope)
                    }
                }
                _ => self.next(p, scope),
            },
            P::TplExpr(Expr::Ident { .. }) | P::PatIdent(_) => {
                if let Some(&parent) = self.path.last() {
                    if is_reference(p, parent) {
                        let path = self.current_path();
                        self.references.push((scope, ident(p).unwrap(), path));
                    }
                }
                Ok(())
            }
            P::ConstDecl(n) => {
                // VariableDeclaration whose parent is a ConstTag
                if let Node::ConstTag { id, init, .. } = &self.ast.nodes[n] {
                    let init = Some(nodes::template_expr(init));
                    for ident in extract_identifiers(nodes::pattern_p(id)) {
                        let b = self.scopes.declare(scope, ident, Kind::Template, DeclKind::Const, init)?;
                        self.scopes.binding_mut(b).is_template_declaration = true;
                    }
                }
                self.next(p, scope)
            }
            P::Fragment(f) => {
                let s = self.scopes.child(scope, self.ast.fragments[f].transparent);
                self.scopes.map.insert(p.key(), s);
                self.next(p, s)
            }
            P::Attr(a) => self.visit_attr(p, a, scope),
            P::Node(n) => self.visit_node(p, n, scope),
            _ => self.next(p, scope),
        }
    }

    fn visit_attr(&mut self, p: P<'s>, a: &'s Attr<'s>, scope: ScopeId) -> Res {
        match a {
            Attr::Directive { kind: "LetDirective", name, expression, start, end, .. } => {
                let mut ids = Vec::new();
                match expression {
                    Some(e) => extract_identifiers_from_destructuring(nodes::template_expr(e), &mut ids),
                    None => {
                        let id = self.scopes.synthetic_id(name);
                        ids.push(Id { span: Some((*start as u32, *end as u32)), ..id });
                    }
                }
                let parent = *self.path.last().unwrap();
                for id in ids {
                    self.scopes.declare(scope, id, Kind::Template, DeclKind::Const, None)?;
                    let first = self.scopes.push_path(parent, NO_PATH);
                    let path = self.scopes.push_path(p, first);
                    self.scopes.reference(scope, id, path);
                }
                Ok(())
            }
            Attr::Directive { kind: "TransitionDirective" | "AnimateDirective" | "UseDirective", name, expression, .. } => {
                let first = name.split('.').next().unwrap_or("");
                let id = self.name_id(first);
                self.reference_now(scope, id, None);
                if let Some(e) = expression {
                    self.visit_child(p, nodes::template_expr(e), scope)?;
                }
                Ok(())
            }
            Attr::Directive { kind: "BindDirective", expression: Some(e), .. } => {
                let ep = nodes::template_expr(e);
                if !matches!(ep, P::Js(AstKind::SequenceExpression(_))) {
                    self.updates.push((scope, ep));
                }
                self.next(p, scope)
            }
            Attr::StyleDirective { name, value: crate::ast::AttrValue::True, .. } => {
                let id = self.name_id(name);
                self.reference_now(scope, id, Some(p));
                self.next(p, scope)
            }
            _ => self.next(p, scope),
        }
    }

    fn visit_node(&mut self, p: P<'s>, n: NodeId, scope: ScopeId) -> Res {
        let ast = self.ast;
        match &ast.nodes[n] {
            Node::Element(el) => match el.kind {
                "SvelteFragment" | "SlotElement" | "SvelteElement" | "RegularElement" => {
                    let s = self.scopes.child(scope, false);
                    self.scopes.map.insert(p.key(), s);
                    self.next(p, s)
                }
                "Component" | "SvelteSelf" | "SvelteComponent" => {
                    if el.kind == "Component" {
                        let first = el.name.split('.').next().unwrap_or("");
                        let id = self.name_id(first);
                        self.reference_now(scope, id, None);
                    }
                    self.visit_component(p, n, scope)
                }
                _ => self.next(p, scope),
            },
            Node::EachBlock { expression, context, body, fallback, index, key, .. } => {
                self.visit_child(p, nodes::template_expr(expression), scope)?;
                if let Some(f) = fallback {
                    self.visit_child(p, P::Fragment(*f), scope)?;
                }
                let s = self.scopes.child(scope, false);
                self.scopes.map.insert(p.key(), s);
                if let Some(c) = context {
                    let cp = nodes::pattern_p(c);
                    let ids = extract_identifiers(cp);
                    for id in ids {
                        let b = self.scopes.declare(s, id, Kind::Each, DeclKind::Const, None)?;
                        let inside_rest = is_inside_rest(cp, id.key, false);
                        self.scopes.binding_mut(b).inside_rest = inside_rest;
                    }
                    self.visit_child(p, cp, s)?;
                }
                if let Some(index) = index {
                    let is_keyed =
                        key.as_ref().is_some_and(|k| ident(nodes::template_expr(k)).is_none_or(|i| i.name != index));
                    let name: &'s str = self.alloc.alloc_str(index);
                    let id = self.name_id(name);
                    self.scopes.declare(s, id, if is_keyed { Kind::Template } else { Kind::Static }, DeclKind::Const, Some(p))?;
                }
                if let Some(k) = key {
                    self.visit_child(p, nodes::template_expr(k), s)?;
                }
                for &child in &ast.fragments[*body].nodes {
                    self.visit_child(p, P::Node(child), s)?;
                }
                Ok(())
            }
            Node::AwaitBlock { expression, value, error, pending, then, catch, .. } => {
                self.visit_child(p, nodes::template_expr(expression), scope)?;
                if let Some(f) = pending {
                    self.visit_child(p, P::Fragment(*f), scope)?;
                }
                for (frag, pattern) in [(then, value), (catch, error)] {
                    let Some(frag) = frag else { continue };
                    self.visit_child(p, P::Fragment(*frag), scope)?;
                    if let Some(pattern) = pattern {
                        let frag_scope = self.scopes.map[&P::Fragment(*frag).key()];
                        let value_scope = self.scopes.child(scope, false);
                        let pp = nodes::pattern_p(pattern);
                        self.scopes.map.insert(pp.key(), value_scope);
                        self.visit_child(p, pp, value_scope)?;
                        for id in extract_identifiers(pp) {
                            self.scopes.declare(frag_scope, id, Kind::Template, DeclKind::Const, None)?;
                            self.scopes.declare(value_scope, id, Kind::Normal, DeclKind::Const, None)?;
                        }
                    }
                }
                Ok(())
            }
            Node::SnippetBlock { expression, parameters, body, .. } => {
                let id = ident(nodes::template_expr(expression)).unwrap();
                self.scopes.declare(scope, id, Kind::Normal, DeclKind::Function, Some(p))?;
                let child = self.scopes.child(scope, false);
                self.scopes.scopes[child as usize].track_refs = true;
                self.scopes.map.insert(p.key(), child);
                for param in nodes::snippet_params(parameters) {
                    for id in extract_identifiers(param) {
                        self.scopes.declare(child, id, Kind::Snippet, DeclKind::Let, None)?;
                    }
                }
                self.next(p, child)?;
                let body_scope = self.scopes.map[&P::Fragment(*body).key()];
                let decls: Vec<(&str, BindingId)> =
                    self.scopes.scope(body_scope).declarations.iter().map(|(k, v)| (*k, *v)).collect();
                for (name, b) in decls {
                    if self.scopes.scope(child).declarations.contains_key(name) {
                        return Err(e::declaration_duplicate(self.scopes.binding(b).node.err_loc(), name));
                    }
                }
                Ok(())
            }
            _ => self.next(p, scope),
        }
    }

    fn visit_component(&mut self, p: P<'s>, n: NodeId, scope: ScopeId) -> Res {
        let ast = self.ast;
        let Node::Element(el) = &ast.nodes[n] else { unreachable!() };
        let default = self.scopes.child(scope, false);
        let mut slot_scopes: Vec<(&'s str, ScopeId)> = vec![("default", default)];

        if el.kind == "SvelteComponent" {
            if let Some(e) = &el.expression {
                self.visit_child(p, nodes::template_expr(e), scope)?;
            }
        }

        let default_scope = if determine_slot(ast, n).is_some() { scope } else { default };
        for a in &el.attributes {
            let s = if matches!(a, Attr::Directive { kind: "LetDirective", .. }) { default_scope } else { scope };
            self.visit_child(p, P::Attr(a), s)?;
        }

        self.scopes.component_scopes.insert(n, slot_scopes.clone());
        for &child in &ast.fragments[el.fragment].nodes {
            let mut s = default_scope;
            if let Some(slot_name) = determine_slot(ast, child) {
                let slot_scope = self.scopes.child(scope, false);
                match slot_scopes.iter_mut().find(|(k, _)| *k == slot_name) {
                    Some(entry) => entry.1 = slot_scope,
                    None => slot_scopes.push((slot_name, slot_scope)),
                }
                self.scopes.component_scopes.insert(n, slot_scopes.clone());
                s = slot_scope;
            }
            self.visit_child(p, P::Node(child), s)?;
        }
        Ok(())
    }
}

/// Whether the identifier `key` is inside a RestElement of `pattern`
fn is_inside_rest(p: P, key: usize, inside: bool) -> bool {
    use AstKind as K;
    if let Some(id) = ident(p) {
        return inside && id.key == key;
    }
    match p {
        P::Js(K::ObjectPattern(o)) => {
            o.properties.iter().any(|prop| is_inside_rest(nodes::binding(&prop.value), key, inside))
                || o.rest.as_ref().is_some_and(|r| is_inside_rest(nodes::binding(&r.argument), key, true))
        }
        P::Js(K::ObjectAssignmentTarget(o)) => {
            o.properties.iter().any(|prop| match prop {
                AssignmentTargetProperty::AssignmentTargetPropertyIdentifier(x) => {
                    is_inside_rest(P::Js(K::IdentifierReference(&x.binding)), key, inside)
                }
                AssignmentTargetProperty::AssignmentTargetPropertyProperty(x) => {
                    is_inside_rest(nodes::target_maybe_default(&x.binding), key, inside)
                }
            }) || o.rest.as_ref().is_some_and(|r| is_inside_rest(nodes::target(&r.target), key, true))
        }
        P::Js(K::ArrayPattern(a)) => {
            a.elements.iter().flatten().any(|el| is_inside_rest(nodes::binding(el), key, inside))
                || a.rest.as_ref().is_some_and(|r| is_inside_rest(nodes::binding(&r.argument), key, true))
        }
        P::Js(K::ArrayAssignmentTarget(a)) => {
            a.elements.iter().flatten().any(|el| is_inside_rest(nodes::target_maybe_default(el), key, inside))
                || a.rest.as_ref().is_some_and(|r| is_inside_rest(nodes::target(&r.target), key, true))
        }
        P::Js(K::AssignmentPattern(a)) => is_inside_rest(nodes::binding(&a.left), key, inside),
        P::Js(K::AssignmentTargetWithDefault(a)) => is_inside_rest(nodes::target(&a.binding), key, inside),
        _ => false,
    }
}

/// `determine_slot`: the static `slot` attribute of an element-like node
pub fn determine_slot<'s>(ast: &'s Ast<'s>, n: NodeId) -> Option<&'s str> {
    let Node::Element(el) = &ast.nodes[n] else { return None };
    if !matches!(
        el.kind,
        "SvelteElement" | "RegularElement" | "SvelteFragment" | "Component" | "SvelteComponent" | "SvelteSelf" | "SlotElement"
    ) {
        return None;
    }
    for a in &el.attributes {
        if let Attr::Attribute { name: "slot", value, .. } = a {
            if let Some(text) = super::utils::text_value(value) {
                return Some(text);
            }
        }
    }
    None
}
