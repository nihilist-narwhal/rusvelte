//! The client transform (`phases/3-transform/client`): turns an analysed component into the
//! program `compile(..., { generate: 'client' })` prints.
//!
//! Visitors follow zimmerframe like the server transform does (see `server/mod.rs`): each
//! visitor gets the original node, with `self.path` holding its ancestors, and returns its
//! replacement. The JS `state` is [`State`], copied where the JS spreads it; the objects the
//! JS shares between state copies (the statement arrays, the template, the memoizer, the
//! `transform` record) are reference-counted here and copied where the JS copies them.
//!
//! `state.transform` maps names to closures (`read`, `assign`, `mutate`, `update`), as in the
//! JS; they get the [`Client`] to call back into the transform.

mod blocks;
mod component;
mod directives;
mod element;
mod fragment;
mod js_visitors;
mod template;
mod utils;

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use rustc_hash::{FxHashMap, FxHashSet};

use crate::analyze::nodes::P;
use crate::analyze::scope::{BindingId, DeclKind, Kind, ScopeId};
use crate::analyze::Analyzer;
use crate::estree::builders as b;
use crate::estree::convert::Converter;
use crate::estree::{Node, NodeKind};

use super::js::PathNode;
use super::options::CompileOptions;

pub use template::Template;
pub use utils::Memoizer;

/// An array the JS shares between copies of `state`
pub type Shared<T> = Rc<RefCell<Vec<T>>>;

pub fn shared<T>() -> Shared<T> {
    Rc::new(RefCell::new(Vec::new()))
}

pub type ReadFn = Rc<dyn Fn(&mut Client, &Node) -> Node>;
pub type AssignFn = Rc<dyn Fn(&mut Client, &Node, Node, bool) -> Node>;
pub type MutateFn = Rc<dyn Fn(&mut Client, &Node, Node) -> Node>;
pub type UpdateFn = Rc<dyn Fn(&mut Client, &Node) -> Node>;

/// An entry of `state.transform`
#[derive(Clone)]
pub struct Transform {
    /// turn `foo` into e.g. `$.get(foo)`
    pub read: ReadFn,
    /// turn `foo = bar` into e.g. `$.set(foo, bar)`
    pub assign: Option<AssignFn>,
    /// turn `foo.bar = baz` into e.g. `$.mutate(foo, $.get(foo).bar = baz);`
    pub mutate: Option<MutateFn>,
    /// turn `foo++` into e.g. `$.update(foo)`
    pub update: Option<UpdateFn>,
}

impl Transform {
    pub fn read(read: ReadFn) -> Transform {
        Transform { read, assign: None, mutate: None, update: None }
    }
}

/// `state.transform`, an object the JS shares between state copies until it spreads it
pub type TransformMap = Rc<RefCell<FxHashMap<String, Transform>>>;

/// `{ ...transform }`
pub fn copy_transform(t: &TransformMap) -> TransformMap {
    Rc::new(RefCell::new(t.borrow().clone()))
}

/// `read: get_value`
pub fn get_value_fn() -> ReadFn {
    Rc::new(|_, node| get_value(node.clone()))
}

/// `read: b.call`
pub fn call_fn() -> ReadFn {
    Rc::new(|_, node| b::call(node.clone(), ()))
}

/// `get_value(node)`: `$.get(node)`
pub fn get_value(node: Node) -> Node {
    b::call("$.get", vec![node])
}

/// `state.async_consts`
#[derive(Clone)]
pub struct AsyncConsts {
    pub id: Node,
    pub thunks: Vec<Node>,
}

/// `ComponentClientTransformState`
#[derive(Clone)]
pub struct State {
    pub scope: ScopeId,
    pub is_instance: bool,
    pub transform: TransformMap,
    pub in_constructor: bool,
    pub in_derived: bool,
    /// `state.state_fields`: index into `Analyzer::state_fields` (0 is the empty map)
    pub state_fields: u32,
    /// the anchor node for the current context
    pub node: Node,
    pub init: Shared<Node>,
    pub update: Shared<Node>,
    pub after_update: Shared<Node>,
    pub snippets: Shared<Node>,
    pub consts: Shared<Node>,
    pub let_directives: Shared<Node>,
    pub template: Rc<RefCell<Template>>,
    pub memoizer: Rc<RefCell<Memoizer>>,
    /// `metadata.namespace`
    pub namespace: &'static str,
    /// `metadata.bound_contenteditable`
    pub bound_contenteditable: bool,
    pub preserve_whitespace: bool,
    pub is_standalone: bool,
    pub async_consts: Rc<RefCell<Option<AsyncConsts>>>,
    pub store_to_invalidate: Option<String>,
}

impl State {
    /// `{ ...state, transform: { ...state.transform } }`
    pub fn with_transform_copy(&self) -> State {
        State { transform: copy_transform(&self.transform), ..self.clone() }
    }
}

