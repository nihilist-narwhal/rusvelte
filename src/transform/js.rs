//! Helpers over the owned ESTree the transform builds (`src/estree`): the parts of
//! `utils/ast.js`, `is-reference` and `scope.js` (`get_rune`) that the transforms call on
//! ESTree nodes, and the path a zimmerframe-style walk keeps.

use crate::analyze::nodes::P;
use crate::analyze::scope::{ScopeId, Scopes};
use crate::estree::{LiteralValue, Node, NodeKind};

/// The JS part of `context.path` in a walk of a JS tree: the ancestors of the node being
/// visited, innermost first, up to the node the walk started from (the template nodes
/// around the walk, the outer part of `context.path`, are the transform's `tpl_path`).
///
/// It is a list on the stack: the visitors get it as a parameter, and `next`/`visit_in`
/// visit a node's children with the node pushed in front of it. The entries are the visited
/// nodes themselves, so they can be compared by identity with their children
/// (`is_reference`).
#[derive(Clone, Copy)]
pub struct Ancestors<'n>(Option<(&'n Node, &'n Ancestors<'n>)>);

impl Ancestors<'static> {
    /// The ancestors of the node a walk starts from
    pub const ROOT: Ancestors<'static> = Ancestors(None);
}

impl<'n> Ancestors<'n> {
    /// The ancestors of `node`'s children
    pub fn push(&'n self, node: &'n Node) -> Ancestors<'n> {
        Ancestors(Some((node, self)))
    }

    /// The parent, if it is a JS node
    pub fn parent(&self) -> Option<&'n Node> {
        self.0.map(|(node, _)| node)
    }

    /// The nodes, innermost first
    pub fn iter(&self) -> impl Iterator<Item = &'n Node> + use<'n> {
        let mut rest = *self;
        std::iter::from_fn(move || {
            let (node, parent) = rest.0?;
            rest = *parent;
            Some(node)
        })
    }

    pub fn len(&self) -> usize {
        self.iter().count()
    }
}

/// An entry of `context.path`: a JS node or a template node
#[derive(Clone, Copy)]
pub enum PathNode<'s, 'n> {
    Js(&'n Node),
    Tpl(P<'s>),
}

impl<'s, 'n> PathNode<'s, 'n> {
    /// The JS node, if it is one
    pub fn js(&self) -> Option<&'n Node> {
        match *self {
            PathNode::Js(n) => Some(n),
            PathNode::Tpl(_) => None,
        }
    }

    /// The node's `type`
    pub fn ty(&self, ast: &crate::ast::Ast) -> &'static str {
        match self {
            PathNode::Js(n) => n.type_name(),
            PathNode::Tpl(p) => p.ty(ast),
        }
    }
}

/// `context.path` of a JS node, innermost first: its JS ancestors, then the template nodes
/// around the walk
pub fn context_path<'s, 'n>(tpl_path: &'n [P<'s>], ancestors: &Ancestors<'n>) -> impl Iterator<Item = PathNode<'s, 'n>> + use<'s, 'n> {
    ancestors.iter().map(PathNode::Js).chain(tpl_path.iter().rev().map(|&p| PathNode::Tpl(p)))
}

/// `context.path.at(-i)` (`i >= 1`)
pub fn path_at<'s, 'n>(tpl_path: &'n [P<'s>], ancestors: &Ancestors<'n>, i: usize) -> Option<PathNode<'s, 'n>> {
    context_path(tpl_path, ancestors).nth(i - 1)
}

/// `context.path[i]`
pub fn path_from_root<'s, 'n>(tpl_path: &'n [P<'s>], ancestors: &Ancestors<'n>, i: usize) -> Option<PathNode<'s, 'n>> {
    let len = tpl_path.len() + ancestors.len();
    if i >= len { None } else { path_at(tpl_path, ancestors, len - i) }
}

