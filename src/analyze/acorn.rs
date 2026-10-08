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
//! - `checkUnreserved`: `The keyword 'x' is reserved` (strict mode reserved words), and the
//!   `yield`/`await`/`arguments` rules
//! - `Octal literal in strict mode`, `Invalid escape sequence` (string literals)
//! - `Unexpected token` for what acorn (ecmaVersion 16) can't parse: a function declaration
//!   as the body of an `if`, a loop or a label, `using` declarations, import attributes
//!   written with `assert`, and `import defer` / `import source`
//!
//! acorn's `Export 'x' is not defined` never fires for components (they can export snippets
//! declared in the template); [`check_module`] raises it for `.svelte.js` modules.

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
const ASYNC: u16 = 256;
const GENERATOR: u16 = 512;
const VAR: u16 = TOP | FUNCTION | STATIC_BLOCK;

/// acorn's `keywords` (ecmaVersion 6 and up, a module)
const KEYWORDS: &[&str] = &[
    "break", "case", "catch", "continue", "debugger", "default", "do", "else", "finally", "for", "function", "if", "return",
    "switch", "throw", "try", "var", "while", "with", "null", "true", "false", "instanceof", "typeof", "void", "delete",
    "new", "in", "this", "const", "class", "extends", "export", "import", "super",
];

