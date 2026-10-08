//! Port of `svelte2tsx/nodes/ExportedNames.ts`.

use indexmap::IndexMap;
use oxc_ast::ast::*;
use oxc_span::GetSpan;

use super::hoistable::HoistableInterfaces;
use super::ts::ScriptAst;
use super::*;
use crate::svelte2tsx::transform::{index_of, surround_with_ignore_comments};

#[derive(Debug, Clone, Default)]
pub struct ExportedName {
    pub is_let: bool,
    pub ty: Option<String>,
    pub identifier_text: Option<String>,
    pub required: bool,
    pub doc: Option<String>,
    pub is_named_export: bool,
}

#[derive(Clone, Copy)]
pub struct ListRef<'r, 'a> {
    pub list: &'r VariableDeclaration<'a>,
    /// start of the TS VariableStatement (its `export` keyword if any)
    pub stmt_start: u32,
}

#[derive(Clone)]
struct PossibleExport<'r, 'a> {
    declaration: ListRef<'r, 'a>,
    info: ExportedName,
}

#[derive(Default)]
struct PropsRune {
    comment: String,
    ty: String,
    bindings: Vec<String>,
}

pub struct ExportedNames<'r, 'a> {
    pub hoistable: HoistableInterfaces,
    pub uses_accessors: bool,
    pub uses_props_type: bool,
    has_runes_globals: bool,
    props: PropsRune,
    pub exports: IndexMap<String, ExportedName>,
    possible_exports: IndexMap<String, PossibleExport<'r, 'a>>,
    done_declaration_transformation: Vec<u32>,
    getters: IndexMap<String, ()>,
    ast_offset: usize,
    basename: String,
    is_ts_file: bool,
    is_svelte5_plus: bool,
    is_runes: bool,
    emit_jsdoc: bool,
}

pub fn is_kit_route_file(basename: &str) -> bool {
    let base = match basename.split_once('@') {
        Some((b, _)) => b,
        None => strip_ext(basename),
    };
    matches!(base, "+page" | "+layout" | "+error")
}

pub fn is_kit_error_file(basename: &str) -> bool {
    strip_ext(basename) == "+error"
}

/// `basename.slice(0, -path.extname(basename).length)`
fn strip_ext(basename: &str) -> &str {
    match basename.rfind('.') {
        Some(i) if i > 0 => &basename[..i],
        // no extension: `slice(0, -0)` is empty
        _ => "",
    }
}

impl<'r, 'a> ExportedNames<'r, 'a> {
    pub fn new(ast_offset: usize, basename: &str, is_ts_file: bool, is_svelte5_plus: bool, is_runes: bool, emit_jsdoc: bool) -> Self {
        ExportedNames {
            hoistable: HoistableInterfaces::default(),
            uses_accessors: false,
            uses_props_type: false,
            has_runes_globals: false,
            props: PropsRune::default(),
            exports: IndexMap::new(),
            possible_exports: IndexMap::new(),
            done_declaration_transformation: Vec::new(),
            getters: IndexMap::new(),
            ast_offset,
            basename: basename.to_string(),
            is_ts_file,
            is_svelte5_plus,
            is_runes,
            emit_jsdoc,
        }
    }

    fn emit_kit_type(&self, out: &mut Out, kit_type: &str, name_start: usize, name_end: usize) -> Result<()> {
        if kit_type.is_empty() {
            return Ok(());
        }
        if self.emit_jsdoc && !self.is_ts_file {
            out.prepend_str(name_start, &format!("/** @type {{{}}} */ ", &kit_type[2..]))
        } else {
            out.prepend_str(name_end, &surround_with_ignore_comments(kit_type))
        }
    }