/// `is_reference(node, parent)` from `is-reference`
pub fn is_reference(node: &Node, parent: Option<&Node>) -> bool {
    match &node.kind {
        NodeKind::MemberExpression(m) => !m.computed && is_reference(&m.object, Some(node)),
        NodeKind::Identifier(_) => {
            let Some(parent) = parent else { return true };
            match &parent.kind {
                NodeKind::MemberExpression(m) => m.computed || std::ptr::eq(node, &*m.object),
                NodeKind::MethodDefinition(m) => m.computed,
                NodeKind::MetaProperty(m) => std::ptr::eq(node, &*m.meta),
                NodeKind::PropertyDefinition(d) => d.computed || d.value.as_deref().is_some_and(|v| std::ptr::eq(node, v)),
                NodeKind::Property(p) => p.computed || std::ptr::eq(node, &*p.value),
                NodeKind::ExportSpecifier(s) => std::ptr::eq(node, &*s.local),
                NodeKind::ImportSpecifier(s) => std::ptr::eq(node, &*s.local),
                NodeKind::LabeledStatement(_) | NodeKind::BreakStatement(_) | NodeKind::ContinueStatement(_) => false,
                _ => true,
            }
        }
        _ => false,
    }
}

/// The identifier name of a node
pub fn ident(node: &Node) -> Option<&str> {
    match &node.kind {
        NodeKind::Identifier(i) => Some(i.name.as_str()),
        _ => None,
    }
}

/// `object(expression)`: the identifier at the root of a member expression chain
pub fn object(node: &Node) -> Option<&Node> {
    let mut n = node;
    while let NodeKind::MemberExpression(m) = &n.kind {
        n = &m.object;
    }
    matches!(n.kind, NodeKind::Identifier(_)).then_some(n)
}

/// `unwrap_pattern`: the identifiers and member expressions a pattern assigns to
pub fn unwrap_pattern<'n>(pattern: &'n Node, out: &mut Vec<&'n Node>) {
    match &pattern.kind {
        NodeKind::ArrayPattern(a) => {
            for e in a.elements.iter().flatten() {
                unwrap_pattern(e, out);
            }
        }
        NodeKind::ObjectPattern(o) => {
            for p in &o.properties {
                match &p.kind {
                    NodeKind::RestElement(r) => unwrap_pattern(&r.argument, out),
                    NodeKind::Property(p) => unwrap_pattern(&p.value, out),
                    _ => {}
                }
            }
        }
        NodeKind::RestElement(r) => unwrap_pattern(&r.argument, out),
        NodeKind::AssignmentPattern(a) => unwrap_pattern(&a.left, out),
        _ => out.push(pattern),
    }
}

/// `extract_identifiers(pattern)`
pub fn extract_identifiers(pattern: &Node) -> Vec<&Node> {
    let mut nodes = Vec::new();
    unwrap_pattern(pattern, &mut nodes);
    nodes.into_iter().filter(|n| matches!(n.kind, NodeKind::Identifier(_))).collect()
}

/// `get_name(node)`: the name of a property key
pub fn get_name(node: &Node) -> Option<String> {
    match &node.kind {
        NodeKind::Literal(l) => Some(match &l.value {
            LiteralValue::String(s) => s.to_string(),
            LiteralValue::Number(n) => crate::analyze::evaluate::number_to_string(*n),
            LiteralValue::Boolean(b) => b.to_string(),
            LiteralValue::Null => "null".into(),
            LiteralValue::BigInt(b) => b.to_string(),
            LiteralValue::RegExp(r) => format!("/{}/{}", r.pattern, r.flags),
        }),
        NodeKind::PrivateIdentifier(p) => Some(format!("#{}", p.name)),
        NodeKind::Identifier(i) => Some(i.name.to_string()),
        _ => None,
    }
}

/// `get_global_keypath(node, scope)`
pub fn get_global_keypath(node: &Node, scopes: &Scopes, scope: ScopeId) -> Option<String> {
    let mut n = node;
    let mut joined = String::new();
    while let NodeKind::MemberExpression(m) = &n.kind {
        if m.computed {
            return None;
        }
        let NodeKind::Identifier(p) = &m.property.kind else { return None };
        joined.insert_str(0, p.name.as_str());
        joined.insert(0, '.');
        n = &m.object;
    }
    if let NodeKind::CallExpression(c) = &n.kind {
        if matches!(c.callee.kind, NodeKind::Identifier(_)) {
            joined.insert_str(0, "()");
            n = &c.callee;
        }
    }
    let NodeKind::Identifier(id) = &n.kind else { return None };
    if scopes.get(scope, id.name.as_str()).is_some() {
        return None;
    }
    Some(format!("{}{}", id.name, joined))
}

