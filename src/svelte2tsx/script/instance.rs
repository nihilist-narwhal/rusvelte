//! Port of `svelte2tsx/processInstanceScriptContent.ts`.
//!
//! svelte2tsx walks the script with TypeScript's `forEachChild`; this walks oxc's AST and
//! reports nodes the way TypeScript's AST has them (`export` is a modifier, a shorthand
//! property has one identifier, a parameter's name and initializer have the parameter as
//! their parent, ...), since the flags the walk keeps depend on that.

use std::collections::HashSet;

use oxc_ast::ast::*;
use oxc_ast::AstKind;
use oxc_ast_visit::{walk, Visit};
use oxc_span::{GetSpan, Span};

use super::events::{ComponentEvents, TypeDeclRef};
use super::exported::{ExportedNames, ListRef};
use super::generics::Generics;
use super::stores::*;
use super::ts::ScriptAst;
use super::*;
use crate::svelte2tsx::htmlx::Verbatim;

pub struct InstanceResult {
    pub uses_props: bool,
    pub uses_rest_props: bool,
    pub uses_slots: bool,
    pub uses_slots_interface: bool,
    pub generics: Generics,
    /// Svelte 5 only
    pub has_top_level_await: bool,
}

/// Where an identifier sits, as far as `handleIdentifier` cares (its TS parent)
#[derive(Clone, Copy, PartialEq, Debug)]
enum Role {
    /// a `LabeledStatement`'s label
    Label,
    /// the name or initializer of a `Parameter`
    Parameter,
    /// a `BindingElement`'s `propertyName`
    BindingPropName,
    /// an `ImportSpecifier`'s `propertyName`
    ImportPropName,
    /// the name in a property access, property assignment, property signature, property
    /// declaration, type reference, type alias or interface: never a store
    Excluded,
    Other,
}

struct ScopeData {
    declared: HashSet<String>,
    parent: Option<usize>,
}

struct Pending {
    name: String,
    scope: usize,
    is_props_id: bool,
}

/// An interface or type alias (`InterfacesAndTypes`): name, TS start (with `export`), end
struct TypeDecl {
    name: String,
    start: u32,
    end: u32,
}

struct Walker<'w, 'r, 'a, 's> {
    out: &'w mut Out<'s>,
    ast: &'r ScriptAst<'a>,
    off: usize,
    script_start: usize,
    exported: &'w mut ExportedNames<'r, 'a>,
    events: &'w mut ComponentEvents,
    implicit: &'w mut ImplicitStoreValues,
    top_names: ImplicitTopLevelNames,
    generics: Generics,
    type_decls: Vec<TypeDecl>,
    mode_ts: bool,
    svelte5_plus: bool,
    uses_props: bool,
    uses_rest_props: bool,
    uses_slots: bool,
    uses_slots_interface: bool,
    has_top_level_await: bool,
    is_declaration: bool,
    /// `variableDeclarationNode`: set while in a declaration's name, with whether its
    /// initializer is `$props()`
    var_decl_props: Option<bool>,
    is_props_declaration_rune: bool,
    scopes: Vec<ScopeData>,
    current: usize,
    pending: Vec<Pending>,
    parents: Vec<AstKind<'a>>,
    err: Option<MagicStringError>,
}

