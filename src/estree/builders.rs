//! A port of `svelte/src/compiler/utils/builders.js` (`import * as b from '#compiler/builders'`).
//!
//! Every exported builder has a function of the same name (Rust keywords as raw identifiers:
//! `b::r#await`, `b::r#let`, `b::r#if`, ...; the shared JS constants `b.empty`, `b.true`,
//! `b.void0`, ... are functions returning a fresh node). Conventions:
//!
//! - A `string | Expression` parameter takes `impl Into<Node>`: a `&str`/`String` becomes an
//!   `Identifier` (`From<&str> for Node`), as `id(callee)` does in the JS.
//! - An optional or nullable node parameter takes `impl Into<Option<Node>>`, so both `node`
//!   and `None` work.
//! - Operators and kinds are their source text (`"==="`, `"let"`, `"get"`); an unknown one
//!   panics.
//! - Trailing parameters with JS defaults (`computed = false`, `async = false`, ...) are
//!   dropped from the short form and present in a `_with` variant: `member(o, p)` is
//!   `b.member(o, p)`, `member_with(o, p, computed, optional)` is `b.member(o, p, computed, optional)`.
//! - `call` arguments take [`Args`]: `()`, a node, an array or a `Vec` of nodes, or of
//!   `Option<Node>` for the JS's `false | undefined | null` holes.

use super::*;

impl From<&str> for Node {
    fn from(name: &str) -> Self {
        id(name)
    }
}

impl From<String> for Node {
    fn from(name: String) -> Self {
        id(name)
    }
}

impl From<&String> for Node {
    fn from(name: &String) -> Self {
        id(name.as_str())
    }
}

impl From<Atom> for Node {
    fn from(name: Atom) -> Self {
        id(name)
    }
}

impl From<&Atom> for Node {
    fn from(name: &Atom) -> Self {
        id(name.clone())
    }
}

impl From<&str> for LiteralValue {
    fn from(s: &str) -> Self {
        LiteralValue::String(Atom::from(s))
    }
}

impl From<String> for LiteralValue {
    fn from(s: String) -> Self {
        LiteralValue::String(Atom::from(s))
    }
}

impl From<Atom> for LiteralValue {
    fn from(s: Atom) -> Self {
        LiteralValue::String(s)
    }
}

impl From<bool> for LiteralValue {
    fn from(b: bool) -> Self {
        LiteralValue::Boolean(b)
    }
}

impl From<f64> for LiteralValue {
    fn from(n: f64) -> Self {
        LiteralValue::Number(n)
    }
}

impl From<i32> for LiteralValue {
    fn from(n: i32) -> Self {
        LiteralValue::Number(n as f64)
    }
}

impl From<u32> for LiteralValue {
    fn from(n: u32) -> Self {
        LiteralValue::Number(n as f64)
    }
}

impl From<usize> for LiteralValue {
    fn from(n: usize) -> Self {
        LiteralValue::Number(n as f64)
    }
}

impl From<i64> for LiteralValue {
    fn from(n: i64) -> Self {
        LiteralValue::Number(n as f64)
    }
}

/// The arguments of [`call`]: `()`, a node, or a list of nodes or of `Option<Node>`s
pub trait Args {
    fn into_args(self) -> Vec<Option<Node>>;
}

impl Args for () {
    fn into_args(self) -> Vec<Option<Node>> {
        Vec::new()
    }
}

impl Args for Node {
    fn into_args(self) -> Vec<Option<Node>> {
        vec![Some(self)]
    }
}

impl Args for Option<Node> {
    fn into_args(self) -> Vec<Option<Node>> {
        vec![self]
    }
}

impl Args for Vec<Node> {
    fn into_args(self) -> Vec<Option<Node>> {
        self.into_iter().map(Some).collect()
    }
}

impl Args for Vec<Option<Node>> {
    fn into_args(self) -> Vec<Option<Node>> {
        self
    }
}

impl<const N: usize> Args for [Node; N] {
    fn into_args(self) -> Vec<Option<Node>> {
        self.into_iter().map(Some).collect()
    }
}

impl<const N: usize> Args for [Option<Node>; N] {
    fn into_args(self) -> Vec<Option<Node>> {
        self.into_iter().collect()
    }
}

