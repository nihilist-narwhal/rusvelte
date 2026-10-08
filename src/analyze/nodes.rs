//! A uniform view over the template AST and the oxc JS AST, shaped like the ESTree/Svelte
//! AST that Svelte's analysis walks with zimmerframe: [`P`] is a node (as it appears in a
//! `path`), [`each_child`] yields a node's children in the order zimmerframe visits them
//! (the key order of acorn's/Svelte's objects).
//!
//! TypeScript is invisible here, like after `remove_typescript_nodes`: type-only nodes are
//! skipped, `x as T`/`x!`/`x satisfies T`/`<T>x` are their expression, and parenthesized
//! expressions are their contents (Svelte's acorn doesn't keep parentheses).

use oxc_allocator::GetAddress;
use oxc_ast::AstKind;
use oxc_ast::ast::*;
use oxc_span::GetSpan;

use crate::ast::{self, Ast, Attr, AttrValue, Chunk, Expr, FragId, Node, NodeId, Pattern};

/// A node of the analysed tree
#[derive(Clone, Copy, Debug)]
pub enum P<'s> {
    Fragment(FragId),
    /// A Fragment `visit_component` builds for the children of one slot (index into a side table)
    SlotFragment(u32),
    Node(NodeId),
    Attr(&'s Attr<'s>),
    /// `Text`/`ExpressionTag` inside an attribute value
    Chunk(&'s Chunk<'s>),
    /// The `value` attribute Svelte makes out of a `<textarea>`'s dynamic children
    TextareaValue(NodeId),
    /// An Identifier or Literal Svelte builds itself (`Expr::Ident`/`Expr::Literal`)
    TplExpr(&'s Expr<'s>),
    /// `Pattern::Ident`
    PatIdent(&'s Pattern<'s>),
    /// The VariableDeclaration of a `{@const}` tag
    ConstDecl(NodeId),
    /// The VariableDeclarator of a `{@const}` tag
    ConstDeclarator(NodeId),
    Js(AstKind<'s>),
    /// The AssignmentPattern of a parameter with a default value
    ParamDefault(&'s FormalParameter<'s>),
    /// The AssignmentPattern in `({ a = 1 } = x)`
    AtpiDefault(&'s AssignmentTargetPropertyIdentifier<'s>),
    /// A statement `remove_typescript_nodes` replaced by an EmptyStatement
    Empty,
}

/// The address of the node a kind refers to (its identity). oxc's `Address` has no getter,
/// but its derived `Hash` writes the inner `usize`, which [`AddressHasher`] captures.
#[inline]
pub fn addr(kind: &AstKind) -> usize {
    use std::hash::Hash;
    let mut h = AddressHasher(0);
    kind.address().hash(&mut h);
    h.0
}

/// A `Hasher` that keeps the last `usize` written to it
struct AddressHasher(usize);

impl std::hash::Hasher for AddressHasher {
    #[inline]
    fn write_usize(&mut self, n: usize) {
        self.0 = n;
    }
    fn write(&mut self, _: &[u8]) {
        unreachable!("`Address` hashes as a single usize")
    }
    fn finish(&self) -> u64 {
        self.0 as u64
    }
}

#[inline]
fn ptr<T>(r: &T) -> usize {
    r as *const T as usize
}

impl<'s> P<'s> {
    /// Identity (what `===` compares in the JS)
    pub fn key(&self) -> usize {
        match *self {
            P::Fragment(f) => f << 3 | 1,
            P::SlotFragment(i) => (i as usize) << 3 | 2,
            P::Node(n) => n << 3 | 3,
            P::Attr(a) => ptr(a),
            P::Chunk(c) => ptr(c),
            P::TextareaValue(n) => n << 3 | 4,
            P::TplExpr(e) => ptr(e),
            P::PatIdent(p) => ptr(p),
            P::ConstDecl(n) => n << 3 | 5,
            P::ConstDeclarator(n) => n << 3 | 6,
            P::Js(k) => addr(&k),
            P::ParamDefault(p) => ptr(p) + 1,
            P::AtpiDefault(p) => ptr(p) + 1,
            P::Empty => 7,
        }
    }

    pub fn is(&self, other: P) -> bool {
        self.key() == other.key()
    }

    /// The ESTree / Svelte AST `type`
    pub fn ty(&self, ast: &Ast) -> &'static str {
        match *self {
            P::Fragment(_) | P::SlotFragment(_) => "Fragment",
            P::Node(n) => ast.nodes[n].type_name(),
            P::Attr(a) => a.type_name(),
            P::Chunk(Chunk::Text { .. }) => "Text",
            P::Chunk(Chunk::Expression { .. }) => "ExpressionTag",
            P::TextareaValue(_) => "Attribute",
            P::TplExpr(Expr::Literal { .. }) => "Literal",
            P::TplExpr(_) | P::PatIdent(_) => "Identifier",
            P::ConstDecl(_) => "VariableDeclaration",
            P::ConstDeclarator(_) => "VariableDeclarator",
            P::Js(k) => js_type(k),
            P::ParamDefault(_) | P::AtpiDefault(_) => "AssignmentPattern",
            P::Empty => "EmptyStatement",
        }
    }

    /// `start`, if the node has one
    pub fn start(&self, ast: &Ast) -> Option<usize> {
        self.span(ast).map(|s| s.0)
    }

    /// `[start, end]`, if the node has them
    pub fn span(&self, ast: &Ast) -> Option<(usize, usize)> {
        Some(match *self {
            P::Fragment(_) | P::SlotFragment(_) | P::Empty => return None,
            P::Node(n) => (ast.nodes[n].start(), ast.nodes[n].end().unwrap_or(usize::MAX)),
            P::Attr(a) => (a.start(), a.end()),
            P::Chunk(c) => match c {
                Chunk::Text { start, end, .. } | Chunk::Expression { start, end, .. } => (*start, *end),
            },
            P::TextareaValue(_) => return None,
            P::TplExpr(e) => (e.start(), e.end()),
            P::PatIdent(Pattern::Ident { start, end, .. }) => (*start, *end),
            P::PatIdent(_) => return None,
            P::ConstDecl(n) => match &ast.nodes[n] {
                Node::ConstTag { start, end, .. } => (start + 2, end - 1),
                _ => return None,
            },
            P::ConstDeclarator(n) => match &ast.nodes[n] {
                Node::ConstTag { id, declarator_end, .. } => (id.start(), *declarator_end),
                _ => return None,
            },
            P::Js(k) => estree_span(k),
            P::ParamDefault(p) => (p.span.start as usize, p.span.end as usize),
            P::AtpiDefault(p) => (p.span.start as usize, p.span.end as usize),
        })
    }

    /// The oxc node, for JS nodes
    pub fn js_kind(&self) -> Option<AstKind<'s>> {
        match *self {
            P::Js(k) => Some(k),
            _ => None,
        }
    }

    pub fn node(&self) -> Option<NodeId> {
        match *self {
            P::Node(n) => Some(n),
            _ => None,
        }
    }
}

