//! periscopic's `analyze`, as svelte2tsx runs it on a root `{#snippet}` (a fake
//! `FunctionDeclaration` whose body is the snippet's template nodes): the names the snippet
//! uses from outside.

use std::collections::HashSet;

use oxc_ast::ast::*;
use oxc_ast::AstKind;
use oxc_ast_visit::{walk, Visit};

use crate::ast::{Ast, DebugArgs, Declaration as SvelteDeclaration, Expr, Node, Pattern};
use crate::js::JsExpr;
use crate::legacy::*;

struct Scope {
    parent: Option<usize>,
    block: bool,
    declarations: HashSet<String>,
    references: HashSet<String>,
}

pub struct Periscope<'m, 'a> {
    ast: &'m Ast<'a>,
    scopes: Vec<Scope>,
    current: usize,
    references: Vec<(usize, String)>,
    /// for each entered oxc node: whether it created a scope
    entered: Vec<bool>,
    parents: Vec<AstKind<'a>>,
}

impl<'m, 'a> Periscope<'m, 'a> {
    fn new(ast: &'m Ast<'a>) -> Self {
        Periscope {
            ast,
            scopes: vec![Scope { parent: None, block: false, declarations: HashSet::new(), references: HashSet::new() }],
            current: 0,
            references: Vec::new(),
            entered: Vec::new(),
            parents: Vec::new(),
        }
    }

    fn push(&mut self, block: bool) {
        self.scopes.push(Scope { parent: Some(self.current), block, declarations: HashSet::new(), references: HashSet::new() });
        self.current = self.scopes.len() - 1;
    }

    fn pop(&mut self) {
        self.current = self.scopes[self.current].parent.unwrap_or(0);
    }

    fn declare(&mut self, name: &str) {
        self.scopes[self.current].declarations.insert(name.to_string());
    }

    fn reference(&mut self, name: &str) {
        self.references.push((self.current, name.to_string()));
    }

    /// `Scope.add_declaration` for a VariableDeclaration (`var` goes up out of blocks)
    fn add_var_declaration(&mut self, v: &VariableDeclaration) {
        let mut scope = self.current;
        if v.kind == VariableDeclarationKind::Var {
            while self.scopes[scope].block {
                match self.scopes[scope].parent {
                    Some(p) => scope = p,
                    None => break,
                }
            }
        }
        for d in &v.declarations {
            let mut ids = Vec::new();
            crate::svelte2tsx::script::binding_names(&d.id, &mut ids);
            for id in ids {
                self.scopes[scope].declarations.insert(id.name.to_string());
            }
        }
    }

    fn declare_params(&mut self, params: &FormalParameters) {
        let mut ids = Vec::new();
        for p in &params.items {
            crate::svelte2tsx::script::binding_names(&p.pattern, &mut ids);
        }
        if let Some(rest) = &params.rest {
            crate::svelte2tsx::script::binding_names(&rest.rest.argument, &mut ids);
        }
        for id in ids {
            self.declare(&id.name);
        }
    }

    fn find_owner(&self, mut scope: usize, name: &str) -> bool {
        loop {
            if self.scopes[scope].declarations.contains(name) {
                return true;
            }
            match self.scopes[scope].parent {
                Some(p) => scope = p,
                None => return false,
            }
        }
    }

    fn globals(mut self) -> HashSet<String> {
        let mut globals = HashSet::new();
        let refs = std::mem::take(&mut self.references);
        for (scope, name) in refs.iter().rev() {
            if self.scopes[*scope].references.contains(name) {
                continue;
            }
            let mut s = Some(*scope);
            while let Some(i) = s {
                self.scopes[i].references.insert(name.clone());
                s = self.scopes[i].parent;
            }
            if !self.find_owner(*scope, name) {
                globals.insert(name.clone());
            }
        }
        globals
    }

    // --- template nodes, in estree-walker's key order -----------------------------------

    fn expr(&mut self, e: &Expr<'a>) {
        match e {
            Expr::Js(js) => self.js(js),
            Expr::Ident { name, .. } => self.reference(name),
            Expr::Literal { .. } => {}
        }
    }

    fn js(&mut self, js: &JsExpr<'a>) {
        let e = js.inner();
        // SAFETY of lifetimes: the expression lives as long as the AST
        self.visit_expression(self.alloc(e));
    }

    fn pattern(&mut self, p: &Pattern<'a>) {
        match p {
            Pattern::Ident { name, type_ann, .. } => {
                self.reference(name);
                if let Some(t) = type_ann {
                    self.type_ann(t);
                }
            }
            Pattern::Destructure { assign, type_ann } => {
                if let Expression::AssignmentExpression(a) = assign.inner() {
                    let a = self.alloc(&**a);
                    self.visit_assignment_target(&a.left);
                }
                if let Some(t) = type_ann {
                    self.type_ann(t);
                }
            }
        }
    }