fn n(kind: NodeKind) -> Node {
    Node::new(kind)
}

fn bx(node: impl Into<Node>) -> BoxNode {
    Box::new(node.into())
}

fn opt(node: impl Into<Option<Node>>) -> Option<BoxNode> {
    node.into().map(Box::new)
}

fn variable_kind(kind: &str) -> VariableKind {
    match kind {
        "var" => VariableKind::Var,
        "let" => VariableKind::Let,
        "const" => VariableKind::Const,
        "using" => VariableKind::Using,
        "await using" => VariableKind::AwaitUsing,
        _ => panic!("unknown variable declaration kind {kind:?}"),
    }
}

fn property_kind(kind: &str) -> PropertyKind {
    match kind {
        "init" => PropertyKind::Init,
        "get" => PropertyKind::Get,
        "set" => PropertyKind::Set,
        _ => panic!("unknown property kind {kind:?}"),
    }
}

fn method_kind(kind: &str) -> MethodKind {
    match kind {
        "constructor" => MethodKind::Constructor,
        "method" => MethodKind::Method,
        "get" => MethodKind::Get,
        "set" => MethodKind::Set,
        _ => panic!("unknown method kind {kind:?}"),
    }
}

/// `b.array(elements = [])`
pub fn array(elements: impl IntoIterator<Item = impl Into<Option<Node>>>) -> Node {
    n(NodeKind::ArrayExpression(ArrayExpression { elements: elements.into_iter().map(Into::into).collect() }))
}

/// `b.array_pattern(elements)`
pub fn array_pattern(elements: impl IntoIterator<Item = impl Into<Option<Node>>>) -> Node {
    n(NodeKind::ArrayPattern(ArrayExpression { elements: elements.into_iter().map(Into::into).collect() }))
}

/// `b.assignment_pattern(left, right)`
pub fn assignment_pattern(left: Node, right: Node) -> Node {
    n(NodeKind::AssignmentPattern(AssignmentPattern { left: bx(left), right: bx(right) }))
}

/// `b.arrow(params, body)`
pub fn arrow(params: Vec<Node>, body: Node) -> Node {
    arrow_with(params, body, false)
}

/// `b.arrow(params, body, async)`: `async () => await x()` becomes `() => x()` unless the
/// awaited expression contains another `await`
pub fn arrow_with(params: Vec<Node>, body: Node, is_async: bool) -> Node {
    if is_async {
        if let NodeKind::AwaitExpression(a) = &body.kind {
            if !has_await_expression(&a.argument) {
                let NodeKind::AwaitExpression(a) = body.kind else { unreachable!() };
                return arrow(params, *a.argument);
            }
        }
    }
    let expression = !body.is("BlockStatement");
    n(NodeKind::ArrowFunctionExpression(ArrowFunctionExpression { params, body: bx(body), expression, is_async }))
}

/// `b.assignment(operator, left, right)`
pub fn assignment(operator: &str, left: Node, right: Node) -> Node {
    let operator = assignment_operator(operator).unwrap_or_else(|| panic!("unknown assignment operator {operator:?}"));
    n(NodeKind::AssignmentExpression(AssignmentExpression { operator, left: bx(left), right: bx(right) }))
}

/// `b.await(argument)`
pub fn r#await(argument: Node) -> Node {
    n(NodeKind::AwaitExpression(Argument { argument: bx(argument) }))
}

/// `b.binary(operator, left, right)`
pub fn binary(operator: &str, left: Node, right: Node) -> Node {
    let operator = binary_operator(operator).unwrap_or_else(|| panic!("unknown binary operator {operator:?}"));
    n(NodeKind::BinaryExpression(BinaryExpression { operator, left: bx(left), right: bx(right) }))
}

/// `b.block(body)`
pub fn block(body: Vec<Node>) -> Node {
    n(NodeKind::BlockStatement(Body { body }))
}

/// `b.class_expression(id, body, superClass)`. Like the JS, `id` is ignored.
pub fn class_expression(_id: impl Into<Option<Node>>, body: Node, super_class: impl Into<Option<Node>>) -> Node {
    n(NodeKind::ClassExpression(Class { id: None, super_class: opt(super_class), body: bx(body) }))
}

