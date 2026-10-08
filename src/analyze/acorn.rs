//! Errors acorn raises while parsing a `<script>` that oxc's parser leaves to its semantic
//! analysis. Svelte reports them as `js_parse_error`. Scripts are modules, so strict:
//!
//! - `Identifier 'x' has already been declared` (acorn's `scope.js`: functions at the top
//!   level are lexical, functions directly in a function are vars)
//! - `Argument name clash`, `Binding eval in strict mode`, `Assigning to arguments in strict mode`
//! - `Deleting local variable in strict mode`, `'with' in strict mode`, `Invalid number`
//! - `Label 'x' is already declared`, `Unsyntactic break` / `Unsyntactic continue`
//! - `Redefinition of __proto__ property`
//! - `Private field '#x' must be declared in an enclosing class`
//! - `'super' keyword outside a method`, `super() call outside constructor of a subclass`
//!
//! acorn's `Export 'x' is not defined` never fires: components can export snippets declared
//! in the template.

use oxc_ast::ast::*;
use oxc_ast_visit::{Visit, walk};
use oxc_syntax::scope::ScopeFlags;

use crate::error::CompileError;
use crate::errors as e;

const TOP: u16 = 1;
const FUNCTION: u16 = 2;
const SIMPLE_CATCH: u16 = 4;
const STATIC_BLOCK: u16 = 8;
const ARROW: u16 = 16;
const SUPER: u16 = 32;
const DIRECT_SUPER: u16 = 64;
const FIELD_INIT: u16 = 128;
const VAR: u16 = TOP | FUNCTION | STATIC_BLOCK;

#[derive(Clone, Copy, PartialEq)]
enum Bind {
    Var,
    Lexical,
    SimpleCatch,
}

#[derive(Default)]
struct Scope<'a> {
    flags: u16,
    var: Vec<&'a str>,
    lexical: Vec<&'a str>,
    functions: Vec<&'a str>,
}

#[derive(Clone, Copy, PartialEq)]
enum LabelKind {
    Loop,
    Switch,
    Other,
}

struct Label<'a> {
    name: Option<&'a str>,
    kind: LabelKind,
}

#[derive(Default)]
struct PrivateNames<'a> {
    declared: Vec<&'a str>,
    used: Vec<(&'a str, u32)>,
}

struct Checker<'a> {
    source: &'a str,
    scopes: Vec<Scope<'a>>,
    labels: Vec<Label<'a>>,
    private: Vec<PrivateNames<'a>>,
    error: Option<(u32, String)>,
    ts: bool,
    /// flags for the next function scope (set by method/object-method visitors)
    next_function_flags: u16,
}

impl<'a> Checker<'a> {
    fn fail(&mut self, pos: u32, message: impl Into<String>) {
        if self.error.is_none() {
            self.error = Some((pos, message.into()));
        }
    }

    fn enter(&mut self, flags: u16) {
        self.scopes.push(Scope { flags, ..Default::default() });
    }

    fn exit(&mut self) {
        self.scopes.pop();
    }

    fn treat_functions_as_var_in(scope: &Scope) -> bool {
        scope.flags & FUNCTION != 0
    }

    /// `currentThisScope().flags`
    fn this_flags(&self) -> u16 {
        for s in self.scopes.iter().rev() {
            if s.flags & (VAR | FIELD_INIT | STATIC_BLOCK) != 0 && s.flags & ARROW == 0 {
                return s.flags;
            }
        }
        0
    }

