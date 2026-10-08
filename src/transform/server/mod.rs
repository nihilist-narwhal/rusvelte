//! The server transform (`phases/3-transform/server`): turns an analysed component into the
//! program `compile(..., { generate: 'server' })` prints.
//!
//! The JS parts are walked the way zimmerframe walks them: each visitor gets the original node
//! (with `self.path` holding its ancestors) and returns its replacement; nodes without a
//! visitor get their children visited ([`Server::next`]). Template nodes are visited by
//! [`Server::visit_node`] and friends. `state` is copied like the JS spreads it; the arrays the
//! JS shares between state copies (`init`, `template`) are shared [`Shared`] vectors here.

mod js_visitors;
mod template;
mod utils;

use std::cell::RefCell;
use std::rc::Rc;

use rustc_hash::FxHashMap;

use crate::analyze::nodes::P;
use crate::analyze::scope::{BindingId, Kind, ScopeId};
use crate::analyze::Analyzer;
use crate::estree::builders as b;
use crate::estree::convert::Converter;
use crate::estree::{Node, NodeKind};

use super::js::PathNode;
use super::options::CompileOptions;

/// An array the JS shares between copies of `state`
pub type Shared<T> = Rc<RefCell<Vec<T>>>;

pub fn shared<T>() -> Shared<T> {
    Rc::new(RefCell::new(Vec::new()))
}

/// `async_consts` of the component state
#[derive(Clone)]
pub struct AsyncConsts {
    pub id: String,
    pub thunks: Vec<Node>,
}

/// `ComponentServerTransformState`
#[derive(Clone)]
pub struct State {
    pub scope: ScopeId,
    /// transforming `<script>` (the instance)
    pub is_instance: bool,
    pub namespace: &'static str,
    pub preserve_whitespace: bool,
    pub is_standalone: bool,
    pub init: Shared<Node>,
    pub template: Shared<Node>,
    pub async_consts: Option<Rc<RefCell<AsyncConsts>>>,
    /// index into `Analyzer::state_fields` for the class body being transformed
    pub state_fields: Option<u32>,
}

impl State {
    /// `{ ...state, init: [], template: [] }`
    pub fn with_new_arrays(&self) -> State {
        State { init: shared(), template: shared(), ..self.clone() }
    }
}

pub struct Server<'a, 's> {
    pub an: &'a mut Analyzer<'s>,
    pub options: &'a CompileOptions,
    pub conv: Converter<'a>,
    pub path: Vec<PathNode<'s>>,
    /// `state.hoisted`
    pub hoisted: Vec<Node>,
    /// `state.legacy_reactive_statements`: LabeledStatement start → transformed statement
    pub legacy_reactive_statements: Vec<(u32, Node)>,
    /// `state.filename` (relative to `rootDir`)
    pub filename: String,
    pub dev: bool,
    /// The converted instance script's statements and declarators, by origin
    pub instance_nodes: FxHashMap<usize, Node>,
}

impl<'a, 's> Server<'a, 's> {
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

    pub fn convert_program(&self, p: P<'s>) -> Node {
        match p {
            P::Js(oxc_ast::AstKind::Program(program)) => self.conv.program(program),
            _ => b::program(vec![]),
        }
    }
}

/// Index the statements and declarators of a converted program by origin
fn index_nodes(node: &Node, out: &mut FxHashMap<usize, Node>) {
    match &node.kind {
        NodeKind::Program(p) => {
            for s in &p.body {
                index_statement(s, out);
            }
        }
        _ => {}
    }
}

fn index_statement(s: &Node, out: &mut FxHashMap<usize, Node>) {
    if let Some(o) = s.origin {
        out.insert(o, s.clone());
    }
    match &s.kind {
        NodeKind::VariableDeclaration(d) => {
            for decl in &d.declarations {
                if let Some(o) = decl.origin {
                    out.insert(o, decl.clone());
                }
            }
        }
        NodeKind::ExportNamedDeclaration(e) => {
            if let Some(d) = &e.declaration {
                index_statement(d, out);
            }
        }
        _ => {}
    }
}

