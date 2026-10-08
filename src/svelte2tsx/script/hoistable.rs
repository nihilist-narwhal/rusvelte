//! Port of `svelte2tsx/nodes/HoistableInterfaces.ts`: which interfaces/types the `$props()`
//! type depends on, and whether they can move out of the render function.

use std::collections::HashSet;

use indexmap::IndexMap;
use oxc_ast::ast::*;
use oxc_ast_visit::{walk, Visit};
use oxc_span::GetSpan;

use super::ts::ScriptAst;
use super::*;

/// A node to move: TS's `node.pos` and `node.end` (script-relative)
#[derive(Debug, Clone, Copy)]
pub struct NodeRange {
    pub pos: usize,
    pub end: usize,
}

struct Deps {
    type_deps: HashSet<String>,
    value_deps: HashSet<String>,
    node: NodeRange,
}

#[derive(Default)]
struct PropsInterface {
    name: String,
    node: Option<NodeRange>,
    type_deps: HashSet<String>,
    value_deps: HashSet<String>,
}

#[derive(Default)]
pub struct HoistableInterfaces {
    module_types: HashSet<String>,
    disallowed_types: HashSet<String>,
    disallowed_values: HashSet<String>,
    interface_map: IndexMap<String, Deps>,
    props_interface: PropsInterface,
}

/// The TS view of a top-level statement: `export` is a modifier, not a wrapper
pub fn unwrap_export<'r, 'a>(stmt: &'r Statement<'a>) -> Option<&'r Declaration<'a>> {
    match stmt {
        Statement::ExportDeclaration(e) => Some(&e.declaration),
        _ => stmt.as_declaration(),
    }
}

impl HoistableInterfaces {
    pub fn analyze_snippets(&mut self, root_snippets: &[RootSnippet]) {
        let mut prev: Option<usize> = None;
        while prev != Some(self.disallowed_values.len()) {
            prev = Some(self.disallowed_values.len());
            for s in root_snippets {
                let hoist = s.globals.is_empty() || s.globals.iter().all(|id| self.is_allowed_reference(id));
                if !hoist {
                    self.disallowed_values.insert(s.name.clone());
                }
            }
        }
    }

    /// Should be called before `analyze_instance_script_node`
    pub fn analyze_module_script_node(&mut self, stmt: &Statement) {
        if let Statement::ImportDeclaration(import) = stmt {
            self.analyze_import(import, true);
        }
        match unwrap_export(stmt) {
            Some(Declaration::TSTypeAliasDeclaration(t)) => {
                self.module_types.insert(t.id.name.to_string());
            }
            Some(Declaration::TSInterfaceDeclaration(i)) => {
                self.module_types.insert(i.id.name.to_string());
            }
            Some(Declaration::TSEnumDeclaration(e)) => {
                self.module_types.insert(e.id.name.to_string());
            }
            Some(Declaration::TSNamespaceDeclaration(n)) => {
                self.module_types.insert(n.id.name.to_string());
            }
            Some(Declaration::TSGlobalDeclaration(_)) => {
                self.module_types.insert("global".into());
            }
            _ => {}
        }
    }

    fn analyze_import(&mut self, import: &ImportDeclaration, element_type_only: bool) {
        let Some(specifiers) = &import.specifiers else { return };
        let is_type_only = import.import_kind.is_type();
        for s in specifiers {
            match s {
                ImportDeclarationSpecifier::ImportSpecifier(s) => {
                    if is_type_only || (element_type_only && s.import_kind.is_type()) {
                        self.module_types.insert(s.local.name.to_string());
                    }
                }
                ImportDeclarationSpecifier::ImportDefaultSpecifier(s) => {
                    if is_type_only {
                        self.module_types.insert(s.local.name.to_string());
                    }
                }
                ImportDeclarationSpecifier::ImportNamespaceSpecifier(s) => {
                    if is_type_only {
                        self.module_types.insert(s.local.name.to_string());
                    }
                }
            }
        }
    }