    fn declare(&mut self, name: &'a str, bind: Bind, pos: u32) {
        if self.error.is_some() {
            return;
        }
        let mut redeclared = false;
        let n = self.scopes.len();
        match bind {
            Bind::Lexical => {
                let scope = &mut self.scopes[n - 1];
                redeclared = scope.lexical.contains(&name) || scope.functions.contains(&name) || scope.var.contains(&name);
                scope.lexical.push(name);
            }
            Bind::SimpleCatch => self.scopes[n - 1].lexical.push(name),
            Bind::Var => {
                for i in (0..n).rev() {
                    let scope = &self.scopes[i];
                    if (scope.lexical.contains(&name) && !(scope.flags & SIMPLE_CATCH != 0 && scope.lexical.first() == Some(&name)))
                        || (!Self::treat_functions_as_var_in(scope) && scope.functions.contains(&name))
                    {
                        redeclared = true;
                        break;
                    }
                    let scope = &mut self.scopes[i];
                    scope.var.push(name);
                    if scope.flags & VAR != 0 {
                        break;
                    }
                }
            }
        }
        if redeclared {
            self.fail(pos, format!("Identifier '{name}' has already been declared"));
        }
    }

    /// `checkLValSimple` for a binding identifier: strict reserved names, clashes, declaration
    fn bind_identifier(&mut self, id: &BindingIdentifier<'a>, bind: Bind, clashes: Option<&mut Vec<&'a str>>) {
        let name = id.name.as_str();
        if name == "eval" || name == "arguments" {
            self.fail(id.span.start, format!("Binding {name} in strict mode"));
        }
        if let Some(clashes) = clashes {
            if clashes.contains(&name) {
                self.fail(id.span.start, "Argument name clash");
            }
            clashes.push(name);
        }
        self.declare(name, bind, id.span.start);
    }

    fn declare_pattern(&mut self, pattern: &BindingPattern<'a>, bind: Bind, mut clashes: Option<&mut Vec<&'a str>>) {
        match pattern {
            BindingPattern::BindingIdentifier(id) => self.bind_identifier(id, bind, clashes),
            BindingPattern::ObjectPattern(o) => {
                for p in &o.properties {
                    self.declare_pattern(&p.value, bind, clashes.as_deref_mut());
                }
                if let Some(r) = &o.rest {
                    self.declare_pattern(&r.argument, bind, clashes.as_deref_mut());
                }
            }
            BindingPattern::ArrayPattern(a) => {
                for el in a.elements.iter().flatten() {
                    self.declare_pattern(el, bind, clashes.as_deref_mut());
                }
                if let Some(r) = &a.rest {
                    self.declare_pattern(&r.argument, bind, clashes.as_deref_mut());
                }
            }
            BindingPattern::AssignmentPattern(a) => self.declare_pattern(&a.left, bind, clashes),
        }
    }

    fn declare_params(&mut self, params: &FormalParameters<'a>) {
        let mut clashes: Vec<&'a str> = Vec::new();
        for p in &params.items {
            if let BindingPattern::BindingIdentifier(id) = &p.pattern {
                if self.ts && id.name == "this" {
                    continue;
                }
            }
            self.declare_pattern(&p.pattern, Bind::Var, Some(&mut clashes));
        }
        if let Some(rest) = &params.rest {
            self.declare_pattern(&rest.rest.argument, Bind::Var, Some(&mut clashes));
        }
    }

    /// A function's parameters and body, in its own scope (labels are per function)
    fn function_inner(&mut self, flags: u16, params: &FormalParameters<'a>, body: Option<&FunctionBody<'a>>) {
        self.enter(flags);
        self.declare_params(params);
        self.visit_formal_parameters(params);
        let labels = std::mem::take(&mut self.labels);
        if let Some(body) = body {
            for s in &body.statements {
                self.visit_statement(s);
            }
        }
        self.labels = labels;
        self.exit();
    }

    /// `checkLValSimple` for an assignment target identifier
    fn assign_identifier(&mut self, id: &IdentifierReference<'a>) {
        let name = id.name.as_str();
        if name == "eval" || name == "arguments" {
            self.fail(id.span.start, format!("Assigning to {name} in strict mode"));
        }
    }

