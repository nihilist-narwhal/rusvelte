//! A port of Svelte's analysis phase (`phases/2-analyze`), far enough to reproduce the
//! warnings and errors of `compile(source, { dev: true, generate: false, filename })`.
//!
//! Entry point: [`compile_diagnostics`].

mod a11y;
#[allow(clippy::all)]
mod a11y_data;
mod comments;
pub(crate) mod css;
pub mod blockers;
pub mod evaluate;
pub(crate) mod acorn;
pub(crate) mod nodes;
pub(crate) mod scope;
mod ts;
pub(crate) mod utils;
pub(crate) mod visit;
#[allow(clippy::all)]
pub mod warnings;

use oxc_allocator::Allocator;
use oxc_ast::AstKind;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::ast::{Ast, Attr, FragId, Node, NodeId, Root};
use crate::error::{Loc, Result};
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

/// The compiler options that change which warnings and errors `compile` reports
#[derive(Debug, Clone, Default)]
pub struct CompileOptions {
    /// `runes`: `None` to infer it from the component
    pub runes: Option<bool>,
    /// `customElement`
    pub custom_element: bool,
    /// `experimental.async`
    pub experimental_async: bool,
    /// `namespace` (`html` when `None`); `<svelte:options namespace>` overrides it
    pub namespace: Option<String>,
    /// `name`: the component's name (otherwise derived from the filename)
    pub name: Option<String>,
}

/// The warnings `svelte.compile(source, { dev: true, generate: false, filename })` reports,
/// or the error it throws. `filename` should be the file's basename.
pub fn compile_diagnostics(source: &str, filename: &str) -> std::result::Result<Vec<Diagnostic>, Diagnostic> {
    compile_diagnostics_with(source, filename, &CompileOptions::default())
}

