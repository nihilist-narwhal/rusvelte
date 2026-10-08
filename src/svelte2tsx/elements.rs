//! Port of `htmlxtojsx_v2/nodes/Element.ts` and `InlineComponent.ts`.
//!
//! Both kinds live in one arena; `parent` links form the chain the JS code walks.

use super::transform::*;
use crate::magic_string::MagicString;

const VOID_TAGS: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source", "track", "wbr",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Element,
    Component,
}

/// What the element needs from its legacy node
#[derive(Debug, Clone)]
pub struct NodeInfo<'a> {
    pub name: &'a str,
    pub start: usize,
    pub end: usize,
    pub first_child_start: Option<usize>,
    pub has_children: bool,
    /// svelte:element `this`: `Some(Ok(range))` for an expression, `Some(Err(name))` for a static tag
    pub tag: Option<std::result::Result<(usize, usize), String>>,
    /// svelte:component `this` expression range
    pub expression: Option<(usize, usize)>,
    /// for `<slot name="...">`: the first value chunk of the `name` attribute (range, is_text)
    pub slot_name_value: Option<(usize, usize)>,
    /// whether an `is="..."` attribute's first text value contains a dash
    pub is_attr_has_dash: bool,
}

#[derive(Debug)]
pub struct El<'a> {
    pub kind: Kind,
    pub node: NodeInfo<'a>,
    pub parent: Option<usize>,
    pub child: Option<usize>,
    pub tag_name: &'a str,
    pub typings_namespace: String,
    tag_name_end: usize,
    start_tag_start: usize,
    start_tag_end: usize,
    is_selfclosing: bool,
    name: String,
    // Element
    referenced_name: bool,
    start_end: Ts,
    attrs: Ts,
    slot_lets: Option<(Ts, Ts)>,
    actions: Ts,
    action_ids: Vec<String>,
    end_t: Ts,
    // InlineComponent
    start_t: Ts,
    events: Ts,
    snippet_props: Vec<String>,
    /// the deferred `const $$_name = ...` rewrite of `start_t`
    pending_name_decl: Option<(usize, String)>,
}

pub struct Elements<'a> {
    pub arena: Vec<El<'a>>,
}

fn depth(arena: &[El], parent: Option<usize>) -> usize {
    let mut idx = 0;
    let mut p = parent;
    while let Some(i) = p {
        p = arena[i].parent;
        idx += 1;
    }
    idx
}

impl<'a> Elements<'a> {
    pub fn new() -> Self {
        Elements { arena: Vec::new() }
    }

    /// `new Element(str, node, typingsNamespace, parent)`
    pub fn new_element(&mut self, str: &mut MagicString, node: NodeInfo<'a>, typings_namespace: &str, parent: Option<usize>) -> Result<usize> {
        let original = str.original;
        let id = self.arena.len();
        if let Some(p) = parent {
            self.arena[p].child = Some(id);
        }
        let tag_name = if node.name == "svelte:body" { "body" } else { node.name };
        let is_selfclosing = byte_at(original, node.end.wrapping_sub(2)) == Some(b'/')
            || VOID_TAGS.contains(&node.name)
            || (!node.has_children && !ends_with_closing_tag(&original[node.start..node.end], node.name));
        let start_tag_start = node.start;
        let start_tag_end = compute_start_tag_end(original, &node, is_selfclosing);
        let tag_name_end = start_tag_start + node.name.len() + 1;

        let mut attrs = Ts::new();
        if is_space_at(original, tag_name_end) {
            attrs.push(T::Delete(tag_name_end));
            attrs.push(T::Range(tag_name_end, next_char(original, tag_name_end)));
            str.overwrite(tag_name_end, next_char(original, tag_name_end), "", true)?;
        }

        let d = depth(&self.arena, parent);
        let name = match node.name {
            "svelte:options" | "svelte:head" | "svelte:window" | "svelte:body" | "svelte:fragment" => {
                format!("$$_svelte{}{d}", &node.name[7..])
            }
            "svelte:element" => format!("$$_svelteelement{d}"),
            "slot" => format!("$$_slot{d}"),
            n => format!("$$_{}{d}", sanitize_prop_name(n)),
        };

        self.arena.push(El {
            kind: Kind::Element,
            node,
            parent,
            child: None,
            tag_name,
            typings_namespace: typings_namespace.to_string(),
            tag_name_end,
            start_tag_start,
            start_tag_end,
            is_selfclosing,
            name,
            referenced_name: false,
            start_end: vec!["});".into()],
            attrs,
            slot_lets: None,
            actions: Ts::new(),
            action_ids: Vec::new(),
            end_t: Ts::new(),
            start_t: Ts::new(),
            events: Ts::new(),
            snippet_props: Vec::new(),
            pending_name_decl: None,
        });
        Ok(id)
    }