    fn type_ann(&mut self, t: &crate::ast::TypeAnn<'a>) {
        let mut e = t.expr.inner();
        if let Expression::SequenceExpression(seq) = e {
            e = seq.expressions[0].without_parentheses();
        }
        if let Expression::TSAsExpression(as_expr) = e {
            let ty = self.alloc(&as_expr.type_annotation);
            self.visit_ts_type(ty);
        }
    }

    fn children(&mut self, children: &[LNode<'_, 'a>]) {
        for c in children {
            self.node(c);
        }
    }

    fn node(&mut self, n: &LNode<'_, 'a>) {
        match n {
            LNode::Text(_) | LNode::Comment { .. } => {}
            LNode::MustacheTag { expression, .. } | LNode::RawMustacheTag { expression, .. } | LNode::RenderTag { expression, .. } => {
                self.expr(expression)
            }
            LNode::DebugTag { identifiers, .. } => match identifiers {
                DebugArgs::All => {}
                DebugArgs::One(e) => self.expr(e),
                DebugArgs::Sequence(Expr::Js(js)) => {
                    if let Expression::SequenceExpression(seq) = js.inner() {
                        let seq = self.alloc(&**seq);
                        for e in &seq.expressions {
                            self.visit_expression(e);
                        }
                    }
                }
                DebugArgs::Sequence(e) => self.expr(e),
            },
            LNode::ConstTag { id, init, .. } => {
                self.pattern(id);
                self.expr(init);
            }
            LNode::DeclarationTag { id, .. } => {
                if let Node::DeclarationTag { declaration: SvelteDeclaration::Js(stmt), .. } = &self.ast.nodes[*id] {
                    let s = self.alloc(&stmt.stmt);
                    self.visit_statement(s);
                }
            }
            LNode::IfBlock { expression, children, else_block, .. } => {
                self.expr(expression);
                self.children(children);
                if let Some(e) = else_block {
                    self.children(&e.children);
                }
            }
            LNode::EachBlock { children, context, expression, key, else_block, .. } => {
                self.children(children);
                if let Some(c) = context {
                    self.pattern(c);
                }
                self.expr(expression);
                if let Some(k) = key {
                    self.expr(k);
                }
                if let Some(e) = else_block {
                    self.children(&e.children);
                }
            }
            LNode::KeyBlock { expression, children, .. } => {
                self.expr(expression);
                self.children(children);
            }
            LNode::AwaitBlock { expression, value, error, pending, then, catch, .. } => {
                self.expr(expression);
                if let Some(v) = value {
                    self.pattern(v);
                }
                if let Some(e) = error {
                    self.pattern(e);
                }
                for b in [pending, then, catch] {
                    self.children(&b.children);
                }
            }
            LNode::SnippetBlock { expression, parameters, children, .. } => {
                // an unknown node to periscopic: nothing is declared
                self.expr(expression);
                if let Some(params) = parameters {
                    if let Expression::ArrowFunctionExpression(f) = &params.expr {
                        let f = self.alloc(&**f);
                        for p in &f.params.items {
                            self.visit_formal_parameter(p);
                        }
                        if let Some(rest) = &f.params.rest {
                            self.visit_formal_parameter_rest(rest);
                        }
                    }
                }
                self.children(children);
            }
            LNode::Element(el) => {
                if let Some(LTag::Expr(e)) = &el.tag {
                    self.expr(e);
                }
                if let Some(e) = el.expression {
                    self.expr(e);
                }
                for a in &el.attributes {
                    self.attribute(a);
                }
                if let Some(children) = &el.children {
                    self.children(children);
                }
            }
        }
    }

    fn attribute(&mut self, a: &LAttr<'_, 'a>) {
        match a {
            LAttr::Attribute { value: LAttrValue::Chunks(chunks), .. } => {
                for c in chunks {
                    match c {
                        LChunk::MustacheTag { expression, .. } | LChunk::AttributeShorthand { expression, .. } => self.expr(expression),
                        LChunk::Text(_) => {}
                    }
                }
            }
            LAttr::Attribute { .. } => {}
            LAttr::Other { attr, .. } => {
                use crate::ast::Attr;
                match attr {
                    Attr::Spread { expression, .. } | Attr::Attach { expression, .. } => self.expr(expression),
                    Attr::Directive { expression: Some(e), .. } => self.expr(e),
                    _ => {}
                }
            }
        }
    }
}