pub struct Client<'a, 's> {
    pub an: &'a mut Analyzer<'s>,
    pub options: &'a CompileOptions,
    pub conv: Converter<'a>,
    pub locator: &'a crate::locator::Locator<'a>,
    /// `analysis.css.hash` (empty without `<style>`) and the elements the CSS scopes
    pub css_hash: String,
    pub scoped: FxHashSet<crate::ast::NodeId>,
    pub path: Vec<PathNode<'s>>,
    /// `state.hoisted`
    pub hoisted: Vec<Node>,
    /// `state.templates`: deduplicated templates
    pub templates: FxHashMap<String, String>,
    pub legacy_reactive_imports: Vec<Node>,
    /// `state.legacy_reactive_statements`: LabeledStatement start → transformed statement
    pub legacy_reactive_statements: Vec<(u32, Node)>,
    /// `state.events`, in insertion order
    pub events: Vec<String>,
    pub instance_level_snippets: Vec<Node>,
    pub module_level_snippets: Vec<Node>,
    /// `state.filename` (relative to `rootDir`)
    pub filename: String,
    pub dev: bool,
    /// Elements the analysis gives an empty `class`/`style` attribute
    pub synthetic_class: FxHashSet<crate::ast::NodeId>,
    pub synthetic_style: FxHashSet<crate::ast::NodeId>,
    /// The converted scripts' nodes, by origin (the programs are kept in `programs`)
    pub js_nodes: FxHashMap<usize, *const Node>,
    pub programs: Vec<Box<Node>>,
    /// `node.metadata.is_controlled` (set during the transform)
    pub is_controlled: FxHashSet<crate::ast::NodeId>,
    /// `analysis.needs_mutation_validation`
    pub needs_mutation_validation: bool,
    /// `analysis.needs_props`, which `OnDirective` can set
    pub needs_props: bool,
    /// memoizer placeholder ids → their final names (`$0`, ...)
    pub memo_names: FxHashMap<String, String>,
    pub next_memo: u32,
    /// `get_store()` results of store subscriptions, by store name
    pub store_cache: FxHashMap<String, Node>,
    /// `analysis.immutable` (`runes || options.immutable`)
    pub immutable: bool,
    /// `analysis.accessors`
    pub accessors: bool,
}

impl<'a, 's> Client<'a, 's> {
    pub fn ast(&self) -> &'s crate::ast::Ast<'s> {
        self.an.ast
    }

    /// `scope.get(name)`
    pub fn get(&self, scope: ScopeId, name: &str) -> Option<BindingId> {
        self.an.sc.get(scope, name)
    }