    /// `new InlineComponent(str, node, parent)`
    pub fn new_component(&mut self, str: &mut MagicString, node: NodeInfo<'a>, parent: Option<usize>) -> Result<usize> {
        let original = str.original;
        let id = self.arena.len();
        if let Some(p) = parent {
            self.arena[p].child = Some(id);
        }
        let is_selfclosing = byte_at(original, node.end.wrapping_sub(2)) == Some(b'/');
        let start_tag_start = node.start;
        let start_tag_end = compute_start_tag_end(original, &node, is_selfclosing);
        let tag_name_end = start_tag_start + node.name.len() + 1;

        let mut props = Ts::new();
        if is_space_at(original, tag_name_end) {
            props.push(T::Delete(tag_name_end));
            props.push(T::Range(tag_name_end, next_char(original, tag_name_end)));
            str.overwrite(tag_name_end, next_char(original, tag_name_end), "", true)?;
        }

        let d = depth(&self.arena, parent);
        let (name, start_t, start_end, pending) = if node.name == "svelte:self" {
            let name = format!("$$_svelteself{d}");
            let decl = format!("{{ const {name} = __sveltets_2_createComponentAny({{");
            (name, vec![T::from("{ __sveltets_2_createComponentAny({")], vec![T::from("});")], (0, decl))
        } else {
            let is_svelte_component = node.name == "svelte:component";
            let reversed: String = sanitize_prop_name(node.name).chars().rev().collect();
            let name = format!("$$_{reversed}{d}");
            let constructor_name = format!("{name}C");
            let (ns, ne) = if is_svelte_component {
                node.expression.unwrap_or((0, 0))
            } else {
                let s = index_of(original, node.name, node.start).unwrap_or(0);
                (s, s + node.name.len())
            };
            let decl = format!("); const {name} = new {constructor_name}({{ target: __sveltets_2_any(), props: {{");
            (
                name,
                vec![
                    T::from(format!("{{ const {constructor_name} = __sveltets_2_ensureComponent(")),
                    T::Range(ns, ne),
                    T::from(format!("); new {constructor_name}({{ target: __sveltets_2_any(), props: {{")),
                ],
                vec![T::from("}});")],
                (2, decl),
            )
        };

        self.arena.push(El {
            kind: Kind::Component,
            tag_name: node.name,
            node,
            parent,
            child: None,
            typings_namespace: String::new(),
            tag_name_end,
            start_tag_start,
            start_tag_end,
            is_selfclosing,
            name,
            referenced_name: false,
            start_end,
            attrs: props,
            slot_lets: None,
            actions: Ts::new(),
            action_ids: Vec::new(),
            end_t: Ts::new(),
            start_t,
            events: Ts::new(),
            snippet_props: Vec::new(),
            pending_name_decl: Some(pending),
        });
        Ok(id)
    }

    /// The `name` getter, with its side effect (the name is declared because it's used)
    pub fn name(&mut self, id: usize) -> String {
        let el = &mut self.arena[id];
        match el.kind {
            Kind::Element => el.referenced_name = true,
            Kind::Component => {
                if let Some((idx, decl)) = el.pending_name_decl.take() {
                    el.start_t[idx] = T::Str(decl);
                }
            }
        }
        el.name.clone()
    }

    pub fn kind(&self, id: usize) -> Kind {
        self.arena[id].kind
    }

    pub fn parent(&self, id: usize) -> Option<usize> {
        self.arena[id].parent
    }