    pub fn handle_variable_statement(&mut self, out: &mut Out, ast: &'r ScriptAst<'a>, list: ListRef<'r, 'a>, export_start: Option<u32>, top_level: bool) -> Result<()> {
        let decl = list.list;
        if let Some(export_start) = export_start {
            let is_let = decl.kind == VariableDeclarationKind::Let;
            let is_const = decl.kind == VariableDeclarationKind::Const;
            for d in &decl.declarations {
                match &d.id {
                    BindingPattern::BindingIdentifier(id) => {
                        let ty = declarator_type(d).map(|t| text(ast.text, t.span()).to_string());
                        let doc = self.doc_for_declarator(ast, list, d);
                        self.add_export(out, ast, &id.name, is_let, Some((&id.name, doc)), ty, d.init.is_none(), false)?;
                    }
                    BindingPattern::ObjectPattern(_) | BindingPattern::ArrayPattern(_) => {
                        for el in binding_elements(&d.id) {
                            self.add_export_for_binding_pattern(out, ast, el.name, is_let)?;
                        }
                    }
                    _ => {}
                }
            }
            if is_let {
                self.prop_type_assert_to_user_defined(out, ast, list)?;
            } else if is_const {
                for (i, d) in decl.declarations.iter().enumerate() {
                    if let Some(id) = binding_ident(&d.id) {
                        self.add_getter(&id.name);
                        let has_type = d.type_annotation.is_some() || (i == 0 && ast.has_jsdoc_type(list.stmt_start));
                        let is_kit_export = is_kit_route_file(&self.basename) && id.name == "snapshot";
                        let kit_type = if is_kit_export && !has_type { ": import('./$types.js').Snapshot" } else { "" };
                        self.emit_kit_type(out, kit_type, id.span.start as usize + self.ast_offset, id.span.end as usize + self.ast_offset)?;
                    }
                }
            }
            self.remove_export(out, export_start as usize, export_start as usize + "export".len())?;
        } else if top_level {
            let is_let = decl.kind == VariableDeclarationKind::Let;
            for d in &decl.declarations {
                match &d.id {
                    BindingPattern::BindingIdentifier(id) => {
                        let ty = declarator_type(d).map(|t| text(ast.text, t.span()).to_string());
                        let doc = self.doc_for_declarator(ast, list, d);
                        self.possible_exports.insert(
                            id.name.to_string(),
                            PossibleExport {
                                declaration: list,
                                info: ExportedName { is_let, ty, identifier_text: Some(id.name.to_string()), required: d.init.is_none(), doc, ..Default::default() },
                            },
                        );
                    }
                    BindingPattern::ObjectPattern(_) | BindingPattern::ArrayPattern(_) => {
                        for el in binding_elements(&d.id) {
                            if let Some(id) = binding_ident(el.name) {
                                self.possible_exports.insert(id.name.to_string(), PossibleExport { declaration: list, info: ExportedName { is_let, ..Default::default() } });
                            }
                        }
                    }
                    _ => {}
                }
            }
            for d in &decl.declarations {
                if let Some(Expression::CallExpression(call)) = &d.init {
                    if text(ast.text, call.callee.span()) == "$props" {
                        self.handle_props_rune(out, ast, list, d, call)?;
                        break;
                    }
                }
            }
        }
        Ok(())
    }

    /// `getDoc(target)` for a declarator's identifier: the declarator's leading doc, else the statement's
    fn doc_for_declarator(&self, ast: &ScriptAst, list: ListRef, d: &VariableDeclarator) -> Option<String> {
        let start = d.span.start;
        ast.last_leading_doc(ast.full_start(start), start as usize).or_else(|| {
            let s = list.stmt_start;
            ast.last_leading_doc(ast.full_start(s), s as usize)
        })
    }

    pub fn handle_export_function_or_class(&mut self, out: &mut Out, ast: &'r ScriptAst<'a>, export_start: u32, name: Option<&str>) -> Result<()> {
        self.remove_export(out, export_start as usize, export_start as usize + "export".len())?;
        if let Some(name) = name {
            self.add_getter(name);
            self.add_export(out, ast, name, false, None, None, false, false)?;
        }
        Ok(())
    }

    /// `export { a, b as c }` (with or without `from`)
    pub fn handle_export_declaration(&mut self, out: &mut Out, ast: &'r ScriptAst<'a>, specifiers: &[ExportSpecifier], span: oxc_span::Span) -> Result<()> {
        for spec in specifiers {
            let local = spec.local.name().to_string();
            let renamed = spec.local.span() != spec.exported.span();
            if renamed {
                // addExport(ne.propertyName, false, ne.name): `getDoc(target)` looks at the
                // specifier, then at `target.parent.parent.parent`, which is the source file
                let doc = ast
                    .last_leading_doc(ast.full_start(spec.span.start), spec.span.start as usize)
                    .or_else(|| ast.leading_doc_of_file());
                let exported = spec.exported.name().to_string();
                self.add_export(out, ast, &local, false, Some((&exported, doc)), None, false, true)?;
            } else {
                self.add_export(out, ast, &local, false, None, None, false, true)?;
            }
        }
        self.remove_export(out, span.start as usize, span.end as usize)
    }

