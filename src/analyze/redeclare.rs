//! The scope check acorn does while parsing a `<script>` (which oxc's parser leaves to its
//! semantic analysis): `Identifier 'x' has already been declared`, which Svelte reports as a
//! `js_parse_error`. (acorn's `Export 'x' is not defined` never fires: Svelte's components
//! can export snippets declared in the template.)
//!
//! A port of acorn's `scope.js` (`declareName`, `checkLocalExport`) for module code, which is
//! strict: functions at the top level are lexical, functions directly in a function are vars.

use oxc_ast::ast::*;
use oxc_ast_visit::{Visit, walk};
use oxc_syntax::scope::ScopeFlags;

use crate::error::CompileError;
use crate::errors as e;

const TOP: u8 = 1;
const FUNCTION: u8 = 2;
const SIMPLE_CATCH: u8 = 4;
const STATIC_BLOCK: u8 = 8;
const VAR: u8 = TOP | FUNCTION | STATIC_BLOCK;

#[derive(Clone, Copy, PartialEq)]
enum Bind {
    Var,
    Lexical,
    SimpleCatch,
}

#[derive(Default)]
struct Scope<'a> {
    flags: u8,
    var: Vec<&'a str>,
    lexical: Vec<&'a str>,
    functions: Vec<&'a str>,
}

struct Checker<'a> {
    scopes: Vec<Scope<'a>>,
    error: Option<(u32, String)>,
    ts: bool,
}

impl<'a> Checker<'a> {
    fn enter(&mut self, flags: u8) {
        self.scopes.push(Scope { flags, ..Default::default() });
    }

    fn exit(&mut self) {
        self.scopes.pop();
    }

    fn treat_functions_as_var_in(scope: &Scope) -> bool {
        // module code: only function scopes
        scope.flags & FUNCTION != 0
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
            self.error = Some((pos, format!("Identifier '{name}' has already been declared")));
        }
    }

    fn declare_pattern(&mut self, pattern: &BindingPattern<'a>, bind: Bind) {
        match pattern {
            BindingPattern::BindingIdentifier(id) => self.declare(id.name.as_str(), bind, id.span.start),
            BindingPattern::ObjectPattern(o) => {
                for p in &o.properties {
                    self.declare_pattern(&p.value, bind);
                }
                if let Some(r) = &o.rest {
                    self.declare_pattern(&r.argument, bind);
                }
            }
            BindingPattern::ArrayPattern(a) => {
                for el in a.elements.iter().flatten() {
                    self.declare_pattern(el, bind);
                }
                if let Some(r) = &a.rest {
                    self.declare_pattern(&r.argument, bind);
                }
            }
            BindingPattern::AssignmentPattern(a) => self.declare_pattern(&a.left, bind),
        }
    }

    fn declare_params(&mut self, params: &FormalParameters<'a>) {
        for p in &params.items {
            if let BindingPattern::BindingIdentifier(id) = &p.pattern {
                if self.ts && id.name == "this" {
                    continue;
                }
            }
            self.declare_pattern(&p.pattern, Bind::Var);
        }
        if let Some(rest) = &params.rest {
            self.declare_pattern(&rest.rest.argument, Bind::Var);
        }
    }

    /// A function's parameters and body, in its own scope
    fn function_inner(&mut self, params: &FormalParameters<'a>, body: Option<&FunctionBody<'a>>) {
        self.enter(FUNCTION);
        self.declare_params(params);
        self.visit_formal_parameters(params);
        if let Some(body) = body {
            for s in &body.statements {
                self.visit_statement(s);
            }
        }
        self.exit();
    }
}

impl<'a> Visit<'a> for Checker<'a> {
    fn visit_statement(&mut self, it: &Statement<'a>) {
        if self.error.is_none() {
            walk::walk_statement(self, it);
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
            self.declare_pattern(&d.id, bind);
            if let Some(init) = &d.init {
                self.visit_expression(init);
            }
        }
    }

    fn visit_for_statement(&mut self, it: &ForStatement<'a>) {
        self.enter(0);
        walk::walk_for_statement(self, it);
        self.exit();
    }

    fn visit_for_in_statement(&mut self, it: &ForInStatement<'a>) {
        self.enter(0);
        walk::walk_for_in_statement(self, it);
        self.exit();
    }

    fn visit_for_of_statement(&mut self, it: &ForOfStatement<'a>) {
        self.enter(0);
        walk::walk_for_of_statement(self, it);
        self.exit();
    }

    fn visit_switch_statement(&mut self, it: &SwitchStatement<'a>) {
        self.visit_expression(&it.discriminant);
        self.enter(0);
        for c in &it.cases {
            self.visit_switch_case(c);
        }
        self.exit();
    }

    fn visit_catch_clause(&mut self, it: &CatchClause<'a>) {
        match &it.param {
            Some(param) => {
                let simple = matches!(param.pattern, BindingPattern::BindingIdentifier(_));
                self.enter(if simple { SIMPLE_CATCH } else { 0 });
                self.declare_pattern(&param.pattern, if simple { Bind::SimpleCatch } else { Bind::Lexical });
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
        if it.declare || it.body.is_none() {
            // TS overloads and ambient declarations
            return;
        }
        if it.is_declaration() {
            if let Some(id) = &it.id {
                let as_var = Self::treat_functions_as_var_in(self.scopes.last().unwrap());
                // strict mode: async/generators are lexical too
                let bind = if as_var { Bind::Var } else { Bind::Lexical };
                self.declare(id.name.as_str(), bind, id.span.start);
            }
        }
        self.function_inner(&it.params, it.body.as_deref());
    }

    fn visit_arrow_function_expression(&mut self, it: &ArrowFunctionExpression<'a>) {
        match &it.body {
            ArrowFunctionBody::FunctionBody(b) => self.function_inner(&it.params, Some(b)),
            body => {
                self.enter(FUNCTION);
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
                self.declare(id.name.as_str(), Bind::Lexical, id.span.start);
            }
        }
        if let Some(h) = &it.heritage {
            self.visit_expression(&h.expression);
        }
        self.visit_class_body(&it.body);
    }

    fn visit_static_block(&mut self, it: &StaticBlock<'a>) {
        self.enter(STATIC_BLOCK);
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
            self.declare(local.name.as_str(), Bind::Lexical, local.span.start);
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

/// The first scope error acorn would raise while parsing `program`
pub fn check(program: &Program, ts: bool) -> Option<CompileError> {
    let mut c = Checker { scopes: Vec::new(), error: None, ts };
    c.enter(TOP);
    for s in &program.body {
        c.visit_statement(s);
        if c.error.is_some() {
            break;
        }
    }
    c.error.map(|(pos, message)| e::js_parse_error(pos as usize, &message))
}