/// `get_rune(node, scope)`: the rune a call expression calls, if any
pub fn get_rune(node: Option<&Node>, scopes: &Scopes, scope: ScopeId) -> Option<&'static str> {
    let NodeKind::CallExpression(c) = &node?.kind else { return None };
    let keypath = get_global_keypath(&c.callee, scopes, scope)?;
    crate::analyze::utils::is_rune(&keypath)
}

/// Whether a node is a statement or declaration (`type` ends with `Statement`/`Declaration`)
pub fn is_statement(node: &Node) -> bool {
    let t = node.type_name();
    t.ends_with("Statement") || t.ends_with("Declaration")
}

/// zimmerframe's default for a node without a visitor: the node with its children visited
pub fn map_children(node: &Node, f: &mut dyn FnMut(&Node) -> Node) -> Node {
    node.map_children(f)
}

// ---------------------------------------------------------------------------------------
// `utils/ast.js`

use crate::estree::builders as b;

/// `unwrap_optional`: a ChainExpression's expression
pub fn unwrap_optional(node: &Node) -> &Node {
    match &node.kind {
        NodeKind::ChainExpression(c) => &c.expression,
        _ => node,
    }
}

/// `is_simple_expression`
pub fn is_simple_expression(node: &Node) -> bool {
    match &node.kind {
        NodeKind::Literal(_) | NodeKind::Identifier(_) | NodeKind::ArrowFunctionExpression(_) | NodeKind::FunctionExpression(_) => true,
        NodeKind::ConditionalExpression(c) => {
            is_simple_expression(&c.test) && is_simple_expression(&c.consequent) && is_simple_expression(&c.alternate)
        }
        NodeKind::BinaryExpression(e) => !matches!(e.left.kind, NodeKind::PrivateIdentifier(_)) && is_simple_expression(&e.left) && is_simple_expression(&e.right),
        NodeKind::LogicalExpression(e) => is_simple_expression(&e.left) && is_simple_expression(&e.right),
        _ => false,
    }
}

/// `is_expression_async`
pub fn is_expression_async(e: &Node) -> bool {
    let not_private = |n: &Node| !matches!(n.kind, NodeKind::PrivateIdentifier(_));
    match &e.kind {
        NodeKind::AwaitExpression(_) => true,
        NodeKind::ArrayPattern(a) | NodeKind::ArrayExpression(a) => a.elements.iter().flatten().any(|el| match &el.kind {
            NodeKind::SpreadElement(s) => is_expression_async(&s.argument),
            _ => is_expression_async(el),
        }),
        NodeKind::AssignmentPattern(a) => is_expression_async(&a.left) || is_expression_async(&a.right),
        NodeKind::AssignmentExpression(a) => is_expression_async(&a.left) || is_expression_async(&a.right),
        NodeKind::BinaryExpression(a) => (not_private(&a.left) && is_expression_async(&a.left)) || is_expression_async(&a.right),
        NodeKind::LogicalExpression(a) => is_expression_async(&a.left) || is_expression_async(&a.right),
        NodeKind::CallExpression(c) => {
            (!matches!(c.callee.kind, NodeKind::Super) && is_expression_async(&c.callee))
                || c.arguments.iter().any(|a| match &a.kind {
                    NodeKind::SpreadElement(s) => is_expression_async(&s.argument),
                    _ => is_expression_async(a),
                })
        }
        NodeKind::NewExpression(c) => {
            (!matches!(c.callee.kind, NodeKind::Super) && is_expression_async(&c.callee))
                || c.arguments.iter().any(|a| match &a.kind {
                    NodeKind::SpreadElement(s) => is_expression_async(&s.argument),
                    _ => is_expression_async(a),
                })
        }
        NodeKind::ChainExpression(c) => is_expression_async(&c.expression),
        NodeKind::ConditionalExpression(c) => is_expression_async(&c.test) || is_expression_async(&c.alternate) || is_expression_async(&c.consequent),
        NodeKind::ImportExpression(i) => is_expression_async(&i.source),
        NodeKind::MemberExpression(m) => {
            (!matches!(m.object.kind, NodeKind::Super) && is_expression_async(&m.object)) || (not_private(&m.property) && is_expression_async(&m.property))
        }
        NodeKind::ObjectPattern(o) | NodeKind::ObjectExpression(o) => o.properties.iter().any(|p| match &p.kind {
            NodeKind::SpreadElement(s) => is_expression_async(&s.argument),
            NodeKind::Property(p) => is_expression_async(&p.key) || is_expression_async(&p.value),
            _ => false,
        }),
        NodeKind::RestElement(r) => is_expression_async(&r.argument),
        NodeKind::SequenceExpression(s) => s.expressions.iter().any(is_expression_async),
        NodeKind::TemplateLiteral(t) => t.expressions.iter().any(is_expression_async),
        NodeKind::TaggedTemplateExpression(t) => is_expression_async(&t.tag) || is_expression_async(&t.quasi),
        NodeKind::UnaryExpression(u) => is_expression_async(&u.argument),
        NodeKind::UpdateExpression(u) => is_expression_async(&u.argument),
        NodeKind::YieldExpression(y) => y.argument.as_deref().is_some_and(is_expression_async),
        _ => false,
    }
}