    fn add_getter(&mut self, name: &str) {
        self.getters.insert(name.to_string(), ());
    }

    fn remove_export(&self, out: &mut Out, start: usize, end: usize) -> Result<()> {
        let export_start = index_of(out.original(), "export", start + self.ast_offset).unwrap_or(usize::MAX);
        out.ms.remove(export_start, export_start.wrapping_add(end - start))?;
        Ok(())
    }

    /// `addExport(name, isLet, target, type, required, isNamedExport)`; `target` is
    /// `(identifier text, doc)`
    #[allow(clippy::too_many_arguments)]
    fn add_export(&mut self, out: &mut Out, ast: &'r ScriptAst<'a>, name: &str, is_let: bool, target: Option<(&str, Option<String>)>, ty: Option<String>, required: bool, is_named_export: bool) -> Result<()> {
        let existing = self.possible_exports.get(name).cloned();
        let e = existing.as_ref().map(|e| &e.info);
        let info = match target {
            Some((target, doc)) => ExportedName {
                is_let: is_let || e.is_some_and(|e| e.is_let),
                ty: ty.or_else(|| e.and_then(|e| e.ty.clone())),
                identifier_text: Some(target.to_string()),
                required: required || e.is_some_and(|e| e.required),
                doc: doc.or_else(|| e.and_then(|e| e.doc.clone())),
                is_named_export,
            },
            None => ExportedName {
                is_let: is_let || e.is_some_and(|e| e.is_let),
                ty: e.and_then(|e| e.ty.clone()),
                identifier_text: None,
                required: e.is_some_and(|e| e.required),
                doc: e.and_then(|e| e.doc.clone()),
                is_named_export,
            },
        };
        self.exports.insert(name.to_string(), info);
        if let Some(existing) = existing {
            if existing.info.is_let {
                self.prop_type_assert_to_user_defined(out, ast, existing.declaration)?;
            }
        }
        Ok(())
    }

    fn add_export_for_binding_pattern(&mut self, out: &mut Out, ast: &'r ScriptAst<'a>, name: &BindingPattern, is_let: bool) -> Result<()> {
        if let Some(id) = binding_ident(name) {
            return self.add_export(out, ast, &id.name, is_let, None, None, false, false);
        }
        for el in binding_elements(name) {
            self.add_export_for_binding_pattern(out, ast, el.name, is_let)?;
        }
        Ok(())
    }