    fn check_assignment_target(&mut self, t: &AssignmentTarget<'a>) {
        match t {
            AssignmentTarget::AssignmentTargetIdentifier(id) => self.assign_identifier(id),
            AssignmentTarget::ObjectAssignmentTarget(o) => {
                for p in &o.properties {
                    match p {
                        AssignmentTargetProperty::AssignmentTargetPropertyIdentifier(x) => self.assign_identifier(&x.binding),
                        AssignmentTargetProperty::AssignmentTargetPropertyProperty(x) => self.check_maybe_default(&x.binding),
                    }
                }
                if let Some(r) = &o.rest {
                    self.check_assignment_target(&r.target);
                }
            }
            AssignmentTarget::ArrayAssignmentTarget(a) => {
                for el in a.elements.iter().flatten() {
                    self.check_maybe_default(el);
                }
                if let Some(r) = &a.rest {
                    self.check_assignment_target(&r.target);
                }
            }
            _ => {}
        }
    }

    fn check_maybe_default(&mut self, t: &AssignmentTargetMaybeDefault<'a>) {
        match t {
            AssignmentTargetMaybeDefault::AssignmentTargetWithDefault(d) => self.check_assignment_target(&d.binding),
            _ => self.check_assignment_target(t.as_assignment_target().unwrap()),
        }
    }

    fn use_private(&mut self, name: &'a str, pos: u32) {
        match self.private.last_mut() {
            Some(p) => p.used.push((name, pos)),
            None => self.fail(pos, format!("Private field '#{name}' must be declared in an enclosing class")),
        }
    }

    fn is_loop(s: &Statement) -> bool {
        matches!(
            s,
            Statement::ForStatement(_)
                | Statement::ForInStatement(_)
                | Statement::ForOfStatement(_)
                | Statement::WhileStatement(_)
                | Statement::DoWhileStatement(_)
        )
    }

    fn loop_body(&mut self, f: impl FnOnce(&mut Self)) {
        self.labels.push(Label { name: None, kind: LabelKind::Loop });
        f(self);
        self.labels.pop();
    }

    fn break_continue(&mut self, label: Option<&LabelIdentifier<'a>>, is_break: bool, start: u32) {
        let mut found = false;
        for lab in &self.labels {
            if label.is_none() || lab.name == label.map(|l| l.name.as_str()) {
                if lab.kind != LabelKind::Other && (is_break || lab.kind == LabelKind::Loop) {
                    found = true;
                    break;
                }
                if label.is_some() && is_break {
                    found = true;
                    break;
                }
            }
        }
        if !found {
            self.fail(start, if is_break { "Unsyntactic break" } else { "Unsyntactic continue" });
        }
    }
}