    /// `element.tagName` (`undefined` on an InlineComponent)
    pub fn tag_name(&self, id: usize) -> &'a str {
        match self.arena[id].kind {
            Kind::Element => self.arena[id].tag_name,
            Kind::Component => "undefined",
        }
    }

    /// `element.typingsNamespace` (`undefined` on an InlineComponent)
    pub fn typings_namespace(&self, id: usize) -> &str {
        match self.arena[id].kind {
            Kind::Element => &self.arena[id].typings_namespace,
            Kind::Component => "undefined",
        }
    }

    /// `addAttribute` / `addProp`
    pub fn add_attribute(&mut self, id: usize, name: Ts, value: Option<Ts>) {
        let attrs = &mut self.arena[id].attrs;
        attrs.extend(name);
        if let Some(value) = value {
            attrs.push(":".into());
            attrs.extend(value);
        }
        attrs.push(",".into());
    }

    pub fn add_slot_name(&mut self, id: usize, transformation: Ts) {
        let el = &mut self.arena[id];
        let lets = el.slot_lets.get_or_insert_with(|| (Ts::new(), Ts::new()));
        lets.0 = transformation;
    }

    pub fn add_slot_let(&mut self, id: usize, transformation: Ts) {
        let el = &mut self.arena[id];
        let lets = el.slot_lets.get_or_insert_with(|| (vec!["default".into()], Ts::new()));
        lets.1.extend(transformation);
        lets.1.push(",".into());
    }

    pub fn append_to_start_end(&mut self, id: usize, value: Ts) {
        self.arena[id].start_end.extend(value);
    }

    /// Element.addAction
    pub fn add_action(&mut self, str: &MagicString, id: usize, start: usize, name: &str, expression: Option<(usize, usize)>, leading: Ts, trailing: Ts) {
        let original = str.original;
        let el = &mut self.arena[id];
        let action_id = format!("$$action_{}", el.action_ids.len());
        el.action_ids.push(action_id.clone());
        if el.actions.is_empty() {
            el.actions.push("{".into());
        }
        el.actions.extend(leading);
        el.actions.push(format!("const {action_id} = __sveltets_2_ensureAction(").into());
        let (a, b) = directive_name_range(original, start, name);
        el.actions.push(T::Range(a, b));
        el.actions.push(format!("({}.mapElementTag('{}')", el.typings_namespace, el.tag_name).into());
        if let Some(expr) = expression {
            el.actions.push(",(".into());
            let (s, e) = range_with_trailing_property_access(original, expr);
            el.actions.push(T::Range(s, e));
            el.actions.push(")".into());
        }
        el.actions.push("));".into());
        el.actions.extend(trailing);
    }

    /// InlineComponent.addEvent
    pub fn add_event(&mut self, str: &mut MagicString, id: usize, name: (usize, usize), expression: Option<(usize, usize)>, leading: Ts, trailing: Ts) -> Result<()> {
        let el_name = self.name(id);
        let range = surround_with(str, name, "\"", "\"")?;
        let el = &mut self.arena[id];
        el.events.extend(leading);
        el.events.push(format!("{el_name}.$on(").into());
        el.events.push(T::Range(range.0, range.1));
        el.events.push(", ".into());
        match expression {
            Some((s, e)) => el.events.push(T::Range(s, e)),
            None => el.events.push("() => {}".into()),
        }
        el.events.push(");".into());
        el.events.extend(trailing);
        Ok(())
    }

    /// InlineComponent.addImplicitSnippetProp
    pub fn add_implicit_snippet_prop(&mut self, original: &str, id: usize, name: (usize, usize), transforms: Ts) {
        self.add_attribute(id, vec![T::Range(name.0, name.1)], Some(transforms));
        self.arena[id].snippet_props.push(original[name.0..name.1].to_string());
    }

    pub fn is_custom_element(&self, id: usize) -> bool {
        let el = &self.arena[id];
        el.tag_name.contains('-') || el.node.is_attr_has_dash
    }

    pub fn perform_transformation(&mut self, str: &mut MagicString, id: usize) -> Result<()> {
        match self.arena[id].kind {
            Kind::Element => self.perform_element(str, id),
            Kind::Component => self.perform_component(str, id),
        }
    }

    fn perform_element(&mut self, str: &mut MagicString, id: usize) -> Result<()> {
        let original = str.original;
        self.arena[id].end_t.push("}".into());

        let mut slot_let = Ts::new();
        if let Some((name, lets)) = self.arena[id].slot_lets.clone() {
            let parent = self.arena[id].parent.expect("slot lets need a parent");
            let parent_name = self.name(parent);
            slot_let.push(format!("{{const {{{},", surround_with_ignore_comments("$$_$$")).into());
            slot_let.extend(lets);
            if name.first() == Some(&T::from("default")) {
                slot_let.push(format!("}} = {parent_name}.$$slot_def.default;$$_$$;").into());
            } else {
                slot_let.push(format!("}} = {parent_name}.$$slot_def[\"").into());
                slot_let.extend(name);
                slot_let.push("\"];$$_$$;".into());
            }
            self.arena[id].end_t.push("}".into());
        }

        if !self.arena[id].action_ids.is_empty() {
            self.arena[id].end_t.push("}".into());
        }

        let start_t = self.element_start_transformation(str, id)?;
        let el = &self.arena[id];
        if el.is_selfclosing {
            let mut transform_end = el.start_tag_end;
            if byte_at(original, transform_end.wrapping_sub(1)) != Some(b'>')
                && (transform_end == el.tag_name_end || transform_end == el.tag_name_end + 1)
            {
                transform_end = el.start_tag_start;
                str.remove(el.start_tag_start, el.start_tag_start + 1)?;
            }
            let mut all = slot_let;
            all.extend(el.actions.iter().cloned());
            all.extend(start_t);
            all.extend(el.attrs.iter().cloned());
            all.extend(el.start_end.iter().cloned());
            all.extend(el.end_t.iter().cloned());
            transform(str, el.start_tag_start, transform_end, &all)?;
        } else {
            let mut all = slot_let;
            all.extend(el.actions.iter().cloned());
            all.extend(start_t);
            all.extend(el.attrs.iter().cloned());
            all.extend(el.start_end.iter().cloned());
            transform(str, el.start_tag_start, el.start_tag_end, &all)?;

            let node = &el.node;
            let closing_start = last_index_of(original, "</", node.end.saturating_sub(1)).map_or(0, |i| i + 2);
            let closing_tag = original.get(closing_start..node.end.saturating_sub(1)).unwrap_or("");
            let tag_end_idx = original[node.start..node.end].rfind(&format!("</{}", node.name));
            let end_start = match tag_end_idx {
                Some(i) if closing_tag.trim() == node.name => i + node.start,
                _ => node.end,
            };
            transform(str, end_start, node.end, &el.end_t.clone())?;
        }
        Ok(())
    }

    fn element_start_transformation(&mut self, str: &mut MagicString, id: usize) -> Result<Ts> {
        let el = &self.arena[id];
        let create_element = format!("{}.createElement", el.typings_namespace);
        let add_actions = if el.action_ids.is_empty() {
            String::new()
        } else {
            format!(", __sveltets_2_union({})", el.action_ids.join(","))
        };
        let mut statement: Ts = match el.node.name {
            "svelte:options" | "svelte:head" | "svelte:window" | "svelte:body" | "svelte:fragment" => {
                vec![format!("{create_element}(\"{}\"{add_actions}, {{", el.node.name).into()]
            }
            "svelte:element" => {
                let node_name = match &el.node.tag {
                    Some(Ok((s, e))) => T::Range(*s, *e),
                    Some(Err(name)) => format!("\"{name}\"").into(),
                    None => "\"\"".into(),
                };
                vec![format!("{create_element}(").into(), node_name, format!("{add_actions}, {{").into()]
            }
            "slot" => {
                let slot_name = match el.node.slot_name_value {
                    Some(range) => {
                        let r = surround_with(str, range, "\"", "\"")?;
                        T::Range(r.0, r.1)
                    }
                    None => "\"default\"".into(),
                };
                vec!["__sveltets_createSlot(".into(), slot_name, ", {".into()]
            }
            _ => vec![
                format!("{create_element}(\"").into(),
                T::Range(el.node.start + 1, el.tag_name_end),
                format!("\"{add_actions}, {{").into(),
            ],
        };
        let el = &self.arena[id];
        if let T::Str(first) = &mut statement[0] {
            if el.referenced_name {
                *first = format!("const {} = {first}", el.name);
            }
            *first = format!("{{ {first}");
        }
        Ok(statement)
    }

    fn perform_component(&mut self, str: &mut MagicString, id: usize) -> Result<()> {
        let original = str.original;
        let mut named_slot_let = Ts::new();
        let mut default_slot_let = Ts::new();
        if let Some((name, lets)) = self.arena[id].slot_lets.clone() {
            if name.first() == Some(&T::from("default")) {
                let own_name = self.name(id);
                default_slot_let.push(format!("{{const {{{},", surround_with_ignore_comments("$$_$$")).into());
                default_slot_let.extend(lets);
                default_slot_let.push(format!("}} = {own_name}.$$slot_def.default;$$_$$;").into());
            } else {
                let parent = self.arena[id].parent.expect("named slot lets need a parent");
                let parent_name = self.name(parent);
                named_slot_let.push(format!("{{const {{{},", surround_with_ignore_comments("$$_$$")).into());
                named_slot_let.extend(lets);
                named_slot_let.push(format!("}} = {parent_name}.$$slot_def[\"").into());
                named_slot_let.extend(name);
                named_slot_let.push("\"];$$_$$;".into());
            }
            self.arena[id].end_t.push("}".into());
        }

        let snippet_vars = self.arena[id].snippet_props.join(", ");
        let snippet_decl = if snippet_vars.is_empty() {
            String::new()
        } else {
            let own_name = self.name(id);
            surround_with_ignore_comments(&format!("const {{{snippet_vars}}} = {own_name}.$$prop_def;"))
        };

        let el = &mut self.arena[id];
        if el.is_selfclosing {
            el.end_t.push("}".into());
            let mut all = named_slot_let;
            all.extend(el.start_t.iter().cloned());
            all.extend(el.attrs.iter().cloned());
            all.extend(el.start_end.iter().cloned());
            all.extend(el.events.iter().cloned());
            all.extend(default_slot_let);
            all.push(snippet_decl.into());
            all.extend(el.end_t.iter().cloned());
            transform(str, el.start_tag_start, el.start_tag_end, &all)?;
        } else {
            let node = &el.node;
            let end_start = match original[node.start..node.end].rfind(&format!("</{}", node.name)) {
                None => {
                    // Can happen in loose parsing mode when there's no closing tag
                    el.start_tag_end = (node.end.saturating_sub(1)).max(el.tag_name_end);
                    node.end
                }
                Some(i) => i + node.start,
            };
            if !node.name.starts_with("svelte:") && end_start != node.end {
                // Ensure the end tag is mapped, too. </Component> -> Component}
                el.end_t.push(T::Range(end_start + 2, end_start + node.name.len() + 2));
            }
            el.end_t.push("}".into());

            let mut transformation_end = el.start_tag_end;
            if transformation_end == el.tag_name_end {
                transformation_end = el.start_tag_start;
                str.remove(el.start_tag_start, el.start_tag_start + 1)?;
            }
            let mut all = named_slot_let;
            all.extend(el.start_t.iter().cloned());
            all.extend(el.attrs.iter().cloned());
            all.extend(el.start_end.iter().cloned());
            all.extend(el.events.iter().cloned());
            all.push(snippet_decl.into());
            all.extend(default_slot_let);
            transform(str, el.start_tag_start, transformation_end, &all)?;
            transform(str, end_start, el.node.end, &el.end_t.clone())?;
        }
        Ok(())
    }
}

fn compute_start_tag_end(original: &str, node: &NodeInfo, is_selfclosing: bool) -> usize {
    if let Some(s) = node.first_child_start {
        return s;
    }
    if is_selfclosing {
        node.end
    } else {
        last_index_of(original, ">", node.end.saturating_sub(2)).map_or(0, |i| i + 1)
    }
}

/// `/<\/name\s*>$/`
fn ends_with_closing_tag(s: &str, name: &str) -> bool {
    let Some(rest) = s.strip_suffix('>') else { return false };
    let rest = rest.trim_end_matches(crate::parser::utils::is_whitespace_char);
    rest.ends_with(&format!("</{name}"))
}
