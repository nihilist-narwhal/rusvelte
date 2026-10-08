//! Port of `svelte2tsx/nodes/Generics.ts`: the `generics` attribute and `$$Generic` types.

use oxc_allocator::Allocator;
use oxc_ast::ast::*;
use oxc_parser::Parser;
use oxc_span::{GetSpan, SourceType};

use super::*;
use crate::svelte2tsx::htmlx::Verbatim;
use crate::svelte2tsx::transform::surround_with_ignore_comments;

#[derive(Default)]
pub struct Generics {
    /// The whole `T extends boolean`
    definitions: Vec<String>,
    type_references: Vec<String>,
    /// The `T` in `T extends boolean`
    references: Vec<String>,
    /// The attribute value's `(start, end)` (with quotes), if there's a `generics` attribute
    pub generics_attr: Option<(usize, usize)>,
}

impl Generics {
    pub fn new(script: Option<&Verbatim>) -> Self {
        let mut g = Generics::default();
        let Some(attr) = script.and_then(|s| s.attributes.iter().find(|a| a.name == "generics")) else { return g };
        let Some((start, end, raw)) = attr.value else { return g };
        if raw.is_empty() {
            return g;
        }
        g.generics_attr = Some((start, end));
        let src = format!("<{raw}>() => {{}}");
        let alloc = Allocator::default();
        let ret = Parser::new(&alloc, &src, SourceType::ts()).parse();
        if let Some(Statement::ExpressionStatement(s)) = ret.program.body.first() {
            if let Expression::ArrowFunctionExpression(arrow) = &s.expression {
                if let Some(tp) = &arrow.type_parameters {
                    g.definitions = tp.params.iter().map(|p| text(&src, p.span).to_string()).collect();
                    g.references = tp.params.iter().map(|p| p.name.name.to_string()).collect();
                }
            }
        }
        g
    }

    /// `type T = $$Generic<..>`: collect it and remove the declaration. `span` is the TS node's
    /// (including an `export` keyword).
    pub fn add_if_is_generic(&mut self, out: &mut Out, ast_offset: usize, alias: &TSTypeAliasDeclaration, span: oxc_span::Span, src: &str) -> Result<()> {
        let Some(r) = generic_type(alias) else { return Ok(()) };
        if self.generics_attr.is_some() {
            return Err(MagicStringError(
                "Invalid $$Generic declaration: $$Generic definitions are not allowed when the generics attribute is present on the script tag".into(),
            ));
        }
        let args = r.type_arguments.as_ref().map_or(0, |a| a.params.len());
        if args > 1 {
            return Err(MagicStringError("Invalid $$Generic declaration: Only one type argument allowed".into()));
        }
        let name = alias.id.name.to_string();
        if args == 1 {
            let reference = text(src, r.type_arguments.as_ref().unwrap().params[0].span()).to_string();
            self.definitions.push(format!("{name} extends {reference}"));
            self.type_references.push(reference);
        } else {
            self.definitions.push(name.clone());
        }
        self.references.push(name);
        out.ms.remove(ast_offset + span.start as usize, ast_offset + span.end as usize)?;
        Ok(())
    }

    pub fn throw_if_is_generic(alias: &TSTypeAliasDeclaration) -> Result<()> {
        if generic_type(alias).is_some() {
            return Err(MagicStringError("$$Generic declarations are only allowed in the instance script".into()));
        }
        Ok(())
    }

    pub fn type_references(&self) -> &[String] {
        &self.type_references
    }

    pub fn references(&self) -> &[String] {
        &self.references
    }

    pub fn to_definition_string(&self, add_ignore: bool) -> String {
        if self.definitions.is_empty() {
            return String::new();
        }
        let s = format!("<{}>", self.definitions.join(","));
        if add_ignore {
            surround_with_ignore_comments(&s)
        } else {
            s
        }
    }

    pub fn to_references_string(&self) -> String {
        if self.references.is_empty() {
            String::new()
        } else {
            format!("<{}>", self.references.join(","))
        }
    }

    pub fn to_references_any_string(&self) -> String {
        if self.references.is_empty() {
            String::new()
        } else {
            format!("<{}>", vec!["any"; self.references.len()].join(","))
        }
    }

    pub fn has(&self) -> bool {
        !self.definitions.is_empty()
    }
}

fn generic_type<'r, 'a>(alias: &'r TSTypeAliasDeclaration<'a>) -> Option<&'r TSTypeReference<'a>> {
    match &alias.type_annotation {
        TSType::TSTypeReference(r) => match &r.type_name {
            TSTypeName::IdentifierReference(id) if id.name == "$$Generic" => Some(r),
            _ => None,
        },
        _ => None,
    }
}