    pub fn analyze_instance_script_node(&mut self, ast: &ScriptAst, stmt: &Statement) {
        if let Statement::ImportDeclaration(import) = stmt {
            self.analyze_import(import, false);
        }
        let node = NodeRange { pos: ast.full_start(stmt.span().start), end: stmt.span().end as usize };
        let decl = unwrap_export(stmt);
        match decl {
            Some(Declaration::TSInterfaceDeclaration(i)) => {
                let name = i.id.name.to_string();
                let generics = type_param_names(i.type_parameters.as_deref());
                let mut type_deps = HashSet::new();
                let mut value_deps = HashSet::new();
                for member in &i.body.body {
                    match member {
                        TSSignature::TSPropertySignature(p) => {
                            if let Some(t) = &p.type_annotation {
                                collect(&t.type_annotation, &mut type_deps, &mut value_deps, &generics, Some(&name));
                            }
                        }
                        TSSignature::TSIndexSignature(s) => {
                            collect(&s.type_annotation.type_annotation, &mut type_deps, &mut value_deps, &generics, Some(&name));
                            collect(&s.parameter.type_annotation.type_annotation, &mut type_deps, &mut value_deps, &generics, Some(&name));
                        }
                        _ => {}
                    }
                }
                for h in &i.extends {
                    if let TSTypeName::IdentifierReference(id) = &h.type_name {
                        if !generics.iter().any(|g| g == id.name.as_str()) {
                            type_deps.insert(id.name.to_string());
                        }
                    }
                    let mut c = Collector { type_deps: &mut type_deps, value_deps: &mut value_deps, generics: &generics, root: Some(&name) };
                    if let Some(args) = &h.type_arguments {
                        c.visit_ts_type_parameter_instantiation(args);
                    }
                }
                if self.module_types.contains(&name) {
                    self.disallowed_types.insert(name);
                } else {
                    self.interface_map.insert(name, Deps { type_deps, value_deps, node });
                }
            }
            Some(Declaration::TSTypeAliasDeclaration(t)) => {
                let name = t.id.name.to_string();
                let generics = type_param_names(t.type_parameters.as_deref());
                let mut type_deps = HashSet::new();
                let mut value_deps = HashSet::new();
                collect(&t.type_annotation, &mut type_deps, &mut value_deps, &generics, Some(&name));
                if self.module_types.contains(&name) {
                    self.disallowed_types.insert(name);
                } else {
                    self.interface_map.insert(name, Deps { type_deps, value_deps, node });
                }
            }
            Some(Declaration::VariableDeclaration(v)) => {
                for d in &v.declarations {
                    let mut ids = Vec::new();
                    binding_names(&d.id, &mut ids);
                    for id in ids {
                        self.disallowed_values.insert(id.name.to_string());
                    }
                }
            }
            Some(Declaration::FunctionDeclaration(f)) => {
                if let Some(id) = &f.id {
                    self.disallowed_values.insert(id.name.to_string());
                }
            }
            Some(Declaration::ClassDeclaration(c)) => {
                if let Some(id) = &c.id {
                    self.disallowed_values.insert(id.name.to_string());
                }
            }
            Some(Declaration::TSEnumDeclaration(e)) => {
                self.disallowed_values.insert(e.id.name.to_string());
            }
            Some(Declaration::TSNamespaceDeclaration(n)) => {
                self.disallowed_types.insert(n.id.name.to_string());
                self.disallowed_values.insert(n.id.name.to_string());
            }
            Some(Declaration::TSGlobalDeclaration(_)) => {
                self.disallowed_types.insert("global".into());
                self.disallowed_values.insert("global".into());
            }
            _ => {}
        }
        // `export default function foo() {}` is a FunctionDeclaration in TS
        if let Statement::ExportDefaultDeclaration(e) = stmt {
            match &e.declaration {
                ExportDefaultDeclarationKind::FunctionDeclaration(f) => {
                    if let Some(id) = &f.id {
                        self.disallowed_values.insert(id.name.to_string());
                    }
                }
                ExportDefaultDeclarationKind::ClassDeclaration(c) => {
                    if let Some(id) = &c.id {
                        self.disallowed_values.insert(id.name.to_string());
                    }
                }
                _ => {}
            }
        }
    }

    /// `analyze$propsRune`, given the type argument (or the declarator's type)
    pub fn analyze_props_rune(&mut self, ast: &ScriptAst, generic_arg: &TSType) {
        if let TSType::TSTypeReference(r) = generic_arg {
            let name = entity_root(&r.type_name);
            if let Some(deps) = self.interface_map.get(&name) {
                self.props_interface.type_deps = deps.type_deps.clone();
                self.props_interface.value_deps = deps.value_deps.clone();
                self.props_interface.name = name;
            }
        } else {
            self.props_interface.name = "$$ComponentProps".into();
            let span = generic_arg.span();
            self.props_interface.node = Some(NodeRange { pos: ast.full_start(span.start), end: span.end as usize });
            let p = &mut self.props_interface;
            collect(generic_arg, &mut p.type_deps, &mut p.value_deps, &[], None);
        }
    }

    pub fn add_disallowed(&mut self, names: impl IntoIterator<Item = String>) {
        self.disallowed_values.extend(names);
    }

