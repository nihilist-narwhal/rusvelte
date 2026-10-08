//! Ports of `svelte2tsx/nodes/ImplicitStoreValues.ts` and `ImplicitTopLevelNames.ts`.

use indexmap::IndexSet;
use oxc_ast::ast::*;
use oxc_span::GetSpan;

use super::*;
use crate::svelte2tsx::transform::{prev_char, surround_with_ignore_comments};

/// A top-level variable declaration (TS `VariableDeclaration`), as far as stores care
pub struct VarDeclInfo {
    /// `extractIdentifiers(node.name)`
    pub names: Vec<String>,
    /// the end of the declaration (of the last one if there are several in the list)
    pub end: usize,
}

/// A top-level `$: a = ..` statement
pub struct ReactiveInfo {
    pub names: Vec<String>,
    pub end: usize,
}

/// An import binding (TS `ImportClause` or `ImportSpecifier`)
pub struct ImportInfo {
    pub name: Option<String>,
    /// `import { derived } from 'svelte/store'`
    pub svelte_store_derived: bool,
}

pub struct ImplicitStoreValues {
    accessed: IndexSet<String>,
    var_decls: Vec<VarDeclInfo>,
    reactive: Vec<ReactiveInfo>,
    imports: Vec<ImportInfo>,
    render_function_start: usize,
    svelte5_plus: bool,
    /// `(input) => `</>;${input}<>``
    wrap_imports: bool,
}

impl ImplicitStoreValues {
    pub fn new(resolved_in_template: impl IntoIterator<Item = String>, render_function_start: usize, svelte5_plus: bool, wrap_imports: bool) -> Self {
        ImplicitStoreValues {
            accessed: resolved_in_template.into_iter().collect(),
            var_decls: Vec::new(),
            reactive: Vec::new(),
            imports: Vec::new(),
            render_function_start,
            svelte5_plus,
            wrap_imports,
        }
    }

    pub fn add_store_access(&mut self, name: String) {
        self.accessed.insert(name);
    }

    pub fn add_variable_declaration(&mut self, v: VarDeclInfo) {
        self.var_decls.push(v);
    }

    pub fn add_reactive_declaration(&mut self, r: ReactiveInfo) {
        self.reactive.push(r);
    }

    pub fn add_import_statement(&mut self, i: ImportInfo) {
        self.imports.push(i);
    }

    pub fn accessed_stores(&self) -> Vec<String> {
        self.accessed.iter().cloned().collect()
    }

    pub fn globals(&self) -> Vec<String> {
        let mut globals = self.accessed.clone();
        for v in &self.var_decls {
            for n in &v.names {
                globals.shift_remove(n);
            }
        }
        for r in &self.reactive {
            for n in &r.names {
                globals.shift_remove(n);
            }
        }
        for i in &self.imports {
            if let Some(n) = &i.name {
                globals.shift_remove(n);
            }
        }
        globals.into_iter().map(|n| format!("${n}")).collect()
    }

    pub fn modify_code(&self, ast_offset: usize, out: &mut Out) -> Result<()> {
        for v in &self.var_decls {
            let names: Vec<&String> = v.names.iter().filter(|n| self.accessed.contains(*n)).collect();
            if names.is_empty() {
                continue;
            }
            let decls = surround_with_ignore_comments(&store_declarations(&names));
            let end = v.end + ast_offset;
            if out.has_prepends(end) {
                out.prepend_str(end, &decls)?;
            } else {
                out.ms.append_right(end, &decls)?;
            }
        }
        for r in &self.reactive {
            let names: Vec<&String> = r.names.iter().filter(|n| self.accessed.contains(*n)).collect();
            if names.is_empty() {
                continue;
            }
            let decls = surround_with_ignore_comments(&store_declarations(&names));
            let end = r.end + ast_offset;
            let original = out.original();
            if &original[prev_char(original, end)..end] != ";" {
                out.prepend_str(end, &decls)?;
            } else {
                out.ms.append_right(end, &decls)?;
            }
        }
        let names: Vec<&String> = self
            .imports
            .iter()
            .filter(|i| !(self.svelte5_plus && i.svelte_store_derived))
            .filter_map(|i| i.name.as_ref())
            .filter(|n| self.accessed.contains(*n))
            .collect();
        if !names.is_empty() {
            let decls = surround_with_ignore_comments(&store_declarations(&names));
            let decls = if self.wrap_imports { format!("</>;{decls}<>") } else { decls };
            out.ms.append_right(self.render_function_start, &decls)?;
        }
        Ok(())
    }
}

fn store_declarations(names: &[&String]) -> String {
    names.iter().map(|n| format!(";let ${n} = __sveltets_2_store_get({n});")).collect()
}

/// `extractIdentifiers` on a binding name
pub fn binding_identifier_names(p: &BindingPattern) -> Vec<String> {
    let mut ids = Vec::new();
    binding_names(p, &mut ids);
    ids.into_iter().map(|i| i.name.to_string()).collect()
}