/// ESTree `start`/`end` of an oxc node
pub fn estree_span(k: AstKind) -> (usize, usize) {
    let span = k.span();
    match k {
        // TS-ESTree includes the delimiters in a quasi's range, acorn doesn't
        AstKind::TemplateElement(t) => {
            let _ = t;
            (span.start as usize, span.end as usize)
        }
        _ => (span.start as usize, span.end as usize),
    }
}

pub fn js_type(k: AstKind) -> &'static str {
    use AstKind as K;
    match k {
        K::Program(_) => "Program",
        K::IdentifierName(_) | K::IdentifierReference(_) | K::BindingIdentifier(_) | K::LabelIdentifier(_) => {
            "Identifier"
        }
        K::ThisExpression(_) => "ThisExpression",
        K::ArrayExpression(_) => "ArrayExpression",
        K::ObjectExpression(_) => "ObjectExpression",
        K::ObjectProperty(_)
        | K::BindingProperty(_)
        | K::AssignmentTargetPropertyIdentifier(_)
        | K::AssignmentTargetPropertyProperty(_) => "Property",
        K::TemplateLiteral(_) => "TemplateLiteral",
        K::TaggedTemplateExpression(_) => "TaggedTemplateExpression",
        K::TemplateElement(_) => "TemplateElement",
        K::ComputedMemberExpression(_) | K::StaticMemberExpression(_) | K::PrivateFieldExpression(_) => {
            "MemberExpression"
        }
        K::CallExpression(_) => "CallExpression",
        K::NewExpression(_) => "NewExpression",
        K::ImportMeta(_) | K::NewTarget(_) => "MetaProperty",
        K::SpreadElement(_) => "SpreadElement",
        K::UpdateExpression(_) => "UpdateExpression",
        K::UnaryExpression(_) => "UnaryExpression",
        K::BinaryExpression(_) | K::PrivateInExpression(_) => "BinaryExpression",
        K::LogicalExpression(_) => "LogicalExpression",
        K::ConditionalExpression(_) => "ConditionalExpression",
        K::AssignmentExpression(_) => "AssignmentExpression",
        K::ArrayAssignmentTarget(_) | K::ArrayPattern(_) => "ArrayPattern",
        K::ObjectAssignmentTarget(_) | K::ObjectPattern(_) => "ObjectPattern",
        K::AssignmentTargetRest(_) | K::BindingRestElement(_) => "RestElement",
        K::AssignmentTargetWithDefault(_) | K::AssignmentPattern(_) => "AssignmentPattern",
        K::SequenceExpression(_) => "SequenceExpression",
        K::Super(_) => "Super",
        K::AwaitExpression(_) => "AwaitExpression",
        K::ChainExpression(_) => "ChainExpression",
        K::ParenthesizedExpression(_) => "ParenthesizedExpression",
        K::BlockStatement(_) | K::FunctionBody(_) => "BlockStatement",
        K::VariableDeclaration(_) => "VariableDeclaration",
        K::VariableDeclarator(_) => "VariableDeclarator",
        K::EmptyStatement(_) => "EmptyStatement",
        K::ExpressionStatement(_) => "ExpressionStatement",
        K::IfStatement(_) => "IfStatement",
        K::DoWhileStatement(_) => "DoWhileStatement",
        K::WhileStatement(_) => "WhileStatement",
        K::ForStatement(_) => "ForStatement",
        K::ForInStatement(_) => "ForInStatement",
        K::ForOfStatement(_) => "ForOfStatement",
        K::ContinueStatement(_) => "ContinueStatement",
        K::BreakStatement(_) => "BreakStatement",
        K::ReturnStatement(_) => "ReturnStatement",
        K::WithStatement(_) => "WithStatement",
        K::SwitchStatement(_) => "SwitchStatement",
        K::SwitchCase(_) => "SwitchCase",
        K::LabeledStatement(_) => "LabeledStatement",
        K::ThrowStatement(_) => "ThrowStatement",
        K::TryStatement(_) => "TryStatement",
        K::CatchClause(_) => "CatchClause",
        K::DebuggerStatement(_) => "DebuggerStatement",
        K::Function(f) => {
            if f.is_expression() {
                "FunctionExpression"
            } else {
                "FunctionDeclaration"
            }
        }
        K::ArrowFunctionExpression(_) => "ArrowFunctionExpression",
        K::YieldExpression(_) => "YieldExpression",
        K::Class(c) => {
            if c.is_expression() {
                "ClassExpression"
            } else {
                "ClassDeclaration"
            }
        }
        K::ClassBody(_) => "ClassBody",
        K::MethodDefinition(_) => "MethodDefinition",
        K::PropertyDefinition(_) | K::AccessorProperty(_) => "PropertyDefinition",
        K::PrivateIdentifier(_) => "PrivateIdentifier",
        K::StaticBlock(_) => "StaticBlock",
        K::ImportExpression(_) => "ImportExpression",
        K::ImportDeclaration(_) => "ImportDeclaration",
        K::ImportSpecifier(_) => "ImportSpecifier",
        K::ImportDefaultSpecifier(_) => "ImportDefaultSpecifier",
        K::ImportNamespaceSpecifier(_) => "ImportNamespaceSpecifier",
        K::ImportAttribute(_) => "ImportAttribute",
        K::ExportDeclaration(_) | K::ExportNamedDeclaration(_) | K::ExportFromDeclaration(_) => {
            "ExportNamedDeclaration"
        }
        K::ExportDefaultDeclaration(_) => "ExportDefaultDeclaration",
        K::ExportAllDeclaration(_) => "ExportAllDeclaration",
        K::ExportSpecifier(_) => "ExportSpecifier",
        K::BooleanLiteral(_)
        | K::NullLiteral(_)
        | K::NumericLiteral(_)
        | K::StringLiteral(_)
        | K::BigIntLiteral(_)
        | K::RegExpLiteral(_) => "Literal",
        K::Decorator(_) => "Decorator",
        _ => "TSNode",
    }
}