impl<'a> Visit<'a> for Checker<'a> {
    fn visit_statement(&mut self, it: &Statement<'a>) {
        if self.error.is_none() {
            walk::walk_statement(self, it);
        }
    }

    fn visit_expression(&mut self, it: &Expression<'a>) {
        if self.error.is_none() {
            walk::walk_expression(self, it);
        }
    }

    fn visit_block_statement(&mut self, it: &BlockStatement<'a>) {
        self.enter(0);
        walk::walk_block_statement(self, it);
        self.exit();
    }

    fn visit_variable_declaration(&mut self, it: &VariableDeclaration<'a>) {
        if it.declare {
            return;
        }
        let bind = if it.kind == VariableDeclarationKind::Var { Bind::Var } else { Bind::Lexical };
        for d in &it.declarations {
            self.declare_pattern(&d.id, bind, None);
            self.visit_binding_pattern(&d.id);
            if let Some(init) = &d.init {
                self.visit_expression(init);
            }
        }
    }

    fn visit_for_statement(&mut self, it: &ForStatement<'a>) {
        self.enter(0);
        if let Some(init) = &it.init {
            self.visit_for_statement_init(init);
        }
        if let Some(t) = &it.test {
            self.visit_expression(t);
        }
        if let Some(u) = &it.update {
            self.visit_expression(u);
        }
        self.loop_body(|me| me.visit_statement(&it.body));
        self.exit();
    }

    fn visit_for_in_statement(&mut self, it: &ForInStatement<'a>) {
        self.enter(0);
        self.visit_for_statement_left(&it.left);
        self.visit_expression(&it.right);
        self.loop_body(|me| me.visit_statement(&it.body));
        self.exit();
    }

    fn visit_for_of_statement(&mut self, it: &ForOfStatement<'a>) {
        self.enter(0);
        self.visit_for_statement_left(&it.left);
        self.visit_expression(&it.right);
        self.loop_body(|me| me.visit_statement(&it.body));
        self.exit();
    }

    fn visit_while_statement(&mut self, it: &WhileStatement<'a>) {
        self.visit_expression(&it.test);
        self.loop_body(|me| me.visit_statement(&it.body));
    }

    fn visit_do_while_statement(&mut self, it: &DoWhileStatement<'a>) {
        self.loop_body(|me| me.visit_statement(&it.body));
        self.visit_expression(&it.test);
    }

    fn visit_switch_statement(&mut self, it: &SwitchStatement<'a>) {
        self.visit_expression(&it.discriminant);
        self.labels.push(Label { name: None, kind: LabelKind::Switch });
        self.enter(0);
        for c in &it.cases {
            self.visit_switch_case(c);
        }
        self.exit();
        self.labels.pop();
    }

    fn visit_labeled_statement(&mut self, it: &LabeledStatement<'a>) {
        let name = it.label.name.as_str();
        if self.labels.iter().any(|l| l.name == Some(name)) {
            self.fail(it.label.span.start, format!("Label '{name}' is already declared"));
        }
        // `a: b: while (...)`: `a` gets the kind of the statement the labels are on
        let mut body = &it.body;
        while let Statement::LabeledStatement(inner) = body {
            body = &inner.body;
        }
        let kind = if Self::is_loop(body) {
            LabelKind::Loop
        } else if matches!(body, Statement::SwitchStatement(_)) {
            LabelKind::Switch
        } else {
            LabelKind::Other
        };
        self.labels.push(Label { name: Some(name), kind });
        self.visit_statement(&it.body);
        self.labels.pop();
    }

    fn visit_break_statement(&mut self, it: &BreakStatement<'a>) {
        self.break_continue(it.label.as_ref(), true, it.span.start);
    }

    fn visit_continue_statement(&mut self, it: &ContinueStatement<'a>) {
        self.break_continue(it.label.as_ref(), false, it.span.start);
    }

    fn visit_with_statement(&mut self, it: &WithStatement<'a>) {
        self.fail(it.span.start, "'with' in strict mode");
    }

    fn visit_catch_clause(&mut self, it: &CatchClause<'a>) {
        match &it.param {
            Some(param) => {
                let simple = matches!(param.pattern, BindingPattern::BindingIdentifier(_));
                self.enter(if simple { SIMPLE_CATCH } else { 0 });
                self.declare_pattern(&param.pattern, if simple { Bind::SimpleCatch } else { Bind::Lexical }, None);
                self.visit_binding_pattern(&param.pattern);
            }
            None => self.enter(0),
        }
        // the body shares the catch scope
        for s in &it.body.body {
            self.visit_statement(s);
        }
        self.exit();
    }

    fn visit_function(&mut self, it: &Function<'a>, _flags: ScopeFlags) {
        let method_flags = std::mem::take(&mut self.next_function_flags);
        if it.declare || it.body.is_none() {
            // TS overloads and ambient declarations
            return;
        }
        if it.is_declaration() {
            if let Some(id) = &it.id {
                let as_var = Self::treat_functions_as_var_in(self.scopes.last().unwrap());
                let bind = if as_var { Bind::Var } else { Bind::Lexical };
                self.bind_identifier(id, bind, None);
            }
        }
        self.function_inner(FUNCTION | method_flags, &it.params, it.body.as_deref());
    }

    fn visit_arrow_function_expression(&mut self, it: &ArrowFunctionExpression<'a>) {
        match &it.body {
            ArrowFunctionBody::FunctionBody(b) => self.function_inner(FUNCTION | ARROW, &it.params, Some(b)),
            body => {
                self.enter(FUNCTION | ARROW);
                self.declare_params(&it.params);
                self.visit_formal_parameters(&it.params);
                self.visit_expression(body.as_expression().unwrap());
                self.exit();
            }
        }
    }

    fn visit_class(&mut self, it: &Class<'a>) {
        if it.declare {
            return;
        }
        if it.is_declaration() {
            if let Some(id) = &it.id {
                self.bind_identifier(id, Bind::Lexical, None);
            }
        }
        if let Some(h) = &it.heritage {
            self.visit_expression(&h.expression);
        }
        let has_super_class = it.heritage.is_some();
        self.private.push(PrivateNames::default());
        // declared private names
        for el in &it.body.body {
            let key = match el {
                ClassElement::MethodDefinition(m) => &m.key,
                ClassElement::PropertyDefinition(p) => &p.key,
                ClassElement::AccessorProperty(p) => &p.key,
                _ => continue,
            };
            if let PropertyKey::PrivateIdentifier(p) = key {
                self.private.last_mut().unwrap().declared.push(p.name.as_str());
            }
        }
        for el in &it.body.body {
            if self.error.is_some() {
                break;
            }
            match el {
                ClassElement::MethodDefinition(m) => {
                    if !matches!(m.key, PropertyKey::PrivateIdentifier(_)) {
                        self.visit_property_key(&m.key);
                    }
                    let direct = m.kind == MethodDefinitionKind::Constructor && has_super_class;
                    self.next_function_flags = SUPER | if direct { DIRECT_SUPER } else { 0 };
                    self.visit_function(&m.value, ScopeFlags::empty());
                    self.next_function_flags = 0;
                }
                ClassElement::PropertyDefinition(p) => {
                    if !matches!(p.key, PropertyKey::PrivateIdentifier(_)) {
                        self.visit_property_key(&p.key);
                    }
                    if let Some(v) = &p.value {
                        self.enter(FIELD_INIT | SUPER);
                        self.visit_expression(v);
                        self.exit();
                    }
                }
                ClassElement::AccessorProperty(p) => {
                    if !matches!(p.key, PropertyKey::PrivateIdentifier(_)) {
                        self.visit_property_key(&p.key);
                    }
                    if let Some(v) = &p.value {
                        self.enter(FIELD_INIT | SUPER);
                        self.visit_expression(v);
                        self.exit();
                    }
                }
                ClassElement::StaticBlock(b) => {
                    self.enter(STATIC_BLOCK | SUPER);
                    let labels = std::mem::take(&mut self.labels);
                    for s in &b.body {
                        self.visit_statement(s);
                    }
                    self.labels = labels;
                    self.exit();
                }
                ClassElement::TSIndexSignature(_) => {}
            }
        }
        let names = self.private.pop().unwrap();
        for (name, pos) in names.used {
            if !names.declared.contains(&name) {
                match self.private.last_mut() {
                    Some(parent) => parent.used.push((name, pos)),
                    None => self.fail(pos, format!("Private field '#{name}' must be declared in an enclosing class")),
                }
            }
        }
    }

    fn visit_object_expression(&mut self, it: &ObjectExpression<'a>) {
        let mut proto = false;
        for p in &it.properties {
            if let ObjectPropertyKind::ObjectProperty(prop) = p {
                if !prop.computed && !prop.method && !prop.shorthand && prop.kind == PropertyKind::Init {
                    let name = match &prop.key {
                        PropertyKey::StaticIdentifier(k) => Some(k.name.as_str()),
                        PropertyKey::StringLiteral(s) => Some(s.value.as_str()),
                        _ => None,
                    };
                    if name == Some("__proto__") {
                        if proto {
                            self.fail(prop.key.span().start, "Redefinition of __proto__ property");
                        }
                        proto = true;
                    }
                }
                if prop.method || prop.kind != PropertyKind::Init {
                    self.visit_property_key(&prop.key);
                    if let Expression::FunctionExpression(f) = &prop.value {
                        self.next_function_flags = SUPER;
                        self.visit_function(f, ScopeFlags::empty());
                        self.next_function_flags = 0;
                        continue;
                    }
                    self.visit_expression(&prop.value);
                    continue;
                }
                self.visit_object_property(prop);
            } else {
                self.visit_object_property_kind(p);
            }
        }
    }

    fn visit_unary_expression(&mut self, it: &UnaryExpression<'a>) {
        if it.operator == UnaryOperator::Delete {
            if let Expression::Identifier(_) = &it.argument {
                self.fail(it.span.start, "Deleting local variable in strict mode");
            }
        }
        walk::walk_unary_expression(self, it);
    }

    fn visit_assignment_expression(&mut self, it: &AssignmentExpression<'a>) {
        self.check_assignment_target(&it.left);
        walk::walk_assignment_expression(self, it);
    }

    fn visit_update_expression(&mut self, it: &UpdateExpression<'a>) {
        if let SimpleAssignmentTarget::AssignmentTargetIdentifier(id) = &it.argument {
            self.assign_identifier(id);
        }
        walk::walk_update_expression(self, it);
    }

    fn visit_numeric_literal(&mut self, it: &NumericLiteral<'a>) {
        // legacy octal and decimal-with-leading-zero literals
        let raw = &self.source[it.span.start as usize..it.span.end as usize];
        let b = raw.as_bytes();
        if b.len() >= 2 && b[0] == b'0' && b[1].is_ascii_digit() {
            self.fail(it.span.start, "Invalid number");
        }
    }

    fn visit_private_field_expression(&mut self, it: &PrivateFieldExpression<'a>) {
        self.visit_expression(&it.object);
        self.use_private(it.field.name.as_str(), it.field.span.start);
    }

    fn visit_private_in_expression(&mut self, it: &PrivateInExpression<'a>) {
        self.use_private(it.left.name.as_str(), it.left.span.start);
        self.visit_expression(&it.right);
    }

    fn visit_call_expression(&mut self, it: &CallExpression<'a>) {
        if let Expression::Super(s) = &it.callee {
            if self.this_flags() & SUPER == 0 {
                self.fail(s.span.start, "'super' keyword outside a method");
            } else if self.this_flags() & DIRECT_SUPER == 0 {
                self.fail(s.span.start, "super() call outside constructor of a subclass");
            }
            for a in &it.arguments {
                self.visit_argument(a);
            }
            return;
        }
        walk::walk_call_expression(self, it);
    }

    fn visit_super(&mut self, it: &Super) {
        if self.this_flags() & SUPER == 0 {
            self.fail(it.span.start, "'super' keyword outside a method");
        }
    }

    fn visit_static_block(&mut self, it: &StaticBlock<'a>) {
        self.enter(STATIC_BLOCK | SUPER);
        for s in &it.body {
            self.visit_statement(s);
        }
        self.exit();
    }

    fn visit_import_declaration(&mut self, it: &ImportDeclaration<'a>) {
        if it.import_kind.is_type() {
            return;
        }
        for s in it.specifiers.iter().flatten() {
            let local = match s {
                ImportDeclarationSpecifier::ImportSpecifier(x) => {
                    if x.import_kind.is_type() {
                        continue;
                    }
                    &x.local
                }
                ImportDeclarationSpecifier::ImportDefaultSpecifier(x) => &x.local,
                ImportDeclarationSpecifier::ImportNamespaceSpecifier(x) => &x.local,
            };
            self.bind_identifier(local, Bind::Lexical, None);
        }
    }

    // type-level code declares nothing
    fn visit_ts_type_alias_declaration(&mut self, _it: &TSTypeAliasDeclaration<'a>) {}
    fn visit_ts_interface_declaration(&mut self, _it: &TSInterfaceDeclaration<'a>) {}
    fn visit_ts_enum_declaration(&mut self, _it: &TSEnumDeclaration<'a>) {}
    fn visit_ts_namespace_declaration(&mut self, _it: &TSNamespaceDeclaration<'a>) {}
    fn visit_ts_external_module_declaration(&mut self, _it: &TSExternalModuleDeclaration<'a>) {}
    fn visit_ts_global_declaration(&mut self, _it: &TSGlobalDeclaration<'a>) {}
    fn visit_ts_type_annotation(&mut self, _it: &TSTypeAnnotation<'a>) {}
}

/// Reword a `js_parse_error` from oxc the way acorn reports it, for the errors where both
/// agree on the position (or where acorn's is the next token)
pub fn reword_parse_error(err: CompileError, source: &str) -> CompileError {
    if err.code != "js_parse_error" {
        return err;
    }
    let Some((pos, _)) = err.position else { return err };
    let message = err.first_line();
    let (message, pos) = if message == "Expected a semicolon or an implicit semicolon after a statement, but found none" {
        // acorn complains at the next token
        let rest = source.get(pos..).unwrap_or("");
        let skipped = rest.len() - rest.trim_start_matches(super::utils::is_js_whitespace).len();
        ("Unexpected token".to_string(), pos + skipped)
    } else if message.starts_with("Expected `") && message.contains("` but found `") && !message.contains("` or `") {
        ("Unexpected token".to_string(), pos)
    } else if message == "Missing initializer in const declaration" {
        // acorn stops at the token after the binding (e.g. the `:` of a type annotation)
        match binding_end(source, pos) {
            Some(end) => {
                let rest = &source[end..];
                let skipped = rest.len() - rest.trim_start_matches(super::utils::is_js_whitespace).len();
                ("Unexpected token".to_string(), end + skipped)
            }
            None => return err,
        }
    } else if message == "Cannot assign to this expression" {
        ("Assigning to rvalue".to_string(), pos)
    } else if let Some(c) = message.strip_prefix("Invalid Character `").and_then(|m| m.strip_suffix('`')) {
        (format!("Unexpected character '{c}'"), pos)
    } else if message == "`await` is only allowed within async functions and at the top levels of modules" {
        ("Cannot use keyword 'await' outside an async function".to_string(), pos)
    } else if message == "A 'return' statement can only be used within a function body." {
        ("'return' outside of function".to_string(), pos)
    } else if message == "Unexpected new.target expression" {
        ("'new.target' can only be used in functions and class static block".to_string(), pos)
    } else {
        return err;
    };
    e::js_parse_error(pos, &message)
}

/// The end of the binding (identifier or bracketed pattern) starting at `pos`
fn binding_end(source: &str, pos: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    match bytes.get(pos)? {
        b'{' | b'[' => {
            let close = crate::parser::utils::find_matching_bracket(&source[pos..], 0, bytes[pos])?;
            Some(pos + close + 1)
        }
        _ => {
            let rest = &source[pos..];
            let len = rest
                .char_indices()
                .find(|&(_, c)| !(c.is_alphanumeric() || c == '_' || c == '$'))
                .map_or(rest.len(), |(i, _)| i);
            (len > 0).then_some(pos + len)
        }
    }
}

/// The first error acorn would raise while parsing `program` that oxc's parser didn't.
/// `source` is the whole component (spans are offsets into it).
pub fn check(program: &Program, source: &str, ts: bool) -> Option<CompileError> {
    let mut c = Checker {
        source,
        scopes: Vec::new(),
        labels: Vec::new(),
        private: Vec::new(),
        error: None,
        ts,
        next_function_flags: 0,
    };
    c.enter(TOP);
    for s in &program.body {
        c.visit_statement(s);
        if c.error.is_some() {
            break;
        }
    }
    c.error.map(|(pos, message)| e::js_parse_error(pos as usize, &message))
}

use oxc_span::GetSpan;
