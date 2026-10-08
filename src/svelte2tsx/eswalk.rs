//! Walks oxc JS nodes and reports what svelte2tsx's `estree-walker` visitors look at, as if
//! walking the acorn AST: identifiers (with their ESTree parent/key), scope boundaries
//! (`BlockStatement`, `FunctionDeclaration`, `ArrowFunctionExpression`), declaration context
//! (`VariableDeclarator` ids and function params) and `await`s.

use oxc_ast::ast::*;
use oxc_ast::AstKind;
use oxc_ast_visit::{walk, Visit};
use oxc_span::GetSpan;

/// Where an identifier sits, in ESTree terms
#[derive(Debug, Clone, PartialEq)]
pub enum Ctx {
    Other,
    /// `MemberExpression.property`
    MemberProperty { computed: bool },
    /// `Property.key` (object literal or pattern)
    PropertyKey,
    /// `Property.value`
    PropertyValue,
    /// `CallExpression.callee` / `NewExpression.callee`, with the first argument if it's a literal
    Callee { first_arg: Option<Literal> },
}

/// The value of a literal first argument (`dispatch('name')`)
#[derive(Debug, Clone, PartialEq)]
pub enum Literal {
    String(String),
    Other,
}

pub trait EsHandler {
    fn identifier(&mut self, name: &str, start: usize, end: usize, ctx: &Ctx);
    fn scope_push(&mut self);
    fn scope_pop(&mut self);
    fn set_declaration(&mut self, value: bool);
    /// `in_function`: there's a function between the await and the template
    fn await_expression(&mut self, in_function: bool);
}

pub struct EsWalker<'h, H: EsHandler> {
    pub h: &'h mut H,
    ctx: Ctx,
    /// for each function-like node we're in: whether its params toggle the declaration flag
    /// (only FunctionDeclaration and ArrowFunctionExpression in svelte2tsx)
    params_owner: Vec<bool>,
    fn_depth: usize,
    /// `{#each}` rewrite: walk the TSAsExpression ending here as its expression
    pub strip_as_end: Option<u32>,
}

impl<'h, H: EsHandler> EsWalker<'h, H> {
    pub fn new(h: &'h mut H) -> Self {
        EsWalker { h, ctx: Ctx::Other, params_owner: Vec::new(), fn_depth: 0, strip_as_end: None }
    }

    fn ident(&mut self, name: &str, span: oxc_span::Span) {
        let ctx = std::mem::replace(&mut self.ctx, Ctx::Other);
        self.h.identifier(name, span.start as usize, span.end as usize, &ctx);
    }

    fn first_arg(args: &[Argument]) -> Option<Literal> {
        match args.first()? {
            Argument::StringLiteral(s) => Some(Literal::String(s.value.to_string())),
            Argument::NumericLiteral(_) | Argument::BooleanLiteral(_) | Argument::NullLiteral(_) | Argument::BigIntLiteral(_) | Argument::RegExpLiteral(_) => {
                Some(Literal::Other)
            }
            _ => None,
        }
    }

    /// Visit a `PropertyKey` in the key position
    fn key(&mut self, key: &PropertyKey<'_>) {
        self.ctx = Ctx::PropertyKey;
        match key {
            PropertyKey::StaticIdentifier(id) => self.ident(&id.name, id.span),
            other => self.visit_property_key(other),
        }
        self.ctx = Ctx::Other;
    }
}