    /// Appends `;prop = __sveltets_2_any(prop);` to widen the declared types
    fn prop_type_assert_to_user_defined(&mut self, out: &mut Out, ast: &ScriptAst, list: ListRef) -> Result<()> {
        let decl = list.list;
        if self.done_declaration_transformation.contains(&decl.span.start) {
            return Ok(());
        }
        for (i, d) in decl.declarations.iter().enumerate() {
            let has_type = d.type_annotation.is_some() || (i == 0 && ast.has_jsdoc_type(list.stmt_start));
            let name = text(ast.text, d.id.span());
            let is_kit_export = is_kit_route_file(&self.basename) && matches!(name, "data" | "form" | "snapshot");
            let kit_type = if is_kit_export && !has_type {
                let t = match name {
                    "data" => {
                        if self.basename.contains("layout") {
                            "LayoutData"
                        } else {
                            "PageData"
                        }
                    }
                    "form" => "ActionData",
                    _ => "Snapshot",
                };
                format!(": import('./$types.js').{t}")
            } else {
                String::new()
            };
            let name_end = d.id.span().end as usize + self.ast_offset;
            let end = d.span.end as usize + self.ast_offset;
            let bool_init = matches!(d.init, Some(Expression::BooleanLiteral(_)));
            if binding_ident(&d.id).is_some() && (d.init.is_none() || has_type || (!has_type && bool_init)) {
                if name_end == end {
                    if !kit_type.is_empty() && self.emit_jsdoc && !self.is_ts_file {
                        let name_start = d.id.span().start as usize + self.ast_offset;
                        out.prepend_str(name_start, &format!("/** @type {{{}}} */ ", &kit_type[2..]))?;
                    }
                    let kit = if (self.is_ts_file || !self.emit_jsdoc) && !kit_type.is_empty() { kit_type.as_str() } else { "" };
                    out.prepend_str(end, &surround_with_ignore_comments(&format!("{kit};{name} = __sveltets_2_any({name});")))?;
                } else {
                    self.emit_kit_type(out, &kit_type, d.id.span().start as usize + self.ast_offset, name_end)?;
                    out.prepend_str(end, &surround_with_ignore_comments(&format!(";{name} = __sveltets_2_any({name});")))?;
                }
            } else {
                self.emit_kit_type(out, &kit_type, d.id.span().start as usize + self.ast_offset, name_end)?;
            }
        }
        // splitDeclaration: `let a, b` → `let a;let b`
        for pair in decl.declarations.windows(2) {
            let (from, to) = (pair[0].span.end, pair[1].span.start);
            let mut pos = from;
            while let Some((s, e)) = ast.token_at_or_after(pos) {
                if s >= to {
                    break;
                }
                if &ast.text[s as usize..e as usize] == "," {
                    out.overwrite_str(s as usize + self.ast_offset, e as usize + self.ast_offset, ";let ", false)?;
                }
                pos = e;
            }
        }
        self.done_declaration_transformation.push(decl.span.start);
        Ok(())
    }