#[allow(clippy::too_many_arguments)]
pub fn process_instance_script_content<'r, 'a>(
    out: &mut Out,
    ast: &'r ScriptAst<'a>,
    script: &Verbatim,
    exported: &mut ExportedNames<'r, 'a>,
    events: &mut ComponentEvents,
    implicit: &mut ImplicitStoreValues,
    mode_ts: bool,
    module_ast: Option<&ScriptAst>,
    svelte5_plus: bool,
) -> Result<InstanceResult> {
    let off = ast.offset;
    if let Some(m) = module_ast {
        for stmt in &m.program.body {
            exported.hoistable.analyze_module_script_node(stmt);
        }
    }
    let mut w = Walker {
        out,
        ast,
        off,
        script_start: script.start,
        exported,
        events,
        implicit,
        top_names: ImplicitTopLevelNames::default(),
        generics: Generics::new(Some(script)),
        type_decls: Vec::new(),
        mode_ts,
        svelte5_plus,
        uses_props: false,
        uses_rest_props: false,
        uses_slots: false,
        uses_slots_interface: false,
        has_top_level_await: false,
        is_declaration: false,
        var_decl_props: None,
        is_props_declaration_rune: false,
        scopes: vec![ScopeData { declared: HashSet::new(), parent: None }],
        current: 0,
        pending: Vec::new(),
        parents: Vec::new(),
        err: None,
    };
    for stmt in &ast.program.body {
        w.exported.hoistable.analyze_instance_script_node(ast, stmt);
        w.visit_statement(stmt);
        if let Some(e) = w.err.take() {
            return Err(e);
        }
    }

    // resolve stores
    if w.is_props_declaration_rune {
        w.pending.retain(|p| !p.is_props_id);
    }
    for p in std::mem::take(&mut w.pending) {
        let mut scope = Some(p.scope);
        let mut declared = false;
        while let Some(s) = scope {
            if w.scopes[s].declared.contains(&p.name) {
                declared = true;
                break;
            }
            scope = w.scopes[s].parent;
        }
        if !declared {
            w.implicit.add_store_access(p.name[1..].to_string());
        }
    }

    // declare implicit reactive variables we found in the script
    w.top_names.modify_code(w.out, off, &w.scopes[0].declared)?;
    w.implicit.modify_code(off, w.out)?;

    handle_first_instance_import(w.out, ast, module_ast.is_some())?;

    // move interfaces and types out of the render function if they are referenced by a
    // $$Generic, otherwise they would be used before being defined
    let type_refs = w.generics.type_references().to_vec();
    for d in w.type_decls.iter().filter(|d| type_refs.contains(&d.name)) {
        move_node(w.out, ast, script.start, d.start, d.end)?;
    }

    w.exported.hoistable.add_disallowed(w.implicit.accessed_stores());
    let references = w.generics.references().to_vec();
    // +1 because imports are also moved there, and interfaces go after imports
    w.exported.hoistable.move_hoistable_interfaces(w.out, off, script.start + 1, &references)?;

    Ok(InstanceResult {
        uses_props: w.uses_props,
        uses_rest_props: w.uses_rest_props,
        uses_slots: w.uses_slots,
        uses_slots_interface: w.uses_slots_interface,
        generics: w.generics,
        has_top_level_await: w.has_top_level_await,
    })
}

impl<'w, 'r, 'a, 's> Walker<'w, 'r, 'a, 's> {
    fn fail(&mut self, r: Result<()>) {
        if let Err(e) = r {
            self.err.get_or_insert(e);
        }
    }

    fn push_scope(&mut self) {
        self.scopes.push(ScopeData { declared: HashSet::new(), parent: Some(self.current) });
        self.current = self.scopes.len() - 1;
    }

    fn pop_scope(&mut self) {
        self.current = self.scopes[self.current].parent.unwrap_or(0);
    }

    fn is_scope_node(kind: &AstKind) -> bool {
        match kind {
            AstKind::Function(_)
            | AstKind::ArrowFunctionExpression(_)
            | AstKind::FunctionBody(_)
            | AstKind::BlockStatement(_)
            | AstKind::StaticBlock(_)
            | AstKind::MethodDefinition(_)
            | AstKind::TSCallSignatureDeclaration(_)
            | AstKind::TSConstructSignatureDeclaration(_)
            | AstKind::TSMethodSignature(_)
            | AstKind::TSIndexSignature(_)
            | AstKind::TSFunctionType(_)
            | AstKind::TSConstructorType(_) => true,
            AstKind::ObjectProperty(p) => p.method || p.kind != PropertyKind::Init,
            _ => false,
        }
    }

