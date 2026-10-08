//! The server transform (`phases/3-transform/server`): turns an analysed component into the
//! program `compile(..., { generate: 'server' })` prints.
//!
//! The JS parts are walked the way zimmerframe walks them: each visitor gets the original node
//! (with `self.path` holding its ancestors) and returns its replacement; nodes without a
//! visitor get their children visited ([`Server::next`]). Template nodes are visited by
//! [`Server::visit_node`] and friends. `state` is copied like the JS spreads it; the arrays the
//! JS shares between state copies (`init`, `template`) are shared [`Shared`] vectors here.

mod component;
mod element;
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
    /// `state.async_consts`: set (`??=`) on the state object of the fragment or element that
    /// owns the `{@const}` tags, so it's a cell shared by the copies made after it
    pub async_consts: Rc<RefCell<Option<AsyncConsts>>>,
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
    /// A compile error the transform raises (`const_tag_cycle`); the first one wins
    pub error: std::cell::RefCell<Option<crate::error::CompileError>>,
    pub an: &'a mut Analyzer<'s>,
    pub options: &'a CompileOptions,
    pub conv: Converter<'a>,
    pub locator: &'a crate::locator::Locator<'a>,
    /// `analysis.css.hash` and the elements the CSS scopes
    pub css_hash: String,
    pub scoped: rustc_hash::FxHashSet<crate::ast::NodeId>,
    pub path: Vec<PathNode<'s>>,
    /// `state.hoisted`
    pub hoisted: Vec<Node>,
    /// `state.legacy_reactive_statements`: LabeledStatement start → transformed statement
    pub legacy_reactive_statements: Vec<(u32, Node)>,
    /// `state.filename` (relative to `rootDir`)
    pub filename: String,
    pub dev: bool,
    /// Elements the analysis gives an empty `class`/`style` attribute (appended to their attributes)
    pub synthetic_class: rustc_hash::FxHashSet<crate::ast::NodeId>,
    pub synthetic_style: rustc_hash::FxHashSet<crate::ast::NodeId>,
    /// Names of the functions snippets became (the JS marks them `___snippet`)
    pub snippet_fns: Vec<String>,
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
            P::Js(oxc_ast::AstKind::Program(program)) => {
                let mut node = self.conv.program(program);
                node.loc = crate::transform::script_program_loc(&self.conv, self.an.root, program).or(node.loc);
                node
            }
            _ => program_node(vec![]),
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
pub fn server_component(s: &mut Server, inject_css: Option<(String, String)>) -> Node {
    let module_scope = s.an.module_scope;

    // the instance script, converted once; `instance_body` refers to its statements
    let instance_program = s.an.instance_program.map(|p| s.convert_program(p));
    if let Some(p) = &instance_program {
        let mut index = FxHashMap::default();
        index_nodes(p, &mut index);
        s.instance_nodes = index;
    }

    // the assignments of legacy reactive statements (`$: x = ...`), by statement start
    let mut reactive_assignments: Vec<(usize, Vec<Node>)> = Vec::new();
    if let Some(NodeKind::Program(p)) = instance_program.as_ref().map(|p| &p.kind) {
        for statement in &p.body {
            let NodeKind::LabeledStatement(l) = &statement.kind else { continue };
            let NodeKind::ExpressionStatement(e) = &l.body.kind else { continue };
            let NodeKind::AssignmentExpression(a) = &e.expression.kind else { continue };
            let start = statement.span.map_or(0, |s| s.start as usize);
            reactive_assignments.push((start, super::js::extract_identifiers(&a.left).into_iter().cloned().collect()));
        }
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
        async_consts: Default::default(),
        state_fields: None,
    };

    // module
    let module = match s.an.module_program {
        Some(p) => {
            let program = s.convert_program(p);
            s.visit_js(&program, &base)
        }
        None => program_node(vec![]),
    };

    // instance
    let instance_state = State { scope: s.an.instance_scope, is_instance: true, ..base.clone() };
    let instance_program = instance_program.unwrap_or_else(|| program_node(vec![]));
    let instance = s.visit_js(&instance_program, &instance_state);
    let instance_loc = instance.loc;

    // template
    let template_scope = s.scope_of_key(P::Fragment(s.an.root.fragment).key()).unwrap_or(s.an.instance_scope);
    let template_state = State { scope: template_scope, ..base.clone() };
    let template = s.visit_root_fragment(&template_state);

    let mut instance_body = program_body(instance);
    let mut template_body = match template.kind {
        NodeKind::BlockStatement(b) => b.body,
        _ => vec![],
    };

    // bindings to child components: re-render until they're stable (legacy)
    if s.an.uses_component_bindings {
        let is_snippet = |n: &Node| match &n.kind {
            NodeKind::FunctionDeclaration(f) => f.id.as_deref().and_then(super::js::ident).is_some_and(|name| s.snippet_fns.iter().any(|x| x == name)),
            _ => false,
        };
        let (snippets, rest): (Vec<Node>, Vec<Node>) = template_body.into_iter().partition(|n| is_snippet(n));
        let mut body = snippets;
        body.push(b::r#let(b::id("$$settled"), b::r#true()));
        body.push(b::r#let(b::id("$$inner_renderer"), None));
        body.push(b::function_declaration(b::id("$$render_inner"), vec![b::id("$$renderer")], b::block(rest)));
        body.push(b::do_while(
            b::unary("!", b::id("$$settled")),
            b::block(vec![
                b::stmt(b::assignment("=", b::id("$$settled"), b::r#true())),
                b::stmt(b::assignment("=", b::id("$$inner_renderer"), b::call("$$renderer.copy", ()))),
                b::stmt(b::call("$$render_inner", vec![b::id("$$inner_renderer")])),
            ]),
        ));
        body.push(b::stmt(b::call("$$renderer.subsume", vec![b::id("$$inner_renderer")])));
        template_body = body;
    }

    // legacy reactive statements, in order
    let mut legacy_reactive_declarations = Vec::new();
    let order: Vec<u32> = s.an.reactive_statements.iter().map(|rs| rs.node_start as u32).collect();
    for start in order {
        if let Some((_, ids)) = reactive_assignments.iter().find(|(k, _)| *k == start as usize) {
            for id in ids {
                let name = super::js::ident(id).unwrap_or("");
                if s.an.sc.get(s.an.instance_scope, name).is_some_and(|b| s.binding(b).kind == Kind::LegacyReactive) {
                    legacy_reactive_declarations.push(b::declarator(id.clone(), None));
                }
            }
        }
        if let Some((_, statement)) = s.legacy_reactive_statements.iter().find(|(k, _)| *k == start) {
            instance_body.push(statement.clone());
        }
    }
    if !legacy_reactive_declarations.is_empty() {
        instance_body.insert(0, b::declaration("let", legacy_reactive_declarations));
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
    for (name, alias) in s.an.exports.clone() {
        props.push(b::init(alias.as_deref().unwrap_or(&name), b::id(name.as_str())));
    }
    let has_props = !props.is_empty();
    if has_props {
        template_body.push(b::stmt(b::call("$.bind_props", vec![b::id("$$props"), b::object(props)])));
    }

    let mut component_block = instance_body;
    component_block.extend(template_body);
    if let Some(props_id) = s.an.props_id {
        // the declaration's own identifier (with its position, so comments land around it)
        let mut id = b::id(props_id.name);
        if let Some((start, end)) = props_id.span {
            let span = oxc_span::Span::new(start, end);
            id.span = Some(crate::estree::Span::new(start, end));
            id.loc = Some(s.conv.location(span));
        }
        component_block.insert(0, b::r#const(id, b::call("$.props_id", vec![b::id("$$renderer")])));
    }
    let mut component_block = b::block(component_block);
    // trick esrap into including comments
    component_block.loc = instance_loc;

    let should_inject_context = s.dev || s.an.needs_context || defer_store_teardown;
    if should_inject_context {
        let mut args = vec![b::arrow(vec![b::id("$$renderer")], component_block)];
        if s.dev {
            args.push(b::id(s.an.name.as_str()));
        }
        component_block = b::block(vec![b::stmt(b::call("$$renderer.component", args))]);
    }

    let NodeKind::BlockStatement(block) = &mut component_block.kind else { unreachable!() };
    if s.an.uses_rest_props {
        let mut named_props: Vec<String> = s.an.exports.iter().map(|(name, alias)| alias.clone().unwrap_or_else(|| name.clone())).collect();
        for (name, &bid) in s.an.sc.scope(s.an.instance_scope).declarations.iter() {
            let binding = s.binding(bid);
            if binding.kind == Kind::BindableProp {
                named_props.push(binding.prop_alias.unwrap_or(name).to_string());
            }
        }
        block.body.insert(
            0,
            b::r#const(
                b::id("$$restProps"),
                b::call("$.rest_props", vec![b::id("$$sanitized_props"), b::array(named_props.iter().map(|n| b::literal(n.as_str())).collect::<Vec<_>>())]),
            ),
        );
    }
    if s.an.uses_props || s.an.uses_rest_props {
        block.body.insert(0, b::r#const(b::id("$$sanitized_props"), b::call("$.sanitize_props", vec![b::id("$$props")])));
    }
    if s.an.uses_slots {
        block.body.insert(0, b::r#const(b::id("$$slots"), b::call("$.sanitize_slots", vec![b::id("$$props")])));
    }

    let mut body = std::mem::take(&mut s.hoisted);
    body.extend(program_body(module));

    if let Some((hash, code)) = inject_css {
        body.push(b::r#const(b::id("$$css"), b::object(vec![b::init("hash", b::literal(hash.as_str())), b::init("code", b::literal(code.as_str()))])));
        block.body.insert(0, b::stmt(b::call("$$renderer.global.css.add", vec![b::id("$$css")])));
    }

    let should_inject_props = should_inject_context
        || has_props
        || s.an.needs_props
        || s.an.uses_props
        || s.an.uses_rest_props
        || s.an.uses_slots
        || !s.an.slot_names.is_empty();
    let params = if should_inject_props { vec![b::id("$$renderer"), b::id("$$props")] } else { vec![b::id("$$renderer")] };
    let component_function = b::function_declaration(b::id(s.an.name.as_str()), params, component_block);

    if s.options.component_api_4 {
        body.insert(0, b::imports(&[("render", "$$_render")], "svelte/server"));
        body.push(component_function);
        body.push(b::stmt(b::assignment(
            "=",
            b::member_id(&format!("{}.render", s.an.name)),
            b::r#function(
                None,
                vec![b::id("$$props"), b::id("$$opts")],
                b::block(vec![b::r#return(b::call(
                    "$$_render",
                    vec![
                        b::id(s.an.name.as_str()),
                        b::object(vec![
                            b::init("props", b::id("$$props")),
                            b::init("context", b::member_with(b::id("$$opts"), b::id("context"), false, true)),
                        ]),
                    ],
                ))]),
            ),
        )));
        body.push(b::export_default(b::id(s.an.name.as_str())));
    } else if s.dev {
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

    program_node(body)
}

/// `server_module(analysis, options)`: a `.svelte.js` module
pub fn server_module(s: &mut Server) -> Node {
    let state = State {
        scope: s.an.module_scope,
        is_instance: false,
        namespace: "html",
        preserve_whitespace: false,
        is_standalone: false,
        init: shared(),
        template: shared(),
        async_consts: Default::default(),
        state_fields: None,
    };
    let module = match s.an.module_program {
        Some(p) => {
            let program = s.convert_program(p);
            s.visit_js(&program, &state)
        }
        None => program_node(vec![]),
    };
    let mut body = vec![b::import_all("$", "svelte/internal/server")];
    body.extend(program_body(module));
    program_node(body)
}


fn program_body(node: Node) -> Vec<Node> {
    match node.kind {
        NodeKind::Program(p) => p.body,
        _ => vec![],
    }
}

/// `{ type: 'Program', sourceType: 'module', body }`
fn program_node(body: Vec<Node>) -> Node {
    Node::new(NodeKind::Program(crate::estree::Program { body, source_type: crate::estree::SourceType::Module }))
}

thread_local! {
    /// The `class=""`/`style=""` attributes the analysis appends (`create_attribute`)
    static SYNTHETIC: (&'static crate::ast::Attr<'static>, &'static crate::ast::Attr<'static>) = {
        let make = |name: &'static str| -> &'static crate::ast::Attr<'static> {
            Box::leak(Box::new(crate::ast::Attr::Attribute {
                start: usize::MAX,
                end: usize::MAX,
                name,
                name_loc: None,
                value: crate::ast::AttrValue::Sequence(vec![crate::ast::Chunk::Text { start: usize::MAX, end: usize::MAX, raw: "", data: "".into() }]),
            }))
        };
        (make("class"), make("style"))
    };
}

impl<'a, 's> Server<'a, 's> {
    /// An element's attributes, with the analysis' synthetic `class`/`style` at the end
    pub fn element_attributes(&self, n: crate::ast::NodeId) -> Vec<&'s crate::ast::Attr<'s>> {
        let crate::ast::Node::Element(el) = &self.ast().nodes[n] else { return vec![] };
        let mut out: Vec<&'s crate::ast::Attr<'s>> = el.attributes.iter().collect();
        // the `value` the analysis makes out of a `<textarea>`'s dynamic children
        if self.an.textarea_values.contains(&n) {
            out.push(super::client::fragment::TEXTAREA_VALUE.with(|a| *a));
        }
        if self.synthetic_class.contains(&n) {
            out.push(SYNTHETIC.with(|s| s.0));
        }
        if self.synthetic_style.contains(&n) {
            out.push(SYNTHETIC.with(|s| s.1));
        }
        out
    }
}

/// The elements that get a synthetic `class=""` (scoped or with class directives) and
/// `style=""` (with style directives), when they have no such attribute nor a spread
pub fn synthetic_attributes(an: &Analyzer, scoped: &rustc_hash::FxHashSet<crate::ast::NodeId>) -> (rustc_hash::FxHashSet<crate::ast::NodeId>, rustc_hash::FxHashSet<crate::ast::NodeId>) {
    let mut class = rustc_hash::FxHashSet::default();
    let mut style = rustc_hash::FxHashSet::default();
    for &n in &an.elements {
        let crate::ast::Node::Element(el) = &an.ast.nodes[n] else { continue };
        let (mut has_class, mut has_style, mut has_spread, mut has_class_directive, mut has_style_directive) = (false, false, false, false, false);
        for a in &el.attributes {
            match a {
                crate::ast::Attr::Spread { .. } => {
                    has_spread = true;
                    break;
                }
                crate::ast::Attr::Attribute { name, .. } => {
                    has_class |= name.to_lowercase() == "class";
                    has_style |= name.to_lowercase() == "style";
                }
                crate::ast::Attr::Directive { kind: "ClassDirective", .. } => has_class_directive = true,
                crate::ast::Attr::StyleDirective { .. } => has_style_directive = true,
                _ => {}
            }
        }
        if !has_spread && !has_class && (scoped.contains(&n) || has_class_directive) {
            class.insert(n);
        }
        if !has_spread && !has_style && has_style_directive {
            style.insert(n);
        }
    }
    (class, style)
}