    fn handle_props_rune(&mut self, out: &mut Out, ast: &'r ScriptAst<'a>, list: ListRef<'r, 'a>, d: &'r VariableDeclarator<'a>, call: &'r CallExpression<'a>) -> Result<()> {
        let src = ast.text;
        let off = self.ast_offset;
        let mut binding_local_names: Vec<String> = Vec::new();
        if let BindingPattern::ObjectPattern(_) = &d.id {
            for el in binding_elements(&d.id) {
                let Some(local) = binding_ident(el.name) else { continue };
                let key_ok = el.property_name.is_none_or(|k| matches!(k, PropertyKey::StaticIdentifier(_)));
                if !key_ok || el.rest {
                    continue;
                }
                let name = match el.property_name {
                    Some(PropertyKey::StaticIdentifier(k)) => k.name.to_string(),
                    _ => local.name.to_string(),
                };
                if let Some(init) = el.initializer {
                    let mut call = init;
                    if let Expression::TSAsExpression(a) = call {
                        call = &a.expression;
                    }
                    if let Expression::CallExpression(c) = call {
                        if let Expression::Identifier(callee) = &c.callee {
                            if callee.name == "$bindable" {
                                self.props.bindings.push(name);
                                binding_local_names.push(local.name.to_string());
                            }
                        }
                    }
                }
            }
        }
        if !binding_local_names.is_empty() {
            let s = format!(";{}", binding_local_names.iter().map(|p| format!("{p};")).collect::<String>());
            out.ms.append_left(d.span.end as usize + off, &surround_with_ignore_comments(&s))?;
        }

        // Easy mode: typed $props()
        let type_arg = call.type_arguments.as_ref().and_then(|t| t.params.first());
        if let Some(generic_arg) = type_arg.or(declarator_type(d)) {
            self.hoistable.analyze_props_rune(ast, generic_arg);
            let generic = text(src, generic_arg.span());
            if matches!(generic_arg, TSType::TSTypeReference(_)) {
                self.props.ty = generic.to_string();
            } else {
                self.props.ty = "$$ComponentProps".into();
                let pos = ast.full_start(generic_arg.span().start) + off;
                out.prepend_str(pos, &format!(";type {} = ", self.props.ty))?;
                let end = generic_arg.span().end as usize + off;
                out.ms.append_left(end, ";")?;
                let list_pos = ast.full_start(list.list.span.start) + off;
                out.ms.move_(pos, end, list_pos)?;
                out.ms.append_right(end, &surround_with_ignore_comments(&self.props.ty))?;
            }
            return Ok(());
        }

        // Hard mode: JSDoc or untyped
        if !self.is_ts_file {
            let mut comment: Option<(String, usize)> = None;
            for pos in [ast.full_start(d.span.start), ast.full_start(list.list.span.start)] {
                for c in ast.leading_comment_ranges(pos).iter().rev() {
                    let t = &src[c.pos..c.end];
                    if has_word(t, "@type") {
                        comment = Some((t.to_string(), c.pos + off));
                        break;
                    }
                }
                if comment.is_some() {
                    break;
                }
            }
            match &comment {
                Some((c, start)) if is_inline_object_type(c) => {
                    self.props.comment = "/** @type {$$ComponentProps} */".into();
                    let original = out.original();
                    let type_start = index_of(original, "@type", *start).unwrap_or(0);
                    out.ms.overwrite(type_start, type_start + 5, "@typedef", false)?;
                    let end = index_of(original, "*/", *start).unwrap_or(0);
                    out.ms.overwrite(end, end + 2, &format!(" $$ComponentProps */{}", self.props.comment), false)?;
                }
                _ => self.props.comment = comment.map(|(c, _)| c).unwrap_or_default(),
            }
        }
        if !self.props.comment.is_empty() {
            return Ok(());
        }

        // best-effort props from the object pattern
        let mut with_unknown = false;
        let mut props: Vec<String> = Vec::new();
        let is_kit_route = is_kit_route_file(&self.basename);
        let is_kit_layout = is_kit_route && self.basename.contains("layout");
        let props_str = if let BindingPattern::ObjectPattern(_) = &d.id {
            for el in binding_elements(&d.id) {
                let local = binding_ident(el.name);
                let key_ok = el.property_name.is_none_or(|k| matches!(k, PropertyKey::StaticIdentifier(_)));
                if local.is_none() || !key_ok || el.rest {
                    with_unknown = true;
                    continue;
                }
                let name = match el.property_name {
                    Some(PropertyKey::StaticIdentifier(k)) => k.name.to_string(),
                    _ => local.unwrap().name.to_string(),
                };
                if is_kit_route {
                    if name == "data" {
                        props.push(format!("data: import('./$types.js').{}", if is_kit_layout { "LayoutData" } else { "PageData" }));
                    }
                    if name == "form" && !is_kit_layout {
                        props.push("form: import('./$types.js').ActionData".into());
                    }
                    if name == "params" {
                        props.push(format!("params: import('./$types.js').{}['params']", if is_kit_layout { "LayoutProps" } else { "PageProps" }));
                    }
                } else if is_kit_error_file(&self.basename) {
                    if name == "error" {
                        props.push("error: App.Error".into());
                    }
                } else if let Some(init) = el.initializer {
                    let initializer = match init {
                        Expression::CallExpression(c) if matches!(&c.callee, Expression::Identifier(i) if i.name == "$bindable") => {
                            c.arguments.first().and_then(|a| a.as_expression())
                        }
                        other => Some(other),
                    };
                    let ty = match initializer {
                        None => "any".to_string(),
                        Some(Expression::TSAsExpression(a)) => text(src, a.type_annotation.span()).to_string(),
                        Some(Expression::StringLiteral(_)) => "string".into(),
                        Some(Expression::NumericLiteral(_)) => "number".into(),
                        Some(Expression::BooleanLiteral(_)) => "boolean".into(),
                        Some(Expression::Identifier(i)) if i.name != "undefined" => format!("typeof {}", i.name),
                        Some(Expression::ArrowFunctionExpression(_)) => "Function".into(),
                        Some(Expression::ObjectExpression(_)) => "Record<string, any>".into(),
                        Some(Expression::ArrayExpression(_)) => "any[]".into(),
                        Some(_) => "any".into(),
                    };
                    props.push(format!("{name}?: {ty}"));
                } else {
                    props.push(format!("{name}: any"));
                }
            }
            if is_kit_layout {
                props.push("children: import('svelte').Snippet".into());
            }
            if !props.is_empty() {
                format!("{{ {} }}{}", props.join(", "), if with_unknown { " & Record<string, any>" } else { "" })
            } else if with_unknown {
                "Record<string, any>".into()
            } else {
                "Record<string, never>".into()
            }
        } else {
            "Record<string, any>".into()
        };

        if self.is_ts_file {
            self.props.ty = "$$ComponentProps".into();
            if !props.is_empty() || with_unknown {
                let list_pos = ast.full_start(list.list.span.start) + off;
                out.prepend_str(list_pos, &surround_with_ignore_comments(&format!(";type $$ComponentProps = {props_str};")))?;
                out.prepend_str(d.id.span().end as usize + off, &format!(": {}", self.props.ty))?;
            }
        } else {
            self.props.comment = "/** @type {$$ComponentProps} */".into();
            if !props.is_empty() || with_unknown {
                let pos = ast.full_start(d.span.start) + off;
                out.prepend_str(pos, &format!("/** @typedef {{{props_str}}} $$ComponentProps */{}", self.props.comment))?;
            }
        }
        Ok(())
    }