    /// `handleIdentifier`
    fn identifier(&mut self, name: &str, role: Role) {
        match name {
            "$$props" => {
                self.uses_props = true;
                return;
            }
            "$$restProps" => {
                self.uses_rest_props = true;
                return;
            }
            "$$slots" => {
                self.uses_slots = true;
                return;
            }
            _ => {}
        }
        if role == Role::Label {
            return;
        }
        if name == "props" && self.var_decl_props == Some(true) {
            self.is_props_declaration_rune = true;
        }
        if self.is_declaration || role == Role::Parameter {
            if role != Role::ImportPropName && role != Role::BindingPropName && (name.starts_with('$') || self.current == 0) {
                // track all top level declared identifiers and all $ prefixed identifiers
                self.scopes[self.current].declared.insert(name.to_string());
            }
            return;
        }
        if !name.starts_with('$') || role == Role::Excluded {
            return;
        }
        let n = self.parents.len();
        let parent = self.parents.last();
        let grandparent = n.checked_sub(2).map(|i| &self.parents[i]);
        let mut is_props_id = false;
        if name == "$props" {
            if let (Some(AstKind::StaticMemberExpression(m)), Some(AstKind::CallExpression(c))) = (parent, grandparent) {
                if c.arguments.is_empty() {
                    is_props_id = text(self.ast.text, m.span) == "$props.id";
                }
            }
        }
        // `const { ...props } = $props()`
        let is_rune = matches!(name, "$props" | "$derived" | "$state")
            && matches!(parent, Some(AstKind::CallExpression(_)))
            && matches!(grandparent, Some(AstKind::VariableDeclarator(d)) if text(self.ast.text, d.id.span()).contains(&name[1..]));
        if !is_rune {
            self.pending.push(Pending { name: name.to_string(), scope: self.current, is_props_id });
        }
    }

    /// A binding element's name (the TS `BindingElement.name` toggles the declaration flag)
    fn binding_element(&mut self, p: &BindingPattern<'a>) {
        match p {
            BindingPattern::AssignmentPattern(a) => {
                let kind = AstKind::AssignmentPattern(self.alloc(a));
                self.enter_node(kind);
                self.is_declaration = true;
                self.visit_binding_pattern(&a.left);
                self.is_declaration = false;
                self.visit_expression(&a.right);
                self.leave_node(kind);
            }
            _ => {
                self.is_declaration = true;
                self.visit_binding_pattern(p);
                self.is_declaration = false;
            }
        }
    }

    /// The TS node's span for a declaration that may be wrapped in `export`
    fn ts_span(&self, span: Span) -> Span {
        match self.parents.last() {
            Some(AstKind::ExportDeclaration(e)) => e.span,
            _ => span,
        }
    }

    fn export_start(&self) -> Option<u32> {
        match self.parents.last() {
            Some(AstKind::ExportDeclaration(e)) => Some(e.span.start),
            Some(AstKind::ExportDefaultDeclaration(e)) => Some(e.span.start),
            _ => None,
        }
    }

    fn type_declaration(&mut self, name: &str, decl: TypeDeclRef, span: Span) -> Result<()> {
        let ts_span = self.ts_span(span);
        if let TypeDeclRef::Alias(t) = decl {
            self.generics.add_if_is_generic(self.out, self.off, t, ts_span, self.ast.text)?;
        }
        match name {
            "$$Events" => self.events.set_component_events_interface(self.ast, decl)?,
            "$$Slots" => self.uses_slots_interface = true,
            "$$Props" => self.exported.uses_props_type = true,
            _ => {}
        }
        self.type_decls.push(TypeDecl { name: name.to_string(), start: ts_span.start, end: ts_span.end });
        Ok(())
    }

