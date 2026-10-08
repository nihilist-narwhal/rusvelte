//! A port of Svelte's analysis phase (`phases/2-analyze`), far enough to reproduce the
//! warnings and errors of `compile(source, { dev: true, generate: false, filename })`.
//!
//! Entry point: [`compile_diagnostics`].

mod a11y;
#[allow(clippy::all)]
mod a11y_data;
mod comments;
mod css;
mod nodes;
mod scope;
mod ts;
mod utils;
mod visit;
#[allow(clippy::all)]
pub mod warnings;

use oxc_allocator::Allocator;
use oxc_ast::AstKind;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::ast::{Ast, Attr, FragId, Node, NodeId, Root};
use crate::error::{CompileError, Loc, Result};
use crate::errors as e;
use crate::locator::Locator;
use nodes::P;
use scope::{BindingId, DeclKind, Id, Kind, ScopeId, Scopes};
use warnings::W;

/// A line/column position, as `locate-character` reports it (`column` and `character` in
/// UTF-16 units, lines broken on `\n` only)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    pub line: usize,
    pub column: usize,
    pub character: usize,
}

/// A warning or error, as reported by `svelte/compiler`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub code: &'static str,
    pub message: String,
    pub start: Option<Position>,
    pub end: Option<Position>,
}

/// A warning with byte offsets
#[derive(Debug, Clone)]
pub struct Warning {
    pub code: &'static str,
    pub message: String,
    pub position: Option<(usize, usize)>,
}

/// The warnings `svelte.compile(source, { dev: true, generate: false, filename })` reports,
/// or the error it throws. `filename` should be the file's basename.
pub fn compile_diagnostics(source: &str, filename: &str) -> std::result::Result<Vec<Diagnostic>, Diagnostic> {
    let source = source.strip_prefix('\u{feff}').unwrap_or(source);
    let alloc = Allocator::default();
    let mut warnings = Vec::new();
    let (result, locator) = match crate::parse_with_warnings(&alloc, source, &mut warnings) {
        Ok(component) => {
            let result = analyze_component(&alloc, &component, source, filename, &mut warnings);
            (result, component.locator.clone())
        }
        Err(err) => (Err(err), std::rc::Rc::new(Locator::new(source))),
    };
    let pos = |b: usize| {
        let (line, column, character) = locator.line_column(b);
        Position { line, column, character }
    };
    match result {
        Ok(()) => Ok(warnings
            .into_iter()
            .map(|w| Diagnostic {
                code: w.code,
                message: w.message,
                start: w.position.map(|p| pos(p.0)),
                end: w.position.map(|p| pos(p.1)),
            })
            .collect()),
        Err(err) => Err(Diagnostic {
            code: err.code,
            message: err.message,
            start: err.position.map(|p| pos(p.0)),
            end: err.position.map(|p| pos(p.1)),
        }),
    }
}

/// `ExpressionMetadata` (only what diagnostics depend on)
#[derive(Default, Debug)]
pub(crate) struct ExprMeta {
    pub has_await: bool,
    pub dependencies: Vec<BindingId>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum AstType {
    Module,
    Instance,
    Template,
}

/// The analysis state (zimmerframe `state`), copied when visitors change it
#[derive(Clone, Copy, Debug)]
pub(crate) struct State<'s> {
    pub scope: ScopeId,
    pub ast_type: AstType,
    pub parent_element: Option<&'s str>,
    pub in_declaration_tag: bool,
    /// index into `Analyzer::component_slots`
    pub component_slots: u32,
    /// index into `Analyzer::metas`
    pub expression: Option<u32>,
    /// index into `Analyzer::state_fields`
    pub state_fields: u32,
    pub function_depth: u32,
    /// index into `Analyzer::reactive_statements`
    pub reactive_statement: Option<u32>,
    pub derived_function_depth: i32,
}

pub(crate) struct ReactiveStatement {
    pub assignments: Vec<BindingId>,
    pub dependencies: Vec<BindingId>,
    pub node_start: usize,
    pub node_end: usize,
}