    // --- output strings --------------------------------------------------------------------

    pub fn create_class_getters(&self, generics: &str) -> String {
        self.getters
            .keys()
            .map(|name| {
                if self.is_runes_mode() {
                    format!("\n    get {name}() {{ return $$render{generics}().exports.{name} }}")
                } else {
                    format!("\n    get {name}() {{ return __sveltets_2_nonNullable(this.$$prop_def.{name}) }}")
                }
            })
            .collect()
    }

    pub fn create_class_accessors(&self) -> String {
        self.exports
            .values()
            .filter(|v| !v.identifier_text.as_ref().is_some_and(|t| self.getters.contains_key(t)))
            .map(|v| {
                let name = v.identifier_text.clone().unwrap_or_else(|| "undefined".into());
                format!("\n    get {name}() {{ return this.$$prop_def.{name} }}\n    /**accessor*/\n    set {name}(_) {{}}")
            })
            .collect()
    }

    pub fn create_props_str(&self, uses_props_or_rest_props: bool) -> String {
        let names: Vec<(&String, &ExportedName)> = self.exports.iter().collect();
        if self.is_runes_mode() {
            if !self.props.ty.is_empty() {
                return format!("{{}} as any as {}", self.props.ty);
            }
            if !self.props.comment.is_empty() {
                return format!("{}({{}})", self.props.comment);
            }
            return if self.is_ts_file { "{} as Record<string, never>".into() } else { "/** @type {Record<string, never>} */ ({})".into() };
        }
        if self.uses_props_type {
            let lets: Vec<_> = names.iter().filter(|(_, v)| v.is_let).cloned().collect();
            let others: Vec<_> = names.iter().filter(|(_, v)| !v.is_let).cloned().collect();
            return format!(
                "{{ ...__sveltets_2_ensureRightProps<{{{}}}>(__sveltets_2_any(\"\") as $$Props)}} as {}$$Props",
                self.return_elements_type(&lets, true, false).join(","),
                if others.is_empty() { String::new() } else { format!("{{{}}} & ", self.return_elements_type(&others, true, false).join(",")) }
            );
        }
        if names.is_empty() && !uses_props_or_rest_props {
            return if self.is_ts_file { "{} as Record<string, never>".into() } else { "/** @type {Record<string, never>} */ ({})".into() };
        }
        let dont_add_type_def = !self.is_ts_file || names.iter().all(|(_, v)| v.ty.is_none() && v.required);
        let elements = return_elements(&names, dont_add_type_def, false);
        if dont_add_type_def {
            return format!("{{{}}}", elements.join(" , "));
        }
        format!("{{{}}} as {{{}}}", elements.join(" , "), self.return_elements_type(&names, true, false).join(", "))
    }

    pub fn has_no_props(&self) -> bool {
        if self.is_runes_mode() {
            return self.props.ty.is_empty() && self.props.comment.is_empty();
        }
        self.exports.is_empty()
    }

    pub fn create_bindings_str(&self) -> String {
        if self.is_runes_mode() {
            format!("__sveltets_$$bindings('{}')", self.props.bindings.join("', '"))
        } else {
            "\"\"".into()
        }
    }

