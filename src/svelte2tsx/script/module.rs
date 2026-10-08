//! Port of `svelte2tsx/processModuleScriptTag.ts`.

use oxc_ast::ast::*;
use oxc_ast::AstKind;
use oxc_ast_visit::{walk, Visit};

use super::generics::Generics;
use super::instance::handle_type_assertion;
use super::stores::*;
use super::ts::ScriptAst;
use super::*;
use crate::svelte2tsx::htmlx::Verbatim;
use crate::svelte2tsx::transform::{index_of, last_index_of};

pub fn process_module_script_tag(
    out: &mut Out,
    ast: &ScriptAst,
    script: &Verbatim,
    implicit: &mut ImplicitStoreValues,
    rewrite: Option<&crate::svelte2tsx::rewrite_imports::RewriteExternalImports>,
) -> Result<()> {
    rewrite_jsdoc_imports(out, ast, rewrite)?;
    if Generics::new(Some(script)).generics_attr.is_some() {
        return Err(MagicStringError("The generics attribute is only allowed on the instance script".into()));
    }
    let mut w = ModuleWalker { out, off: ast.offset, implicit, rewrite, parents: Vec::new(), err: None };
    for stmt in &ast.program.body {
        w.visit_statement(stmt);
        if let Some(e) = w.err.take() {
            return Err(e);
        }
    }
    implicit.modify_code(ast.offset, out)?;

    let original = out.original();
    let start_tag_end = index_of(original, ">", script.start).map_or(0, |i| i + 1);
    let end_tag_start = last_index_of(original, "<", script.end - 1).unwrap_or(usize::MAX);
    out.ms.overwrite(script.start, start_tag_end, ";", true)?;
    out.ms.overwrite(end_tag_start, script.end, ";", true)?;
    Ok(())
}

struct ModuleWalker<'w, 's, 'a> {
    out: &'w mut Out<'s>,
    off: usize,
    implicit: &'w mut ImplicitStoreValues,
    rewrite: Option<&'w crate::svelte2tsx::rewrite_imports::RewriteExternalImports>,
    parents: Vec<AstKind<'a>>,
    err: Option<MagicStringError>,
}

impl ModuleWalker<'_, '_, '_> {
    fn on_enter(&mut self, kind: AstKind) -> Result<()> {
        if let Some((span, value)) = import_specifier(&kind) {
            rewrite_specifier(self.out, self.off, span, value, self.rewrite)?;
        }
        match kind {
            AstKind::VariableDeclarator(d) => {
                let end = match self.parents.last() {
                    Some(AstKind::VariableDeclaration(v)) if v.declarations.len() > 1 => v.declarations.last().unwrap().span.end,
                    _ => d.span.end,
                };
                self.implicit.add_variable_declaration(VarDeclInfo { names: binding_identifier_names(&d.id), end: end as usize });
            }
            AstKind::CatchParameter(c) => {
                self.implicit.add_variable_declaration(VarDeclInfo { names: binding_identifier_names(&c.pattern), end: c.span.end as usize });
            }
            AstKind::TSTypeAssertion(t) => handle_type_assertion(self.out, self.off, t)?,
            AstKind::TSTypeAliasDeclaration(t) => {
                Generics::throw_if_is_generic(t)?;
                only_in_instance(&t.id.name)?;
            }
            AstKind::TSInterfaceDeclaration(i) => only_in_instance(&i.id.name)?,
            _ => {}
        }
        Ok(())
    }
}

fn only_in_instance(name: &str) -> Result<()> {
    if matches!(name, "$$Events" | "$$Slots" | "$$Props") {
        return Err(MagicStringError(format!("{name} can only be declared in the instance script")));
    }
    Ok(())
}

impl<'a> Visit<'a> for ModuleWalker<'_, '_, 'a> {
    fn enter_node(&mut self, kind: AstKind<'a>) {
        if let Err(e) = self.on_enter(kind) {
            self.err.get_or_insert(e);
        }
        self.parents.push(kind);
    }

    fn leave_node(&mut self, _kind: AstKind<'a>) {
        self.parents.pop();
    }

    fn visit_import_declaration(&mut self, it: &ImportDeclaration<'a>) {
        if let Some(specifiers) = &it.specifiers {
            let default = specifiers.iter().find_map(|s| match s {
                ImportDeclarationSpecifier::ImportDefaultSpecifier(d) => Some(d.local.name.to_string()),
                _ => None,
            });
            self.implicit.add_import_statement(ImportInfo { name: default, svelte_store_derived: false });
            for s in specifiers {
                if let ImportDeclarationSpecifier::ImportSpecifier(s) = s {
                    let derived = s.local.name == "derived" && it.source.value == "svelte/store";
                    self.implicit.add_import_statement(ImportInfo { name: Some(s.local.name.to_string()), svelte_store_derived: derived });
                }
            }
        }
        walk::walk_import_declaration(self, it);
    }
}