/// `build_fallback(expression, fallback)`
pub fn build_fallback(expression: Node, fallback: Node) -> Node {
    if is_simple_expression(&fallback) {
        return b::call("$.fallback", vec![expression, fallback]);
    }
    if let NodeKind::AwaitExpression(a) = &fallback.kind {
        if is_simple_expression(&a.argument) {
            return b::r#await(b::call("$.fallback", vec![expression, (*a.argument).clone()]));
        }
    }
    if is_expression_async(&fallback) {
        b::r#await(b::call("$.fallback", vec![expression, b::thunk_with(fallback, true), b::r#true()]))
    } else {
        b::call("$.fallback", vec![expression, b::thunk(fallback), b::r#true()])
    }
}

/// `build_assignment_value(operator, left, right)`
pub fn build_assignment_value(operator: &str, left: Node, right: Node) -> Node {
    if operator == "=" {
        return right;
    }
    let op = &operator[..operator.len() - 1];
    if matches!(operator, "||=" | "&&=" | "??=") {
        b::logical(op, left, right)
    } else {
        b::binary(op, left, right)
    }
}

/// `save(expression)`: `(await $.save(expression))()`
pub fn save(expression: Node) -> Node {
    b::call(b::r#await(b::call("$.save", vec![expression])), ())
}

/// A path of `extract_paths`
#[derive(Clone)]
pub struct DestructuredAssignment {
    pub node: Node,
    pub is_rest: bool,
    pub has_default_value: bool,
    pub expression: Node,
    pub update_expression: Node,
}

/// `extract_paths(param, initial)`. The ids of `inserts` are named `#0`, `#1`, ... (the JS
/// names them `#` and the caller renames the shared identifier objects); give them their
/// names with [`rename_placeholders`] on every expression that may contain them.
pub fn extract_paths(param: &Node, initial: Node) -> (Vec<(Node, Node)>, Vec<DestructuredAssignment>) {
    let mut inserts = Vec::new();
    let mut paths = Vec::new();
    extract_paths_inner(&mut paths, &mut inserts, param, initial.clone(), initial, false);
    (inserts, paths)
}

fn extract_paths_inner(
    paths: &mut Vec<DestructuredAssignment>,
    inserts: &mut Vec<(Node, Node)>,
    param: &Node,
    expression: Node,
    update_expression: Node,
    has_default_value: bool,
) {
    match &param.kind {
        NodeKind::Identifier(_) | NodeKind::MemberExpression(_) => paths.push(DestructuredAssignment {
            node: param.clone(),
            is_rest: false,
            has_default_value,
            expression,
            update_expression,
        }),
        NodeKind::ObjectPattern(o) => {
            for prop in &o.properties {
                if let NodeKind::RestElement(r) = &prop.kind {
                    let mut props = Vec::new();
                    for p in &o.properties {
                        if let NodeKind::Property(pp) = &p.kind {
                            match &pp.key.kind {
                                NodeKind::Identifier(i) if !pp.computed => props.push(b::literal(i.name.as_str())),
                                NodeKind::Literal(_) => props.push(b::literal(get_name(&pp.key).unwrap_or_default().as_str())),
                                _ => props.push(b::call("String", vec![(*pp.key).clone()])),
                            }
                        }
                    }
                    let rest_expression = b::call("$.exclude_from_object", vec![expression.clone(), b::array(props)]);
                    if matches!(r.argument.kind, NodeKind::Identifier(_)) {
                        paths.push(DestructuredAssignment {
                            node: (*r.argument).clone(),
                            is_rest: true,
                            has_default_value,
                            expression: rest_expression.clone(),
                            update_expression: rest_expression,
                        });
                    } else {
                        extract_paths_inner(paths, inserts, &r.argument, rest_expression.clone(), rest_expression, has_default_value);
                    }
                } else if let NodeKind::Property(p) = &prop.kind {
                    let computed = p.computed || !matches!(p.key.kind, NodeKind::Identifier(_));
                    let object_expression = b::member_with(expression.clone(), (*p.key).clone(), computed, false);
                    extract_paths_inner(paths, inserts, &p.value, object_expression.clone(), object_expression, has_default_value);
                }
            }
        }
        NodeKind::ArrayPattern(a) => {
            let id = b::id(format!("#{}", inserts.len()).as_str());
            let last_is_rest = a.elements.last().and_then(|e| e.as_ref()).is_some_and(|e| matches!(e.kind, NodeKind::RestElement(_)));
            let mut args = vec![Some(expression.clone())];
            if !last_is_rest {
                args.push(Some(b::literal(a.elements.len() as f64)));
            }
            let value = b::call("$.to_array", args);
            inserts.push((id.clone(), value));
            for (i, element) in a.elements.iter().enumerate() {
                let Some(element) = element else { continue };
                if let NodeKind::RestElement(r) = &element.kind {
                    let rest_expression = b::call(b::member(id.clone(), "slice"), vec![b::literal(i as f64)]);
                    if matches!(r.argument.kind, NodeKind::Identifier(_)) {
                        paths.push(DestructuredAssignment {
                            node: (*r.argument).clone(),
                            is_rest: true,
                            has_default_value,
                            expression: rest_expression.clone(),
                            update_expression: rest_expression,
                        });
                    } else {
                        extract_paths_inner(paths, inserts, &r.argument, rest_expression.clone(), rest_expression, has_default_value);
                    }
                } else {
                    let array_expression = b::member_with(id.clone(), b::literal(i as f64), true, false);
                    extract_paths_inner(paths, inserts, element, array_expression.clone(), array_expression, has_default_value);
                }
            }
        }
        NodeKind::AssignmentPattern(a) => {
            let fallback_expression = build_fallback(expression, (*a.right).clone());
            if matches!(a.left.kind, NodeKind::Identifier(_)) {
                paths.push(DestructuredAssignment {
                    node: (*a.left).clone(),
                    is_rest: false,
                    has_default_value: true,
                    expression: fallback_expression,
                    update_expression,
                });
            } else {
                extract_paths_inner(paths, inserts, &a.left, fallback_expression, update_expression, true);
            }
        }
        _ => {}
    }
}

/// Give the `#i` placeholders of [`extract_paths`] their names
pub fn rename_placeholders(node: &mut Node, names: &[String]) {
    if let NodeKind::Identifier(i) = &mut node.kind {
        if let Some(index) = i.name.strip_prefix('#').and_then(|n| n.parse::<usize>().ok()) {
            if let Some(name) = names.get(index) {
                i.name = name.as_str().into();
            }
        }
        return;
    }
    node.for_each_child_mut(&mut |c| rename_placeholders(c, names));
}