/// [`compile_diagnostics`] with compiler options (`compilerOptions` of svelte.config.js)
pub fn compile_diagnostics_with(
    source: &str,
    filename: &str,
    options: &CompileOptions,
) -> std::result::Result<Vec<Diagnostic>, Diagnostic> {
    let source = source.strip_prefix('\u{feff}').unwrap_or(source);
    let alloc = Allocator::default();
    let mut warnings = Vec::new();
    let (result, locator) = match crate::parse_with_warnings(&alloc, source, &mut warnings) {
        Ok(component) => {
            let result = analyze_component(&alloc, &component, source, filename, options, &mut warnings).map(|_| ());
            (result, component.locator.clone())
        }
        Err(err) => (Err(acorn::reword_parse_error(err, source)), std::rc::Rc::new(Locator::new(source))),
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

/// `node.metadata` of template nodes (the fields code generation reads)
#[derive(Default, Debug, Clone)]
pub struct NodeMeta {
    /// elements: in the SVG / MathML namespace
    pub svg: bool,
    pub mathml: bool,
    pub has_spread: bool,
    /// `<option>{expr}</option>`: the ExpressionTag used as its value
    pub synthetic_value_node: Option<NodeId>,
    /// IfBlock: the else-if blocks folded into this one
    pub flattened: Option<Vec<NodeId>>,
    /// EachBlock
    pub keyed: bool,
    pub contains_group_binding: bool,
    pub transitive_deps: Vec<BindingId>,
    /// SnippetBlock
    pub can_hoist: bool,
    /// RenderTag: the callee isn't a plain (normal) binding
    pub dynamic: bool,
}

/// `attribute.metadata`
#[derive(Default, Debug, Clone, Copy)]
pub struct AttrMeta {
    /// `class={...}` needs `clsx`
    pub needs_clsx: bool,
    /// an event attribute that can be delegated
    pub delegated: bool,
}

/// `bind:` directive metadata
#[derive(Default, Debug, Clone)]
pub struct BindMeta {
    /// `metadata.binding` (unset for `bind:group`, whose metadata Svelte replaces)
    pub binding: Option<BindingId>,
    /// `bind:group`: the group's name and the each blocks contributing to it (innermost first)
    pub binding_group_name: Option<String>,
    pub parent_each_blocks: Vec<NodeId>,
}

/// `ExpressionMetadata`
#[derive(Default, Debug, Clone)]
pub struct ExprMeta {
    /// references state directly, or might (via member/call expressions)
    pub has_state: bool,
    /// involves a call expression
    pub has_call: bool,
    pub has_await: bool,
    /// an `await` restores the reaction context afterwards
    pub has_pickled_await: bool,
    pub has_member_expression: bool,
    /// includes an assignment or an update
    pub has_assignment: bool,
    /// bindings referenced eagerly (not inside functions), in insertion order
    pub dependencies: Vec<BindingId>,
    pub references: Vec<BindingId>,
}

impl ExprMeta {
    /// `merge(other)`
    pub fn merge(&mut self, other: &ExprMeta) {
        self.has_state |= other.has_state;
        self.has_call |= other.has_call;
        self.has_await |= other.has_await;
        self.has_pickled_await |= other.has_pickled_await;
        self.has_member_expression |= other.has_member_expression;
        self.has_assignment |= other.has_assignment;
        for d in &other.dependencies {
            if !self.dependencies.contains(d) {
                self.dependencies.push(*d);
            }
        }
        for r in &other.references {
            if !self.references.contains(r) {
                self.references.push(*r);
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum AstType {
    Module,
    Instance,
    Template,
    /// `null`: a `.svelte.js` module (`analyze_module`)
    None,
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
    /// `derived_function_depth` (-1 outside `$derived`/`{@const}`)
    pub derived_function_depth: i64,
    /// index into `Analyzer::reactive_statements`
    pub reactive_statement: Option<u32>,
    /// `async_consts` of the fragment being visited: index into `Analyzer::async_runs`
    pub async_consts: u32,
}

/// `state.async_consts` of the analysis (`{ id, declaration_count }`)
#[derive(Clone, Copy, Debug)]
pub(crate) struct AsyncRun {
    /// index into `Analyzer::promise_ids`
    pub id: u32,
    pub declaration_count: u32,
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
    /// `type`: the rune (`$state`, `$state.raw`, `$derived`, `$derived.by`)
    pub rune: &'static str,
    /// `key`: the name of the private backing field (without `#`)
    pub key: String,
    /// `value`: the rune call
    pub value_key: usize,
}

/// `scope.tracing`: the label of `$inspect.trace(...)`
#[derive(Clone, Debug)]
pub enum Tracing {
    /// the trace's argument (its key)
    Expression(usize),
    /// `label (location)`: the label and the start of the function (located by the transform)
    Label { label: String, start: usize },
}

pub(crate) struct Analyzer<'s> {
    pub ast: &'s Ast<'s>,
    pub root: &'s Root<'s>,
    pub source: &'s str,
    pub alloc: &'s Allocator,
    pub sc: Scopes<'s>,
    pub path: Vec<P<'s>>,
    pub warnings: &'s mut Vec<Warning>,
    /// How many warnings were emitted before `state.adjust` makes the filename relative to
    /// `rootDir` (the earlier ones keep the filename as given)
    pub warnings_before_adjust: usize,

    // svelte-ignore
    pub ignore_sets: Vec<FxHashSet<&'s str>>,
    pub ignore_stack: Vec<u32>,
    pub ignore_map: FxHashMap<usize, u32>,

    // analysis
    pub runes: bool,
    pub maybe_runes: bool,
    pub custom_element: bool,
    pub experimental_async: bool,
    /// the component's namespace (`options.namespace` or `<svelte:options namespace>`)
    pub namespace: &'static str,
    pub custom_element_props: bool,
    pub name: String,
    pub module_scope: ScopeId,
    pub instance_scope: ScopeId,
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
    /// `bind:` directive metadata (by attribute address)
    pub bind_meta: FxHashMap<usize, BindMeta>,
    /// `analysis.binding_groups`: (keypath, bindings) → group name
    pub binding_groups: Vec<(String, Vec<Option<BindingId>>, String)>,
    /// `attribute.metadata` (by attribute address)
    pub attr_meta: FxHashMap<usize, AttrMeta>,
    /// `node.metadata` of template nodes
    pub node_meta: FxHashMap<NodeId, NodeMeta>,
    /// `fragment.metadata.dynamic` (by fragment id)
    pub fragment_dynamic: FxHashSet<usize>,
    /// `analysis.instance_body`
    pub instance_body: blockers::InstanceBody<'s>,
    /// `analysis.needs_props`, `uses_props`, `uses_rest_props`, `uses_component_bindings`
    pub needs_props: bool,
    pub uses_props: bool,
    pub uses_rest_props: bool,
    pub uses_component_bindings: bool,
    /// `analysis.exports` (legacy): (name, alias)
    pub exports: Vec<(String, Option<String>)>,
    /// `analysis.needs_context`
    pub needs_context: bool,
    /// `analysis.pickled_awaits` (AwaitExpression keys)
    pub pickled_awaits: FxHashSet<usize>,
    /// `analysis.async_deriveds`: `$derived(...)` call key → its metadata
    pub async_deriveds: Vec<(usize, u32)>,
    /// node key → its `metadata.expression` (index into `metas`)
    pub meta_of: FxHashMap<usize, u32>,
    pub component_slots: Vec<FxHashSet<String>>,
    pub state_fields: Vec<Vec<StateField>>,
    /// `analysis.classes`: ClassBody key → index into `state_fields`
    pub classes: FxHashMap<usize, u32>,
    /// `binding.metadata.exclude_props` of rest props (by `binding.node.key`)
    pub exclude_props: FxHashMap<usize, Vec<String>>,
    /// `binding.legacy_indirect_bindings`
    pub legacy_indirect_bindings: FxHashMap<BindingId, Vec<BindingId>>,
    /// `scope.tracing`
    pub scope_tracing: FxHashMap<ScopeId, Tracing>,
    /// `analysis.tracing`
    pub tracing: bool,
    /// the `async_consts` of each fragment visit (see `State::async_consts`)
    pub async_runs: Vec<Option<AsyncRun>>,
    /// the names of the `promises` ids of async `{@const}`/declaration tags
    pub promise_ids: Vec<String>,
    /// `metadata.promises_id` of `{@const}` and declaration tags: index into `promise_ids`
    pub promises_id: FxHashMap<NodeId, u32>,
    /// the next blocker identity
    pub next_blocker: u32,
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

    /// `node.metadata`, created on first use
    pub fn node_meta_mut(&mut self, n: NodeId) -> &mut NodeMeta {
        self.node_meta.entry(n).or_default()
    }

    /// `mark_subtree_dynamic(context.path)`
    pub fn mark_subtree_dynamic(&mut self) {
        for i in (0..self.path.len()).rev() {
            let key = match self.path[i] {
                P::Fragment(f) => f,
                // the slot fragments share the component fragment's `metadata`
                P::SlotFragment(s) => self.slot_fragments[s as usize].0,
                _ => continue,
            };
            if !self.fragment_dynamic.insert(key) {
                return;
            }
        }
    }

    /// The blockers of an expression's references (`#get_blockers`), deduplicated by identity
    pub fn meta_blockers(&self, m: u32) -> Vec<blockers::Blocker> {
        let mut out: Vec<blockers::Blocker> = Vec::new();
        for &r in &self.metas[m as usize].references {
            if let Some(b) = self.binding(r).blocker {
                if !out.contains(&b) {
                    out.push(b);
                }
            }
        }
        out
    }

    /// A new `ExpressionMetadata`, as `owner.metadata.expression`
    pub fn new_meta(&mut self, owner: P<'s>) -> u32 {
        self.metas.push(ExprMeta::default());
        let id = (self.metas.len() - 1) as u32;
        self.meta_of.insert(owner.key(), id);
        id
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

impl<'s> Analyzer<'s> {
    /// An analyzer with nothing analysed yet
    pub(crate) fn blank(
        ast: &'s Ast<'s>,
        root: &'s Root<'s>,
        source: &'s str,
        alloc: &'s Allocator,
        sc: Scopes<'s>,
        warnings: &'s mut Vec<Warning>,
        filename: &'s str,
    ) -> Analyzer<'s> {
        Analyzer {
            ast,
            root,
            source,
            alloc,
            sc,
            path: Vec::new(),
            warnings,
            warnings_before_adjust: 0,
            ignore_sets: Vec::new(),
            ignore_stack: Vec::new(),
            ignore_map: FxHashMap::default(),
            runes: false,
            maybe_runes: false,
            custom_element: false,
            experimental_async: false,
            namespace: "html",
            custom_element_props: false,
            name: String::new(),
            module_scope: 0,
            instance_scope: 0,
            module_program: None,
            instance_program: None,
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
            needs_context: false,
            needs_props: false,
            uses_props: false,
            uses_rest_props: false,
            uses_component_bindings: false,
            exports: Vec::new(),
            instance_body: blockers::InstanceBody::default(),
            node_meta: FxHashMap::default(),
            attr_meta: FxHashMap::default(),
            bind_meta: FxHashMap::default(),
            binding_groups: Vec::new(),
            fragment_dynamic: FxHashSet::default(),
            pickled_awaits: FxHashSet::default(),
            async_deriveds: Vec::new(),
            meta_of: FxHashMap::default(),
            component_slots: Vec::new(),
            state_fields: vec![Vec::new()],
            classes: FxHashMap::default(),
            exclude_props: FxHashMap::default(),
            legacy_indirect_bindings: FxHashMap::default(),
            scope_tracing: FxHashMap::default(),
            tracing: false,
            async_runs: vec![None],
            promise_ids: Vec::new(),
            promises_id: FxHashMap::default(),
            next_blocker: 1 << 20,
            reactive_statements: Vec::new(),
            slot_fragments: Vec::new(),
            emptied_fragment: None,
            textarea_values: FxHashSet::default(),
            leading_comments: FxHashMap::default(),
            filename,
            child_index: 0,
            has_comments: !root.comments.is_empty() || ast.nodes.iter().any(|n| matches!(n, Node::Comment { .. })),
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

/// What `analyze_component` returns (the parts of `ComponentAnalysis` ported so far)
pub(crate) struct ComponentAnalysis<'s> {
    /// `get_component_name(options.filename)`, which `cssHash` gets as `name`
    pub component_name: String,
    pub custom_element: bool,
    /// The CSS analysis, when there's a `<style>`
    pub css: Option<css::Meta<'s>>,
    /// `analysis.css.has_global`
    pub css_has_global: bool,
    /// Everything else the analysis computed (scopes, bindings, metadata), for code generation
    pub an: Analyzer<'s>,
}

pub(crate) fn analyze_component<'s>(
    alloc: &'s Allocator,
    component: &'s crate::Component<'s>,
    source: &'s str,
    filename: &'s str,
    compile_options: &CompileOptions,
    warnings: &'s mut Vec<Warning>,
) -> Result<ComponentAnalysis<'s>> {
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
    // `'runes' in parsed_options ? parsed_options.runes : options.runes`
    let runes_option: Option<bool> = match options.and_then(|o| o.values.get("runes")) {
        Some(v) => v.as_bool(),
        None => compile_options.runes,
    };
    let custom_element_options = options.and_then(|o| o.values.get("customElement"));

    let mut an = Analyzer::blank(ast, root, source, alloc, sc, warnings, filename);
    an.custom_element = custom_element_options.is_some() || compile_options.custom_element;
    an.experimental_async = compile_options.experimental_async;
    an.namespace = match root.options.as_ref().and_then(|o| o.values.get("namespace")).and_then(|v| v.as_str()).or(compile_options.namespace.as_deref()) {
        Some("svg") => "svg",
        Some("mathml") => "mathml",
        _ => "html",
    };
    an.custom_element_props = custom_element_options.and_then(|c| c.get("props")).is_some_and(|p| !p.is_null());
    an.module_scope = module.scope;
    an.instance_scope = instance.scope;
    an.module_program = module_program;
    an.instance_program = instance_program;

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

    an.name = an.sc.generate(module.scope, compile_options.name.as_deref().unwrap_or(&component_name));
    // `state.adjust(...)`: later warnings get the filename relative to `rootDir`
    an.warnings_before_adjust = an.warnings.len();

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
                Some("customElement") if !compile_options.custom_element => {
                    an.warn(Some(P::Attr(a)), warnings::options_missing_custom_element())
                }
                Some("immutable") if runes => an.warn(Some(P::Attr(a)), warnings::options_deprecated_immutable()),
                _ => {}
            }
        }
    }

    blockers::calculate_blockers(&mut an);

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
            derived_function_depth: -1,
            reactive_statement: None,
            async_consts: 0,
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
        visit::order_reactive_statements(&mut an)?;
    }

    visit::module_exports(&an)?;
    let exports_snippet = exports_snippet(&an);

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

    let css = match &root.css {
        Some(css) => Some(css::analyze(&mut an, css)?),
        None => None,
    };

    // scoped custom elements get their class through properties, set at runtime
    if let Some(meta) = &css {
        for n in an.elements.clone() {
            if meta.scoped_elements.contains(&n) && an.is_custom_element_node(n) {
                let path = an.saved_path(n).to_vec();
                for p in path.into_iter().rev() {
                    let key = match p {
                        P::Fragment(f) => f,
                        P::SlotFragment(s) => an.slot_fragments[s as usize].0,
                        _ => continue,
                    };
                    if !an.fragment_dynamic.insert(key) {
                        break;
                    }
                }
            }
        }
    }

    Ok(ComponentAnalysis {
        css_has_global: exports_snippet || css.as_ref().is_some_and(|c| c.has_global),
        component_name,
        custom_element: an.custom_element,
        css,
        an,
    })
}

/// `analyze_module(source, options)`: the analysis of a `.svelte.js` module. `component` is
/// the module wrapped as a component without template, its program as the module script.
pub(crate) fn analyze_module<'s>(
    alloc: &'s Allocator,
    component: &'s crate::Component<'s>,
    source: &'s str,
    filename: &'s str,
    compile_options: &CompileOptions,
    warnings: &'s mut Vec<Warning>,
) -> Result<Analyzer<'s>> {
    let ast = &component.ast;
    let root = &component.root;
    let script = root.module.as_ref().expect("a module program");
    let program = P::Js(AstKind::Program(&script.content.program));

    let mut sc = Scopes::default();
    let created = scope::create_scopes(&mut sc, ast, alloc, Some(program), false, None)?;
    let scope = created.scope;

    for (name, references) in sc.scope(scope).references.iter() {
        let name: &str = name;
        if !name.starts_with('$') || RESERVED.contains(&name) {
            continue;
        }
        let first = sc.refs[references[0] as usize].node;
        if name == "$" || name.as_bytes().get(1) == Some(&b'$') {
            return Err(e::global_reference_invalid(first.err_loc(), name));
        }
        if sc.get(scope, &name[1..]).is_some() && utils::is_rune(name).is_none() {
            return Err(e::store_invalid_subscription_module(first.err_loc()));
        }
    }

    let mut an = Analyzer::blank(ast, root, source, alloc, sc, warnings, filename);
    an.experimental_async = compile_options.experimental_async;
    an.module_scope = scope;
    // there is no instance; the visitors only look at it for `ast_type === 'instance'`
    an.instance_scope = scope;
    an.module_program = Some(program);
    an.runes = true;
    an.name = filename.to_string();
    // `state.adjust(...)` (see `analyze_component`)
    an.warnings_before_adjust = an.warnings.len();

    comments::attach_all(&mut an);

    an.component_slots.push(FxHashSet::default());
    let state = State {
        scope,
        ast_type: AstType::None,
        parent_element: None,
        in_declaration_tag: false,
        component_slots: (an.component_slots.len() - 1) as u32,
        expression: None,
        state_fields: 0,
        function_depth: 0,
        derived_function_depth: -1,
        reactive_statement: None,
        async_consts: 0,
    };
    an.visit(program, &state)?;
    Ok(an)
}

/// Whether the module script exports a snippet (which sets `analysis.css.has_global`, so that
/// bundlers keep the CSS when only the snippet is imported)
fn exports_snippet(an: &Analyzer) -> bool {
    use oxc_ast::ast::ModuleExportName;
    let Some(program) = an.module_program else { return false };
    nodes::children(program, an.ast).into_iter().any(|s| {
        let P::Js(AstKind::ExportNamedDeclaration(d)) = s else { return false };
        d.specifiers.iter().any(|spec| {
                let name = match &spec.local {
                    ModuleExportName::IdentifierReference(r) => r.name.as_str(),
                    ModuleExportName::IdentifierName(r) => r.name.as_str(),
                    ModuleExportName::StringLiteral(_) => return false,
                };
                an.get(an.module_scope, name).is_some_and(|b| {
                    matches!(an.binding(b).initial, Some(P::Node(n)) if matches!(an.ast.nodes[n], Node::SnippetBlock { .. }))
                })
            })
    })
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