/// A class state field (`ClassBody` visitor)
pub(crate) struct StateField {
    pub name: String,
    pub node_key: usize,
    /// start of the PropertyDefinition / AssignmentExpression
    pub node_start: usize,
    pub is_assignment: bool,
}

pub(crate) struct Analyzer<'s> {
    pub ast: &'s Ast<'s>,
    pub root: &'s Root<'s>,
    pub source: &'s str,
    pub alloc: &'s Allocator,
    pub sc: Scopes<'s>,
    pub path: Vec<P<'s>>,
    pub warnings: &'s mut Vec<Warning>,

    // svelte-ignore
    pub ignore_sets: Vec<FxHashSet<&'s str>>,
    pub ignore_stack: Vec<u32>,
    pub ignore_map: FxHashMap<usize, u32>,

    // analysis
    pub runes: bool,
    pub maybe_runes: bool,
    pub custom_element: bool,
    pub custom_element_props: bool,
    pub name: String,
    pub module_scope: ScopeId,
    pub instance_scope: ScopeId,
    pub template_scope: ScopeId,
    pub module_program: Option<P<'s>>,
    pub instance_program: Option<P<'s>>,
    pub uses_slots: bool,
    pub uses_render_tags: bool,
    pub uses_event_attributes: bool,
    pub event_directive_node: Option<(usize, usize, &'s str)>,
    pub slot_names: Vec<(&'s str, NodeId)>,
    pub snippets: Vec<NodeId>,
    /// render tags / components and whether their snippets are resolved
    pub snippet_renderers: Vec<(NodeId, bool)>,
    /// `node.metadata.snippets` of render tags and components
    pub renderer_snippets: FxHashMap<NodeId, Vec<NodeId>>,
    /// `snippet.metadata.sites`
    pub snippet_sites: FxHashMap<NodeId, Vec<NodeId>>,
    pub elements: Vec<NodeId>,
    /// `node.metadata.path` of elements, render tags and components
    pub node_paths: FxHashMap<NodeId, (u32, u32)>,
    pub path_store: Vec<P<'s>>,
    pub props_id: Option<Id<'s>>,
    pub has_props_rune: bool,
    pub metas: Vec<ExprMeta>,
    pub component_slots: Vec<FxHashSet<String>>,
    pub state_fields: Vec<Vec<StateField>>,
    pub reactive_statements: Vec<ReactiveStatement>,
    /// the per-slot fragments `visit_component` creates: (fragment, nodes)
    pub slot_fragments: Vec<(FragId, Vec<NodeId>)>,
    /// a `<textarea>` fragment whose children were moved into a `value` attribute, while it's visited
    pub emptied_fragment: Option<FragId>,
    /// `<textarea>`s whose children were moved into a `value` attribute
    pub textarea_values: FxHashSet<NodeId>,
    /// `leadingComments` (start, value) of JS nodes, for svelte-ignore
    pub leading_comments: FxHashMap<usize, Vec<(usize, &'s str)>>,
    pub filename: &'s str,
    /// index of the node being visited in its fragment (set by `next`)
    pub child_index: usize,
    /// whether there are any HTML or JS comments (svelte-ignore)
    pub has_comments: bool,
}

impl<'s> Analyzer<'s> {
    pub fn loc(&self, p: P<'s>) -> Loc {
        match p.span(self.ast) {
            Some((s, e)) => Loc::Range(s, e),
            None => Loc::None,
        }
    }

    pub fn node_loc(&self, n: NodeId) -> Loc {
        self.loc(P::Node(n))
    }

    /// `w.xxx(node, ...)`: report a warning unless it is ignored
    pub fn warn(&mut self, node: Option<P<'s>>, w: W) {
        let span = node.and_then(|p| p.span(self.ast));
        self.warn_at(node.map(|p| p.key()), span, w);
    }

    pub fn warn_range(&mut self, start: usize, end: usize, w: W) {
        self.warn_at(None, Some((start, end)), w);
    }

    pub fn warn_id(&mut self, id: Id<'s>, w: W) {
        self.warn_at(Some(id.key), id.loc(), w);
    }

    pub fn warn_at(&mut self, key: Option<usize>, span: Option<(usize, usize)>, w: W) {
        let top = key.and_then(|k| self.ignore_map.get(&k).copied()).or_else(|| self.ignore_stack.last().copied());
        if let Some(top) = top {
            if self.ignore_sets[top as usize].contains(w.code) {
                return;
            }
        }
        self.warnings.push(Warning { code: w.code, message: w.message, position: span });
    }

    pub fn push_ignore(&mut self, ignores: Vec<&'s str>) {
        let mut set: FxHashSet<&'s str> = match self.ignore_stack.last() {
            Some(&top) => self.ignore_sets[top as usize].clone(),
            None => FxHashSet::default(),
        };
        set.extend(ignores);
        self.ignore_sets.push(set);
        self.ignore_stack.push((self.ignore_sets.len() - 1) as u32);
    }

    pub fn pop_ignore(&mut self) {
        self.ignore_stack.pop();
    }

    pub fn get(&self, scope: ScopeId, name: &str) -> Option<BindingId> {
        self.sc.get(scope, name)
    }

    pub fn binding(&self, b: BindingId) -> &scope::Binding<'s> {
        self.sc.binding(b)
    }

    pub fn new_meta(&mut self) -> u32 {
        self.metas.push(ExprMeta::default());
        (self.metas.len() - 1) as u32
    }

    pub fn ty(&self, p: P) -> &'static str {
        p.ty(self.ast)
    }

    /// The nodes of a fragment (or of a per-slot fragment)
    pub fn fragment_nodes(&self, p: P<'s>) -> &[NodeId] {
        match p {
            P::Fragment(f) => &self.ast.fragments[f].nodes,
            P::SlotFragment(i) => &self.slot_fragments[i as usize].1,
            _ => &[],
        }
    }

    /// `[first.start, last.end]` of the `leadingComments` of the root of a template expression:
    /// the comments of its parse that come before it
    pub fn leading_comment_range(&self, e: &crate::ast::Expr<'s>, root_start: usize) -> Option<(usize, usize)> {
        let crate::ast::Expr::Js(js) = e else { return None };
        let ctx = js.comments?;
        let mut range: Option<(usize, usize)> = None;
        for c in &self.root.comments[..ctx.upto as usize] {
            if c.start >= ctx.index as usize && c.start < root_start {
                range = Some((range.map_or(c.start, |r| r.0), c.end));
            }
        }
        range
    }

    /// `node.metadata.path = [...context.path]`
    pub fn save_path(&mut self, n: NodeId) {
        let start = self.path_store.len() as u32;
        self.path_store.extend_from_slice(&self.path);
        self.node_paths.insert(n, (start, self.path.len() as u32));
    }

    pub fn saved_path(&self, n: NodeId) -> &[P<'s>] {
        match self.node_paths.get(&n) {
            Some(&(start, len)) => &self.path_store[start as usize..(start + len) as usize],
            None => &[],
        }
    }

    pub fn element(&self, n: NodeId) -> Option<&'s crate::ast::Element<'s>> {
        match &self.ast.nodes[n] {
            Node::Element(el) => Some(el),
            _ => None,
        }
    }
}