    /// What `walk` does when entering a TS node
    fn on_enter(&mut self, kind: AstKind<'a>) -> Result<()> {
        let depth = self.parents.len();
        let parent = self.parents.last().copied();
        let ast = self.ast;
        match kind {
            AstKind::VariableDeclaration(v) => {
                // a VariableStatement (not the list of a `for`)
                if !matches!(parent, Some(AstKind::ForStatement(_) | AstKind::ForInStatement(_) | AstKind::ForOfStatement(_))) {
                    let export_start = self.export_start();
                    let top_level = depth == 0 || (depth == 1 && export_start.is_some());
                    let list = ListRef { list: v, stmt_start: export_start.unwrap_or(v.span.start) };
                    self.exported.handle_variable_statement(self.out, ast, list, export_start, top_level)?;
                }
            }
            AstKind::Function(f) => {
                if matches!(f.r#type, FunctionType::FunctionDeclaration | FunctionType::TSDeclareFunction) {
                    if let Some(start) = self.export_start() {
                        let name = f.id.as_ref().map(|id| id.name.as_str());
                        self.exported.handle_export_function_or_class(self.out, ast, start, name)?;
                    }
                }
            }
            AstKind::Class(c) => {
                if c.r#type == ClassType::ClassDeclaration {
                    if let Some(start) = self.export_start() {
                        let name = c.id.as_ref().map(|id| id.name.as_str());
                        self.exported.handle_export_function_or_class(self.out, ast, start, name)?;
                    }
                }
            }
            AstKind::ExportNamedDeclaration(e) => self.exported.handle_export_declaration(self.out, ast, &e.specifiers, e.span)?,
            AstKind::ExportFromDeclaration(e) => self.exported.handle_export_declaration(self.out, ast, &e.specifiers, e.span)?,
            AstKind::ImportDeclaration(i) => {
                move_node(self.out, ast, self.script_start(), i.span.start, i.span.end)?;
                self.events.check_if_import_is_event_dispatcher(i);
            }
            AstKind::TSImportEqualsDeclaration(i) => {
                let end = i.span.end as usize + self.off;
                let original = self.out.original();
                if original.as_bytes().get(end.wrapping_sub(1)) != Some(&b';') {
                    self.out.prepend_str(end, ";")?;
                }
            }
            AstKind::VariableDeclarator(d) => {
                self.events.check_if_is_string_literal_declaration(d);
                self.events.check_if_declaration_instantiated_event_dispatcher(self.out, ast, d)?;
                // only top level declarations can be stores
                let top_level = match depth {
                    1 => true,
                    2 => matches!(
                        self.parents[0],
                        AstKind::ExportDeclaration(_) | AstKind::ForStatement(_) | AstKind::ForInStatement(_) | AstKind::ForOfStatement(_)
                    ),
                    _ => false,
                };
                if top_level {
                    let end = match parent {
                        Some(AstKind::VariableDeclaration(v)) if v.declarations.len() > 1 => v.declarations.last().unwrap().span.end,
                        _ => d.span.end,
                    };
                    self.implicit.add_variable_declaration(VarDeclInfo { names: binding_identifier_names(&d.id), end: end as usize });
                }
            }
            AstKind::CatchParameter(c) => {
                if depth == 2 {
                    self.implicit.add_variable_declaration(VarDeclInfo { names: binding_identifier_names(&c.pattern), end: c.span.end as usize });
                }
            }
            AstKind::CallExpression(c) => self.events.check_if_call_expression_is_dispatch(c)?,
            AstKind::TSTypeAliasDeclaration(t) => self.type_declaration(&t.id.name, TypeDeclRef::Alias(t), t.span)?,
            AstKind::TSInterfaceDeclaration(i) => self.type_declaration(&i.id.name, TypeDeclRef::Interface(i), i.span)?,
            AstKind::LabeledStatement(l) => {
                if depth == 0 && l.label.name == "$" {
                    if binary_assignment(l).is_some() {
                        self.top_names.add(l);
                        self.implicit.add_reactive_declaration(ReactiveInfo { names: names_from_labeled_statement(l), end: l.span.end as usize });
                    }
                    self.top_names.handle_reactive_statement(self.out, self.off, ast.text, l)?;
                }
            }
            AstKind::AwaitExpression(a) => {
                if self.svelte5_plus && self.current == 0 && self.ts_await_expression(a.span.start) {
                    self.has_top_level_await = true;
                }
            }
            AstKind::TSTypeAssertion(t) => {
                if !self.mode_ts {
                    handle_type_assertion(self.out, self.off, t)?;
                }
            }
            _ => {}
        }
        if Self::is_scope_node(&kind) {
            self.push_scope();
        }
        Ok(())
    }

    /// Whether TS parses this `await` as an AwaitExpression: always in a module, but in a
    /// script only when an identifier, keyword or literal follows on the same line
    fn ts_await_expression(&self, start: u32) -> bool {
        let ast = self.ast;
        let is_module = ast.program.body.iter().any(|s| {
            matches!(
                s,
                Statement::ImportDeclaration(_)
                    | Statement::ExportAllDeclaration(_)
                    | Statement::ExportDefaultDeclaration(_)
                    | Statement::ExportDeclaration(_)
                    | Statement::ExportNamedDeclaration(_)
                    | Statement::ExportFromDeclaration(_)
                    | Statement::TSExportAssignment(_)
            )
        });
        if is_module {
            return true;
        }
        let Some((s, _)) = ast.token_at_or_after(start + "await".len() as u32) else { return false };
        let between = &ast.text[start as usize + 5..s as usize];
        let next = ast.text[s as usize..].chars().next().unwrap_or(' ');
        !between.contains(['\n', '\r', '\u{2028}', '\u{2029}'])
            && (next.is_alphanumeric() || matches!(next, '_' | '$' | '"' | '\'' | '`' | '\\') || !next.is_ascii())
    }

    fn script_start(&self) -> usize {
        self.script_start
    }
}