    pub fn binding(&self, b: BindingId) -> &crate::analyze::scope::Binding<'s> {
        self.an.sc.binding(b)
    }

    /// `state.scopes.get(node)`
    pub fn scope_of_key(&self, key: usize) -> Option<ScopeId> {
        self.an.sc.map.get(&key).copied()
    }

    /// `scope.generate(name)`
    pub fn generate(&mut self, scope: ScopeId, name: &str) -> String {
        self.an.sc.generate(scope, name)
    }

    /// `is_state_source(binding, analysis)`
    pub fn is_state_source(&self, b: BindingId) -> bool {
        let binding = self.binding(b);
        matches!(binding.kind, Kind::State | Kind::RawState) && (!self.immutable || binding.reassigned || self.accessors)
    }

    /// `is_prop_source(binding, state)`
    pub fn is_prop_source(&self, b: BindingId) -> bool {
        let binding = self.binding(b);
        matches!(binding.kind, Kind::Prop | Kind::BindableProp)
            && (!self.an.runes || self.accessors || binding.reassigned || binding.initial.is_some() || binding.updated())
    }

    /// `build_getter(node, state)`
    pub fn build_getter(&mut self, node: &Node, st: &State) -> Node {
        let Some(name) = node.identifier_name() else { return node.clone() };
        let t = st.transform.borrow().get(name.as_str()).cloned();
        if let Some(t) = t {
            let binding = self.get(st.scope, name);
            let is_declaration = binding.is_some_and(|b| node.origin.is_some() && node.origin == Some(self.binding(b).node.key));
            if !is_declaration {
                return (t.read)(self, node);
            }
        }
        node.clone()
    }

    /// `get_transform(scope, state)`
    pub fn get_transform(&self, scope: ScopeId, st: &State) -> TransformMap {
        let mut transform = st.transform.borrow().clone();
        for (name, &b) in self.an.sc.scope(scope).declarations.iter() {
            let binding = self.binding(b);
            if binding.kind == Kind::Normal || (binding.kind == Kind::State && !self.is_state_source(b)) {
                transform.remove(*name);
            }
        }
        Rc::new(RefCell::new(transform))
    }

    /// `{ ...state, transform: get_transform(scope, state), scope }`, the `set_scope` visitor
    pub fn with_scope(&self, scope: ScopeId, st: &State) -> State {
        State { transform: self.get_transform(scope, st), scope, ..st.clone() }
    }

    /// `get_prop_source(binding, state, name, initial)`
    pub fn get_prop_source(&self, b: BindingId, name: &str, initial: Option<Node>) -> Node {
        let binding = self.binding(b);
        let mut args = vec![b::id("$$props"), b::literal(name)];
        let mut flags: u32 = 0;
        if binding.kind == Kind::BindableProp {
            flags |= PROPS_IS_BINDABLE;
        }
        if self.immutable {
            flags |= PROPS_IS_IMMUTABLE;
        }
        if self.an.runes {
            flags |= PROPS_IS_RUNES;
        }
        let updated = if self.immutable { binding.reassigned || (self.an.runes && binding.mutated) } else { binding.updated() };
        if self.accessors || updated {
            flags |= PROPS_IS_UPDATED;
        }
        let mut arg = None;
        if let Some(initial) = initial {
            if super::js::is_simple_expression(&initial) {
                arg = Some(initial);
            } else {
                let is_bare_call = matches!(&initial.kind, NodeKind::CallExpression(c) if c.callee.is("Identifier") && c.arguments.is_empty());
                if is_bare_call {
                    let NodeKind::CallExpression(c) = initial.kind else { unreachable!() };
                    arg = Some(*c.callee);
                } else {
                    arg = Some(b::thunk(initial));
                }
                flags |= PROPS_IS_LAZY_INITIAL;
            }
        }
        if flags != 0 || arg.is_some() {
            args.push(b::literal(flags as f64));
            if let Some(a) = arg {
                args.push(a);
            }
        }
        b::call("$.prop", args)
    }

    /// `binding.is_function()`
    pub fn binding_is_function(&self, b: BindingId) -> bool {
        let binding = self.binding(b);
        if binding.updated() {
            return false;
        }
        matches!(binding.initial, Some(P::Js(oxc_ast::AstKind::ArrowFunctionExpression(_) | oxc_ast::AstKind::Function(_))))
    }

    /// `should_proxy(node, scope)` on a transformed expression
    pub fn should_proxy(&self, node: &Node, scope: ScopeId) -> bool {
        match &node.kind {
            NodeKind::Literal(_)
            | NodeKind::TemplateLiteral(_)
            | NodeKind::ArrowFunctionExpression(_)
            | NodeKind::FunctionExpression(_)
            | NodeKind::UnaryExpression(_)
            | NodeKind::BinaryExpression(_) => return false,
            NodeKind::Identifier(i) if i.name == "undefined" => return false,
            _ => {}
        }
        if let NodeKind::Identifier(i) = &node.kind {
            if let Some(b) = self.get(scope, &i.name) {
                let binding = self.binding(b);
                if !binding.reassigned {
                    if let Some(init) = binding.initial {
                        use oxc_ast::AstKind as K;
                        let excluded = match init {
                            P::Js(K::Function(f)) => !f.is_expression(),
                            P::Js(K::Class(c)) => !c.is_expression(),
                            P::Js(K::ImportDeclaration(_)) => true,
                            P::Node(n) => matches!(self.ast().nodes[n], crate::ast::Node::EachBlock { .. } | crate::ast::Node::SnippetBlock { .. }),
                            _ => false,
                        };
                        if !excluded {
                            return self.an.should_proxy(Some(init), None);
                        }
                    }
                }
            }
        }
        true
    }

    /// `scope.evaluate(expression)` on a transformed expression
    pub fn evaluate(&self, node: &Node, scope: ScopeId) -> crate::analyze::evaluate::Evaluation {
        self.an.sc.evaluate_estree(self.an.ast, node, scope)
    }

    /// The converted script node of an analysis node (`binding.initial` and the like)
    pub fn js_node(&self, p: P<'s>) -> Option<Node> {
        // SAFETY: the programs the pointers point into are kept alive (and unmodified) in `programs`
        self.js_nodes.get(&p.key()).map(|&n| unsafe { (*n).clone() })
    }

    /// The memoizer placeholder id `#`, renamed when the memoizer is applied
    pub fn memo_id(&mut self) -> Node {
        self.next_memo += 1;
        b::id(format!("\u{1}{}", self.next_memo))
    }

    /// `is_ignored(node, code)`: a `svelte-ignore` comment covers the node (dev only)
    pub fn is_ignored_key(&self, key: usize, code: &str) -> bool {
        if !self.dev {
            return false;
        }
        self.an.ignore_map.get(&key).is_some_and(|&i| self.an.ignore_sets[i as usize].contains(code))
    }

    pub fn is_ignored(&self, node: &Node, code: &str) -> bool {
        match node.origin {
            Some(o) => self.is_ignored_key(o, code),
            None => false,
        }
    }

    /// `locator(offset)`: 1-based line, 0-based column (UTF-16)
    pub fn locate(&self, offset: usize) -> (usize, usize) {
        let (line, column, _) = self.locator.line_column(offset);
        (line, column)
    }

    /// `locate_node(node)`: `filename:line:column`
    pub fn locate_node(&self, offset: usize) -> String {
        let (line, column) = self.locate(offset);
        format!("{}:{}:{}", crate::analyze::utils::sanitize_location(&self.filename), line, column)
    }

    /// `async_thunk(body, metadata)`
    pub fn async_thunk(&self, body: Node, meta: u32) -> Node {
        if !self.an.metas[meta as usize].has_pickled_await {
            return b::arrow_with(vec![], body, true);
        }
        let block = if body.is("BlockStatement") { body } else { b::block(vec![b::r#return(body)]) };
        b::arrow_with(
            vec![],
            b::block(vec![Node::new(NodeKind::TryStatement(crate::estree::TryStatement {
                block: Box::new(block),
                handler: None,
                finalizer: Some(Box::new(b::block(vec![b::stmt(b::call("$.unsave", ()))]))),
            }))]),
            true,
        )
    }

    /// `create_derived(state, expression, metadata)`
    pub fn create_derived(&self, expression: Node, meta: Option<u32>) -> Node {
        if let Some(m) = meta {
            if self.an.metas[m as usize].has_await {
                return super::js::save(b::call("$.async_derived", vec![self.async_thunk(expression, m)]));
            }
        }
        b::call(if self.an.runes { "$.derived" } else { "$.derived_safe_equal" }, vec![b::thunk(expression)])
    }

    /// `metadata.is_async()`
    pub fn meta_is_async(&self, meta: u32) -> bool {
        self.an.metas[meta as usize].has_await || !self.an.meta_blockers(meta).is_empty()
    }

    /// `metadata.has_blockers()`
    pub fn meta_has_blockers(&self, meta: u32) -> bool {
        !self.an.meta_blockers(meta).is_empty()
    }

    /// `metadata.blockers()`
    pub fn meta_blockers_array(&self, meta: u32) -> Node {
        b::array(self.an.meta_blockers(meta).into_iter().map(blocker_expression).collect::<Vec<_>>())
    }

    /// `node.metadata.expression` of a template node
    pub fn meta_of_node(&self, n: crate::ast::NodeId) -> u32 {
        self.an.meta_of.get(&P::Node(n).key()).copied().unwrap_or(0)
    }

    pub fn meta_of_key(&self, key: usize) -> u32 {
        self.an.meta_of.get(&key).copied().unwrap_or(0)
    }

    pub fn convert_program(&self, p: P<'s>) -> Node {
        match p {
            P::Js(oxc_ast::AstKind::Program(program)) => self.conv.program(program),
            _ => program_node(vec![]),
        }
    }

    /// Keep a converted program alive and index its nodes by origin
    fn register_program(&mut self, program: Node) -> *const Node {
        let boxed = Box::new(program);
        let ptr: *const Node = &*boxed;
        fn index(n: &Node, out: &mut FxHashMap<usize, *const Node>) {
            if let Some(o) = n.origin {
                out.entry(o).or_insert(n as *const Node);
            }
            n.for_each_child(&mut |c| index(c, out));
        }
        index(&boxed, &mut self.js_nodes);
        self.programs.push(boxed);
        ptr
    }
}

pub const PROPS_IS_IMMUTABLE: u32 = 1;
pub const PROPS_IS_RUNES: u32 = 1 << 1;
pub const PROPS_IS_UPDATED: u32 = 1 << 2;
pub const PROPS_IS_BINDABLE: u32 = 1 << 3;
pub const PROPS_IS_LAZY_INITIAL: u32 = 1 << 4;

pub const EACH_ITEM_REACTIVE: u32 = 1;
pub const EACH_INDEX_REACTIVE: u32 = 1 << 1;
pub const EACH_IS_CONTROLLED: u32 = 1 << 2;
pub const EACH_IS_ANIMATED: u32 = 1 << 3;
pub const EACH_ITEM_IMMUTABLE: u32 = 1 << 4;

pub const TEMPLATE_FRAGMENT: u32 = 1;
pub const TEMPLATE_USE_IMPORT_NODE: u32 = 1 << 1;
pub const TEMPLATE_USE_SVG: u32 = 1 << 2;
pub const TEMPLATE_USE_MATHML: u32 = 1 << 3;

pub const TRANSITION_IN: u32 = 1;
pub const TRANSITION_OUT: u32 = 1 << 1;
pub const TRANSITION_GLOBAL: u32 = 1 << 2;

/// `$$promises[i]`
pub fn blocker_expression(bl: crate::analyze::blockers::Blocker) -> Node {
    b::member_with(b::id("$$promises"), b::literal(bl.index as f64), true, false)
}

/// `{ type: 'Program', sourceType: 'module', body }`
pub fn program_node(body: Vec<Node>) -> Node {
    Node::new(NodeKind::Program(crate::estree::Program { body, source_type: crate::estree::SourceType::Module }))
}

fn program_body(node: Node) -> Vec<Node> {
    match node.kind {
        NodeKind::Program(p) => p.body,
        _ => vec![],
    }
}

/// Give the memoizer placeholders their final names
fn rename_memo_ids(node: &mut Node, names: &FxHashMap<String, String>) {
    if let NodeKind::Identifier(i) = &mut node.kind {
        if i.name.starts_with('\u{1}') {
            let name = names.get(i.name.as_str()).cloned().unwrap_or_else(|| "#".into());
            i.name = name.as_str().into();
        }
        return;
    }
    node.for_each_child_mut(&mut |c| rename_memo_ids(c, names));
}

/// A fresh root state (`state` of `client_component`)
pub fn root_state(c: &Client) -> State {
    State {
        scope: c.an.module_scope,
        is_instance: false,
        transform: Rc::new(RefCell::new(FxHashMap::default())),
        in_constructor: false,
        in_derived: false,
        state_fields: 0,
        node: b::id("$$anchor"),
        init: shared(),
        update: shared(),
        after_update: shared(),
        snippets: shared(),
        consts: shared(),
        let_directives: shared(),
        template: Rc::new(RefCell::new(Template::default())),
        memoizer: Rc::new(RefCell::new(Memoizer::default())),
        namespace: match c.options.namespace.as_str() {
            "svg" => "svg",
            "mathml" => "mathml",
            _ => "html",
        },
        bound_contenteditable: false,
        preserve_whitespace: c.options.preserve_whitespace,
        is_standalone: false,
        async_consts: Rc::new(RefCell::new(None)),
        store_to_invalidate: None,
    }
}

/// The custom element options of `<svelte:options customElement={...}>`
pub struct CustomElementOptions {
    pub tag: Option<String>,
    /// `shadow`: `None` = open (default), `Some("none")`, or an object expression's source
    pub shadow: Option<serde_json::Value>,
    pub props: Vec<(String, serde_json::Value)>,
    pub extend: Option<String>,
    pub is_boolean: bool,
}

/// `client_component(analysis, options)`
pub fn client_component(c: &mut Client, inject_css: Option<(String, String)>) -> Node {
    // the scripts, converted once
    let module_program = c.an.module_program.map(|p| c.convert_program(p));
    let instance_program = c.an.instance_program.map(|p| c.convert_program(p));
    let module_ptr = module_program.map(|p| c.register_program(p));
    let instance_ptr = instance_program.map(|p| c.register_program(p));

    let mut hoisted = vec![b::import_all("$", "svelte/internal/client")];
    for h in c.an.instance_body.hoisted.clone() {
        if let Some(n) = c.js_node(h) {
            hoisted.push(n);
        }
    }
    c.hoisted = hoisted;

    let state = root_state(c);

    // SAFETY: the programs are kept alive in `c.programs` and not modified
    let module = match module_ptr {
        Some(p) => {
            let program = unsafe { &*p };
            c.visit_js(program, &state)
        }
        None => program_node(vec![]),
    };

    let instance_state = State {
        transform: copy_transform(&state.transform),
        scope: c.an.instance_scope,
        is_instance: true,
        ..state.clone()
    };
    let instance = match instance_ptr {
        Some(p) => {
            let program = unsafe { &*p };
            c.visit_js(program, &instance_state)
        }
        None => {
            // the JS walks an empty program (the visitor still runs)
            let empty = program_node(vec![]);
            c.visit_js(&empty, &instance_state)
        }
    };
    let instance_loc = instance.loc;

    let template_state = State { transform: instance_state.transform.clone(), scope: c.an.instance_scope, ..state.clone() };
    let template = c.visit_root_fragment(&template_state);

    let mut module_body = program_body(module);
    let imports = std::mem::take(&mut c.legacy_reactive_imports);
    for (i, s) in imports.into_iter().enumerate() {
        module_body.insert(i, s);
    }

    let mut store_setup: Vec<Node> = Vec::new();
    let mut store_init = b::empty();
    let mut legacy_reactive_declarations = Vec::new();
    let mut needs_store_cleanup = false;

    let decls: Vec<(String, BindingId)> =
        c.an.sc.scope(c.an.instance_scope).declarations.iter().map(|(n, b)| (n.to_string(), *b)).collect();
    for (name, bid) in &decls {
        let kind = c.binding(*bid).kind;
        if kind == Kind::LegacyReactive {
            legacy_reactive_declarations.push(b::r#const(
                b::id(name.as_str()),
                b::call("$.mutable_source", vec![None, if c.immutable { Some(b::r#true()) } else { None }]),
            ));
        }
        if kind == Kind::StoreSub {
            if store_setup.is_empty() {
                needs_store_cleanup = true;
                store_init = b::r#const(b::array_pattern(vec![b::id("$$stores"), b::id("$$cleanup")]), b::call("$.setup_stores", ()));
            }
            let store_reference = c.build_getter(&b::id(&name[1..]), &instance_state);
            let store_get = b::call("$.store_get", vec![store_reference.clone(), b::literal(name.as_str()), b::id("$$stores")]);
            let id = binding_id_node(c, *bid);
            store_setup.push(b::r#const(
                id,
                if c.dev {
                    b::thunk(b::sequence(vec![b::call("$.validate_store", vec![store_reference, b::literal(&name[1..])]), store_get]))
                } else {
                    b::thunk(store_get)
                },
            ));
        }
    }

    let mut instance_body = program_body(instance);
    let order: Vec<u32> = c.an.reactive_statements.iter().map(|rs| rs.node_start as u32).collect();
    for start in &order {
        if let Some((_, statement)) = c.legacy_reactive_statements.iter().find(|(k, _)| k == start) {
            instance_body.push(statement.clone());
        }
    }
    if !order.is_empty() {
        instance_body.push(b::stmt(b::call("$.legacy_pre_effect_reset", ())));
    }

    let group_binding_declarations: Vec<Node> =
        c.an.binding_groups.iter().map(|(_, _, name)| b::r#const(b::id(name.as_str()), b::array(Vec::<Node>::new()))).collect();

    // the component's exports
    let mut component_returned_object: Vec<Node> = Vec::new();
    for (name, alias) in c.an.exports.clone() {
        let key = alias.clone().unwrap_or_else(|| name.clone());
        let binding = c.get(c.an.instance_scope, &name);
        let expression = c.build_getter(&b::id(name.as_str()), &instance_state);
        let getter = b::get(&key, vec![b::r#return(expression.clone())]);
        if expression.is("Identifier") {
            let dk = binding.map(|b| c.binding(b).declaration_kind);
            if matches!(dk, Some(DeclKind::Let | DeclKind::Var)) {
                component_returned_object.push(getter);
                component_returned_object.push(b::set(&key, vec![b::stmt(b::assignment("=", expression, b::id("$$value")))]));
                continue;
            } else if !c.dev {
                component_returned_object.push(b::init(&key, expression));
                continue;
            }
        }
        let kind = binding.map(|b| c.binding(b).kind);
        if matches!(kind, Some(Kind::Prop | Kind::BindableProp)) {
            component_returned_object.push(getter);
            component_returned_object.push(b::set(&key, vec![b::stmt(b::call(name.as_str(), vec![b::id("$$value")]))]));
            continue;
        }
        if matches!(kind, Some(Kind::State | Kind::RawState)) {
            let value = if kind == Some(Kind::State) { b::call("$.proxy", vec![b::id("$$value")]) } else { b::id("$$value") };
            component_returned_object.push(getter);
            component_returned_object.push(b::set(&key, vec![b::stmt(b::call("$.set", vec![b::id(name.as_str()), value]))]));
            continue;
        }
        component_returned_object.push(getter);
    }

    let properties: Vec<(String, BindingId)> = decls
        .iter()
        .filter(|(name, b)| matches!(c.binding(*b).kind, Kind::Prop | Kind::BindableProp) && !name.starts_with("$$"))
        .cloned()
        .collect();

    if c.accessors {
        for (name, bid) in &properties {
            let key = c.binding(*bid).prop_alias.map(str::to_string).unwrap_or_else(|| name.clone());
            let getter = b::get(&key, vec![b::r#return(b::call(b::id(name.as_str()), ()))]);
            let mut setter = b::set(&key, vec![b::stmt(b::call(b::id(name.as_str()), vec![b::id("$$value")])), b::stmt(b::call("$.flush", ()))]);
            if c.an.runes {
                if let Some(initial) = c.binding(*bid).initial {
                    if let Some(init) = c.js_node(initial) {
                        // `set foo($$value = expression)`
                        if let NodeKind::Property(p) = &mut setter.kind {
                            if let NodeKind::FunctionExpression(f) = &mut p.value.kind {
                                f.params[0] = b::assignment_pattern(b::id("$$value"), init);
                            }
                        }
                    }
                }
            }
            component_returned_object.push(getter);
            component_returned_object.push(setter);
        }
    }

    if c.options.component_api_4 {
        component_returned_object.push(b::init("$set", b::id("$.update_legacy_props")));
        component_returned_object.push(b::init(
            "$on",
            b::arrow(
                vec![b::id("$$event_name"), b::id("$$event_cb")],
                b::call("$.add_legacy_event_listener", vec![b::id("$$props"), b::id("$$event_name"), b::id("$$event_cb")]),
            ),
        ));
    } else if c.dev {
        component_returned_object.insert(0, b::spread(b::call(b::id("$.legacy_api"), ())));
    }

    let mut push_args = vec![b::id("$$props"), b::literal(c.an.runes)];
    if c.dev {
        push_args.push(b::id(c.an.name.as_str()));
    }

    let mut component_block: Vec<Node> = vec![store_init];
    component_block.extend(legacy_reactive_declarations);
    component_block.extend(group_binding_declarations);

    let should_inject_context = c.dev || c.an.needs_context || !order.is_empty() || !component_returned_object.is_empty();

    component_block.extend(std::mem::take(&mut c.instance_level_snippets));
    component_block.extend(instance_body);

    if should_inject_context && !component_returned_object.is_empty() {
        component_block.push(b::var(b::id("$$exports"), b::object(component_returned_object.clone())));
    }
    for (i, s) in store_setup.into_iter().enumerate() {
        component_block.insert(i, s);
    }

    if !c.an.runes && c.an.needs_context {
        component_block.push(b::stmt(b::call("$.init", vec![if c.immutable { Some(b::r#true()) } else { None }])));
    }

    component_block.extend(match template.kind {
        NodeKind::BlockStatement(bl) => bl.body,
        _ => vec![],
    });

    if c.needs_mutation_validation {
        component_block.insert(0, b::var(b::id("$$ownership_validator"), b::call("$.create_ownership_validator", vec![b::id("$$props")])));
    }

    let should_inject_props = should_inject_context
        || c.needs_props
        || c.an.uses_props
        || c.an.uses_rest_props
        || c.an.uses_slots
        || !c.an.slot_names.is_empty();

    if !c.an.runes {
        for (name, alias) in c.an.exports.clone() {
            let getter = c.build_getter(&b::id(name.as_str()), &instance_state);
            component_block.push(b::stmt(b::call(
                "$.bind_prop",
                vec![b::id("$$props"), b::literal(alias.as_deref().unwrap_or(&name)), getter],
            )));
        }
    }

    if let Some((hash, code)) = inject_css {
        c.hoisted.push(b::r#const(b::id("$$css"), b::object(vec![b::init("hash", b::literal(hash.as_str())), b::init("code", b::literal(code.as_str()))])));
        component_block.insert(0, b::stmt(b::call("$.append_styles", vec![b::id("$$anchor"), b::id("$$css")])));
    }

    if should_inject_context {
        component_block.insert(0, b::stmt(b::call("$.push", push_args)));
        let to_push = if !component_returned_object.is_empty() {
            let pop_call = b::call("$.pop", vec![b::id("$$exports")]);
            if needs_store_cleanup { b::var(b::id("$$pop"), pop_call) } else { b::r#return(pop_call) }
        } else {
            b::stmt(b::call("$.pop", ()))
        };
        component_block.push(to_push);
    }

    if needs_store_cleanup {
        component_block.push(b::stmt(b::call("$$cleanup", ())));
        if !component_returned_object.is_empty() {
            component_block.push(b::r#return(b::id("$$pop")));
        }
    }

    if c.an.uses_rest_props {
        let mut named_props: Vec<String> = c.an.exports.iter().map(|(n, a)| a.clone().unwrap_or_else(|| n.clone())).collect();
        for (name, bid) in &decls {
            let binding = c.binding(*bid);
            if binding.kind == Kind::BindableProp {
                named_props.push(binding.prop_alias.map(str::to_string).unwrap_or_else(|| name.clone()));
            }
        }
        component_block.insert(
            0,
            b::r#const(
                b::id("$$restProps"),
                b::call(
                    "$.legacy_rest_props",
                    vec![b::id("$$sanitized_props"), b::array(named_props.iter().map(|n| b::literal(n.as_str())).collect::<Vec<_>>())],
                ),
            ),
        );
    }

    if c.an.uses_props || c.an.uses_rest_props {
        let mut to_remove = vec![b::literal("children"), b::literal("$$slots"), b::literal("$$events"), b::literal("$$legacy")];
        if c.an.custom_element {
            to_remove.push(b::literal("$$host"));
        }
        component_block.insert(
            0,
            b::r#const(b::id("$$sanitized_props"), b::call("$.legacy_rest_props", vec![b::id("$$props"), b::array(to_remove)])),
        );
    }

    if c.an.uses_slots {
        component_block.insert(0, b::r#const(b::id("$$slots"), b::call("$.sanitize_slots", vec![b::id("$$props")])));
    }

    // imports first, then module-level snippets, the module body and hoisted statements
    let mut imports = Vec::new();
    let mut rest = Vec::new();
    for entry in module_body.into_iter().chain(std::mem::take(&mut c.hoisted)) {
        if entry.is("ImportDeclaration") {
            imports.push(entry);
        } else {
            rest.push(entry);
        }
    }
    let mut body = imports;
    body.extend(std::mem::take(&mut c.module_level_snippets));
    body.extend(rest);

    let mut component_block = b::block(component_block);
    component_block.loc = instance_loc;

    let name = c.an.name.clone();
    if c.options.component_api_4 {
        if let NodeKind::BlockStatement(bl) = &mut component_block.kind {
            bl.body.insert(
                0,
                b::r#if(
                    b::id("new.target"),
                    b::r#return(b::call(
                        "$$_createClassComponent",
                        vec![b::object(vec![b::init("component", b::id(name.as_str())), b::spread(b::id("$$anchor"))])],
                    )),
                    None,
                ),
            );
        }
    } else if c.dev {
        if let NodeKind::BlockStatement(bl) = &mut component_block.kind {
            bl.body.insert(0, b::stmt(b::call("$.check_target", vec![b::id("new.target")])));
        }
    }

    if let Some(props_id) = c.an.props_id {
        let mut id = b::id(props_id.name);
        if let Some((start, end)) = props_id.span {
            id.span = Some(crate::estree::Span::new(start, end));
            id.loc = Some(c.conv.location(oxc_span::Span::new(start, end)));
        }
        if let NodeKind::BlockStatement(bl) = &mut component_block.kind {
            bl.body.insert(0, b::r#const(id, b::call("$.props_id", ())));
        }
    }

    let component = b::function_declaration(
        b::id(name.as_str()),
        if should_inject_props { vec![b::id("$$anchor"), b::id("$$props")] } else { vec![b::id("$$anchor")] },
        component_block,
    );

    if c.options.hmr {
        let id = b::id(name.as_str());
        let mut accept_fn_body =
            vec![b::stmt(b::call(b::member(b::member_with(id.clone(), b::id("$.HMR"), true, false), "update"), vec![b::id("module.default")]))];
        if !c.css_hash.is_empty() {
            accept_fn_body.insert(0, b::stmt(b::call("$.cleanup_styles", vec![b::literal(c.css_hash.as_str())])));
        }
        let hmr = b::block(vec![
            b::stmt(b::assignment("=", id.clone(), b::call("$.hmr", vec![id.clone()]))),
            b::stmt(b::call("import.meta.hot.accept", vec![b::arrow(vec![b::id("module")], b::block(accept_fn_body))])),
        ]);
        body.push(component);
        body.push(b::r#if(b::id("import.meta.hot"), hmr, None));
        body.push(b::export_default(b::id(name.as_str())));
    } else {
        body.push(b::export_default(component));
    }

    if c.dev {
        body.insert(
            0,
            b::stmt(b::assignment("=", b::member_with(b::id(name.as_str()), b::id("$.FILENAME"), true, false), b::literal(c.filename.as_str()))),
        );
    }

    if c.options.experimental_async {
        body.insert(0, b::imports(&[], "svelte/internal/flags/async"));
    }
    if !c.an.runes {
        body.insert(0, b::imports(&[], "svelte/internal/flags/legacy"));
    }
    if c.an.tracing {
        body.insert(0, b::imports(&[], "svelte/internal/flags/tracing"));
    }
    if c.options.disclose_version {
        body.insert(0, b::imports(&[], "svelte/internal/disclose-version"));
    }
    if c.options.component_api_4 {
        body.insert(0, b::imports(&[("createClassComponent", "$$_createClassComponent")], "svelte/legacy"));
    }

    if !c.events.is_empty() {
        body.push(b::stmt(b::call("$.delegate", vec![b::array(c.events.iter().map(|e| b::literal(e.as_str())).collect::<Vec<_>>())])));
    }

    if c.an.custom_element {
        body.push(custom_element_definition(c, &properties));
    }

    let mut program = program_node(body);
    let names = std::mem::take(&mut c.memo_names);
    rename_memo_ids(&mut program, &names);
    program
}

/// The identifier of a binding's declaration (`binding.node`)
fn binding_id_node(c: &Client, b: BindingId) -> Node {
    let binding = c.binding(b);
    let mut id = b::id(binding.node.name);
    if let Some((start, end)) = binding.node.span {
        id.span = Some(crate::estree::Span::new(start, end));
        id.loc = Some(c.conv.location(oxc_span::Span::new(start, end)));
    }
    id.origin = Some(binding.node.key);
    id
}

/// `$.create_custom_element(...)`
fn custom_element_definition(c: &Client, properties: &[(String, BindingId)]) -> Node {
    let ce = c.an.root.options.as_ref().and_then(|o| o.values.get("customElement")).cloned();
    let ce_obj = ce.as_ref().and_then(|v| v.as_object()).cloned();
    let mut props_str = Vec::new();
    let mut ce_prop_keys: Vec<String> = Vec::new();
    if let Some(props) = ce_obj.as_ref().and_then(|o| o.get("props")).and_then(|p| p.as_object()) {
        for (name, def) in props {
            let binding = c.get(c.an.instance_scope, name);
            let key = binding.and_then(|b| c.binding(b).prop_alias).map(str::to_string).unwrap_or_else(|| name.clone());
            let mut ty = def.get("type").and_then(|t| t.as_str()).map(str::to_string);
            if ty.is_none() {
                if let Some(b) = binding {
                    if let Some(P::Js(oxc_ast::AstKind::BooleanLiteral(_))) = c.binding(b).initial {
                        ty = Some("Boolean".into());
                    }
                }
            }
            let mut value = Vec::new();
            if let Some(a) = def.get("attribute").and_then(|a| a.as_str()) {
                value.push(b::init("attribute", b::literal(a)));
            }
            if def.get("reflect").and_then(|r| r.as_bool()) == Some(true) {
                value.push(b::init("reflect", b::r#true()));
            }
            if let Some(t) = ty {
                value.push(b::init("type", b::literal(t.as_str())));
            }
            ce_prop_keys.push(key.clone());
            props_str.push(b::init(&key, b::object(value)));
        }
    }
    for (name, bid) in properties {
        let key = c.binding(*bid).prop_alias.map(str::to_string).unwrap_or_else(|| name.clone());
        if ce_prop_keys.contains(&key) {
            continue;
        }
        props_str.push(b::init(&key, b::object(vec![])));
    }
    let slots_str = b::array(c.an.slot_names.iter().map(|(n, _)| b::literal(*n)).collect::<Vec<_>>());
    let accessors_str = b::array(c.an.exports.iter().map(|(n, a)| b::literal(a.as_deref().unwrap_or(n))).collect::<Vec<_>>());
    let shadow = ce_obj.as_ref().and_then(|o| o.get("shadow")).and_then(|s| s.as_str()).map(str::to_string);
    let shadow_root_init = match shadow.as_deref() {
        None | Some("open") => Some(b::object(vec![b::init("mode", b::literal("open"))])),
        Some("none") => None,
        Some(_) => None,
    };
    let create_ce = b::call(
        "$.create_custom_element",
        vec![Some(b::id(c.an.name.as_str())), Some(b::object(props_str)), Some(slots_str), Some(accessors_str), shadow_root_init, None],
    );
    let tag = ce_obj.as_ref().and_then(|o| o.get("tag")).and_then(|t| t.as_str()).map(str::to_string).or_else(|| ce.as_ref().and_then(|v| v.as_str()).map(str::to_string));
    match tag {
        Some(tag) => {
            let define = b::stmt(b::call("customElements.define", vec![b::literal(tag.as_str()), create_ce]));
            if c.options.hmr {
                b::r#if(b::binary("==", b::call("customElements.get", vec![b::literal(tag.as_str())]), b::null()), define, None)
            } else {
                define
            }
        }
        None => b::stmt(create_ce),
    }
}

/// A cell the transform's closures share (`uses_index` and the like)
pub type Flag = Rc<Cell<bool>>;