// ---------------------------------------------------------------------------------------
// oxc enums → P (with TS wrappers and parentheses removed)

/// Strip parentheses and TS expression wrappers
pub fn strip<'s>(mut e: &'s Expression<'s>) -> &'s Expression<'s> {
    loop {
        e = match e {
            Expression::ParenthesizedExpression(p) => &p.expression,
            Expression::TSAsExpression(x) => &x.expression,
            Expression::TSSatisfiesExpression(x) => &x.expression,
            Expression::TSNonNullExpression(x) => &x.expression,
            Expression::TSTypeAssertion(x) => &x.expression,
            Expression::TSInstantiationExpression(x) => &x.expression,
            _ => return e,
        }
    }
}

pub fn expr<'s>(e: &'s Expression<'s>) -> P<'s> {
    P::Js(AstKind::from_expression(strip(e)))
}

pub fn template_expr<'s>(e: &'s Expr<'s>) -> P<'s> {
    match e {
        Expr::Js(js) => expr(js.effective_root()),
        _ => P::TplExpr(e),
    }
}

pub fn member<'s>(m: &'s MemberExpression<'s>) -> P<'s> {
    P::Js(match m {
        MemberExpression::ComputedMemberExpression(x) => AstKind::ComputedMemberExpression(x),
        MemberExpression::StaticMemberExpression(x) => AstKind::StaticMemberExpression(x),
        MemberExpression::PrivateFieldExpression(x) => AstKind::PrivateFieldExpression(x),
    })
}

pub fn simple_target<'s>(t: &'s SimpleAssignmentTarget<'s>) -> P<'s> {
    match t {
        SimpleAssignmentTarget::AssignmentTargetIdentifier(id) => P::Js(AstKind::IdentifierReference(id)),
        SimpleAssignmentTarget::TSAsExpression(x) => expr(&x.expression),
        SimpleAssignmentTarget::TSSatisfiesExpression(x) => expr(&x.expression),
        SimpleAssignmentTarget::TSNonNullExpression(x) => expr(&x.expression),
        SimpleAssignmentTarget::TSTypeAssertion(x) => expr(&x.expression),
        _ => member(t.as_member_expression().unwrap()),
    }
}

pub fn target<'s>(t: &'s AssignmentTarget<'s>) -> P<'s> {
    match t {
        AssignmentTarget::ArrayAssignmentTarget(x) => P::Js(AstKind::ArrayAssignmentTarget(x)),
        AssignmentTarget::ObjectAssignmentTarget(x) => P::Js(AstKind::ObjectAssignmentTarget(x)),
        _ => simple_target(t.as_simple_assignment_target().unwrap()),
    }
}

pub fn target_maybe_default<'s>(t: &'s AssignmentTargetMaybeDefault<'s>) -> P<'s> {
    match t {
        AssignmentTargetMaybeDefault::AssignmentTargetWithDefault(x) => P::Js(AstKind::AssignmentTargetWithDefault(x)),
        _ => target(t.as_assignment_target().unwrap()),
    }
}

pub fn binding<'s>(b: &'s BindingPattern<'s>) -> P<'s> {
    P::Js(match b {
        BindingPattern::BindingIdentifier(x) => AstKind::BindingIdentifier(x),
        BindingPattern::ObjectPattern(x) => AstKind::ObjectPattern(x),
        BindingPattern::ArrayPattern(x) => AstKind::ArrayPattern(x),
        BindingPattern::AssignmentPattern(x) => AstKind::AssignmentPattern(x),
    })
}

pub fn param<'s>(p: &'s FormalParameter<'s>) -> P<'s> {
    if p.initializer.is_some() { P::ParamDefault(p) } else { binding(&p.pattern) }
}

pub fn property_key<'s>(k: &'s PropertyKey<'s>) -> P<'s> {
    match k {
        PropertyKey::StaticIdentifier(x) => P::Js(AstKind::IdentifierName(x)),
        PropertyKey::PrivateIdentifier(x) => P::Js(AstKind::PrivateIdentifier(x)),
        _ => expr(k.as_expression().unwrap()),
    }
}

pub fn argument<'s>(a: &'s Argument<'s>) -> P<'s> {
    match a {
        Argument::SpreadElement(x) => P::Js(AstKind::SpreadElement(x)),
        _ => expr(a.as_expression().unwrap()),
    }
}

pub fn module_export_name<'s>(n: &'s ModuleExportName<'s>) -> P<'s> {
    P::Js(match n {
        ModuleExportName::IdentifierName(x) => AstKind::IdentifierName(x),
        ModuleExportName::IdentifierReference(x) => AstKind::IdentifierReference(x),
        ModuleExportName::StringLiteral(x) => AstKind::StringLiteral(x),
    })
}

pub fn function_p<'s>(f: &'s Function<'s>) -> P<'s> {
    P::Js(AstKind::Function(f))
}