    pub fn create_exports_str(&self) -> String {
        let names: Vec<(&String, &ExportedName)> = self.exports.iter().collect();
        let runes = self.is_runes_mode();
        let others: Vec<_> = names.iter().filter(|(_, v)| !v.is_let || (runes && v.is_named_export)).cloned().collect();
        let needs_accessors = self.uses_accessors && !names.is_empty() && !runes;
        if !self.is_svelte5_plus {
            return String::new();
        }
        let mut s = String::new();
        if !others.is_empty() || runes || needs_accessors {
            let exports = if needs_accessors { &names } else { &others };
            if !others.is_empty() || needs_accessors {
                if self.is_ts_file {
                    let referenced = return_elements(if runes { &others } else { &[] }, false, true);
                    s += &format!(
                        ", exports: {{{}}} as any as {{ {} }}",
                        referenced.join(","),
                        self.return_elements_type(exports, true, true).join(",")
                    );
                } else {
                    s += &format!(", exports: /** @type {{{{{}}}}} */ ({{}})", self.return_elements_type(exports, false, true).join(","));
                }
            } else {
                s += ", exports: {}";
            }
            s += &format!(", bindings: {}", self.create_bindings_str());
        } else {
            s += &format!(", exports: {{}}, bindings: {}", self.create_bindings_str());
        }
        s
    }

    fn return_elements_type(&self, names: &[(&String, &ExportedName)], add_doc: bool, force_required: bool) -> Vec<String> {
        names
            .iter()
            .map(|(key, value)| {
                let doc = match (&value.doc, add_doc) {
                    (Some(d), true) => format!("\n{d}"),
                    _ => String::new(),
                };
                let ident = format!(
                    "{doc}{}{}",
                    value.identifier_text.as_deref().unwrap_or(key),
                    if value.required || force_required { "" } else { "?" }
                );
                match &value.ty {
                    None => format!("{ident}: typeof {key}"),
                    Some(t) => format!("{ident}: {t}"),
                }
            })
            .collect()
    }

    pub fn create_optional_props_array(&self) -> Vec<String> {
        if self.is_runes_mode() {
            return Vec::new();
        }
        self.exports
            .iter()
            .filter(|(_, e)| !e.required)
            .map(|(name, e)| format!("'{}'", e.identifier_text.as_deref().unwrap_or(name)))
            .collect()
    }

    pub fn has_exports(&self) -> bool {
        if self.uses_accessors {
            !self.exports.is_empty()
        } else {
            self.exports.values().any(|v| !v.is_let)
        }
    }

    pub fn has_props_rune(&self) -> bool {
        self.is_svelte5_plus && (!self.props.ty.is_empty() || !self.props.comment.is_empty())
    }

    pub fn check_globals_for_runes(&mut self, globals: &[String]) {
        self.has_runes_globals = self.is_svelte5_plus && globals.iter().any(|g| matches!(g.as_str(), "$state" | "$derived" | "$effect"));
    }

    pub fn enter_runes_mode(&mut self) {
        self.is_runes = true;
    }

    pub fn is_runes_mode(&self) -> bool {
        self.has_runes_globals || self.has_props_rune() || self.is_runes
    }
}

fn return_elements(names: &[(&String, &ExportedName)], dont_add_type_def: bool, only_typed: bool) -> Vec<String> {
    names
        .iter()
        .filter(|(_, v)| !only_typed || v.ty.is_some())
        .map(|(key, value)| {
            let doc = match (&value.doc, dont_add_type_def) {
                (Some(d), true) => format!("\n{d}"),
                _ => String::new(),
            };
            format!("{doc}{}: {key}", value.identifier_text.as_deref().unwrap_or(key))
        })
        .collect()
}

/// `/\b<word>\b/`-ish: the word followed by a non-word char
fn has_word(s: &str, word: &str) -> bool {
    s.match_indices(word).any(|(i, _)| !s[i + word.len()..].starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_'))
}

/// `/\/\*\*[^@]*?@type\s*{\s*{.*}\s*}\s*\*\//`
fn is_inline_object_type(c: &str) -> bool {
    static RE: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"/\*\*[^@]*?@type\s*\{\s*\{.*\}\s*\}\s*\*/").unwrap());
    RE.is_match(c)
}