/// `b.labeled(name, body)`
pub fn labeled(name: &str, body: Node) -> Node {
    n(NodeKind::LabeledStatement(LabeledStatement { label: bx(id(name)), body: bx(body) }))
}

/// `b.call(callee, ...args)`: missing arguments become `void 0`, or are removed at the end
pub fn call(callee: impl Into<Node>, args: impl Args) -> Node {
    let mut args = args.into_args();
    let mut i = args.len();
    let mut popping = true;
    while i > 0 {
        i -= 1;
        if args[i].is_none() {
            if popping {
                args.pop();
            } else {
                args[i] = Some(void0());
            }
        } else {
            popping = false;
        }
    }
    n(NodeKind::CallExpression(CallExpression {
        callee: bx(callee),
        arguments: args.into_iter().map(Option::unwrap).collect(),
        optional: false,
    }))
}

/// `b.maybe_call(callee, ...args)`: `callee?.(...args)`
pub fn maybe_call(callee: impl Into<Node>, args: impl Args) -> Node {
    let mut expression = call(callee, args);
    if let NodeKind::CallExpression(c) = &mut expression.kind {
        c.optional = true;
    }
    n(NodeKind::ChainExpression(ExpressionWrapper { expression: Box::new(expression) }))
}

/// `b.unary(operator, argument)`
pub fn unary(operator: &str, argument: Node) -> Node {
    let operator = unary_operator(operator).unwrap_or_else(|| panic!("unknown unary operator {operator:?}"));
    n(NodeKind::UnaryExpression(UnaryExpression { operator, argument: bx(argument) }))
}

/// `b.void0`
pub fn void0() -> Node {
    unary("void", literal(0))
}

/// `b.conditional(test, consequent, alternate)`
pub fn conditional(test: Node, consequent: Node, alternate: Node) -> Node {
    n(NodeKind::ConditionalExpression(ConditionalExpression {
        test: bx(test),
        consequent: bx(consequent),
        alternate: bx(alternate),
    }))
}

/// `b.logical(operator, left, right)`
pub fn logical(operator: &str, left: Node, right: Node) -> Node {
    let operator = logical_operator(operator).unwrap_or_else(|| panic!("unknown logical operator {operator:?}"));
    n(NodeKind::LogicalExpression(LogicalExpression { operator, left: bx(left), right: bx(right) }))
}

/// `b.declaration(kind, declarations)`
pub fn declaration(kind: &str, declarations: Vec<Node>) -> Node {
    n(NodeKind::VariableDeclaration(VariableDeclaration { kind: variable_kind(kind), declarations }))
}

/// `b.declarator(pattern, init)`
pub fn declarator(pattern: impl Into<Node>, init: impl Into<Option<Node>>) -> Node {
    n(NodeKind::VariableDeclarator(VariableDeclarator { id: bx(pattern), init: opt(init) }))
}

/// `b.empty`
pub fn empty() -> Node {
    n(NodeKind::EmptyStatement)
}

/// `b.export_default(declaration)`
pub fn export_default(declaration: Node) -> Node {
    n(NodeKind::ExportDefaultDeclaration(ExportDefaultDeclaration { declaration: bx(declaration) }))
}

/// `b.for_of(left, right, body)`
pub fn for_of(left: Node, right: Node, body: Node) -> Node {
    for_of_with(left, right, body, false)
}

/// `b.for_of(left, right, body, await)`
pub fn for_of_with(left: Node, right: Node, body: Node, is_await: bool) -> Node {
    n(NodeKind::ForOfStatement(ForInStatement { left: bx(left), right: bx(right), body: bx(body), is_await }))
}

/// `b.function_declaration(id, params, body)`
pub fn function_declaration(id: Node, params: Vec<Node>, body: Node) -> Node {
    function_declaration_with(id, params, body, false)
}

/// `b.function_declaration(id, params, body, async)`
pub fn function_declaration_with(id: Node, params: Vec<Node>, body: Node, is_async: bool) -> Node {
    n(NodeKind::FunctionDeclaration(Function { id: Some(bx(id)), params, body: bx(body), generator: false, is_async }))
}