/// A statement, or `None` for statements that are removed entirely (never, currently:
/// removed statements become `P::Empty`)
pub fn statement<'s>(s: &'s Statement<'s>) -> P<'s> {
    use Statement as S;
    P::Js(match s {
        S::BlockStatement(x) => AstKind::BlockStatement(x),
        S::BreakStatement(x) => AstKind::BreakStatement(x),
        S::ContinueStatement(x) => AstKind::ContinueStatement(x),
        S::DebuggerStatement(x) => AstKind::DebuggerStatement(x),
        S::DoWhileStatement(x) => AstKind::DoWhileStatement(x),
        S::EmptyStatement(x) => AstKind::EmptyStatement(x),
        S::ExpressionStatement(x) => AstKind::ExpressionStatement(x),
        S::ForInStatement(x) => AstKind::ForInStatement(x),
        S::ForOfStatement(x) => AstKind::ForOfStatement(x),
        S::ForStatement(x) => AstKind::ForStatement(x),
        S::IfStatement(x) => AstKind::IfStatement(x),
        S::LabeledStatement(x) => AstKind::LabeledStatement(x),
        S::ReturnStatement(x) => AstKind::ReturnStatement(x),
        S::SwitchStatement(x) => AstKind::SwitchStatement(x),
        S::ThrowStatement(x) => AstKind::ThrowStatement(x),
        S::TryStatement(x) => AstKind::TryStatement(x),
        S::WhileStatement(x) => AstKind::WhileStatement(x),
        S::WithStatement(x) => AstKind::WithStatement(x),
        S::VariableDeclaration(x) => {
            if x.declare {
                return P::Empty;
            }
            AstKind::VariableDeclaration(x)
        }
        S::FunctionDeclaration(x) => {
            if x.declare || x.body.is_none() {
                return P::Empty;
            }
            AstKind::Function(x)
        }
        S::ClassDeclaration(x) => {
            if x.declare {
                return P::Empty;
            }
            AstKind::Class(x)
        }
        S::ImportDeclaration(x) => {
            if x.import_kind.is_type() {
                return P::Empty;
            }
            if let Some(specs) = &x.specifiers {
                if !specs.is_empty() && specs.iter().all(is_type_specifier) {
                    return P::Empty;
                }
            }
            AstKind::ImportDeclaration(x)
        }
        S::ExportAllDeclaration(x) => {
            if x.export_kind.is_type() {
                return P::Empty;
            }
            AstKind::ExportAllDeclaration(x)
        }
        S::ExportDefaultDeclaration(x) => {
            if matches!(x.declaration, ExportDefaultDeclarationKind::TSInterfaceDeclaration(_)) {
                return P::Empty;
            }
            AstKind::ExportDefaultDeclaration(x)
        }
        S::ExportDeclaration(x) => {
            if export_declaration_removed(&x.declaration) {
                return P::Empty;
            }
            AstKind::ExportDeclaration(x)
        }
        S::ExportNamedDeclaration(x) => {
            if x.export_kind.is_type() || (!x.specifiers.is_empty() && x.specifiers.iter().all(|s| s.export_kind.is_type())) {
                return P::Empty;
            }
            AstKind::ExportNamedDeclaration(x)
        }
        S::ExportFromDeclaration(x) => {
            if x.export_kind.is_type() || (!x.specifiers.is_empty() && x.specifiers.iter().all(|s| s.export_kind.is_type())) {
                return P::Empty;
            }
            AstKind::ExportFromDeclaration(x)
        }
        // TS declarations (enums/namespaces were rejected before analysis)
        _ => return P::Empty,
    })
}

pub fn is_type_specifier(s: &ImportDeclarationSpecifier) -> bool {
    matches!(s, ImportDeclarationSpecifier::ImportSpecifier(s) if s.import_kind.is_type())
}

fn export_declaration_removed(d: &Declaration) -> bool {
    match d {
        Declaration::VariableDeclaration(x) => x.declare,
        Declaration::FunctionDeclaration(x) => x.declare || x.body.is_none(),
        Declaration::ClassDeclaration(x) => x.declare,
        _ => true,
    }
}

pub fn declaration<'s>(d: &'s Declaration<'s>) -> P<'s> {
    P::Js(match d {
        Declaration::VariableDeclaration(x) => AstKind::VariableDeclaration(x),
        Declaration::FunctionDeclaration(x) => AstKind::Function(x),
        Declaration::ClassDeclaration(x) => AstKind::Class(x),
        _ => return P::Empty,
    })
}

// ---------------------------------------------------------------------------------------
// children

/// Something that yields children: `f(cx, child)` for each, stopping at the first error
pub type Res = crate::error::Result<()>;

macro_rules! go {
    ($cx:ident, $f:ident, $p:expr) => {
        $f($cx, $p)?
    };
}