impl<'a, H: EsHandler> Visit<'a> for EsWalker<'_, H> {
    fn enter_node(&mut self, kind: AstKind<'a>) {
        self.ctx = Ctx::Other;
        match kind {
            AstKind::BlockStatement(_) | AstKind::FunctionBody(_) => self.h.scope_push(),
            AstKind::Function(f) => {
                if f.r#type == FunctionType::FunctionDeclaration {
                    self.h.scope_push();
                }
                if matches!(f.r#type, FunctionType::FunctionDeclaration | FunctionType::FunctionExpression) {
                    self.fn_depth += 1;
                }
                self.params_owner.push(f.r#type == FunctionType::FunctionDeclaration);
            }
            AstKind::ArrowFunctionExpression(_) => {
                self.h.scope_push();
                self.fn_depth += 1;
                self.params_owner.push(true);
            }
            AstKind::TSFunctionType(_)
            | AstKind::TSConstructorType(_)
            | AstKind::TSCallSignatureDeclaration(_)
            | AstKind::TSConstructSignatureDeclaration(_)
            | AstKind::TSMethodSignature(_) => self.params_owner.push(false),
            AstKind::VariableDeclarator(_) => self.h.set_declaration(true),
            AstKind::AwaitExpression(_) => self.h.await_expression(self.fn_depth > 0),
            _ => {}
        }
    }

    fn leave_node(&mut self, kind: AstKind<'a>) {
        match kind {
            AstKind::BlockStatement(_) | AstKind::FunctionBody(_) => self.h.scope_pop(),
            AstKind::Function(f) => {
                if f.r#type == FunctionType::FunctionDeclaration {
                    self.h.scope_pop();
                }
                if matches!(f.r#type, FunctionType::FunctionDeclaration | FunctionType::FunctionExpression) {
                    self.fn_depth -= 1;
                }
                self.params_owner.pop();
            }
            AstKind::ArrowFunctionExpression(_) => {
                self.h.scope_pop();
                self.fn_depth -= 1;
                self.params_owner.pop();
            }
            AstKind::TSFunctionType(_)
            | AstKind::TSConstructorType(_)
            | AstKind::TSCallSignatureDeclaration(_)
            | AstKind::TSConstructSignatureDeclaration(_)
            | AstKind::TSMethodSignature(_) => {
                self.params_owner.pop();
            }
            _ => {}
        }
        self.ctx = Ctx::Other;
    }

    fn visit_identifier_name(&mut self, it: &IdentifierName<'a>) {
        self.ident(&it.name, it.span);
    }
    fn visit_identifier_reference(&mut self, it: &IdentifierReference<'a>) {
        self.ident(&it.name, it.span);
    }
    fn visit_binding_identifier(&mut self, it: &BindingIdentifier<'a>) {
        self.ident(&it.name, it.span);
    }
    fn visit_label_identifier(&mut self, it: &LabelIdentifier<'a>) {
        self.ident(&it.name, it.span);
    }

    fn visit_parenthesized_expression(&mut self, it: &ParenthesizedExpression<'a>) {
        // removed by Svelte's `remove_parens`: the inner expression takes its place
        self.visit_expression(&it.expression);
    }

    fn visit_ts_as_expression(&mut self, it: &TSAsExpression<'a>) {
        if self.strip_as_end == Some(it.span.end) {
            self.strip_as_end = None;
            self.visit_expression(&it.expression);
            return;
        }
        walk::walk_ts_as_expression(self, it);
    }

    fn visit_static_member_expression(&mut self, it: &StaticMemberExpression<'a>) {
        let kind = AstKind::StaticMemberExpression(self.alloc(it));
        self.enter_node(kind);
        self.visit_expression(&it.object);
        self.ctx = Ctx::MemberProperty { computed: false };
        self.visit_identifier_name(&it.property);
        self.leave_node(kind);
    }

    fn visit_computed_member_expression(&mut self, it: &ComputedMemberExpression<'a>) {
        let kind = AstKind::ComputedMemberExpression(self.alloc(it));
        self.enter_node(kind);
        self.visit_expression(&it.object);
        self.ctx = Ctx::MemberProperty { computed: true };
        self.visit_expression(&it.expression);
        self.ctx = Ctx::Other;
        self.leave_node(kind);
    }

    fn visit_call_expression(&mut self, it: &CallExpression<'a>) {
        let kind = AstKind::CallExpression(self.alloc(it));
        self.enter_node(kind);
        self.ctx = Ctx::Callee { first_arg: Self::first_arg(&it.arguments) };
        self.visit_expression(&it.callee);
        self.ctx = Ctx::Other;
        if let Some(t) = &it.type_arguments {
            self.visit_ts_type_parameter_instantiation(t);
        }
        self.ctx = Ctx::Other;
        self.visit_arguments(&it.arguments);
        self.leave_node(kind);
    }

    fn visit_new_expression(&mut self, it: &NewExpression<'a>) {
        let kind = AstKind::NewExpression(self.alloc(it));
        self.enter_node(kind);
        self.ctx = Ctx::Callee { first_arg: Self::first_arg(&it.arguments) };
        self.visit_expression(&it.callee);
        self.ctx = Ctx::Other;
        if let Some(t) = &it.type_arguments {
            self.visit_ts_type_parameter_instantiation(t);
        }
        self.ctx = Ctx::Other;
        self.visit_arguments(&it.arguments);
        self.leave_node(kind);
    }

    fn visit_object_property(&mut self, it: &ObjectProperty<'a>) {
        let kind = AstKind::ObjectProperty(self.alloc(it));
        self.enter_node(kind);
        self.key(&it.key);
        self.ctx = Ctx::PropertyValue;
        self.visit_expression(&it.value);
        self.ctx = Ctx::Other;
        self.leave_node(kind);
    }

    fn visit_binding_property(&mut self, it: &BindingProperty<'a>) {
        let kind = AstKind::BindingProperty(self.alloc(it));
        self.enter_node(kind);
        self.key(&it.key);
        self.ctx = Ctx::PropertyValue;
        self.visit_binding_pattern(&it.value);
        self.ctx = Ctx::Other;
        self.leave_node(kind);
    }

    fn visit_assignment_target_property_identifier(&mut self, it: &AssignmentTargetPropertyIdentifier<'a>) {
        // ESTree: Property { key: Identifier, value: Identifier | AssignmentPattern }
        let kind = AstKind::AssignmentTargetPropertyIdentifier(self.alloc(it));
        self.enter_node(kind);
        self.ctx = Ctx::PropertyKey;
        self.ident(&it.binding.name, it.binding.span);
        match &it.init {
            None => {
                self.ctx = Ctx::PropertyValue;
                self.ident(&it.binding.name, it.binding.span);
            }
            Some(init) => {
                // AssignmentPattern { left, right }
                self.ctx = Ctx::Other;
                self.ident(&it.binding.name, it.binding.span);
                self.visit_expression(init);
            }
        }
        self.leave_node(kind);
    }

    fn visit_assignment_target_property_property(&mut self, it: &AssignmentTargetPropertyProperty<'a>) {
        let kind = AstKind::AssignmentTargetPropertyProperty(self.alloc(it));
        self.enter_node(kind);
        self.key(&it.name);
        self.ctx = Ctx::PropertyValue;
        self.visit_assignment_target_maybe_default(&it.binding);
        self.ctx = Ctx::Other;
        self.leave_node(kind);
    }

    fn visit_variable_declarator(&mut self, it: &VariableDeclarator<'a>) {
        let kind = AstKind::VariableDeclarator(self.alloc(it));
        self.enter_node(kind);
        // `id` (with its type annotation, which acorn-typescript puts on the id)
        self.h.set_declaration(true);
        self.visit_binding_pattern(&it.id);
        if let Some(t) = &it.type_annotation {
            self.visit_ts_type_annotation(t);
        }
        self.h.set_declaration(false);
        if let Some(init) = &it.init {
            self.ctx = Ctx::Other;
            self.visit_expression(init);
        }
        self.leave_node(kind);
    }

    fn visit_formal_parameter(&mut self, it: &FormalParameter<'a>) {
        let toggles = self.params_owner.last().copied().unwrap_or(false);
        if toggles {
            self.h.set_declaration(true);
        }
        walk::walk_formal_parameter(self, it);
        if toggles {
            self.h.set_declaration(false);
        }
    }

    fn visit_formal_parameter_rest(&mut self, it: &FormalParameterRest<'a>) {
        let toggles = self.params_owner.last().copied().unwrap_or(false);
        if toggles {
            self.h.set_declaration(true);
        }
        walk::walk_formal_parameter_rest(self, it);
        if toggles {
            self.h.set_declaration(false);
        }
    }
}

/// The span of an expression after Svelte's `remove_parens`
pub fn unparen_span(e: &Expression) -> oxc_span::Span {
    e.without_parentheses().span()
}