impl<'w, 'r, 'a, 's> Visit<'a> for Walker<'w, 'r, 'a, 's> {
    fn enter_node(&mut self, kind: AstKind<'a>) {
        let r = self.on_enter(kind);
        self.fail(r);
        self.parents.push(kind);
    }

    fn leave_node(&mut self, kind: AstKind<'a>) {
        self.parents.pop();
        if Self::is_scope_node(&kind) {
            self.pop_scope();
        }
    }

    fn visit_identifier_name(&mut self, it: &IdentifierName<'a>) {
        let role = match self.parents.last() {
            Some(AstKind::StaticMemberExpression(_)) => Role::Excluded,
            Some(AstKind::ObjectProperty(p)) => {
                if p.method || p.kind != PropertyKind::Init {
                    Role::Other
                } else {
                    Role::Excluded
                }
            }
            Some(AstKind::BindingProperty(_)) => Role::BindingPropName,
            Some(
                AstKind::AssignmentTargetPropertyProperty(_)
                | AstKind::PropertyDefinition(_)
                | AstKind::AccessorProperty(_)
                | AstKind::TSPropertySignature(_),
            ) => Role::Excluded,
            Some(AstKind::ImportSpecifier(_)) => Role::ImportPropName,
            _ => Role::Other,
        };
        self.identifier(&it.name, role);
        walk::walk_identifier_name(self, it);
    }

    fn visit_identifier_reference(&mut self, it: &IdentifierReference<'a>) {
        let role = match self.parents.last() {
            Some(AstKind::TSTypeReference(_)) => Role::Excluded,
            Some(AstKind::FormalParameter(_)) => Role::Parameter,
            _ => Role::Other,
        };
        self.identifier(&it.name, role);
        walk::walk_identifier_reference(self, it);
    }

    fn visit_binding_identifier(&mut self, it: &BindingIdentifier<'a>) {
        let role = match self.parents.last() {
            Some(AstKind::FormalParameter(_) | AstKind::FormalParameterRest(_)) => Role::Parameter,
            Some(AstKind::TSTypeAliasDeclaration(_) | AstKind::TSInterfaceDeclaration(_)) => Role::Excluded,
            _ => Role::Other,
        };
        self.identifier(&it.name, role);
        walk::walk_binding_identifier(self, it);
    }

    fn visit_label_identifier(&mut self, it: &LabelIdentifier<'a>) {
        let role = match self.parents.last() {
            Some(AstKind::LabeledStatement(_)) => Role::Label,
            _ => Role::Other,
        };
        self.identifier(&it.name, role);
        walk::walk_label_identifier(self, it);
    }

    fn visit_ts_index_signature_name(&mut self, it: &TSIndexSignatureName<'a>) {
        let kind = AstKind::TSIndexSignatureName(self.alloc(it));
        self.enter_node(kind);
        self.identifier(&it.name, Role::Parameter);
        self.visit_ts_type_annotation(&it.type_annotation);
        self.leave_node(kind);
    }

    fn visit_variable_declarator(&mut self, it: &VariableDeclarator<'a>) {
        let kind = AstKind::VariableDeclarator(self.alloc(it));
        self.enter_node(kind);
        let props_call = matches!(&it.init, Some(Expression::CallExpression(c)) if text(self.ast.text, c.span) == "$props()");
        self.var_decl_props = Some(props_call);
        self.is_declaration = true;
        self.visit_binding_pattern(&it.id);
        self.is_declaration = false;
        self.var_decl_props = None;
        if let Some(t) = &it.type_annotation {
            self.visit_ts_type_annotation(t);
        }
        if let Some(init) = &it.init {
            self.visit_expression(init);
        }
        self.leave_node(kind);
    }

    fn visit_catch_parameter(&mut self, it: &CatchParameter<'a>) {
        let kind = AstKind::CatchParameter(self.alloc(it));
        self.enter_node(kind);
        self.var_decl_props = Some(false);
        self.is_declaration = true;
        self.visit_binding_pattern(&it.pattern);
        self.is_declaration = false;
        self.var_decl_props = None;
        if let Some(t) = &it.type_annotation {
            self.visit_ts_type_annotation(t);
        }
        self.leave_node(kind);
    }

    fn visit_binding_property(&mut self, it: &BindingProperty<'a>) {
        let kind = AstKind::BindingProperty(self.alloc(it));
        self.enter_node(kind);
        if !it.shorthand {
            self.visit_property_key(&it.key);
        }
        self.binding_element(&it.value);
        self.leave_node(kind);
    }

    fn visit_array_pattern(&mut self, it: &ArrayPattern<'a>) {
        let kind = AstKind::ArrayPattern(self.alloc(it));
        self.enter_node(kind);
        for el in it.elements.iter().flatten() {
            self.binding_element(el);
        }
        if let Some(rest) = &it.rest {
            self.visit_binding_rest_element(rest);
        }
        self.leave_node(kind);
    }

    fn visit_binding_rest_element(&mut self, it: &BindingRestElement<'a>) {
        let kind = AstKind::BindingRestElement(self.alloc(it));
        self.enter_node(kind);
        self.is_declaration = true;
        self.visit_binding_pattern(&it.argument);
        self.is_declaration = false;
        self.leave_node(kind);
    }

    fn visit_formal_parameter_rest(&mut self, it: &FormalParameterRest<'a>) {
        // a TS `Parameter` with a `...`: its name is the parameter's child
        let kind = AstKind::FormalParameterRest(self.alloc(it));
        self.enter_node(kind);
        self.visit_decorators(&it.decorators);
        self.visit_binding_pattern(&it.rest.argument);
        if let Some(t) = &it.type_annotation {
            self.visit_ts_type_annotation(t);
        }
        self.leave_node(kind);
    }

    fn visit_object_property(&mut self, it: &ObjectProperty<'a>) {
        if it.shorthand {
            // a `ShorthandPropertyAssignment` has one identifier
            let kind = AstKind::ObjectProperty(self.alloc(it));
            self.enter_node(kind);
            self.visit_expression(&it.value);
            self.leave_node(kind);
        } else {
            walk::walk_object_property(self, it);
        }
    }

    fn visit_import_declaration(&mut self, it: &ImportDeclaration<'a>) {
        let kind = AstKind::ImportDeclaration(self.alloc(it));
        self.enter_node(kind);
        if let Some(specifiers) = &it.specifiers {
            // ImportClause
            self.is_declaration = true;
            let default = specifiers.iter().find_map(|s| match s {
                ImportDeclarationSpecifier::ImportDefaultSpecifier(d) => Some(d.local.name.to_string()),
                _ => None,
            });
            self.implicit.add_import_statement(ImportInfo { name: default, svelte_store_derived: false });
            for s in specifiers {
                match s {
                    ImportDeclarationSpecifier::ImportSpecifier(s) => {
                        let derived = s.local.name == "derived" && it.source.value == "svelte/store";
                        self.implicit.add_import_statement(ImportInfo { name: Some(s.local.name.to_string()), svelte_store_derived: derived });
                        self.visit_import_specifier(s);
                    }
                    ImportDeclarationSpecifier::ImportDefaultSpecifier(s) => self.visit_import_default_specifier(s),
                    ImportDeclarationSpecifier::ImportNamespaceSpecifier(s) => self.visit_import_namespace_specifier(s),
                }
            }
            self.is_declaration = false;
        }
        self.visit_string_literal(&it.source);
        if let Some(w) = &it.with_clause {
            self.visit_with_clause(w);
        }
        self.leave_node(kind);
    }

    fn visit_import_specifier(&mut self, it: &ImportSpecifier<'a>) {
        let kind = AstKind::ImportSpecifier(self.alloc(it));
        self.enter_node(kind);
        if it.imported.span() != it.local.span {
            self.visit_module_export_name(&it.imported);
        }
        self.visit_binding_identifier(&it.local);
        self.leave_node(kind);
    }

    fn visit_export_specifier(&mut self, it: &ExportSpecifier<'a>) {
        let kind = AstKind::ExportSpecifier(self.alloc(it));
        self.enter_node(kind);
        self.visit_module_export_name(&it.local);
        if it.exported.span() != it.local.span() {
            self.visit_module_export_name(&it.exported);
        }
        self.leave_node(kind);
    }
}