/// `reservedWordsStrict` for a module: `enum`, `await` and the strict mode reserved words
const STRICT_RESERVED: &[&str] =
    &["enum", "await", "implements", "interface", "let", "package", "private", "protected", "public", "static", "yield"];

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

    /// `currentVarScope().flags`
    fn var_flags(&self) -> u16 {
        self.scopes.iter().rev().find(|s| s.flags & VAR != 0).map_or(0, |s| s.flags)
    }

    /// `checkUnreserved` for an identifier acorn reads with `parseIdent(false)`
    fn check_unreserved(&mut self, name: &str, pos: u32) {
        if self.error.is_some() || (name != "arguments" && !STRICT_RESERVED.contains(&name)) {
            return;
        }
        let var = self.var_flags();
        let (in_async, in_generator, in_static_block) = (var & ASYNC != 0, var & GENERATOR != 0, var & STATIC_BLOCK != 0);
        if in_generator && name == "yield" {
            self.fail(pos, "Cannot use 'yield' as identifier inside a generator");
        } else if in_async && name == "await" {
            self.fail(pos, "Cannot use 'await' as identifier inside an async function");
        } else if self.this_flags() & VAR == 0 && name == "arguments" {
            self.fail(pos, "Cannot use 'arguments' in class field initializer");
        } else if in_static_block && (name == "arguments" || name == "await") {
            self.fail(pos, format!("Cannot use {name} in class static initialization block"));
        } else if STRICT_RESERVED.contains(&name) {
            if name == "await" {
                self.fail(pos, "Cannot use keyword 'await' outside an async function");
            } else {
                self.fail(pos, format!("The keyword '{name}' is reserved"));
            }
        }
    }

    /// acorn reads a statement in a single-statement context (the body of an `if`, a loop or a
    /// label) with `parseStatement(context)`, where a function declaration is unexpected
    fn single_statement(&mut self, s: &Statement<'a>) {
        if let Statement::FunctionDeclaration(f) = s {
            self.fail(f.span.start, "Unexpected token");
        }
    }

    /// `readEscapedChar` in strict mode: legacy octal escapes and `\8` / `\9`
    fn check_string(&mut self, raw: &str, start: u32) {
        let b = raw.as_bytes();
        let mut i = 1;
        while i + 1 < b.len() {
            if b[i] != b'\\' {
                i += 1;
                continue;
            }
            match b[i + 1] {
                b'8' | b'9' => {
                    self.fail(start + i as u32 + 1, "Invalid escape sequence");
                    return;
                }
                b'0'..=b'7' => {
                    let mut len = b[i + 1..].iter().take(3).take_while(|c| (b'0'..=b'7').contains(c)).count();
                    let value = |len: usize| b[i + 1..i + 1 + len].iter().fold(0u32, |v, c| v * 8 + u32::from(c - b'0'));
                    if value(len) > 255 {
                        len -= 1;
                    }
                    let next = b.get(i + 1 + len).copied();
                    if len > 1 || b[i + 1] != b'0' || matches!(next, Some(b'8' | b'9')) {
                        self.fail(start + i as u32, "Octal literal in strict mode");
                        return;
                    }
                    i += 1 + len;
                }
                _ => i += 2,
            }
        }
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
        self.check_unreserved(name, id.span.start);
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
            for d in &body.directives {
                self.visit_string_literal(&d.expression);
            }
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
        if matches!(it.kind, VariableDeclarationKind::Using | VariableDeclarationKind::AwaitUsing) {
            // acorn reads `using` as an identifier, then stops at the binding
            if let Some(d) = it.declarations.first() {
                self.fail(d.id.span().start, "Unexpected token");
            }
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
        self.single_statement(&it.body);
        self.loop_body(|me| me.visit_statement(&it.body));
        self.exit();
    }

    fn visit_for_in_statement(&mut self, it: &ForInStatement<'a>) {
        self.enter(0);
        self.visit_for_statement_left(&it.left);
        self.visit_expression(&it.right);
        self.single_statement(&it.body);
        self.loop_body(|me| me.visit_statement(&it.body));
        self.exit();
    }

    fn visit_for_of_statement(&mut self, it: &ForOfStatement<'a>) {
        self.enter(0);
        self.visit_for_statement_left(&it.left);
        self.visit_expression(&it.right);
        self.single_statement(&it.body);
        self.loop_body(|me| me.visit_statement(&it.body));
        self.exit();
    }

    fn visit_while_statement(&mut self, it: &WhileStatement<'a>) {
        self.visit_expression(&it.test);
        self.single_statement(&it.body);
        self.loop_body(|me| me.visit_statement(&it.body));
    }

    fn visit_do_while_statement(&mut self, it: &DoWhileStatement<'a>) {
        self.single_statement(&it.body);
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

    fn visit_if_statement(&mut self, it: &IfStatement<'a>) {
        self.visit_expression(&it.test);
        self.single_statement(&it.consequent);
        self.visit_statement(&it.consequent);
        if let Some(alternate) = &it.alternate {
            self.single_statement(alternate);
            self.visit_statement(alternate);
        }
    }

    fn visit_labeled_statement(&mut self, it: &LabeledStatement<'a>) {
        let name = it.label.name.as_str();
        self.check_unreserved(name, it.label.span.start);
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
        self.single_statement(&it.body);
        self.visit_statement(&it.body);
        self.labels.pop();
    }

    fn visit_break_statement(&mut self, it: &BreakStatement<'a>) {
        if let Some(label) = &it.label {
            self.check_unreserved(label.name.as_str(), label.span.start);
        }
        self.break_continue(it.label.as_ref(), true, it.span.start);
    }

    fn visit_continue_statement(&mut self, it: &ContinueStatement<'a>) {
        if let Some(label) = &it.label {
            self.check_unreserved(label.name.as_str(), label.span.start);
        }
        self.break_continue(it.label.as_ref(), false, it.span.start);
    }

    fn visit_identifier_reference(&mut self, it: &IdentifierReference<'a>) {
        self.check_unreserved(it.name.as_str(), it.span.start);
    }

    fn visit_binding_identifier(&mut self, it: &BindingIdentifier<'a>) {
        self.check_unreserved(it.name.as_str(), it.span.start);
    }

    fn visit_string_literal(&mut self, it: &StringLiteral<'a>) {
        let raw = match &it.raw {
            Some(raw) => raw.as_str(),
            None => self.source.get(it.span.start as usize..it.span.end as usize).unwrap_or(""),
        };
        self.check_string(raw, it.span.start);
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
        } else if let Some(id) = &it.id {
            self.check_unreserved(id.name.as_str(), id.span.start);
        }
        let flags = FUNCTION | method_flags | if it.r#async { ASYNC } else { 0 } | if it.generator { GENERATOR } else { 0 };
        self.function_inner(flags, &it.params, it.body.as_deref());
    }

    fn visit_arrow_function_expression(&mut self, it: &ArrowFunctionExpression<'a>) {
        let flags = FUNCTION | ARROW | if it.r#async { ASYNC } else { 0 };
        match &it.body {
            ArrowFunctionBody::FunctionBody(b) => self.function_inner(flags, &it.params, Some(b)),
            body => {
                self.enter(flags);
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
        } else if let Some(id) = &it.id {
            self.check_unreserved(id.name.as_str(), id.span.start);
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
        let raw = match &it.raw {
            Some(raw) => raw.as_str(),
            None => self.source.get(it.span.start as usize..it.span.end as usize).unwrap_or(""),
        };
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
        // `import defer * as x` / `import source x`: acorn reads `defer` / `source` as the
        // default import and stops at what follows
        if let (Some(_), Some(first)) = (it.phase, it.specifiers.as_ref().and_then(|s| s.first())) {
            let pos = match first {
                ImportDeclarationSpecifier::ImportNamespaceSpecifier(x) => x.span.start,
                ImportDeclarationSpecifier::ImportDefaultSpecifier(x) => x.local.span.start,
                ImportDeclarationSpecifier::ImportSpecifier(x) => x.span.start,
            };
            self.fail(pos, "Unexpected token");
            return;
        }
        // (acorn-typescript declares type-only imports like any other)
        for s in it.specifiers.iter().flatten() {
            let local = match s {
                ImportDeclarationSpecifier::ImportSpecifier(x) => &x.local,
                ImportDeclarationSpecifier::ImportDefaultSpecifier(x) => &x.local,
                ImportDeclarationSpecifier::ImportNamespaceSpecifier(x) => &x.local,
            };
            self.bind_identifier(local, Bind::Lexical, None);
        }
        self.visit_string_literal(&it.source);
        if let Some(with) = &it.with_clause {
            self.visit_with_clause(with);
        }
    }

    fn visit_with_clause(&mut self, it: &WithClause<'a>) {
        // (acorn-typescript reads `assert` too)
        if it.keyword == WithClauseKeyword::Assert && !self.ts {
            // the span starts at the `{`: acorn stops at the keyword
            let start = it.span.start as usize;
            let pos = if self.source[start..].starts_with("assert") {
                start
            } else {
                self.source[..start].trim_end_matches(super::utils::is_js_whitespace).len().saturating_sub("assert".len())
            };
            self.fail(pos as u32, "Unexpected token");
        }
    }

    // type-level code declares nothing, and isn't checked
    fn visit_ts_type(&mut self, _it: &TSType<'a>) {}
    fn visit_ts_type_parameter_declaration(&mut self, _it: &TSTypeParameterDeclaration<'a>) {}
    fn visit_ts_type_parameter_instantiation(&mut self, _it: &TSTypeParameterInstantiation<'a>) {}
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
        // acorn complains at the next token (past any comments)
        let mut next = pos;
        loop {
            let rest = source.get(next..).unwrap_or("");
            let trimmed = rest.trim_start_matches(super::utils::is_js_whitespace);
            next += rest.len() - trimmed.len();
            match trimmed.strip_prefix("/*").and_then(|c| c.find("*/")) {
                Some(end) => next += end + 4,
                None => break,
            }
        }
        ("Unexpected token".to_string(), next)
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
    } else if matches!(
        message,
        "Invalid class declaration"
            | "Lexical declaration cannot appear in a single-statement context"
            | "Async functions can only be declared at the top level or inside a block"
    ) {
        // `parseStatement(context)`
        ("Unexpected token".to_string(), pos)
    } else if let Some(word) =
        message.strip_prefix("Identifier expected. '").and_then(|m| m.strip_suffix("' is a reserved word that cannot be used here."))
    {
        // `checkUnreserved`
        if KEYWORDS.contains(&word) {
            (format!("Unexpected keyword '{word}'"), pos)
        } else {
            (format!("The keyword '{word}' is reserved"), pos)
        }
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
    check_program(program, source, ts, false)
}

/// [`check`] for a `.svelte.js` module (`compileModule`), which acorn parses as a plain module:
/// `export { x }` of an undeclared `x` raises `Export 'x' is not defined` too
pub fn check_module(program: &Program, source: &str) -> Option<CompileError> {
    check_program(program, source, false, true)
}

/// [`check`]'s error as `(position, message)`
pub fn first_error(program: &Program, source: &str, ts: bool) -> Option<(usize, String)> {
    let mut c = Checker::new(source, ts);
    for d in &program.directives {
        c.visit_string_literal(&d.expression);
    }
    for s in &program.body {
        c.visit_statement(s);
        if c.error.is_some() {
            break;
        }
    }
    c.error.map(|(pos, message)| (pos as usize, message))
}

/// A strict mode reserved word acorn reads as an identifier at the start of the statement at
/// `start` (skipping whitespace and comments): `public;`, `let = 1` (but not a `let`
/// declaration, by `isLet`, nor a TypeScript `interface`)
pub fn reserved_statement_start(text: &str, start: usize, ts: bool) -> Option<(usize, String)> {
    let mut i = start;
    loop {
        let rest = &text[i..];
        let trimmed = rest.trim_start_matches(super::utils::is_js_whitespace);
        i += rest.len() - trimmed.len();
        if let Some(c) = trimmed.strip_prefix("/*") {
            i += c.find("*/")? + 4;
        } else if trimmed.starts_with("//") {
            i += trimmed.find('\n').unwrap_or(trimmed.len());
        } else {
            break;
        }
    }
    let rest = &text[i..];
    let len = rest.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '$')).unwrap_or(rest.len());
    let word = &rest[..len];
    if !STRICT_RESERVED.contains(&word) || word == "await" || word == "enum" || (ts && word == "interface") {
        return None;
    }
    if word == "let" {
        // `isLet()`: a `[`, `{`, `\` or an identifier (other than `in` / `instanceof`) next
        // makes it a declaration
        let after = rest[len..].trim_start_matches(super::utils::is_js_whitespace);
        let next_len = after.find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$')).unwrap_or(after.len());
        let next = &after[..next_len];
        let declaration = after.starts_with(['[', '{', '\\'])
            || (!next.is_empty() && !next.starts_with(|c: char| c.is_ascii_digit()) && next != "in" && next != "instanceof");
        if declaration {
            return None;
        }
    }
    Some((i, format!("The keyword '{word}' is reserved")))
}

/// [`check`] for a template expression (`parseExpressionAt`, also a module)
pub fn check_expression(expression: &Expression, source: &str, ts: bool) -> Option<CompileError> {
    let mut c = Checker::new(source, ts);
    c.visit_expression(expression);
    c.error.map(|(pos, message)| e::js_parse_error(pos as usize, &message))
}

/// [`check`] for a declaration tag's statement
pub fn check_statement(statement: &Statement, source: &str, ts: bool) -> Option<CompileError> {
    let mut c = Checker::new(source, ts);
    c.visit_statement(statement);
    c.error.map(|(pos, message)| e::js_parse_error(pos as usize, &message))
}

impl<'a> Checker<'a> {
    fn new(source: &'a str, ts: bool) -> Self {
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
        c
    }
}

fn check_program(program: &Program, source: &str, ts: bool, module: bool) -> Option<CompileError> {
    let mut c = Checker::new(source, ts);
    for d in &program.directives {
        c.visit_string_literal(&d.expression);
    }
    for s in &program.body {
        c.visit_statement(s);
        if c.error.is_some() {
            break;
        }
    }
    if module && c.error.is_none() {
        // `undefinedExports`, reported once the whole program is parsed
        let top = &c.scopes[0];
        let undefined = program.body.iter().find_map(|s| {
            // (`export { x } from '...'` is an ExportFromDeclaration)
            let Statement::ExportNamedDeclaration(d) = s else { return None };
            d.specifiers.iter().find_map(|spec| match &spec.local {
                ModuleExportName::IdentifierReference(local) => {
                    let name = local.name.as_str();
                    (!top.lexical.contains(&name) && !top.var.contains(&name)).then_some((local.span.start, name))
                }
                _ => None,
            })
        });
        if let Some((pos, name)) = undefined {
            c.error = Some((pos, format!("Export '{name}' is not defined")));
        }
    }
    c.error.map(|(pos, message)| e::js_parse_error(pos as usize, &message))
}

use oxc_span::GetSpan;