/// `b.get(name, body)`: a getter property
pub fn get(name: &str, body: Vec<Node>) -> Node {
    prop("get", key(name), r#function(None, vec![], block(body)))
}

/// `b.id(name)`
pub fn id(name: impl Into<Atom>) -> Node {
    n(NodeKind::Identifier(Identifier { name: name.into() }))
}

/// `b.id(name, loc)`
pub fn id_with_loc(name: impl Into<Atom>, loc: Option<SourceLocation>) -> Node {
    let mut node = id(name);
    node.loc = loc;
    node
}

/// `b.private_id(name)`
pub fn private_id(name: impl Into<Atom>) -> Node {
    n(NodeKind::PrivateIdentifier(PrivateIdentifier { name: name.into() }))
}

fn import_namespace(local: &str) -> Node {
    n(NodeKind::ImportNamespaceSpecifier(LocalSpecifier { local: bx(id(local)) }))
}

/// `b.init(name, value)`
pub fn init(name: &str, value: Node) -> Node {
    prop("init", key(name), value)
}

/// `b.literal(value)` (no `raw`; printed from the value)
pub fn literal(value: impl Into<LiteralValue>) -> Node {
    n(NodeKind::Literal(Literal { value: value.into(), raw: None }))
}

/// `b.member(object, property)`
pub fn member(object: Node, property: impl Into<Node>) -> Node {
    member_with(object, property, false, false)
}

/// `b.member(object, property, computed, optional)`
pub fn member_with(object: Node, property: impl Into<Node>, computed: bool, optional: bool) -> Node {
    n(NodeKind::MemberExpression(MemberExpression { object: bx(object), property: bx(property), computed, optional }))
}

/// `b.member_id(path)`: `a.b.c` from `"a.b.c"`
pub fn member_id(path: &str) -> Node {
    let mut parts = path.split('.');
    let mut expression = id(parts.next().unwrap_or(""));
    for part in parts {
        expression = member(expression, id(part));
    }
    expression
}

/// `b.object(properties)`
pub fn object(properties: Vec<Node>) -> Node {
    n(NodeKind::ObjectExpression(ObjectExpression { properties }))
}

/// `b.object_pattern(properties)`
pub fn object_pattern(properties: Vec<Node>) -> Node {
    n(NodeKind::ObjectPattern(ObjectExpression { properties }))
}

/// `b.prop(kind, key, value)`
pub fn prop(kind: &str, key: Node, value: Node) -> Node {
    prop_with(kind, key, value, false)
}

/// `b.prop(kind, key, value, computed)`
pub fn prop_with(kind: &str, key: Node, value: Node, computed: bool) -> Node {
    n(NodeKind::Property(Property {
        key: bx(key),
        value: bx(value),
        kind: property_kind(kind),
        method: false,
        shorthand: false,
        computed,
    }))
}

/// `b.prop_def(key, value)`
pub fn prop_def(key: Node, value: impl Into<Option<Node>>) -> Node {
    prop_def_with(key, value, false, false)
}

/// `b.prop_def(key, value, computed, is_static)`
pub fn prop_def_with(key: Node, value: impl Into<Option<Node>>, computed: bool, is_static: bool) -> Node {
    n(NodeKind::PropertyDefinition(PropertyDefinition {
        key: bx(key),
        value: opt(value),
        computed,
        is_static,
        is_abstract: false,
    }))
}

/// `sanitize_template_string`: escape `` ` ``, `${` and `\`
pub fn sanitize_template_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '`' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            '$' if chars.peek() == Some(&'{') => {
                chars.next();
                out.push_str("\\${");
            }
            c => out.push(c),
        }
    }
    out
}

/// `b.quasi(cooked)`
pub fn quasi(cooked: &str) -> Node {
    quasi_with(cooked, false)
}

/// `b.quasi(cooked, tail)`
pub fn quasi_with(cooked: &str, tail: bool) -> Node {
    let raw = sanitize_template_string(cooked);
    n(NodeKind::TemplateElement(TemplateElement { raw: Atom::from(raw), cooked: Some(Atom::from(cooked)), tail }))
}

/// `b.rest(argument)`
pub fn rest(argument: Node) -> Node {
    n(NodeKind::RestElement(Argument { argument: bx(argument) }))
}