/// Visit the children of `p` in zimmerframe order
pub fn each_child<'s, C, F>(p: P<'s>, ast: &'s Ast<'s>, cx: &mut C, f: &mut F) -> Res
where
    F: FnMut(&mut C, P<'s>) -> Res,
{
    match p {
        P::Fragment(id) => {
            for &n in &ast.fragments[id].nodes {
                go!(cx, f, P::Node(n));
            }
        }
        P::SlotFragment(_) | P::Empty | P::PatIdent(_) | P::TplExpr(_) => {}
        P::Node(id) => node_children(&ast.nodes[id], ast, cx, f)?,
        P::Attr(a) => match a {
            Attr::Attribute { value, .. } | Attr::StyleDirective { value, .. } => attr_value_children(value, cx, f)?,
            Attr::Spread { expression, .. } | Attr::Attach { expression, .. } => go!(cx, f, template_expr(expression)),
            Attr::Directive { expression, .. } => {
                if let Some(e) = expression {
                    go!(cx, f, template_expr(e));
                }
            }
        },
        P::Chunk(c) => {
            if let Chunk::Expression { expression, .. } = c {
                go!(cx, f, template_expr(expression));
            }
        }
        P::TextareaValue(id) => {
            if let Node::Element(el) = &ast.nodes[id] {
                for &n in &ast.fragments[el.fragment].nodes {
                    go!(cx, f, P::Node(n));
                }
            }
        }
        P::ConstDecl(id) => go!(cx, f, P::ConstDeclarator(id)),
        P::ConstDeclarator(id) => {
            if let Node::ConstTag { id: pattern, init, .. } = &ast.nodes[id] {
                go!(cx, f, pattern_p(pattern));
                go!(cx, f, template_expr(init));
            }
        }
        P::ParamDefault(p) => {
            go!(cx, f, binding(&p.pattern));
            go!(cx, f, expr(p.initializer.as_ref().unwrap()));
        }
        P::AtpiDefault(p) => {
            go!(cx, f, P::Js(AstKind::IdentifierReference(&p.binding)));
            go!(cx, f, expr(p.init.as_ref().unwrap()));
        }
        P::Js(k) => js_children(k, cx, f)?,
    }
    Ok(())
}

/// A pattern of the template AST (`{#each}` context, `{:then}` value, `{@const}` id)
pub fn pattern_p<'s>(p: &'s Pattern<'s>) -> P<'s> {
    match p {
        Pattern::Ident { .. } => P::PatIdent(p),
        Pattern::Destructure { assign, .. } => match assign.inner() {
            Expression::AssignmentExpression(a) => target(&a.left),
            other => expr(other),
        },
    }
}

fn attr_value_children<'s, C, F>(value: &'s AttrValue<'s>, cx: &mut C, f: &mut F) -> Res
where
    F: FnMut(&mut C, P<'s>) -> Res,
{
    match value {
        AttrValue::True => {}
        AttrValue::Expression(c) => go!(cx, f, P::Chunk(c)),
        AttrValue::Sequence(chunks) => {
            for c in chunks {
                go!(cx, f, P::Chunk(c));
            }
        }
    }
    Ok(())
}

/// The parameters of a snippet, as ESTree patterns
pub fn snippet_params<'s>(parameters: &'s Option<crate::js::JsExpr<'s>>) -> Vec<P<'s>> {
    let mut out = Vec::new();
    if let Some(arrow) = parameters {
        if let Expression::ArrowFunctionExpression(a) = &arrow.expr {
            for p in &a.params.items {
                out.push(param(p));
            }
            if let Some(rest) = &a.params.rest {
                out.push(P::Js(AstKind::BindingRestElement(&rest.rest)));
            }
        }
    }
    out
}

fn node_children<'s, C, F>(node: &'s Node<'s>, ast: &'s Ast<'s>, cx: &mut C, f: &mut F) -> Res
where
    F: FnMut(&mut C, P<'s>) -> Res,
{
    match node {
        Node::Text { .. } | Node::Comment { .. } => {}
        Node::ExpressionTag { expression, .. } | Node::HtmlTag { expression, .. } | Node::RenderTag { expression, .. } => {
            go!(cx, f, template_expr(expression))
        }
        Node::DebugTag { identifiers, .. } => match identifiers {
            ast::DebugArgs::All => {}
            ast::DebugArgs::One(e) => go!(cx, f, template_expr(e)),
            ast::DebugArgs::Sequence(e) => {
                if let Expr::Js(js) = e {
                    if let Expression::SequenceExpression(seq) = js.inner() {
                        for e in &seq.expressions {
                            go!(cx, f, expr(e));
                        }
                    }
                }
            }
        },
        Node::ConstTag { .. } => {
            // the `declaration` key
            let id = node_id_of(ast, node);
            go!(cx, f, P::ConstDecl(id));
        }
        Node::DeclarationTag { declaration, .. } => {
            if let ast::Declaration::Js(stmt) = declaration {
                go!(cx, f, statement(&stmt.stmt));
            }
        }
        Node::IfBlock { test, consequent, alternate, .. } => {
            go!(cx, f, template_expr(test));
            go!(cx, f, P::Fragment(*consequent));
            if let Some(a) = alternate {
                go!(cx, f, P::Fragment(*a));
            }
        }
        Node::EachBlock { expression, context, body, fallback, key, .. } => {
            go!(cx, f, template_expr(expression));
            go!(cx, f, P::Fragment(*body));
            if let Some(c) = context {
                go!(cx, f, pattern_p(c));
            }
            if let Some(k) = key {
                go!(cx, f, template_expr(k));
            }
            if let Some(fb) = fallback {
                go!(cx, f, P::Fragment(*fb));
            }
        }
        Node::AwaitBlock { expression, value, error, pending, then, catch, .. } => {
            go!(cx, f, template_expr(expression));
            if let Some(v) = value {
                go!(cx, f, pattern_p(v));
            }
            if let Some(e) = error {
                go!(cx, f, pattern_p(e));
            }
            for frag in [pending, then, catch].into_iter().flatten() {
                go!(cx, f, P::Fragment(*frag));
            }
        }
        Node::KeyBlock { expression, fragment, .. } => {
            go!(cx, f, template_expr(expression));
            go!(cx, f, P::Fragment(*fragment));
        }
        Node::SnippetBlock { expression, parameters, body, .. } => {
            go!(cx, f, template_expr(expression));
            for p in snippet_params(parameters) {
                go!(cx, f, p);
            }
            go!(cx, f, P::Fragment(*body));
        }
        Node::Element(el) => {
            for a in &el.attributes {
                go!(cx, f, P::Attr(a));
            }
            go!(cx, f, P::Fragment(el.fragment));
            if let Some(t) = &el.tag {
                go!(cx, f, template_expr(t));
            }
            if let Some(e) = &el.expression {
                go!(cx, f, template_expr(e));
            }
        }
    }
    Ok(())
}

/// The id of a node, given a reference into `ast.nodes`
pub fn node_id_of(ast: &Ast, node: &Node) -> NodeId {
    let base = ast.nodes.as_ptr() as usize;
    (node as *const Node as usize - base) / std::mem::size_of::<Node>()
}

fn js_children<'s, C, F>(k: AstKind<'s>, cx: &mut C, f: &mut F) -> Res
where
    F: FnMut(&mut C, P<'s>) -> Res,
{
    use AstKind as K;
    match k {
        K::Program(p) => {
            let ts = p.source_type.is_typescript();
            for s in &p.body {
                // `remove_typescript_nodes` removes an export whose specifiers (once the type
                // ones are filtered out) are none, even `export {}`
                let empty_export = match s {
                    Statement::ExportNamedDeclaration(x) => x.specifiers.is_empty(),
                    Statement::ExportFromDeclaration(x) => x.specifiers.is_empty(),
                    _ => false,
                };
                go!(cx, f, if ts && empty_export { P::Empty } else { statement(s) });
            }
        }
        K::BlockStatement(b) => {
            for s in &b.body {
                go!(cx, f, statement(s));
            }
        }
        K::StaticBlock(b) => {
            for s in &b.body {
                go!(cx, f, statement(s));
            }
        }
        K::ExpressionStatement(s) => go!(cx, f, expr(&s.expression)),
        K::WithStatement(s) => {
            go!(cx, f, expr(&s.object));
            go!(cx, f, statement(&s.body));
        }
        K::ReturnStatement(s) => {
            if let Some(a) = &s.argument {
                go!(cx, f, expr(a));
            }
        }
        K::LabeledStatement(s) => {
            go!(cx, f, statement(&s.body));
            go!(cx, f, P::Js(K::LabelIdentifier(&s.label)));
        }
        K::BreakStatement(s) => {
            if let Some(l) = &s.label {
                go!(cx, f, P::Js(K::LabelIdentifier(l)));
            }
        }
        K::ContinueStatement(s) => {
            if let Some(l) = &s.label {
                go!(cx, f, P::Js(K::LabelIdentifier(l)));
            }
        }
        K::IfStatement(s) => {
            go!(cx, f, expr(&s.test));
            go!(cx, f, statement(&s.consequent));
            if let Some(a) = &s.alternate {
                go!(cx, f, statement(a));
            }
        }
        K::SwitchStatement(s) => {
            go!(cx, f, expr(&s.discriminant));
            for c in &s.cases {
                go!(cx, f, P::Js(K::SwitchCase(c)));
            }
        }
        K::SwitchCase(c) => {
            // acorn creates `consequent` before `test`
            for s in &c.consequent {
                go!(cx, f, statement(s));
            }
            if let Some(t) = &c.test {
                go!(cx, f, expr(t));
            }
        }
        K::ThrowStatement(s) => go!(cx, f, expr(&s.argument)),
        K::TryStatement(s) => {
            go!(cx, f, P::Js(K::BlockStatement(&s.block)));
            if let Some(h) = &s.handler {
                go!(cx, f, P::Js(K::CatchClause(h)));
            }
            if let Some(fin) = &s.finalizer {
                go!(cx, f, P::Js(K::BlockStatement(fin)));
            }
        }
        K::CatchClause(c) => {
            if let Some(p) = &c.param {
                go!(cx, f, binding(&p.pattern));
            }
            go!(cx, f, P::Js(K::BlockStatement(&c.body)));
        }
        K::WhileStatement(s) => {
            go!(cx, f, expr(&s.test));
            go!(cx, f, statement(&s.body));
        }
        K::DoWhileStatement(s) => {
            go!(cx, f, statement(&s.body));
            go!(cx, f, expr(&s.test));
        }
        K::ForStatement(s) => {
            if let Some(init) = &s.init {
                match init {
                    ForStatementInit::VariableDeclaration(d) => go!(cx, f, P::Js(K::VariableDeclaration(d))),
                    _ => go!(cx, f, expr(init.as_expression().unwrap())),
                }
            }
            if let Some(t) = &s.test {
                go!(cx, f, expr(t));
            }
            if let Some(u) = &s.update {
                go!(cx, f, expr(u));
            }
            go!(cx, f, statement(&s.body));
        }
        K::ForInStatement(s) => {
            go!(cx, f, for_left(&s.left));
            go!(cx, f, expr(&s.right));
            go!(cx, f, statement(&s.body));
        }
        K::ForOfStatement(s) => {
            go!(cx, f, for_left(&s.left));
            go!(cx, f, expr(&s.right));
            go!(cx, f, statement(&s.body));
        }
        K::Function(func) => {
            if let Some(id) = &func.id {
                go!(cx, f, P::Js(K::BindingIdentifier(id)));
            }
            params_children(&func.params, cx, f)?;
            if let Some(body) = &func.body {
                go!(cx, f, P::Js(K::FunctionBody(body)));
            }
        }
        // a function's `body` is a BlockStatement in ESTree
        K::FunctionBody(b) => {
            for s in &b.statements {
                go!(cx, f, statement(s));
            }
        }
        K::ArrowFunctionExpression(a) => {
            params_children(&a.params, cx, f)?;
            match &a.body {
                ArrowFunctionBody::FunctionBody(b) => go!(cx, f, P::Js(K::FunctionBody(b))),
                body => go!(cx, f, expr(body.as_expression().unwrap())),
            }
        }
        K::VariableDeclaration(d) => {
            for d in &d.declarations {
                go!(cx, f, P::Js(K::VariableDeclarator(d)));
            }
        }
        K::VariableDeclarator(d) => {
            go!(cx, f, binding(&d.id));
            if let Some(init) = &d.init {
                go!(cx, f, expr(init));
            }
        }
        K::Class(c) => {
            if let Some(id) = &c.id {
                go!(cx, f, P::Js(K::BindingIdentifier(id)));
            }
            if let Some(h) = &c.heritage {
                go!(cx, f, expr(&h.expression));
            }
            go!(cx, f, P::Js(K::ClassBody(&c.body)));
        }
        K::ClassBody(b) => {
            for el in &b.body {
                match el {
                    ClassElement::StaticBlock(x) => go!(cx, f, P::Js(K::StaticBlock(x))),
                    ClassElement::MethodDefinition(x) => {
                        if x.r#type == MethodDefinitionType::MethodDefinition {
                            go!(cx, f, P::Js(K::MethodDefinition(x)))
                        } else {
                            go!(cx, f, P::Empty)
                        }
                    }
                    ClassElement::PropertyDefinition(x) => {
                        if !x.declare && x.r#type == PropertyDefinitionType::PropertyDefinition {
                            go!(cx, f, P::Js(K::PropertyDefinition(x)))
                        }
                    }
                    ClassElement::AccessorProperty(x) => go!(cx, f, P::Js(K::AccessorProperty(x))),
                    ClassElement::TSIndexSignature(_) => {}
                }
            }
        }
        K::MethodDefinition(m) => {
            go!(cx, f, property_key(&m.key));
            go!(cx, f, function_p(&m.value));
        }
        K::PropertyDefinition(p) => {
            go!(cx, f, property_key(&p.key));
            if let Some(v) = &p.value {
                go!(cx, f, expr(v));
            }
        }
        K::AccessorProperty(p) => {
            go!(cx, f, property_key(&p.key));
            if let Some(v) = &p.value {
                go!(cx, f, expr(v));
            }
        }
        K::ArrayExpression(a) => {
            for el in &a.elements {
                match el {
                    ArrayExpressionElement::SpreadElement(s) => go!(cx, f, P::Js(K::SpreadElement(s))),
                    ArrayExpressionElement::Elision(_) => {}
                    _ => go!(cx, f, expr(el.as_expression().unwrap())),
                }
            }
        }
        K::ObjectExpression(o) => {
            for p in &o.properties {
                match p {
                    ObjectPropertyKind::ObjectProperty(p) => go!(cx, f, P::Js(K::ObjectProperty(p))),
                    ObjectPropertyKind::SpreadProperty(s) => go!(cx, f, P::Js(K::SpreadElement(s))),
                }
            }
        }
        K::ObjectProperty(p) => {
            go!(cx, f, property_key(&p.key));
            go!(cx, f, expr(&p.value));
        }
        K::SpreadElement(s) => go!(cx, f, expr(&s.argument)),
        K::UnaryExpression(u) => go!(cx, f, expr(&u.argument)),
        K::UpdateExpression(u) => go!(cx, f, simple_target(&u.argument)),
        K::BinaryExpression(b) => {
            go!(cx, f, expr(&b.left));
            go!(cx, f, expr(&b.right));
        }
        K::PrivateInExpression(b) => {
            go!(cx, f, P::Js(K::PrivateIdentifier(&b.left)));
            go!(cx, f, expr(&b.right));
        }
        K::LogicalExpression(b) => {
            go!(cx, f, expr(&b.left));
            go!(cx, f, expr(&b.right));
        }
        K::AssignmentExpression(a) => {
            go!(cx, f, target(&a.left));
            go!(cx, f, expr(&a.right));
        }
        K::ConditionalExpression(c) => {
            go!(cx, f, expr(&c.test));
            go!(cx, f, expr(&c.consequent));
            go!(cx, f, expr(&c.alternate));
        }
        K::CallExpression(c) => {
            go!(cx, f, expr(&c.callee));
            for a in &c.arguments {
                go!(cx, f, argument(a));
            }
        }
        K::NewExpression(c) => {
            go!(cx, f, expr(&c.callee));
            for a in &c.arguments {
                go!(cx, f, argument(a));
            }
        }
        K::SequenceExpression(s) => {
            for e in &s.expressions {
                go!(cx, f, expr(e));
            }
        }
        K::YieldExpression(y) => {
            if let Some(a) = &y.argument {
                go!(cx, f, expr(a));
            }
        }
        K::AwaitExpression(a) => go!(cx, f, expr(&a.argument)),
        K::TemplateLiteral(t) => template_literal_children(t, cx, f)?,
        K::TaggedTemplateExpression(t) => {
            go!(cx, f, expr(&t.tag));
            go!(cx, f, P::Js(K::TemplateLiteral(&t.quasi)));
        }
        K::StaticMemberExpression(m) => {
            go!(cx, f, expr(&m.object));
            go!(cx, f, P::Js(K::IdentifierName(&m.property)));
        }
        K::ComputedMemberExpression(m) => {
            go!(cx, f, expr(&m.object));
            go!(cx, f, expr(&m.expression));
        }
        K::PrivateFieldExpression(m) => {
            go!(cx, f, expr(&m.object));
            go!(cx, f, P::Js(K::PrivateIdentifier(&m.field)));
        }
        K::ChainExpression(c) => go!(cx, f, chain_element(&c.expression)),
        K::ImportExpression(i) => {
            go!(cx, f, expr(&i.source));
            if let Some(o) = &i.options {
                go!(cx, f, expr(o));
            }
        }
        K::ParenthesizedExpression(p) => go!(cx, f, expr(&p.expression)),
        // patterns
        K::ObjectPattern(o) => {
            for p in &o.properties {
                go!(cx, f, P::Js(K::BindingProperty(p)));
            }
            if let Some(r) = &o.rest {
                go!(cx, f, P::Js(K::BindingRestElement(r)));
            }
        }
        K::BindingProperty(p) => {
            go!(cx, f, property_key(&p.key));
            go!(cx, f, binding(&p.value));
        }
        K::ArrayPattern(a) => {
            for el in a.elements.iter().flatten() {
                go!(cx, f, binding(el));
            }
            if let Some(r) = &a.rest {
                go!(cx, f, P::Js(K::BindingRestElement(r)));
            }
        }
        K::BindingRestElement(r) => go!(cx, f, binding(&r.argument)),
        K::AssignmentPattern(a) => {
            go!(cx, f, binding(&a.left));
            go!(cx, f, expr(&a.right));
        }
        K::ObjectAssignmentTarget(o) => {
            for p in &o.properties {
                match p {
                    AssignmentTargetProperty::AssignmentTargetPropertyIdentifier(x) => {
                        go!(cx, f, P::Js(K::AssignmentTargetPropertyIdentifier(x)))
                    }
                    AssignmentTargetProperty::AssignmentTargetPropertyProperty(x) => {
                        go!(cx, f, P::Js(K::AssignmentTargetPropertyProperty(x)))
                    }
                }
            }
            if let Some(r) = &o.rest {
                go!(cx, f, P::Js(K::AssignmentTargetRest(r)));
            }
        }
        K::AssignmentTargetPropertyIdentifier(p) => {
            // shorthand: the key is a copy of the value (not a reference), so only the value matters
            if p.init.is_some() {
                go!(cx, f, P::AtpiDefault(p));
            } else {
                go!(cx, f, P::Js(K::IdentifierReference(&p.binding)));
            }
        }
        K::AssignmentTargetPropertyProperty(p) => {
            go!(cx, f, property_key(&p.name));
            go!(cx, f, target_maybe_default(&p.binding));
        }
        K::ArrayAssignmentTarget(a) => {
            for el in a.elements.iter().flatten() {
                go!(cx, f, target_maybe_default(el));
            }
            if let Some(r) = &a.rest {
                go!(cx, f, P::Js(K::AssignmentTargetRest(r)));
            }
        }
        K::AssignmentTargetRest(r) => go!(cx, f, target(&r.target)),
        K::AssignmentTargetWithDefault(d) => {
            go!(cx, f, target(&d.binding));
            go!(cx, f, expr(&d.init));
        }
        // modules
        K::ImportDeclaration(i) => {
            if let Some(specs) = &i.specifiers {
                for s in specs {
                    if is_type_specifier(s) {
                        continue;
                    }
                    go!(cx, f, P::Js(match s {
                        ImportDeclarationSpecifier::ImportSpecifier(x) => K::ImportSpecifier(x),
                        ImportDeclarationSpecifier::ImportDefaultSpecifier(x) => K::ImportDefaultSpecifier(x),
                        ImportDeclarationSpecifier::ImportNamespaceSpecifier(x) => K::ImportNamespaceSpecifier(x),
                    }));
                }
            }
            go!(cx, f, P::Js(K::StringLiteral(&i.source)));
        }
        K::ImportSpecifier(s) => {
            go!(cx, f, module_export_name(&s.imported));
            go!(cx, f, P::Js(K::BindingIdentifier(&s.local)));
        }
        K::ImportDefaultSpecifier(s) => go!(cx, f, P::Js(K::BindingIdentifier(&s.local))),
        K::ImportNamespaceSpecifier(s) => go!(cx, f, P::Js(K::BindingIdentifier(&s.local))),
        K::ExportDeclaration(e) => go!(cx, f, declaration(&e.declaration)),
        K::ExportNamedDeclaration(e) => {
            for s in &e.specifiers {
                if !s.export_kind.is_type() {
                    go!(cx, f, P::Js(K::ExportSpecifier(s)));
                }
            }
        }
        K::ExportFromDeclaration(e) => {
            for s in &e.specifiers {
                if !s.export_kind.is_type() {
                    go!(cx, f, P::Js(K::ExportSpecifier(s)));
                }
            }
            go!(cx, f, P::Js(K::StringLiteral(&e.source)));
        }
        K::ExportSpecifier(s) => {
            go!(cx, f, module_export_name(&s.local));
            go!(cx, f, module_export_name(&s.exported));
        }
        K::ExportDefaultDeclaration(e) => match &e.declaration {
            ExportDefaultDeclarationKind::FunctionDeclaration(x) => go!(cx, f, P::Js(K::Function(x))),
            ExportDefaultDeclarationKind::ClassDeclaration(x) => go!(cx, f, P::Js(K::Class(x))),
            ExportDefaultDeclarationKind::TSInterfaceDeclaration(_) => {}
            other => go!(cx, f, expr(other.as_expression().unwrap())),
        },
        K::ExportAllDeclaration(e) => {
            if let Some(x) = &e.exported {
                go!(cx, f, module_export_name(x));
            }
            go!(cx, f, P::Js(K::StringLiteral(&e.source)));
        }
        _ => {}
    }
    Ok(())
}

fn params_children<'s, C, F>(params: &'s FormalParameters<'s>, cx: &mut C, f: &mut F) -> Res
where
    F: FnMut(&mut C, P<'s>) -> Res,
{
    for (i, p) in params.items.iter().enumerate() {
        // `remove_this_param`: a leading `this` parameter is removed
        if i == 0 {
            if let BindingPattern::BindingIdentifier(id) = &p.pattern {
                if id.name == "this" {
                    continue;
                }
            }
        }
        go!(cx, f, param(p));
    }
    if let Some(rest) = &params.rest {
        go!(cx, f, P::Js(AstKind::BindingRestElement(&rest.rest)));
    }
    Ok(())
}

fn template_literal_children<'s, C, F>(t: &'s TemplateLiteral<'s>, cx: &mut C, f: &mut F) -> Res
where
    F: FnMut(&mut C, P<'s>) -> Res,
{
    // acorn creates `expressions` before `quasis`
    for e in &t.expressions {
        go!(cx, f, expr(e));
    }
    for q in &t.quasis {
        go!(cx, f, P::Js(AstKind::TemplateElement(q)));
    }
    Ok(())
}

fn for_left<'s>(l: &'s ForStatementLeft<'s>) -> P<'s> {
    match l {
        ForStatementLeft::VariableDeclaration(d) => P::Js(AstKind::VariableDeclaration(d)),
        _ => target(l.as_assignment_target().unwrap()),
    }
}

pub fn chain_element<'s>(c: &'s ChainElement<'s>) -> P<'s> {
    match c {
        ChainElement::CallExpression(x) => P::Js(AstKind::CallExpression(x)),
        ChainElement::TSNonNullExpression(x) => expr(&x.expression),
        _ => member(c.as_member_expression().unwrap()),
    }
}

/// The children of `p`, collected (for code that needs indexing)
pub fn children<'s>(p: P<'s>, ast: &'s Ast<'s>) -> Vec<P<'s>> {
    let mut out = Vec::new();
    let _ = each_child(p, ast, &mut out, &mut |out: &mut Vec<P<'s>>, c| {
        out.push(c);
        Ok(())
    });
    out
}

