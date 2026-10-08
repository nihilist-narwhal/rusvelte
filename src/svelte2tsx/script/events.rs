//! Port of `svelte2tsx/nodes/ComponentEvents.ts` and `event-handler.ts`.

use std::collections::HashMap;

use indexmap::{IndexMap, IndexSet};
use oxc_ast::ast::*;
use oxc_span::GetSpan;

use super::ts::ScriptAst;
use super::*;
use crate::svelte2tsx::eswalk::FirstArg;

/// A bubbled event's definition (`string | string[]`, or `undefined`)
#[derive(Debug, Clone)]
pub enum Bubble {
    One(Option<String>),
    Many(Vec<String>),
}

/// What the template walk collects about events (`EventHandler`)
#[derive(Debug, Default)]
pub struct EventHandler {
    pub bubbled: IndexMap<String, Bubble>,
    /// identifiers called in the template, with their first argument
    pub callees: Vec<(String, FirstArg)>,
}

impl EventHandler {
    /// `on:event` without an expression on `parent`
    pub fn handle_event_handler(&mut self, name: &str, parent_type: &str, parent_name: &str) {
        if parent_type == "InlineComponent" {
            if parent_name != "svelte:self" {
                let exp = format!("__sveltets_2_bubbleEventDef(__sveltets_2_instanceOf({parent_name}).$$events_def, '{name}')");
                let new = match self.bubbled.get(name) {
                    Some(Bubble::One(Some(s))) => Bubble::Many(vec![s.clone(), exp]),
                    Some(Bubble::Many(v)) => Bubble::Many(v.iter().cloned().chain([exp]).collect()),
                    _ => Bubble::One(Some(exp)),
                };
                self.bubbled.insert(name.to_string(), new);
            }
            return;
        }
        let def = match parent_type {
            "Element" => Some(format!("__sveltets_2_mapElementEvent('{name}')")),
            "Body" => Some(format!("__sveltets_2_mapBodyEvent('{name}')")),
            "Window" => Some(format!("__sveltets_2_mapWindowEvent('{name}')")),
            _ => None,
        };
        self.bubbled.insert(name.to_string(), Bubble::One(def));
    }

    fn dispatched_events_for(&self, name: &str) -> Result<IndexSet<String>> {
        let mut names = IndexSet::new();
        for (callee, arg) in &self.callees {
            if callee == name {
                match arg {
                    FirstArg::Missing => return Err(MagicStringError("Cannot read properties of undefined (reading 'value')".into())),
                    FirstArg::Value(v) => {
                        names.insert(v.clone());
                    }
                    FirstArg::NoValue => {}
                }
            }
        }
        Ok(names)
    }

    fn bubbled_as_strings(&self) -> Vec<String> {
        self.bubbled
            .iter()
            .map(|(name, e)| match e {
                Bubble::One(Some(s)) => format!("'{name}':{s}"),
                Bubble::One(None) => format!("'{name}':undefined"),
                Bubble::Many(v) => format!("'{name}':__sveltets_2_unionType({})", v.join(",")),
            })
            .collect()
    }
}

/// An interface or type alias declaration
#[derive(Clone, Copy)]
pub enum TypeDeclRef<'r, 'a> {
    Interface(&'r TSInterfaceDeclaration<'a>),
    Alias(&'r TSTypeAliasDeclaration<'a>),
}

#[derive(Debug, Clone)]
pub struct EventInfo {
    pub ty: String,
    pub doc: Option<String>,
}

pub struct ComponentEvents {
    handler: EventHandler,
    strict_events: bool,
    // ComponentEventsFromInterface
    interface_events: Option<IndexMap<String, EventInfo>>,
    interface_dispatcher_import: Option<String>,
    // ComponentEventsFromEventsMap
    events: IndexMap<String, EventInfo>,
    dispatched: IndexSet<String>,
    string_vars: HashMap<String, String>,
    dispatcher_import: Option<String>,
    dispatchers: Vec<(String, Option<String>)>,
}

impl ComponentEvents {
    pub fn new(handler: EventHandler, strict_events: bool) -> Self {
        let events = handler.bubbled.keys().map(|k| (k.clone(), EventInfo { ty: "Event".into(), doc: None })).collect();
        ComponentEvents {
            handler,
            strict_events,
            interface_events: None,
            interface_dispatcher_import: None,
            events,
            dispatched: IndexSet::new(),
            string_vars: HashMap::new(),
            dispatcher_import: None,
            dispatchers: Vec::new(),
        }
    }