/// `b.sequence(expressions)`
pub fn sequence(expressions: Vec<Node>) -> Node {
    n(NodeKind::SequenceExpression(SequenceExpression { expressions }))
}

/// `b.set(name, body)`: a setter property with a `$$value` parameter
pub fn set(name: &str, body: Vec<Node>) -> Node {
    prop("set", key(name), r#function(None, vec![id("$$value")], block(body)))
}

/// `b.spread(argument)`
pub fn spread(argument: Node) -> Node {
    n(NodeKind::SpreadElement(Argument { argument: bx(argument) }))
}

/// `b.stmt(expression)`
pub fn stmt(expression: Node) -> Node {
    n(NodeKind::ExpressionStatement(ExpressionStatement { expression: bx(expression), directive: None }))
}

/// `b.template(elements, expressions)`
pub fn template(elements: Vec<Node>, expressions: Vec<Node>) -> Node {
    n(NodeKind::TemplateLiteral(TemplateLiteral { quasis: elements, expressions }))
}

/// `b.thunk(expression)`
pub fn thunk(expression: Node) -> Node {
    thunk_with(expression, false)
}

/// `b.thunk(expression, async)`
pub fn thunk_with(expression: Node, is_async: bool) -> Node {
    unthunk(arrow_with(vec![], expression, is_async))
}

/// `b.unthunk(expression)`: `(a) => f(a)` becomes `f`
pub fn unthunk(expression: Node) -> Node {
    if let NodeKind::ArrowFunctionExpression(a) = &expression.kind {
        if let NodeKind::CallExpression(c) = &a.body.kind {
            if !a.is_async
                && c.callee.is("Identifier")
                && a.params.len() == c.arguments.len()
                && a.params.iter().zip(&c.arguments).all(|(p, arg)| match (&p.kind, &arg.kind) {
                    (NodeKind::Identifier(p), NodeKind::Identifier(arg)) => p.name == arg.name,
                    _ => false,
                })
            {
                let NodeKind::ArrowFunctionExpression(a) = expression.kind else { unreachable!() };
                let NodeKind::CallExpression(c) = a.body.kind else { unreachable!() };
                return *c.callee;
            }
        }
    }
    expression
}

/// `b.new(expression, ...args)`
pub fn new(expression: impl Into<Node>, args: Vec<Node>) -> Node {
    n(NodeKind::NewExpression(NewExpression { callee: bx(expression), arguments: args }))
}

/// `b.update(operator, argument)`
pub fn update(operator: &str, argument: Node) -> Node {
    update_with(operator, argument, false)
}

/// `b.update(operator, argument, prefix)`
pub fn update_with(operator: &str, argument: Node, prefix: bool) -> Node {
    let operator = update_operator(operator).unwrap_or_else(|| panic!("unknown update operator {operator:?}"));
    n(NodeKind::UpdateExpression(UpdateExpression { operator, prefix, argument: bx(argument) }))
}

/// `b.do_while(test, body)`
pub fn do_while(test: Node, body: Node) -> Node {
    n(NodeKind::DoWhileStatement(WhileStatement { test: bx(test), body: bx(body) }))
}

/// `b.true`
pub fn r#true() -> Node {
    literal(true)
}

/// `b.false`
pub fn r#false() -> Node {
    literal(false)
}

/// `b.null`
pub fn null() -> Node {
    literal(LiteralValue::Null)
}

/// `b.debugger`
pub fn debugger() -> Node {
    n(NodeKind::DebuggerStatement)
}

/// `b.this`
pub fn this() -> Node {
    n(NodeKind::ThisExpression)
}

/// `b.let(pattern, init)`
pub fn r#let(pattern: impl Into<Node>, init: impl Into<Option<Node>>) -> Node {
    declaration("let", vec![declarator(pattern, init)])
}

/// `b.const(pattern, init)`
pub fn r#const(pattern: impl Into<Node>, init: impl Into<Option<Node>>) -> Node {
    declaration("const", vec![declarator(pattern, init)])
}

/// `b.var(pattern, init)`
pub fn var(pattern: impl Into<Node>, init: impl Into<Option<Node>>) -> Node {
    declaration("var", vec![declarator(pattern, init)])
}