/// `server_component(analysis, options)`
pub fn server_component(s: &mut Server) -> Node {
    let module_scope = s.an.module_scope;

    // the instance script, converted once; `instance_body` refers to its statements
    let instance_program = s.an.instance_program.map(|p| s.convert_program(p));
    if let Some(p) = &instance_program {
        let mut index = FxHashMap::default();
        index_nodes(p, &mut index);
        s.instance_nodes = index;
    }

    let mut hoisted = vec![b::import_all("$", "svelte/internal/server")];
    for h in s.an.instance_body.hoisted.clone() {
        if let Some(n) = s.instance_nodes.get(&h.key()) {
            hoisted.push(n.clone());
        }
    }
    s.hoisted = hoisted;

    let base = State {
        scope: module_scope,
        is_instance: false,
        namespace: match s.options.namespace.as_str() {
            "svg" => "svg",
            "mathml" => "mathml",
            _ => "html",
        },
        preserve_whitespace: s.options.preserve_whitespace,
        is_standalone: false,
        init: shared(),
        template: shared(),
        async_consts: None,
        state_fields: None,
    };

    // module
    let module = match s.an.module_program {
        Some(p) => {
            let program = s.convert_program(p);
            s.visit_js(&program, &base)
        }
        None => b::program(vec![]),
    };

    // instance
    let instance_state = State { scope: s.an.instance_scope, is_instance: true, ..base.clone() };
    let instance_program = instance_program.unwrap_or_else(|| b::program(vec![]));
    let instance = s.visit_js(&instance_program, &instance_state);

    // template
    let template_scope = s.scope_of_key(P::Fragment(s.an.root.fragment).key()).unwrap_or(s.an.instance_scope);
    let template_state = State { scope: template_scope, ..base.clone() };
    let template = s.visit_root_fragment(&template_state);

    let mut instance_body = program_body(instance);
    let mut template_body = match template.kind {
        NodeKind::BlockStatement(b) => b.body,
        _ => vec![],
    };

    // legacy reactive statements, in order
    let order: Vec<u32> = s.an.reactive_statements.iter().map(|rs| rs.node_start as u32).collect();
    for start in order {
        if let Some((_, statement)) = s.legacy_reactive_statements.iter().find(|(k, _)| *k == start) {
            instance_body.push(statement.clone());
        }
    }

    // store subscriptions
    let store_subs: Vec<BindingId> = s
        .an
        .sc
        .scope(s.an.instance_scope)
        .declarations
        .values()
        .copied()
        .filter(|&b| s.binding(b).kind == Kind::StoreSub)
        .collect();
    let defer_store_teardown = store_subs.iter().any(|&b| s.binding(b).blocker.is_some());
    if !store_subs.is_empty() {
        instance_body.insert(0, b::var(b::id("$$store_subs"), None));
        let unsubscribe = b::r#if(b::id("$$store_subs"), b::stmt(b::call("$.unsubscribe_stores", vec![b::id("$$store_subs")])), None);
        template_body.push(if defer_store_teardown {
            b::stmt(b::call("$$renderer.on_destroy", vec![b::arrow(vec![], b::block(vec![unsubscribe]))]))
        } else {
            unsubscribe
        });
    }

    // bound props propagate upwards
    let mut props = Vec::new();
    for (name, &bid) in s.an.sc.scope(s.an.instance_scope).declarations.iter() {
        let binding = s.binding(bid);
        if binding.kind == Kind::BindableProp && !name.starts_with("$$") {
            props.push(b::init(binding.prop_alias.unwrap_or(name), b::id(*name)));
        }
    }
    let has_props = !props.is_empty();
    if has_props {
        template_body.push(b::stmt(b::call("$.bind_props", vec![b::id("$$props"), b::object(props)])));
    }

    let mut component_block = instance_body;
    component_block.extend(template_body);
    if let Some(props_id) = s.an.props_id {
        component_block.insert(0, b::r#const(b::id(props_id.name), b::call("$.props_id", vec![b::id("$$renderer")])));
    }
    let mut component_block = b::block(component_block);

    let should_inject_context = s.dev || s.an.needs_context || defer_store_teardown;
    if should_inject_context {
        let mut args = vec![b::arrow(vec![b::id("$$renderer")], component_block)];
        if s.dev {
            args.push(b::id(s.an.name.as_str()));
        }
        component_block = b::block(vec![b::stmt(b::call("$$renderer.component", args))]);
    }

    if s.an.uses_slots {
        if let NodeKind::BlockStatement(bl) = &mut component_block.kind {
            bl.body.insert(0, b::r#const(b::id("$$slots"), b::call("$.sanitize_slots", vec![b::id("$$props")])));
        }
    }

    let mut body = std::mem::take(&mut s.hoisted);
    body.extend(program_body(module));

    let should_inject_props = should_inject_context || has_props || s.an.needs_props() || s.an.uses_slots || !s.an.slot_names.is_empty();
    let params = if should_inject_props { vec![b::id("$$renderer"), b::id("$$props")] } else { vec![b::id("$$renderer")] };
    let component_function = b::function_declaration(b::id(s.an.name.as_str()), params, component_block);

    if s.dev {
        body.push(component_function);
        body.push(b::stmt(b::assignment(
            "=",
            b::member_id(&format!("{}.render", s.an.name)),
            b::r#function(
                None,
                vec![],
                b::block(vec![b::throw_error(
                    "Component.render(...) is no longer valid in Svelte 5. See https://svelte.dev/docs/svelte/v5-migration-guide#Components-are-no-longer-classes for more information",
                )]),
            ),
        )));
        body.push(b::export_default(b::id(s.an.name.as_str())));
        body.insert(
            0,
            b::stmt(b::assignment(
                "=",
                b::member_with(b::id(s.an.name.as_str()), b::id("$.FILENAME"), true, false),
                b::literal(s.filename.as_str()),
            )),
        );
    } else {
        body.push(b::export_default(component_function));
    }

    if s.options.experimental_async {
        body.insert(0, b::imports(&[], "svelte/internal/flags/async"));
    }

    b::program(body)
}

fn program_body(node: Node) -> Vec<Node> {
    match node.kind {
        NodeKind::Program(p) => p.body,
        _ => vec![],
    }
}