    pub fn to_def_string(&self) -> String {
        if self.interface_events.is_some() {
            return "{} as unknown as $$Events".into();
        }
        let mut parts: Vec<String> =
            self.dispatchers.iter().filter_map(|(_, t)| t.as_ref().map(|t| format!("...__sveltets_2_toEventTypings<{t}>()"))).collect();
        parts.extend(self.handler.bubbled_as_strings());
        parts.extend(self.dispatched.iter().map(|e| format!("'{e}': __sveltets_2_customEvent")));
        format!("{{{}}}", parts.join(", "))
    }

    pub fn has_events(&self) -> bool {
        match &self.interface_events {
            Some(e) => !e.is_empty(),
            None => !self.events.is_empty(),
        }
    }

    pub fn has_strict_events(&self) -> bool {
        self.interface_events.is_some() || self.strict_events
    }

    /// A `$$Events` interface or type alias
    pub fn set_component_events_interface(&mut self, ast: &ScriptAst, decl: TypeDeclRef) -> Result<()> {
        let mut map = IndexMap::new();
        match decl {
            TypeDeclRef::Interface(i) => extract_properties(ast, &i.body.body, &mut map)?,
            TypeDeclRef::Alias(t) => match &t.type_annotation {
                TSType::TSTypeLiteral(l) => extract_properties(ast, &l.members, &mut map)?,
                TSType::TSIntersectionType(i) => {
                    for t in &i.types {
                        if let TSType::TSTypeLiteral(l) = t {
                            extract_properties(ast, &l.members, &mut map)?;
                        }
                    }
                }
                _ => {}
            },
        }
        self.interface_events = Some(map);
        Ok(())
    }

    pub fn check_if_import_is_event_dispatcher(&mut self, import: &ImportDeclaration) {
        if self.dispatcher_import.is_none() {
            self.dispatcher_import = import_is_event_dispatcher(import);
        }
        if self.interface_dispatcher_import.is_none() {
            self.interface_dispatcher_import = import_is_event_dispatcher(import);
        }
    }

    pub fn check_if_is_string_literal_declaration(&mut self, d: &VariableDeclarator) {
        if let (Some(id), Some(Expression::StringLiteral(s))) = (binding_ident(&d.id), &d.init) {
            self.string_vars.insert(id.name.to_string(), s.value.to_string());
        }
    }

    pub fn check_if_declaration_instantiated_event_dispatcher(&mut self, out: &mut Out, ast: &ScriptAst, d: &VariableDeclarator) -> Result<()> {
        // ComponentEventsFromEventsMap
        if let Some((name, typing, _)) = instantiated_event_dispatcher(d, self.dispatcher_import.as_deref()) {
            match typing {
                Some(t) => {
                    self.dispatchers.push((name.to_string(), Some(text(ast.text, t.span()).to_string())));
                    if let TSType::TSTypeLiteral(l) = t {
                        for m in &l.members {
                            if let TSSignature::TSPropertySignature(p) = m {
                                let ty = p.type_annotation.as_ref().map_or("any", |t| text(ast.text, t.type_annotation.span()));
                                let info = EventInfo { ty: format!("CustomEvent<{ty}>"), doc: get_doc(ast, p.span) };
                                self.add_to_events(prop_name(ast, &p.key, p.computed)?, Some(info));
                            }
                        }
                    }
                }
                None => {
                    self.dispatchers.push((name.to_string(), None));
                    for evt in self.handler.dispatched_events_for(name)? {
                        self.add_to_events(evt.clone(), None);
                        self.dispatched.insert(evt);
                    }
                }
            }
        }
        // ComponentEventsFromInterface
        if self.interface_events.is_some() {
            if let Some((_, None, call)) = instantiated_event_dispatcher(d, self.interface_dispatcher_import.as_deref()) {
                out.ms.prepend_left(call.callee.span().end as usize + ast.offset, "<__sveltets_2_CustomEvents<$$Events>>")?;
            }
        }
        Ok(())
    }