/// `handleTypeAssertion`: `<Type>a` → `a as Type`
pub fn handle_type_assertion(out: &mut Out, off: usize, t: &TSTypeAssertion) -> Result<()> {
    let assertion_start = t.span.start as usize + off;
    let type_start = t.type_annotation.span().start as usize + off;
    let type_end = t.type_annotation.span().end as usize + off;
    let expression_start = t.expression.span().start as usize + off;
    let expression_end = t.expression.span().end as usize + off;
    out.ms.append_left(expression_end, " as ")?;
    out.ms.move_(assertion_start, type_end, expression_end)?;
    out.ms.remove(assertion_start, type_start)?;
    out.ms.remove(type_end, expression_start)?;
    Ok(())
}

/// `isNewGroup`: two or more line breaks (outside comments) before the node
fn is_new_group(text: &str, full_start: usize, start: usize) -> bool {
    let bytes = text.as_bytes();
    let mut i = full_start;
    let mut newlines = 0;
    while i < start {
        match bytes[i] {
            b'\r' => {
                if bytes.get(i + 1) == Some(&b'\n') {
                    i += 1;
                }
                newlines += 1;
                i += 1;
            }
            b'\n' => {
                newlines += 1;
                i += 1;
            }
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                while i < start && !matches!(bytes[i], b'\n' | b'\r') {
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i < start && !(bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/')) {
                    i += 1;
                }
                i += 2;
            }
            _ => {
                let c = text[i..].chars().next().unwrap();
                if matches!(c, '\u{2028}' | '\u{2029}') {
                    newlines += 1;
                }
                i += c.len_utf8();
            }
        }
        if newlines >= 2 {
            return true;
        }
    }
    false
}