/// `get_component_name(filename)`
fn get_component_name(filename: &str) -> String {
    let mut parts: Vec<&str> = filename.split(['/', '\\']).collect();
    let basename = parts.pop().unwrap_or("");
    let last_dir = parts.last().copied();
    let mut name = basename.replacen(".svelte", "", 1);
    if name == "index" {
        if let Some(dir) = last_dir {
            if !dir.is_empty() && dir != "src" {
                name = dir.to_string();
            }
        }
    }
    let mut chars = name.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

const RESERVED: &[&str] = &["$$props", "$$restProps", "$$slots"];

fn analyze_component<'s>(
    alloc: &'s Allocator,
    component: &'s crate::Component<'s>,
    source: &'s str,
    filename: &'s str,
    warnings: &'s mut Vec<Warning>,
) -> Result<()> {
    let ast = &component.ast;
    let root = &component.root;

    if root.ts {
        ts::check(ast, root)?;
    }

    let mut sc = Scopes::default();
    let module_program = root.module.as_ref().map(|s| P::Js(AstKind::Program(&s.content.program)));
    let instance_program = root.instance.as_ref().map(|s| P::Js(AstKind::Program(&s.content.program)));
    let module = scope::create_scopes(&mut sc, ast, alloc, module_program, false, None)?;
    let instance = scope::create_scopes(&mut sc, ast, alloc, instance_program, true, Some(module.scope))?;
    let template = scope::create_scopes(&mut sc, ast, alloc, Some(P::Fragment(root.fragment)), false, Some(instance.scope))?;

    let options = root.options.as_ref();
    let runes_option: Option<bool> = options.and_then(|o| o.values.get("runes")).and_then(|v| v.as_bool());
    let custom_element_options = options.and_then(|o| o.values.get("customElement"));

    let mut an = Analyzer {
        ast,
        root,
        source,
        alloc,
        sc,
        path: Vec::new(),
        warnings,
        ignore_sets: Vec::new(),
        ignore_stack: Vec::new(),
        ignore_map: FxHashMap::default(),
        runes: false,
        maybe_runes: false,
        custom_element: custom_element_options.is_some(),
        custom_element_props: custom_element_options.and_then(|c| c.get("props")).is_some_and(|p| !p.is_null()),
        name: String::new(),
        module_scope: module.scope,
        instance_scope: instance.scope,
        template_scope: template.scope,
        module_program,
        instance_program,
        uses_slots: false,
        uses_render_tags: false,
        uses_event_attributes: false,
        event_directive_node: None,
        slot_names: Vec::new(),
        snippets: Vec::new(),
        snippet_renderers: Vec::new(),
        renderer_snippets: FxHashMap::default(),
        snippet_sites: FxHashMap::default(),
        elements: Vec::new(),
        node_paths: FxHashMap::default(),
        path_store: Vec::new(),
        props_id: None,
        has_props_rune: false,
        metas: Vec::new(),
        component_slots: Vec::new(),
        state_fields: vec![Vec::new()],
        reactive_statements: Vec::new(),
        slot_fragments: Vec::new(),
        emptied_fragment: None,
        textarea_values: FxHashSet::default(),
        leading_comments: FxHashMap::default(),
        filename,
        child_index: 0,
        has_comments: !root.comments.is_empty() || ast.nodes.iter().any(|n| matches!(n, Node::Comment { .. })),
    };

    comments::attach_all(&mut an);

    // create synthetic bindings for store subscriptions
    let mut legacy_checks: Vec<BindingId> = Vec::new();
    let names: Vec<&'s str> = an.sc.scope(module.scope).references.keys().copied().collect();
    for name in names {
        if !name.starts_with('$') || RESERVED.contains(&name) {
            continue;
        }
        let references = an.sc.scope(module.scope).references[name].clone();
        let first = an.sc.refs[references[0] as usize].node;
        if name == "$" || name.as_bytes().get(1) == Some(&b'$') {
            return Err(e::global_reference_invalid(first.err_loc(), name));
        }
        let store_name = &name[1..];
        let declaration = an.get(instance.scope, store_name);
        let is_rune = utils::is_rune(name).is_some();
        let qualifies = runes_option == Some(false) || !is_rune || {
            match declaration {
                None => false,
                Some(d) => {
                    let init = an.binding(d).initial;
                    let rune = scope::get_rune(&an.sc, init, instance.scope);
                    (rune.is_none() || (store_name != "props" && rune == Some("$props")))
                        && !(name == "$derived"
                            && matches!(init, Some(P::Js(AstKind::ImportDeclaration(i))) if i.source.value == "svelte/store"))
                }
            }
        };
        if !qualifies {
            continue;
        }

        let mut nested: Option<Id> = None;
        'search: for &r in &references {
            let path = an.sc.ref_path(r);
            for p in path.iter().rev() {
                if let Some(&s) = an.sc.map.get(&p.key()) {
                    if let Some(owner) = an.sc.owner(s, store_name) {
                        if owner != module.scope && owner != instance.scope {
                            nested = Some(an.sc.refs[r as usize].node);
                            break 'search;
                        }
                    }
                    break;
                }
            }
        }
        if let Some(node) = nested {
            return Err(e::store_invalid_scoped_subscription(node.err_loc()));
        }

        if runes_option != Some(false) {
            if declaration.is_none() && store_name.as_bytes().first().is_some_and(|b| b.is_ascii_lowercase()) {
                return Err(e::global_reference_invalid(first.err_loc(), name));
            } else if declaration.is_some() && is_rune {
                for &r in &references {
                    let is_call = matches!(an.sc.ref_path(r).last(), Some(P::Js(AstKind::CallExpression(_))));
                    if is_call {
                        let node = an.sc.refs[r as usize].node;
                        an.warn_id(node, warnings::store_rune_conflict(store_name));
                    }
                }
            }
        }

        if let Some(script) = &root.module {
            let (start, end) = (script.content.start, script.content.end);
            for &r in &references {
                let node = an.sc.refs[r as usize].node;
                let Some((s, e)) = node.loc() else { continue };
                if s > start && e < end {
                    let last = an.sc.ref_path(r).last().copied();
                    if scope::get_rune(&an.sc, last, module.scope).is_none() {
                        return Err(e::store_invalid_subscription(node.err_loc()));
                    }
                }
            }
        }

        if let Some(d) = declaration {
            legacy_checks.push(d);
        }

        let id = an.sc.synthetic_id(name);
        let b = an.sc.declare(instance.scope, id, Kind::StoreSub, DeclKind::Synthetic, None)?;
        an.sc.binding_mut(b).references = references.clone();
        an.sc.scopes[instance.scope as usize].references.insert(name, references);
        an.sc.scopes[module.scope as usize].references.shift_remove(name);
    }

    let component_name = get_component_name(filename);

    let runes = runes_option.unwrap_or_else(|| {
        template.has_await
            || instance.has_await
            || an.sc.scope(module.scope).references.keys().any(|k| utils::is_rune(k).is_some())
    });
    an.runes = runes;

    if !runes {
        for d in legacy_checks {
            let b = an.sc.binding_mut(d);
            if b.kind == Kind::Normal && b.declaration_kind == DeclKind::Let && b.reassigned {
                b.kind = Kind::State;
            }
        }
    }

    if runes {
        if let Some(script) = &root.module {
            if let Some(context) = script.attributes.iter().find(|a| a.name() == Some("context")) {
                an.warn(Some(P::Attr(context)), warnings::script_context_deprecated());
            }
        }
    }

    an.name = an.sc.generate(module.scope, &component_name);

    an.maybe_runes = !runes && runes_option != Some(false) && {
        let refs = &an.sc.scope(module.scope).references;
        !refs.contains_key("$$props") && !refs.contains_key("$$restProps")
    } && !instance_body(&an).iter().any(|s| is_legacy_statement(&an, *s));

    if !runes {
        visit::legacy_exports(&mut an);
        visit::legacy_state(&mut an);
    }

    if let Some(o) = options {
        for a in &o.attributes {
            match a.name() {
                Some("accessors") if runes => an.warn(Some(P::Attr(a)), warnings::options_deprecated_accessors()),
                Some("customElement") => an.warn(Some(P::Attr(a)), warnings::options_missing_custom_element()),
                Some("immutable") if runes => an.warn(Some(P::Attr(a)), warnings::options_deprecated_immutable()),
                _ => {}
            }
        }
    }

    if runes {
        if let Some(&r) = an.sc.scope(module.scope).references.get("$$props").map(|r| &r[0]) {
            return Err(e::legacy_props_invalid(an.sc.refs[r as usize].node.err_loc()));
        }
        if let Some(&r) = an.sc.scope(module.scope).references.get("$$restProps").map(|r| &r[0]) {
            return Err(e::legacy_rest_props_invalid(an.sc.refs[r as usize].node.err_loc()));
        }
    } else {
        let id = an.sc.synthetic_id("$$props");
        an.sc.declare(instance.scope, id, Kind::RestProp, DeclKind::Synthetic, None)?;
        let id = an.sc.synthetic_id("$$restProps");
        an.sc.declare(instance.scope, id, Kind::RestProp, DeclKind::Synthetic, None)?;
    }

    for (ast_type, root_p, scope) in [
        (AstType::Module, module_program, module.scope),
        (AstType::Instance, instance_program, instance.scope),
        (AstType::Template, Some(P::Fragment(root.fragment)), template.scope),
    ] {
        let state = State {
            scope,
            ast_type,
            parent_element: None,
            in_declaration_tag: false,
            component_slots: 0,
            expression: None,
            state_fields: 0,
            function_depth: an.sc.scope(scope).function_depth,
            reactive_statement: None,
            derived_function_depth: -1,
        };
        an.has_props_rune = false;
        an.component_slots.push(FxHashSet::default());
        let state = State { component_slots: (an.component_slots.len() - 1) as u32, ..state };
        if let Some(p) = root_p {
            an.visit(p, &state)?;
        }
    }

    if runes {
        visit::non_reactive_updates(&mut an);
    } else {
        visit::export_let_unused(&mut an);
        visit::order_reactive_statements(&an)?;
    }

    visit::module_exports(&an)?;

    if let (Some((start, end, name)), true) = (an.event_directive_node, an.uses_event_attributes) {
        return Err(e::mixed_event_handler_syntaxes((start, end), name));
    }

    // link renderers to their snippets
    for (node, resolved) in std::mem::take(&mut an.snippet_renderers) {
        if !resolved {
            an.renderer_snippets.insert(node, an.snippets.clone());
        }
        for &snippet in an.renderer_snippets.get(&node).map(|v| v.as_slice()).unwrap_or(&[]) {
            let sites = an.snippet_sites.entry(snippet).or_default();
            if !sites.contains(&node) {
                sites.push(node);
            }
        }
    }

    if an.uses_render_tags && (an.uses_slots || (!an.custom_element && !an.slot_names.is_empty())) {
        let pos = match an.slot_names.first() {
            Some(&(_, n)) => an.node_loc(n),
            None => match source.find("$$slot") {
                Some(i) => Loc::At(i),
                None => Loc::At(usize::MAX),
            },
        };
        let pos = match pos {
            Loc::At(usize::MAX) => Loc::None,
            other => other,
        };
        return Err(e::slot_snippet_conflict(pos));
    }

    if let Some(css) = &root.css {
        css::analyze(&mut an, css)?;
    }

    Ok(())
}