/// The globals of a root snippet
pub fn snippet_globals<'a>(ast: &Ast<'a>, expression: &Expr<'a>, parameters: Option<&JsExpr<'a>>, children: &[LNode<'_, 'a>]) -> HashSet<String> {
    let mut p = Periscope::new(ast);
    // FunctionDeclaration: the id is declared outside, the params inside
    let name = match expression {
        Expr::Js(js) => match js.inner() {
            Expression::Identifier(id) => Some(id.name.to_string()),
            _ => None,
        },
        Expr::Ident { name, .. } => Some(name.clone()),
        Expr::Literal { .. } => None,
    };
    if let Some(n) = &name {
        p.declare(n);
    }
    p.push(false);
    let arrow = parameters.and_then(|params| match &params.expr {
        Expression::ArrowFunctionExpression(f) => Some(&**f),
        _ => None,
    });
    if let Some(f) = arrow {
        p.declare_params(&f.params);
    }
    // id, params, body
    p.expr(expression);
    if let Some(f) = arrow {
        let f = p.alloc(f);
        for param in &f.params.items {
            p.visit_formal_parameter(param);
        }
        if let Some(rest) = &f.params.rest {
            p.visit_formal_parameter_rest(rest);
        }
    }
    p.push(true);
    p.children(children);
    p.globals()
}

impl<'a> Visit<'a> for Periscope<'_, 'a> {
    fn enter_node(&mut self, kind: AstKind<'a>) {
        let mut created = false;
        match kind {
            AstKind::Function(f) => {
                if f.r#type == FunctionType::FunctionDeclaration {
                    if let Some(id) = &f.id {
                        self.declare(&id.name);
                    }
                    self.push(false);
                } else {
                    self.push(false);
                    if f.r#type == FunctionType::FunctionExpression {
                        if let Some(id) = &f.id {
                            self.declare(&id.name);
                        }
                    }
                }
                self.declare_params(&f.params);
                created = true;
            }
            AstKind::ArrowFunctionExpression(f) => {
                self.push(false);
                self.declare_params(&f.params);
                created = true;
            }
            AstKind::ForStatement(_) | AstKind::ForInStatement(_) | AstKind::ForOfStatement(_) | AstKind::BlockStatement(_) => {
                self.push(true);
                created = true;
            }
            AstKind::FunctionBody(_) => {
                // a BlockStatement in ESTree
                self.push(true);
                created = true;
            }
            AstKind::Class(c) => {
                if c.r#type == ClassType::ClassDeclaration {
                    if let Some(id) = &c.id {
                        self.declare(&id.name);
                    }
                }
            }
            AstKind::VariableDeclaration(v) => self.add_var_declaration(v),
            AstKind::CatchClause(c) => {
                self.push(true);
                created = true;
                if let Some(param) = &c.param {
                    let mut ids = Vec::new();
                    crate::svelte2tsx::script::binding_names(&param.pattern, &mut ids);
                    for id in ids {
                        self.declare(&id.name);
                    }
                }
            }
            _ => {}
        }
        self.entered.push(created);
        self.parents.push(kind);
    }

    fn leave_node(&mut self, _kind: AstKind<'a>) {
        self.parents.pop();
        if self.entered.pop() == Some(true) {
            self.pop();
        }
    }

    fn visit_identifier_reference(&mut self, it: &IdentifierReference<'a>) {
        self.reference(&it.name);
    }

    fn visit_binding_identifier(&mut self, it: &BindingIdentifier<'a>) {
        // acorn-typescript's type parameter names are strings
        if !matches!(self.parents.last(), Some(AstKind::TSTypeParameter(_) | AstKind::TSMappedType(_))) {
            self.reference(&it.name);
        }
    }

    fn visit_identifier_name(&mut self, it: &IdentifierName<'a>) {
        let is_reference = !matches!(
            self.parents.last(),
            Some(
                AstKind::StaticMemberExpression(_)
                    | AstKind::ObjectProperty(_)
                    | AstKind::BindingProperty(_)
                    | AstKind::AssignmentTargetPropertyProperty(_)
                    | AstKind::MethodDefinition(_)
                    | AstKind::ImportSpecifier(_)
                    | AstKind::ExportSpecifier(_)
            )
        );
        if is_reference {
            self.reference(&it.name);
        }
    }

    fn visit_label_identifier(&mut self, _it: &LabelIdentifier<'a>) {}

    fn visit_ts_index_signature_name(&mut self, it: &TSIndexSignatureName<'a>) {
        self.reference(&it.name);
        walk::walk_ts_index_signature_name(self, it);
    }
}
