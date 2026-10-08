//! The errors `remove_typescript_nodes` throws for TypeScript that Svelte can't strip
//! (enums, decorators, accessor fields, parameter properties, namespaces with values).

use oxc_ast::ast::*;
use oxc_ast_visit::{Visit, walk};
use oxc_span::GetSpan;

use crate::ast::Root;
use crate::error::{CompileError, Result};
use crate::errors as e;

const DECORATORS: &str = "decorators (related TSC proposal is not stage 4 yet)";
const ACCESSORS: &str = "accessor fields (related TSC proposal is not stage 4 yet)";
const MODIFIERS: &str = "accessibility modifiers on constructor parameters";

struct Checker {
    error: Option<CompileError>,
}

impl Checker {
    fn fail(&mut self, span: oxc_span::Span, feature: &str) {
        if self.error.is_none() {
            self.error = Some(e::typescript_invalid_feature((span.start as usize, span.end as usize), feature));
        }
    }

    /// Whether `remove_typescript_nodes` turns a statement into `b.empty`
    fn removed(&mut self, s: &Statement) -> bool {
        match s {
            Statement::TSInterfaceDeclaration(_) | Statement::TSTypeAliasDeclaration(_) => true,
            Statement::VariableDeclaration(v) => v.declare,
            Statement::FunctionDeclaration(f) => f.declare || f.body.is_none(),
            Statement::ClassDeclaration(c) => c.declare,
            Statement::ImportDeclaration(i) => {
                i.import_kind.is_type()
                    || i.specifiers.as_ref().is_some_and(|s| !s.is_empty() && s.iter().all(super::nodes::is_type_specifier))
            }
            Statement::ExportNamedDeclaration(x) => {
                x.export_kind.is_type() || (!x.specifiers.is_empty() && x.specifiers.iter().all(|s| s.export_kind.is_type()))
            }
            Statement::ExportFromDeclaration(x) => {
                x.export_kind.is_type() || (!x.specifiers.is_empty() && x.specifiers.iter().all(|s| s.export_kind.is_type()))
            }
            Statement::ExportAllDeclaration(x) => x.export_kind.is_type(),
            Statement::ExportDeclaration(x) => match &x.declaration {
                Declaration::VariableDeclaration(v) => v.declare,
                Declaration::FunctionDeclaration(f) => f.declare || f.body.is_none(),
                Declaration::ClassDeclaration(c) => c.declare,
                Declaration::TSInterfaceDeclaration(_) | Declaration::TSTypeAliasDeclaration(_) => true,
                Declaration::TSNamespaceDeclaration(_)
                | Declaration::TSExternalModuleDeclaration(_)
                | Declaration::TSGlobalDeclaration(_) => true,
                _ => false,
            },
            Statement::TSNamespaceDeclaration(_) | Statement::TSExternalModuleDeclaration(_) | Statement::TSGlobalDeclaration(_) => true,
            _ => false,
        }
    }

    fn module_block(&mut self, span: oxc_span::Span, body: &TSModuleBlock) {
        let mut kept = false;
        for s in &body.body {
            self.visit_statement(s);
            if !self.removed(s) {
                kept = true;
            }
        }
        if kept {
            self.fail(span, "namespaces with non-type nodes");
        }
    }
}

impl<'a> Visit<'a> for Checker {
    fn visit_decorator(&mut self, it: &Decorator<'a>) {
        self.fail(it.span, DECORATORS);
    }

    fn visit_accessor_property(&mut self, it: &AccessorProperty<'a>) {
        self.fail(it.span, ACCESSORS);
        walk::walk_accessor_property(self, it);
    }

    fn visit_ts_enum_declaration(&mut self, it: &TSEnumDeclaration<'a>) {
        self.fail(it.span, "enums");
    }

    fn visit_method_definition(&mut self, it: &MethodDefinition<'a>) {
        for d in &it.decorators {
            self.visit_decorator(d);
        }
        self.visit_property_key(&it.key);
        if it.kind == MethodDefinitionKind::Constructor {
            for p in &it.value.params.items {
                if p.readonly || p.accessibility.is_some() {
                    self.fail(p.span(), MODIFIERS);
                }
            }
        }
        self.visit_function(&it.value, oxc_syntax::scope::ScopeFlags::empty());
    }

    fn visit_ts_namespace_declaration(&mut self, it: &TSNamespaceDeclaration<'a>) {
        match &it.body {
            TSNamespaceDeclarationBody::TSModuleBlock(b) => self.module_block(it.span, b),
            TSNamespaceDeclarationBody::TSNamespaceDeclaration(n) => self.visit_ts_namespace_declaration(n),
        }
    }

    fn visit_ts_external_module_declaration(&mut self, it: &TSExternalModuleDeclaration<'a>) {
        if let Some(b) = &it.body {
            self.module_block(it.span, b);
        }
    }

    fn visit_ts_global_declaration(&mut self, it: &TSGlobalDeclaration<'a>) {
        self.module_block(it.span, &it.body);
    }
}

pub fn check(_ast: &crate::ast::Ast, root: &Root) -> Result<()> {
    // `remove_typescript_nodes` runs on the fragment, the instance and then the module
    for script in [&root.instance, &root.module].into_iter().flatten() {
        let mut checker = Checker { error: None };
        checker.visit_program(&script.content.program);
        if let Some(err) = checker.error {
            return Err(err);
        }
    }
    Ok(())
}