#[cfg(test)]
mod tests {
    use super::addr;
    use oxc_ast::AstKind;
    use oxc_ast::ast::{Expression, Statement};

    fn ptr<T>(r: &T) -> usize {
        r as *const T as usize
    }

    /// `addr` must be the node's address (a change in oxc's `Address` would break identity)
    #[test]
    fn addr_is_the_node_address() {
        let alloc = oxc_allocator::Allocator::default();
        let source = "let a = 1; foo(a); x.y = 2;";
        let parsed = oxc_parser::Parser::new(&alloc, source, oxc_span::SourceType::mjs()).parse();
        let program = &parsed.program;
        let mut seen = Vec::new();
        let mut check = |kind: AstKind, ptr: usize| {
            assert_eq!(addr(&kind), ptr);
            assert!(!seen.contains(&ptr), "distinct nodes have distinct addresses");
            seen.push(ptr);
        };
        check(AstKind::Program(program), ptr(program));
        for stmt in &program.body {
            match stmt {
                Statement::VariableDeclaration(d) => {
                    check(AstKind::VariableDeclaration(d), ptr(&**d));
                    check(AstKind::VariableDeclarator(&d.declarations[0]), ptr(&d.declarations[0]));
                }
                Statement::ExpressionStatement(e) => {
                    check(AstKind::ExpressionStatement(e), ptr(&**e));
                    match &e.expression {
                        Expression::CallExpression(c) => {
                            check(AstKind::CallExpression(c), ptr(&**c));
                            let Expression::Identifier(id) = &c.callee else { unreachable!() };
                            check(AstKind::IdentifierReference(id), ptr(&**id));
                        }
                        Expression::AssignmentExpression(a) => check(AstKind::AssignmentExpression(a), ptr(&**a)),
                        _ => unreachable!(),
                    }
                }
                _ => unreachable!(),
            }
        }
        assert_eq!(seen.len(), 8);
    }
}