fn instance_body<'s>(an: &Analyzer<'s>) -> Vec<P<'s>> {
    match an.instance_program {
        Some(p) => nodes::children(p, an.ast),
        None => Vec::new(),
    }
}

/// `node.type === 'LabeledStatement' || (export let ...)` in the `maybe_runes` computation
fn is_legacy_statement(an: &Analyzer, s: P) -> bool {
    use oxc_ast::ast::*;
    match s {
        P::Js(AstKind::LabeledStatement(_)) => true,
        P::Js(AstKind::ExportDeclaration(d)) => {
            matches!(&d.declaration, Declaration::VariableDeclaration(v) if v.kind == VariableDeclarationKind::Let)
        }
        P::Js(AstKind::ExportNamedDeclaration(d)) => d.specifiers.iter().any(|s| {
            !s.export_kind.is_type()
                && match &s.local {
                    ModuleExportName::IdentifierReference(r) => an
                        .get(an.instance_scope, r.name.as_str())
                        .is_some_and(|b| an.binding(b).declaration_kind == DeclKind::Let),
                    ModuleExportName::IdentifierName(r) => an
                        .get(an.instance_scope, r.name.as_str())
                        .is_some_and(|b| an.binding(b).declaration_kind == DeclKind::Let),
                    _ => false,
                }
        }),
        _ => false,
    }
}

pub(crate) fn attr_name<'a>(a: &'a Attr<'a>) -> &'a str {
    a.name().unwrap_or("")
}