/// `b.for(init, test, update, body)`
pub fn r#for(
    init: impl Into<Option<Node>>,
    test: impl Into<Option<Node>>,
    update: impl Into<Option<Node>>,
    body: Node,
) -> Node {
    n(NodeKind::ForStatement(ForStatement { init: opt(init), test: opt(test), update: opt(update), body: bx(body) }))
}

/// `b.method(kind, key, params, body)`
pub fn method(kind: &str, key: Node, params: Vec<Node>, body: Vec<Node>) -> Node {
    method_with(kind, key, params, body, false, false)
}

/// `b.method(kind, key, params, body, computed, is_static)`
pub fn method_with(kind: &str, key: Node, params: Vec<Node>, body: Vec<Node>, computed: bool, is_static: bool) -> Node {
    n(NodeKind::MethodDefinition(MethodDefinition {
        key: bx(key),
        value: bx(r#function(None, params, block(body))),
        kind: method_kind(kind),
        computed,
        is_static,
    }))
}

/// `b.function(id, params, body)`
pub fn r#function(id: impl Into<Option<Node>>, params: Vec<Node>, body: Node) -> Node {
    function_with(id, params, body, false)
}

/// `b.function(id, params, body, async)`
pub fn function_with(id: impl Into<Option<Node>>, params: Vec<Node>, body: Node, is_async: bool) -> Node {
    n(NodeKind::FunctionExpression(Function { id: opt(id), params, body: bx(body), generator: false, is_async }))
}

/// `b.if(test, consequent, alternate)`
pub fn r#if(test: Node, consequent: Node, alternate: impl Into<Option<Node>>) -> Node {
    n(NodeKind::IfStatement(IfStatement { test: bx(test), consequent: bx(consequent), alternate: opt(alternate) }))
}

/// `b.import_all(as, source)`: `import * as as from 'source'`
pub fn import_all(r#as: &str, source: &str) -> Node {
    n(NodeKind::ImportDeclaration(ImportDeclaration {
        specifiers: vec![import_namespace(r#as)],
        source: bx(literal(source)),
        attributes: Vec::new(),
    }))
}

/// `b.imports(parts, source)`: `import { a as b, ... } from 'source'`
pub fn imports(parts: &[(&str, &str)], source: &str) -> Node {
    n(NodeKind::ImportDeclaration(ImportDeclaration {
        specifiers: parts
            .iter()
            .map(|(imported, local)| {
                n(NodeKind::ImportSpecifier(ImportSpecifier { imported: bx(id(*imported)), local: bx(id(*local)) }))
            })
            .collect(),
        source: bx(literal(source)),
        attributes: Vec::new(),
    }))
}

/// `b.return(argument)`
pub fn r#return(argument: impl Into<Option<Node>>) -> Node {
    n(NodeKind::ReturnStatement(ReturnStatement { argument: opt(argument) }))
}

/// `b.throw_error(str)`: `throw new Error(str)`
pub fn throw_error(message: &str) -> Node {
    n(NodeKind::ThrowStatement(ThrowStatement { argument: bx(new("Error", vec![literal(message)])) }))
}

/// `regex_is_valid_identifier`: `/^[a-zA-Z_$][a-zA-Z_$0-9]*$/`
pub fn is_valid_identifier(name: &str) -> bool {
    let mut bytes = name.bytes();
    matches!(bytes.next(), Some(b'a'..=b'z' | b'A'..=b'Z' | b'_' | b'$'))
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'$')
}

/// `b.key(name)`: an identifier if `name` is a valid one, otherwise a string literal
pub fn key(name: &str) -> Node {
    if is_valid_identifier(name) { id(name) } else { literal(name) }
}

/// `has_await_expression` (`utils/ast.js`): whether `node` contains an `await` outside of
/// nested functions
pub fn has_await_expression(node: &Node) -> bool {
    match &node.kind {
        NodeKind::AwaitExpression(_) => true,
        NodeKind::FunctionDeclaration(_) | NodeKind::FunctionExpression(_) | NodeKind::ArrowFunctionExpression(_) => false,
        _ => {
            let mut found = false;
            node.for_each_child(&mut |child| {
                if !found && has_await_expression(child) {
                    found = true;
                }
            });
            found
        }
    }
}