/// `moveNode`: move a node (with its leading comments) to the top of the script.
/// `start`/`end` are script-relative; `script_start` is the `<script` tag's start.
pub fn move_node(out: &mut Out, ast: &ScriptAst, script_start: usize, start: u32, end: u32) -> Result<()> {
    let off = ast.offset;
    let original = out.original();
    let full_start = ast.full_start(start);
    let comments = ast.leading_comments_of_full_text(full_start, start as usize);
    if !comments.iter().any(|c| c.has_trailing_new_line) && is_new_group(ast.text, full_start, start as usize) {
        out.ms.append_right(start as usize + off, "\n")?;
    }
    for c in &comments {
        let comment_end = c.end + off;
        out.ms.move_(c.pos + off, comment_end, script_start + 1)?;
        if c.has_trailing_new_line {
            let last = crate::svelte2tsx::transform::prev_char(original, comment_end);
            out.ms.overwrite(last, comment_end, &format!("{}\n", &original[last..comment_end]), false)?;
        }
    }
    let end = end as usize + off;
    out.ms.move_(start as usize + off, end, script_start + 1)?;
    let last = crate::svelte2tsx::transform::prev_char(original, end);
    out.ms.overwrite(last, end, &format!("{}\n", &original[last..end]), false)?;
    Ok(())
}

/// `handleFirstInstanceImport`: put the first import on its own line (and after an empty
/// line if there's a module script), and end the last one with a `;`
fn handle_first_instance_import(out: &mut Out, ast: &ScriptAst, has_module_script: bool) -> Result<()> {
    let mut imports: Vec<&ImportDeclaration> = ast
        .program
        .body
        .iter()
        .filter_map(|s| match s {
            Statement::ImportDeclaration(i) => Some(&**i),
            _ => None,
        })
        .collect();
    imports.sort_by_key(|i| i.span.end);
    let Some(first) = imports.first() else { return Ok(()) };
    let off = ast.offset;
    let full_start = ast.full_start(first.span.start);
    let comments = ast.leading_comments_of_full_text(full_start, first.span.start as usize);
    let start = match comments.first() {
        Some(c) if c.multi_line => c.pos,
        _ => first.span.start as usize,
    };
    out.ms.append_right(start + off, if has_module_script { "\n\n" } else { "\n" })?;

    let last = imports.last().unwrap();
    let end = last.span.end as usize + off;
    let original = out.original();
    let last_char = crate::svelte2tsx::transform::prev_char(original, end);
    if &original[last_char..end] != ";" {
        out.ms.overwrite(last_char, end, &format!("{};\n", &original[last_char..end]), false)?;
    }
    Ok(())
}