    pub fn check_if_call_expression_is_dispatch(&mut self, call: &CallExpression) -> Result<()> {
        let Expression::Identifier(callee) = &call.callee else { return Ok(()) };
        if !self.dispatchers.iter().any(|(name, typing)| typing.is_none() && callee.name == name.as_str()) {
            return Ok(());
        }
        match call.arguments.first() {
            None => return Err(MagicStringError("Cannot read properties of undefined (reading 'kind')".into())),
            Some(Argument::StringLiteral(s)) => {
                let v = s.value.to_string();
                self.add_to_events(v.clone(), None);
                self.dispatched.insert(v);
            }
            Some(Argument::Identifier(id)) => {
                if let Some(s) = self.string_vars.get(id.name.as_str()).filter(|s| !s.is_empty()).cloned() {
                    self.add_to_events(s.clone(), None);
                    self.dispatched.insert(s);
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn add_to_events(&mut self, name: String, info: Option<EventInfo>) {
        if self.events.contains_key(&name) {
            // multiple definitions: merge by falling back to any
            self.events.insert(name.clone(), EventInfo { ty: "CustomEvent<any>".into(), doc: None });
            self.dispatched.insert(name);
        } else {
            self.events.insert(name, info.unwrap_or(EventInfo { ty: "CustomEvent<any>".into(), doc: None }));
        }
    }
}

fn extract_properties(ast: &ScriptAst, members: &[TSSignature], map: &mut IndexMap<String, EventInfo>) -> Result<()> {
    for m in members {
        if let TSSignature::TSPropertySignature(p) = m {
            let ty = p.type_annotation.as_ref().map(|t| text(ast.text, t.type_annotation.span())).filter(|t| !t.is_empty()).unwrap_or("Event");
            map.insert(prop_name(ast, &p.key, p.computed)?, EventInfo { ty: ty.to_string(), doc: get_doc(ast, p.span) });
        }
    }
    Ok(())
}

const NAME_ERROR: &str = "The ComponentEvents interface can only have properties of type Identifier, StringLiteral or ComputedPropertyName. In case of ComputedPropertyName, it must be a const declared within the component and initialized with a string.";

/// `getName`: an identifier or string key, or a computed key naming a top-level string const
fn prop_name(ast: &ScriptAst, key: &PropertyKey, computed: bool) -> Result<String> {
    match key {
        PropertyKey::StaticIdentifier(id) => Ok(id.name.to_string()),
        PropertyKey::StringLiteral(s) if !computed => Ok(s.value.to_string()),
        PropertyKey::Identifier(id) => {
            // `[name]`: look for a top-level `const name = '..'`
            for stmt in &ast.program.body {
                if let Some(Declaration::VariableDeclaration(v)) = super::hoistable::unwrap_export(stmt) {
                    if let Some(d) = v.declarations.iter().find(|d| binding_ident(&d.id).is_some_and(|i| i.name == id.name)) {
                        if let Some(Expression::StringLiteral(s)) = &d.init {
                            if !s.value.is_empty() {
                                return Ok(s.value.to_string());
                            }
                        }
                        break;
                    }
                }
            }
            Err(MagicStringError(NAME_ERROR.into()))
        }
        _ => Err(MagicStringError(NAME_ERROR.into())),
    }
}

/// `getDoc`: the member's last leading doc comment, without the comment markers
fn get_doc(ast: &ScriptAst, span: oxc_span::Span) -> Option<String> {
    let comment = ast.last_leading_doc(ast.full_start(span.start), span.start as usize)?;
    static OPEN: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| regex::Regex::new(r"\s*/\*\*").unwrap());
    static CLOSE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| regex::Regex::new(r"\s*\*/").unwrap());
    static STAR: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| regex::Regex::new(r"\s*\*").unwrap());
    Some(
        comment
            .split('\n')
            .map(|line| {
                let l = OPEN.replace(line, "");
                let l = CLOSE.replace(&l, "");
                let l = STAR.replace(&l, "");
                crate::svelte2tsx::script::js_trim(&l).to_string()
            })
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

fn import_is_event_dispatcher(import: &ImportDeclaration) -> Option<String> {
    if import.source.value != "svelte" {
        return None;
    }
    import.specifiers.as_ref()?.iter().find_map(|s| match s {
        ImportDeclarationSpecifier::ImportSpecifier(s) if s.imported.name() == "createEventDispatcher" => Some(s.local.name.to_string()),
        _ => None,
    })
}

/// `const name = createEventDispatcher<typing>()`
fn instantiated_event_dispatcher<'r, 'a>(
    d: &'r VariableDeclarator<'a>,
    import: Option<&str>,
) -> Option<(&'r str, Option<&'r TSType<'a>>, &'r CallExpression<'a>)> {
    let id = binding_ident(&d.id)?;
    let Some(Expression::CallExpression(call)) = &d.init else { return None };
    match &call.callee {
        Expression::Identifier(callee) if Some(callee.name.as_str()) == import => {
            Some((id.name.as_str(), call.type_arguments.as_ref().and_then(|t| t.params.first()), call))
        }
        _ => None,
    }
}