/// `extractIdentifiers` on an assignment's left-hand side (TS expression semantics: array
/// spread elements are skipped)
pub fn assignment_target_names(t: &AssignmentTarget, out: &mut Vec<String>) {
    match t {
        AssignmentTarget::AssignmentTargetIdentifier(id) => out.push(id.name.to_string()),
        AssignmentTarget::ObjectAssignmentTarget(o) => {
            for p in &o.properties {
                match p {
                    AssignmentTargetProperty::AssignmentTargetPropertyIdentifier(p) => out.push(p.binding.name.to_string()),
                    AssignmentTargetProperty::AssignmentTargetPropertyProperty(p) => maybe_default_names(&p.binding, out),
                }
            }
            if let Some(rest) = &o.rest {
                assignment_target_names(&rest.target, out);
            }
        }
        AssignmentTarget::ArrayAssignmentTarget(a) => {
            for el in a.elements.iter().flatten() {
                maybe_default_names(el, out);
            }
        }
        _ => {
            if let Some(m) = t.as_member_expression() {
                let mut object = m.object();
                while let Some(m) = object.as_member_expression() {
                    object = m.object();
                }
                if let Expression::Identifier(id) = object {
                    out.push(id.name.to_string());
                }
            }
        }
    }
}

fn maybe_default_names(t: &AssignmentTargetMaybeDefault, out: &mut Vec<String>) {
    match t {
        AssignmentTargetMaybeDefault::AssignmentTargetWithDefault(d) => assignment_target_names(&d.binding, out),
        other => {
            if let Some(t) = other.as_assignment_target() {
                assignment_target_names(t, out);
            }
        }
    }
}

/// `getBinaryAssignmentExpr`: `$: a = ..`, `$: ({a} = ..)`, `$: [a] = ..`
pub fn binary_assignment<'r, 'a>(label: &'r LabeledStatement<'a>) -> Option<&'r AssignmentExpression<'a>> {
    let Statement::ExpressionStatement(s) = &label.body else { return None };
    let is_assign = |e: &Expression| -> bool {
        matches!(e, Expression::AssignmentExpression(a) if a.operator == AssignmentOperator::Assign
            && matches!(a.left, AssignmentTarget::AssignmentTargetIdentifier(_) | AssignmentTarget::ObjectAssignmentTarget(_) | AssignmentTarget::ArrayAssignmentTarget(_)))
    };
    match &s.expression {
        e @ Expression::AssignmentExpression(a) if is_assign(e) => Some(a),
        Expression::ParenthesizedExpression(p) => match &p.expression {
            e @ Expression::AssignmentExpression(a) if is_assign(e) => Some(a),
            _ => None,
        },
        _ => None,
    }
}

/// `getNamesFromLabeledStatement`
pub fn names_from_labeled_statement(label: &LabeledStatement) -> Vec<String> {
    let Some(a) = binary_assignment(label) else { return Vec::new() };
    let mut names = Vec::new();
    assignment_target_names(&a.left, &mut names);
    names.retain(|n| !n.starts_with('$'));
    names
}

/// A top-level `$:` statement with an assignment, for `ImplicitTopLevelNames.modifyCode`
pub struct ImplicitName {
    names: Vec<String>,
    label_start: usize,
    /// `$: ({a} = b)`: `(paren start, expression start, expression end, paren end)`
    paren: Option<(usize, usize, usize, usize)>,
}

#[derive(Default)]
pub struct ImplicitTopLevelNames {
    list: Vec<ImplicitName>,
}

impl ImplicitTopLevelNames {
    pub fn add(&mut self, label: &LabeledStatement) {
        let paren = match &label.body {
            Statement::ExpressionStatement(s) => match &s.expression {
                Expression::ParenthesizedExpression(p) => match &p.expression {
                    Expression::AssignmentExpression(a)
                        if matches!(a.left, AssignmentTarget::ObjectAssignmentTarget(_) | AssignmentTarget::ArrayAssignmentTarget(_)) =>
                    {
                        Some((p.span.start as usize, p.expression.span().start as usize, p.expression.span().end as usize, p.span.end as usize))
                    }
                    _ => None,
                },
                _ => None,
            },
            _ => None,
        };
        self.list.push(ImplicitName { names: names_from_labeled_statement(label), label_start: label.label.span.start as usize, paren });
    }

    pub fn handle_reactive_statement(&self, out: &mut Out, ast_offset: usize, src: &str, label: &LabeledStatement) -> Result<()> {
        match binary_assignment(label) {
            Some(a) => {
                let e = &a.right;
                let start = e.span().start as usize + ast_offset;
                let end = e.span().end as usize + ast_offset;
                if matches!(e, Expression::ObjectExpression(_) | Expression::TSAsExpression(_)) || text(src, e.span()).starts_with('{') {
                    out.ms.append_left(start, "(")?;
                    out.ms.append_right(end, ")")?;
                }
                out.ms.prepend_left(start, "__sveltets_2_invalidate(() => ")?;
                out.prepend_str(end, ")")?;
            }
            None => {
                out.ms.prepend_left(label.span.start as usize + ast_offset, ";() => {")?;
                out.prepend_str(label.span.end as usize + ast_offset, "}")?;
            }
        }
        Ok(())
    }

    pub fn modify_code(&self, out: &mut Out, ast_offset: usize, root_variables: &std::collections::HashSet<String>) -> Result<()> {
        for n in &self.list {
            if n.names.is_empty() {
                continue;
            }
            let implicit: Vec<&String> = n.names.iter().filter(|name| !root_variables.contains(*name)).collect();
            let pos = n.label_start + ast_offset;
            if implicit.len() == n.names.len() {
                out.ms.remove(pos, pos + 2)?;
                out.ms.prepend_right(pos, "let ")?;
                if let Some((paren_start, expr_start, expr_end, paren_end)) = n.paren {
                    out.ms.overwrite(paren_start + ast_offset, expr_start + ast_offset, "", true)?;
                    out.overwrite_str(expr_end + ast_offset, paren_end + ast_offset, ")", true)?;
                }
            } else {
                for name in implicit {
                    out.ms.prepend_right(pos, &format!("let {name};\n"))?;
                }
            }
        }
        Ok(())
    }
}