    fn determine_hoistable_interfaces(&mut self) -> IndexMap<String, NodeRange> {
        let mut hoistable: IndexMap<String, NodeRange> = IndexMap::new();
        let mut progress = true;
        while progress {
            progress = false;
            for (name, deps) in &self.interface_map {
                if hoistable.contains_key(name) {
                    continue;
                }
                let mut can_hoist = true;
                for dep in &deps.type_deps {
                    if self.disallowed_types.contains(dep) {
                        self.disallowed_types.insert(name.clone());
                        can_hoist = false;
                        break;
                    }
                    if self.interface_map.contains_key(dep) && !hoistable.contains_key(dep) {
                        can_hoist = false;
                    }
                }
                for dep in &deps.value_deps {
                    if !is_allowed_reference(&self.disallowed_values, dep) {
                        self.disallowed_types.insert(name.clone());
                        can_hoist = false;
                        break;
                    }
                }
                if can_hoist {
                    hoistable.insert(name.clone(), deps.node);
                    progress = true;
                }
            }
        }
        if self.props_interface.name == "$$ComponentProps" {
            let p = &self.props_interface;
            let can_hoist = p
                .type_deps
                .iter()
                .chain(p.value_deps.iter())
                .all(|dep| !self.disallowed_types.contains(dep) && self.is_allowed_reference(dep));
            if can_hoist {
                if let Some(node) = p.node {
                    hoistable.insert(p.name.clone(), node);
                }
            }
        }
        hoistable
    }

    /// Moves the hoistable interfaces to the top of the script, if the `$props()` type is
    /// hoistable. Returns what was hoisted.
    pub fn move_hoistable_interfaces(&mut self, out: &mut Out, ast_offset: usize, script_start: usize, generics: &[String]) -> Result<Option<IndexMap<String, NodeRange>>> {
        if self.props_interface.name.is_empty() {
            return Ok(None);
        }
        for g in generics {
            self.disallowed_types.insert(g.clone());
        }
        let hoistable = self.determine_hoistable_interfaces();
        if !hoistable.contains_key(&self.props_interface.name) {
            return Ok(None);
        }
        let original = out.original();
        for (name, node) in &hoistable {
            let mut pos = node.pos + ast_offset;
            if name == "$$ComponentProps" {
                // So that organize imports doesn't mess with the types
                out.ms.prepend_right(pos, "\n")?;
            } else {
                // node.pos includes preceding whitespace, which could mean we accidentally also
                // move stuff appended to a previous node
                if original.as_bytes().get(pos) == Some(&b'\r') {
                    pos += 1;
                }
                if crate::svelte2tsx::transform::is_space_at(original, pos) {
                    pos = crate::svelte2tsx::transform::next_char(original, pos);
                }
                out.ms.prepend_right(pos, ";\n")?;
                out.ms.append_left(node.end + ast_offset, ";")?;
            }
            out.ms.move_(pos, node.end + ast_offset, script_start)?;
        }
        Ok(Some(hoistable))
    }

    pub fn is_allowed_reference(&self, reference: &str) -> bool {
        is_allowed_reference(&self.disallowed_values, reference)
    }
}

fn is_allowed_reference(disallowed_values: &HashSet<String>, reference: &str) -> bool {
    let mut chars = reference.chars();
    let store = chars.next() == Some('$') && chars.next() != Some('$') && disallowed_values.contains(&reference[1..]);
    !(disallowed_values.contains(reference) || matches!(reference, "$$props" | "$$restProps" | "$$slots") || store)
}

/// A root `{#snippet}` with the names it references from outside
pub struct RootSnippet {
    pub start: usize,
    pub end: usize,
    pub globals: Vec<String>,
    pub name: String,
}

fn type_param_names(params: Option<&TSTypeParameterDeclaration>) -> Vec<String> {
    params.map(|p| p.params.iter().map(|p| p.name.name.to_string()).collect()).unwrap_or_default()
}

/// `getEntityNameRoot`: `foo.bar.baz` -> `foo`
pub fn entity_root(name: &TSTypeName) -> String {
    match name {
        TSTypeName::IdentifierReference(id) => id.name.to_string(),
        TSTypeName::QualifiedName(q) => entity_root(&q.left),
        TSTypeName::ThisExpression(_) => "this".into(),
    }
}

fn collect(ty: &TSType, type_deps: &mut HashSet<String>, value_deps: &mut HashSet<String>, generics: &[String], root: Option<&str>) {
    let mut c = Collector { type_deps, value_deps, generics, root };
    c.visit_ts_type(ty);
}

struct Collector<'c> {
    type_deps: &'c mut HashSet<String>,
    value_deps: &'c mut HashSet<String>,
    generics: &'c [String],
    root: Option<&'c str>,
}

impl<'a> Visit<'a> for Collector<'_> {
    fn visit_ts_type_reference(&mut self, it: &TSTypeReference<'a>) {
        let name = entity_root(&it.type_name);
        if Some(name.as_str()) != self.root && !self.generics.contains(&name) {
            self.type_deps.insert(name);
        }
        walk::walk_ts_type_reference(self, it);
    }

    fn visit_ts_type_query(&mut self, it: &TSTypeQuery<'a>) {
        if let Some(name) = it.expr_name.as_ts_type_name() {
            self.value_deps.insert(entity_root(name));
        }
        walk::walk_ts_type_query(self, it);
    }
}
