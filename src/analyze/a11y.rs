//! Port of `phases/2-analyze/visitors/shared/a11y/index.js`.

use super::a11y_data::{self as data, Schema};
use super::nodes::P;
use super::utils::{self, fuzzymatch, list, text_value};
use super::{Analyzer, warnings as w};
use crate::ast::{Attr, AttrValue, Node, NodeId};
use crate::error::Result;

const INVISIBLE_ELEMENTS: &[&str] = &["meta", "html", "script", "style"];
const ARIA_ATTRIBUTES: &[&str] = &[
    "activedescendant", "atomic", "autocomplete", "braillelabel", "brailleroledescription", "busy", "checked",
    "colcount", "colindex", "colspan", "controls", "current", "describedby", "description", "details", "disabled",
    "dropeffect", "errormessage", "expanded", "flowto", "grabbed", "haspopup", "hidden", "invalid", "keyshortcuts",
    "label", "labelledby", "level", "live", "modal", "multiline", "multiselectable", "orientation", "owns",
    "placeholder", "posinset", "pressed", "readonly", "relevant", "required", "roledescription", "rowcount", "rowindex",
    "rowspan", "selected", "setsize", "sort", "valuemax", "valuemin", "valuenow", "valuetext",
];
const A11Y_DISTRACTING_ELEMENTS: &[&str] = &["blink", "marquee"];
const A11Y_REQUIRED_CONTENT: &[&str] = &["h1", "h2", "h3", "h4", "h5", "h6"];
const A11Y_LABELABLE: &[&str] = &["button", "input", "keygen", "meter", "output", "progress", "select", "textarea"];
const A11Y_INTERACTIVE_HANDLERS: &[&str] = &[
    "keypress", "keydown", "keyup", "click", "contextmenu", "dblclick", "drag", "dragend", "dragenter", "dragexit",
    "dragleave", "dragover", "dragstart", "drop", "mousedown", "mouseenter", "mouseleave", "mousemove", "mouseout",
    "mouseover", "mouseup", "pointerdown", "pointerup", "pointermove", "pointerenter", "pointerleave", "pointerover",
    "pointerout", "pointercancel", "touchstart", "touchend", "touchmove", "touchcancel",
];
const A11Y_RECOMMENDED_INTERACTIVE_HANDLERS: &[&str] = &["click", "mousedown", "mouseup", "keypress", "keydown", "keyup"];
const PRESENTATION_ROLES: &[&str] = &["presentation", "none"];
const COMBOBOX_IF_LIST: &[&str] = &["email", "search", "tel", "text", "url"];
const ADDRESS_TYPE_TOKENS: &[&str] = &["shipping", "billing"];
const AUTOFILL_FIELD_NAME_TOKENS: &[&str] = &[
    "", "on", "off", "name", "honorific-prefix", "given-name", "additional-name", "family-name", "honorific-suffix",
    "nickname", "username", "new-password", "current-password", "one-time-code", "organization-title", "organization",
    "street-address", "address-line1", "address-line2", "address-line3", "address-level4", "address-level3",
    "address-level2", "address-level1", "country", "country-name", "postal-code", "cc-name", "cc-given-name",
    "cc-additional-name", "cc-family-name", "cc-number", "cc-exp", "cc-exp-month", "cc-exp-year", "cc-csc", "cc-type",
    "transaction-currency", "transaction-amount", "language", "bday", "bday-day", "bday-month", "bday-year", "sex",
    "url", "photo",
];
const CONTACT_TYPE_TOKENS: &[&str] = &["home", "work", "mobile", "fax", "pager"];
const AUTOFILL_CONTACT_FIELD_NAME_TOKENS: &[&str] = &[
    "tel", "tel-country-code", "tel-national", "tel-area-code", "tel-local", "tel-local-prefix", "tel-local-suffix",
    "tel-extension", "email", "impp",
];

fn implicit_semantics(name: &str) -> Option<&'static str> {
    Some(match name {
        "a" | "area" | "link" => "link",
        "article" => "article",
        "aside" => "complementary",
        "body" => "document",
        "button" | "summary" => "button",
        "datalist" => "listbox",
        "dd" => "definition",
        "dfn" | "dt" => "term",
        "dialog" => "dialog",
        "details" | "fieldset" | "optgroup" => "group",
        "figure" => "figure",
        "form" => "form",
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => "heading",
        "hr" => "separator",
        "img" => "img",
        "li" => "listitem",
        "main" => "main",
        "menu" | "ol" | "ul" => "list",
        "meter" | "progress" => "progressbar",
        "nav" => "navigation",
        "option" => "option",
        "output" => "status",
        "section" => "region",
        "table" => "table",
        "tbody" | "tfoot" | "thead" => "rowgroup",
        "textarea" => "textbox",
        "tr" => "row",
        _ => return None,
    })
}

fn nested_implicit_semantics(name: &str) -> Option<&'static str> {
    match name {
        "header" => Some("banner"),
        "footer" => Some("contentinfo"),
        _ => None,
    }
}

fn input_type_to_implicit_role(t: &str) -> Option<&'static str> {
    Some(match t {
        "button" | "image" | "reset" | "submit" => "button",
        "checkbox" => "checkbox",
        "radio" => "radio",
        "range" => "slider",
        "number" => "spinbutton",
        "email" | "tel" | "text" | "url" => "textbox",
        "search" => "searchbox",
        _ => return None,
    })
}

fn menuitem_type_to_implicit_role(t: &str) -> Option<&'static str> {
    Some(match t {
        "command" => "menuitem",
        "checkbox" => "menuitemcheckbox",
        "radio" => "menuitemradio",
        _ => return None,
    })
}

fn non_interactive_to_interactive_exceptions(name: &str) -> &'static [&'static str] {
    match name {
        "ul" | "ol" | "menu" => &["listbox", "menu", "menubar", "radiogroup", "tablist", "tree", "treegrid"],
        "li" => &["menuitem", "option", "row", "tab", "treeitem"],
        "table" => &["grid"],
        "td" => &["gridcell"],
        "fieldset" => &["radiogroup", "presentation"],
        _ => &[],
    }
}

fn required_attributes(name: &str) -> Option<&'static [&'static str]> {
    Some(match name {
        "a" => &["href"],
        "area" => &["alt", "aria-label", "aria-labelledby"],
        "html" => &["lang"],
        "iframe" => &["title"],
        "img" => &["alt"],
        "object" => &["title", "aria-label", "aria-labelledby"],
        _ => return None,
    })
}

/// `get_static_value(attribute)`
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Static<'a> {
    Null,
    True,
    Str(&'a str),
}

impl<'a> Static<'a> {
    /// `get_static_text_value`
    fn text(self) -> Option<&'a str> {
        match self {
            Static::Str(s) => Some(s),
            _ => None,
        }
    }
    fn truthy(self) -> bool {
        match self {
            Static::Null => false,
            Static::True => true,
            Static::Str(s) => !s.is_empty(),
        }
    }
}

fn static_value<'a>(a: Option<&'a Attr<'a>>) -> Static<'a> {
    match a {
        Some(Attr::Attribute { value: AttrValue::True, .. }) => Static::True,
        Some(Attr::Attribute { value, .. }) => match text_value(value) {
            Some(t) => Static::Str(t),
            None => Static::Null,
        },
        _ => Static::Null,
    }
}

struct AttrMap<'a> {
    entries: Vec<(&'a str, &'a Attr<'a>)>,
}

impl<'a> AttrMap<'a> {
    fn get(&self, name: &str) -> Option<&'a Attr<'a>> {
        self.entries.iter().find(|(k, _)| *k == name).map(|(_, a)| *a)
    }
    fn has(&self, name: &str) -> bool {
        self.entries.iter().any(|(k, _)| *k == name)
    }
    fn set(&mut self, name: &'a str, a: &'a Attr<'a>) {
        match self.entries.iter_mut().find(|(k, _)| *k == name) {
            Some(entry) => entry.1 = a,
            None => self.entries.push((name, a)),
        }
    }
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum Interactivity {
    Interactive,
    NonInteractive,
    Static,
}

fn match_schema(schema: &Schema, tag_name: &str, map: &AttrMap) -> bool {
    if schema.name != tag_name {
        return false;
    }
    let Some(attributes) = schema.attributes else { return true };
    attributes.iter().all(|(name, value)| {
        let Some(attribute) = map.get(name) else { return false };
        if let Some(v) = value {
            if !v.is_empty() && Some(*v) != static_value(Some(attribute)).text() {
                return false;
            }
        }
        true
    })
}

fn element_interactivity(tag_name: &str, map: &AttrMap) -> Interactivity {
    if data::INTERACTIVE_ELEMENT_ROLE_SCHEMAS.iter().any(|s| match_schema(s, tag_name, map)) {
        return Interactivity::Interactive;
    }
    if tag_name != "header" && data::NON_INTERACTIVE_ELEMENT_ROLE_SCHEMAS.iter().any(|s| match_schema(s, tag_name, map)) {
        return Interactivity::NonInteractive;
    }
    if data::INTERACTIVE_ELEMENT_AX_OBJECT_SCHEMAS.iter().any(|s| match_schema(s, tag_name, map)) {
        return Interactivity::Interactive;
    }
    if data::NON_INTERACTIVE_ELEMENT_AX_OBJECT_SCHEMAS.iter().any(|s| match_schema(s, tag_name, map)) {
        return Interactivity::NonInteractive;
    }
    Interactivity::Static
}

fn is_semantic_role_element(role: &str, tag_name: &str, map: &AttrMap) -> bool {
    for entry in data::ELEMENT_AX_OBJECTS {
        let schema = &entry.schema;
        if schema.name == tag_name
            && schema.attributes.is_none_or(|attrs| {
                attrs.iter().all(|(name, value)| {
                    map.has(name)
                        && match (static_value(map.get(name)), value) {
                            (Static::Str(s), Some(v)) => s == *v,
                            _ => false,
                        }
                })
            })
            && entry.roles.contains(&role)
        {
            return true;
        }
    }
    false
}

fn is_presentation_role(role: Option<&str>) -> bool {
    role.is_some_and(|r| PRESENTATION_ROLES.contains(&r))
}
fn is_interactive_roles(role: Option<&str>) -> bool {
    role.is_some_and(|r| data::INTERACTIVE_ROLES.contains(&r))
}
fn is_non_interactive_roles(role: Option<&str>) -> bool {
    role.is_some_and(|r| data::NON_INTERACTIVE_ROLES.contains(&r))
}
fn is_abstract_role(role: Option<&str>) -> bool {
    role.is_some_and(|r| data::ABSTRACT_ROLES.contains(&r))
}

fn is_hidden_from_screen_reader(tag_name: &str, map: &AttrMap) -> bool {
    if tag_name == "input" && static_value(map.get("type")) == Static::Str("hidden") {
        return true;
    }
    let Some(aria_hidden) = map.get("aria-hidden") else { return false };
    match static_value(Some(aria_hidden)) {
        Static::Null => true,
        Static::True => true,
        Static::Str(s) => s == "true",
    }
}

fn has_disabled_attribute(map: &AttrMap) -> bool {
    if static_value(map.get("disabled")).truthy() {
        return true;
    }
    if let Some(a) = map.get("aria-disabled") {
        if static_value(Some(a)) == Static::Str("true") {
            return true;
        }
    }
    false
}

fn get_implicit_role(name: &str, map: &AttrMap) -> Option<&'static str> {
    if name == "menuitem" {
        let t = static_value(map.get("type")).text().filter(|t| !t.is_empty())?;
        menuitem_type_to_implicit_role(t)
    } else if name == "input" {
        let t = static_value(map.get("type")).text().filter(|t| !t.is_empty())?;
        if map.has("list") && COMBOBOX_IF_LIST.contains(&t) {
            return Some("combobox");
        }
        input_type_to_implicit_role(t)
    } else {
        implicit_semantics(name)
    }
}

fn is_valid_autocomplete(value: Static) -> bool {
    let s = match value {
        Static::True => return false,
        Static::Null => return true,
        Static::Str("") => return true,
        Static::Str(s) => s,
    };
    let lower = utils::js_trim(s).to_lowercase();
    let mut tokens: std::collections::VecDeque<&str> = utils::split_whitespace_js(&lower).into_iter().collect();
    if tokens.front().is_some_and(|t| t.starts_with("section-")) {
        tokens.pop_front();
    }
    if tokens.front().is_some_and(|t| ADDRESS_TYPE_TOKENS.contains(t)) {
        tokens.pop_front();
    }
    if tokens.front().is_some_and(|t| AUTOFILL_FIELD_NAME_TOKENS.contains(t)) {
        tokens.pop_front();
    } else {
        if tokens.front().is_some_and(|t| CONTACT_TYPE_TOKENS.contains(t)) {
            tokens.pop_front();
        }
        if tokens.front().is_some_and(|t| AUTOFILL_CONTACT_FIELD_NAME_TOKENS.contains(t)) {
            tokens.pop_front();
        } else {
            return false;
        }
    }
    if tokens.front() == Some(&"webauthn") {
        tokens.pop_front();
    }
    tokens.is_empty()
}

/// `is_parent(path, elements)`
fn is_parent(an: &Analyzer, elements: &[&str]) -> bool {
    for p in an.path.iter().rev() {
        let Some(n) = p.node() else { continue };
        let Some(el) = an.element(n) else { continue };
        if el.kind == "SvelteElement" {
            return true;
        }
        if el.kind == "RegularElement" {
            return elements.contains(&el.name);
        }
    }
    false
}

/// `has_content(element)`
fn has_content(an: &Analyzer, n: NodeId) -> bool {
    let el = an.element(n).unwrap();
    for &c in &an.ast.fragments[el.fragment].nodes {
        let node = &an.ast.nodes[c];
        if let Node::Text { data, .. } = node {
            if utils::js_trim(data).is_empty() {
                continue;
            }
        }
        if let Node::Element(child) = node {
            if matches!(child.kind, "RegularElement" | "SvelteElement") {
                if child.attributes.iter().any(|a| matches!(a, Attr::Attribute { name: "popover", .. })) {
                    continue;
                }
                if child.name == "img" && child.attributes.iter().any(|a| matches!(a, Attr::Attribute { name: "alt", .. })) {
                    return true;
                }
                if child.name == "selectedcontent" {
                    return true;
                }
                if !has_content(an, c) {
                    continue;
                }
            }
        }
        return true;
    }
    false
}

/// `has_input_child` of `<label>`
fn has_input_child(an: &Analyzer, p: P) -> bool {
    if let P::Node(n) = p {
        match &an.ast.nodes[n] {
            Node::Element(el)
                if matches!(el.kind, "SvelteElement" | "SlotElement" | "Component")
                    || (el.kind == "RegularElement" && (A11Y_LABELABLE.contains(&el.name) || el.name == "slot")) =>
            {
                return true;
            }
            Node::RenderTag { .. } => return true,
            _ => {}
        }
    }
    let mut found = false;
    let _ = super::nodes::each_child(p, an.ast, &mut found, &mut |found: &mut bool, c| {
        if !*found && has_input_child(an, c) {
            *found = true;
        }
        Ok(())
    });
    found
}

pub fn check_element<'s>(an: &mut Analyzer<'s>, n: NodeId) -> Result<()> {
    let el = an.element(n).unwrap();
    let node = P::Node(n);
    let mut attribute_map = AttrMap { entries: Vec::new() };
    let mut handlers: Vec<&'s str> = Vec::new();
    let mut attributes: Vec<&'s Attr<'s>> = Vec::new();
    let is_dynamic_element = el.kind == "SvelteElement";
    let mut has_spread = false;
    let mut has_contenteditable_attr = false;
    let mut has_contenteditable_binding = false;

    let add_handler = |handlers: &mut Vec<&'s str>, h: &'s str| {
        if !handlers.contains(&h) {
            handlers.push(h);
        }
    };

    for a in &el.attributes {
        match a {
            Attr::Attribute { name, .. } => {
                if utils::is_event_attribute(a) {
                    add_handler(&mut handlers, &name[2..]);
                } else {
                    attributes.push(a);
                    attribute_map.set(name, a);
                    if *name == "contenteditable" {
                        has_contenteditable_attr = true;
                    }
                }
            }
            Attr::Spread { .. } => has_spread = true,
            Attr::Directive { kind: "BindDirective", name, .. } => {
                if utils::is_content_editable_binding(name) {
                    has_contenteditable_binding = true;
                }
            }
            Attr::Directive { kind: "OnDirective", name, .. } => add_handler(&mut handlers, name),
            _ => {}
        }
    }

    let interactivity = element_interactivity(el.name, &attribute_map);
    let is_interactive = interactivity == Interactivity::Interactive;
    let is_non_interactive = interactivity == Interactivity::NonInteractive;
    let is_static = interactivity == Interactivity::Static;

    for a in &el.attributes {
        let Attr::Attribute { name, .. } = a else { continue };
        let ap = P::Attr(a);
        let lower = name.to_lowercase();
        if lower.starts_with("aria-") {
            if INVISIBLE_ELEMENTS.contains(&el.name) {
                an.warn(Some(ap), w::a11y_aria_attributes(el.name));
            }
            let ty = &lower[5..];
            if !ARIA_ATTRIBUTES.contains(&ty) {
                let m = fuzzymatch(ty, ARIA_ATTRIBUTES);
                an.warn(Some(ap), w::a11y_unknown_aria_attribute(ty, m.as_deref()));
            }
            if lower == "aria-hidden" && is_heading(el.name) {
                an.warn(Some(ap), w::a11y_hidden(el.name));
            }
            let value = static_value(Some(a));
            if let Some(schema) = data::ARIA_PROPERTIES.iter().find(|p| p.name == lower) {
                validate_aria_attribute_value(an, ap, &lower, schema, value);
            }
            if lower == "aria-activedescendant"
                && !is_dynamic_element
                && !is_interactive
                && !attribute_map.has("tabindex")
                && !has_spread
            {
                an.warn(Some(ap), w::a11y_aria_activedescendant_has_tabindex());
            }
        }

        match lower.as_str() {
            "role" => {
                if INVISIBLE_ELEMENTS.contains(&el.name) {
                    an.warn(Some(ap), w::a11y_misplaced_role(el.name));
                }
                let Static::Str(value) = static_value(Some(a)) else { continue };
                for current_role in utils::split_whitespace_js(value) {
                    let role = if current_role.is_empty() { None } else { Some(current_role) };
                    if role.is_some() && is_abstract_role(role) {
                        an.warn(Some(ap), w::a11y_no_abstract_role(current_role));
                    } else if role.is_some() && !data::ARIA_ROLES.contains(&current_role) {
                        let m = fuzzymatch(current_role, data::ARIA_ROLES);
                        an.warn(Some(ap), w::a11y_unknown_role(current_role, m.as_deref()));
                    }

                    if Some(current_role) == get_implicit_role(el.name, &attribute_map)
                        && !["ul", "ol", "li", "menu"].contains(&el.name)
                        && !((el.name == "a" || el.name == "area") && !attribute_map.has("href"))
                    {
                        an.warn(Some(ap), w::a11y_no_redundant_roles(current_role));
                    }

                    if !is_parent(an, &["section", "article"]) && nested_implicit_semantics(el.name) == Some(current_role) {
                        an.warn(Some(ap), w::a11y_no_redundant_roles(current_role));
                    }

                    if !is_dynamic_element && !is_semantic_role_element(current_role, el.name, &attribute_map) {
                        if let Some(r) = data::ROLES.iter().find(|r| r.name == current_role) {
                            let has_missing = !has_spread
                                && r.required_props.iter().any(|prop| !attributes.iter().any(|a| super::attr_name(a) == *prop));
                            if has_missing {
                                let props: Vec<String> = r.required_props.iter().map(|v| format!("\"{v}\"")).collect();
                                an.warn(Some(ap), w::a11y_role_has_required_aria_props(current_role, &list(&props, "and")));
                            }
                        }
                    }

                    if !has_spread
                        && !has_disabled_attribute(&attribute_map)
                        && !is_hidden_from_screen_reader(el.name, &attribute_map)
                        && !is_presentation_role(role)
                        && is_interactive_roles(role)
                        && is_static
                        && attribute_map.get("tabindex").is_none()
                    {
                        let has_interactive = handlers.iter().any(|h| A11Y_INTERACTIVE_HANDLERS.contains(h));
                        if has_interactive {
                            an.warn(Some(node), w::a11y_interactive_supports_focus(current_role));
                        }
                    }

                    if !has_spread && is_interactive && (is_non_interactive_roles(role) || is_presentation_role(role)) {
                        an.warn(Some(node), w::a11y_no_interactive_element_to_noninteractive_role(el.name, current_role));
                    }

                    if !has_spread
                        && is_non_interactive
                        && is_interactive_roles(role)
                        && !non_interactive_to_interactive_exceptions(el.name).contains(&current_role)
                    {
                        an.warn(Some(node), w::a11y_no_noninteractive_element_to_interactive_role(el.name, current_role));
                    }
                }
            }
            "accesskey" => an.warn(Some(ap), w::a11y_accesskey()),
            "autofocus" => {
                if el.name != "dialog" && !is_parent(an, &["dialog"]) {
                    an.warn(Some(ap), w::a11y_autofocus());
                }
            }
            "scope" => {
                if !is_dynamic_element && el.name != "th" {
                    an.warn(Some(ap), w::a11y_misplaced_scope());
                }
            }
            "tabindex" => {
                let positive = match static_value(Some(a)) {
                    Static::Null => false,
                    Static::True => true,
                    Static::Str(s) => js_to_number(s) > 0.0,
                };
                if positive {
                    an.warn(Some(ap), w::a11y_positive_tabindex());
                }
            }
            _ => {}
        }
    }

    let role = attribute_map.get("role");
    let role_static_value = static_value(role).text();

    if handlers.contains(&"click") {
        let is_non_presentation_role = role_static_value.is_some() && !is_presentation_role(role_static_value);
        if !is_dynamic_element
            && !is_hidden_from_screen_reader(el.name, &attribute_map)
            && (role.is_none() || is_non_presentation_role)
            && !is_interactive
            && !has_spread
        {
            let has_key_event = handlers.iter().any(|h| matches!(*h, "keydown" | "keyup" | "keypress"));
            if !has_key_event {
                an.warn(Some(node), w::a11y_click_events_have_key_events(el.name));
            }
        }
    }

    let role_value: Option<&str> = if role.is_some() { role_static_value } else { get_implicit_role(el.name, &attribute_map) };

    if !is_dynamic_element && !is_interactive && !is_interactive_roles(role_static_value) {
        let tab_index = attribute_map.get("tabindex");
        let tab_index_value = static_value(tab_index).text();
        if tab_index.is_some() && tab_index_value.is_none_or(|v| js_to_number(v) >= 0.0) {
            an.warn(Some(node), w::a11y_no_noninteractive_tabindex());
        }
    }

    if let Some(role_value) = role_value {
        if let Some(r) = data::ROLES.iter().find(|r| r.name == role_value) {
            let is_implicit = !role_value.is_empty() && role.is_none();
            for attr in &attributes {
                let attr_name = super::attr_name(attr);
                let invalid = data::ARIA_PROPERTIES.iter().any(|p| p.name == attr_name) && !r.props.contains(&attr_name);
                if invalid {
                    if is_implicit {
                        an.warn(Some(P::Attr(attr)), w::a11y_role_supports_aria_props_implicit(attr_name, role_value, el.name));
                    } else {
                        an.warn(Some(P::Attr(attr)), w::a11y_role_supports_aria_props(attr_name, role_value));
                    }
                }
            }
        }
    }

    if !has_spread
        && !has_contenteditable_attr
        && !is_hidden_from_screen_reader(el.name, &attribute_map)
        && !is_presentation_role(role_static_value)
        && ((!is_interactive && is_non_interactive_roles(role_static_value)) || (is_non_interactive && role.is_none()))
    {
        let has_interactive = handlers.iter().any(|h| A11Y_RECOMMENDED_INTERACTIVE_HANDLERS.contains(h));
        if has_interactive {
            an.warn(Some(node), w::a11y_no_noninteractive_element_interactions(el.name));
        }
    }

    if !has_spread
        && (role.is_none() || role_static_value.is_some())
        && !is_hidden_from_screen_reader(el.name, &attribute_map)
        && !is_presentation_role(role_static_value)
        && !is_interactive
        && !is_interactive_roles(role_static_value)
        && !is_non_interactive
        && !is_non_interactive_roles(role_static_value)
        && !is_abstract_role(role_static_value)
    {
        let interactive: Vec<String> =
            handlers.iter().filter(|h| A11Y_INTERACTIVE_HANDLERS.contains(h)).map(|h| h.to_string()).collect();
        if !interactive.is_empty() {
            an.warn(Some(node), w::a11y_no_static_element_interactions(el.name, &list(&interactive, "or")));
        }
    }

    if !has_spread && handlers.contains(&"mouseover") && !handlers.contains(&"focus") && !handlers.contains(&"focusin") {
        an.warn(Some(node), w::a11y_mouse_events_have_key_events("mouseover", "focus"));
    }
    if !has_spread && handlers.contains(&"mouseout") && !handlers.contains(&"blur") && !handlers.contains(&"focusout") {
        an.warn(Some(node), w::a11y_mouse_events_have_key_events("mouseout", "blur"));
    }

    let is_labelled = attribute_map.has("aria-label") || attribute_map.has("aria-labelledby") || attribute_map.has("title");

    match el.name {
        "a" | "button" => {
            let is_hidden = static_value(attribute_map.get("aria-hidden")) == Static::Str("true")
                || static_value(attribute_map.get("inert")) != Static::Null;
            if !has_spread && !is_hidden && !is_labelled && !has_content(an, n) {
                an.warn(Some(node), w::a11y_consider_explicit_label());
            }
            if el.name == "a" {
                let href = attribute_map.get("href").or_else(|| attribute_map.get("xlink:href"));
                if let Some(href) = href {
                    if let Some(href_value) = static_value(Some(href)).text() {
                        if href_value.is_empty() || href_value == "#" || js_prefix(href_value) {
                            an.warn(Some(P::Attr(href)), w::a11y_invalid_attribute(href_value, super::attr_name(href)));
                        }
                    }
                } else if !has_spread {
                    let id = static_value(attribute_map.get("id"));
                    let name = static_value(attribute_map.get("name"));
                    let aria_disabled = static_value(attribute_map.get("aria-disabled"));
                    if !id.truthy() && !name.truthy() && aria_disabled != Static::Str("true") {
                        warn_missing_attribute(an, n, &["href"], el.name);
                    }
                }
            }
        }
        "input" => {
            let ty = attribute_map.get("type");
            let type_value = static_value(ty).text();
            if type_value == Some("image") && !has_spread {
                let required = ["alt", "aria-label", "aria-labelledby"];
                if !required.iter().any(|r| attribute_map.has(r)) {
                    warn_missing_attribute(an, n, &required, "input type=\"image\"");
                }
            }
            if let (Some(_), Some(autocomplete)) = (ty, attribute_map.get("autocomplete")) {
                let value = static_value(Some(autocomplete));
                if !is_valid_autocomplete(value) {
                    let v = match value {
                        Static::Str(s) => s.to_string(),
                        Static::True => "true".into(),
                        Static::Null => "null".into(),
                    };
                    an.warn(Some(P::Attr(autocomplete)), w::a11y_autocomplete_valid(&v, type_value.unwrap_or("...")));
                }
            }
        }
        "img" => {
            let alt = static_value(attribute_map.get("alt")).text();
            let aria_hidden = static_value(attribute_map.get("aria-hidden"));
            if let Some(alt) = alt {
                if !alt.is_empty() && !aria_hidden.truthy() && !has_spread && redundant_img_alt(alt) {
                    an.warn(Some(node), w::a11y_img_redundant_alt());
                }
            }
        }
        "label" => {
            if !has_spread && !attribute_map.has("for") && !has_input_child(an, node) {
                an.warn(Some(node), w::a11y_label_has_associated_control());
            }
        }
        "video" => {
            let aria_hidden = attribute_map.get("aria-hidden");
            let aria_hidden_exist = aria_hidden.map(|a| static_value(Some(a)));
            if attribute_map.has("muted") || aria_hidden_exist == Some(Static::Str("true")) || has_spread {
                return Ok(());
            }
            if !attribute_map.has("src") {
                return Ok(());
            }
            let mut has_caption = false;
            let track = an.ast.fragments[el.fragment].nodes.iter().find_map(|&c| match &an.ast.nodes[c] {
                Node::Element(t) if t.kind == "RegularElement" && t.name == "track" => Some(t),
                _ => None,
            });
            if let Some(track) = track {
                has_caption = track.attributes.iter().any(|a| {
                    matches!(a, Attr::Spread { .. })
                        || (matches!(a, Attr::Attribute { name: "kind", .. }) && static_value(Some(a)) == Static::Str("captions"))
                });
            }
            if !has_caption {
                an.warn(Some(node), w::a11y_media_has_caption());
            }
        }
        "figcaption" => {
            if !is_parent(an, &["figure"]) {
                an.warn(Some(node), w::a11y_figcaption_parent());
            }
        }
        "figure" => {
            let children: Vec<NodeId> = an.ast.fragments[el.fragment]
                .nodes
                .iter()
                .copied()
                .filter(|&c| match &an.ast.nodes[c] {
                    Node::Comment { .. } => false,
                    Node::Text { data, .. } => utils::has_non_whitespace(data),
                    _ => true,
                })
                .collect();
            let index = children.iter().position(|&c| {
                matches!(&an.ast.nodes[c], Node::Element(e) if e.kind == "RegularElement" && e.name == "figcaption")
            });
            if let Some(index) = index {
                if index != 0 && index != children.len() - 1 {
                    an.warn(Some(P::Node(children[index])), w::a11y_figcaption_index());
                }
            }
        }
        _ => {}
    }

    if !has_spread && el.name != "a" {
        if let Some(required) = required_attributes(el.name) {
            if !required.iter().any(|r| attribute_map.has(r)) {
                warn_missing_attribute(an, n, required, el.name);
            }
        }
    }

    if A11Y_DISTRACTING_ELEMENTS.contains(&el.name) {
        an.warn(Some(node), w::a11y_distracting_elements(el.name));
    }

    if !has_spread
        && !is_labelled
        && !has_contenteditable_binding
        && A11Y_REQUIRED_CONTENT.contains(&el.name)
        && !has_content(an, n)
    {
        an.warn(Some(node), w::a11y_missing_content(el.name));
    }
    Ok(())
}

fn warn_missing_attribute(an: &mut Analyzer, n: NodeId, attributes: &[&str], name: &str) {
    let first = attributes[0];
    let article = if first.starts_with(['a', 'e', 'i', 'o', 'u']) || first == "href" { "an" } else { "a" };
    let sequence = if attributes.len() > 1 {
        format!("{} or {}", attributes[..attributes.len() - 1].join(", "), attributes[attributes.len() - 1])
    } else {
        first.to_string()
    };
    an.warn(Some(P::Node(n)), w::a11y_missing_attribute(name, article, &sequence));
}

fn validate_aria_attribute_value<'s>(an: &mut Analyzer<'s>, ap: P<'s>, name: &str, schema: &data::AriaProperty, value: Static) {
    let value = match value {
        Static::Null => return,
        Static::True => "",
        Static::Str(s) => s,
    };
    match schema.r#type {
        "id" | "string" => {
            if value.is_empty() {
                an.warn(Some(ap), w::a11y_incorrect_aria_attribute_type(name, "non-empty string"));
            }
        }
        "number" => {
            if value.is_empty() || js_to_number(value).is_nan() {
                an.warn(Some(ap), w::a11y_incorrect_aria_attribute_type(name, "number"));
            }
        }
        "boolean" => {
            if value != "true" && value != "false" {
                an.warn(Some(ap), w::a11y_incorrect_aria_attribute_type_boolean(name));
            }
        }
        "idlist" => {
            if value.is_empty() {
                an.warn(Some(ap), w::a11y_incorrect_aria_attribute_type_idlist(name));
            }
        }
        "integer" => {
            let v = js_to_number(value);
            if value.is_empty() || !(v.is_finite() && v == v.trunc()) {
                an.warn(Some(ap), w::a11y_incorrect_aria_attribute_type_integer(name));
            }
        }
        "token" => {
            let lower = value.to_lowercase();
            if !schema.values.contains(&lower.as_str()) {
                let values: Vec<String> = schema.values.iter().map(|v| format!("\"{v}\"")).collect();
                an.warn(Some(ap), w::a11y_incorrect_aria_attribute_type_token(name, &list(&values, "or")));
            }
        }
        "tokenlist" => {
            let lower = value.to_lowercase();
            if utils::split_whitespace_js(&lower).iter().any(|v| !schema.values.contains(v)) {
                let values: Vec<String> = schema.values.iter().map(|v| format!("\"{v}\"")).collect();
                an.warn(Some(ap), w::a11y_incorrect_aria_attribute_type_tokenlist(name, &list(&values, "or")));
            }
        }
        "tristate" => {
            if value != "true" && value != "false" && value != "mixed" {
                an.warn(Some(ap), w::a11y_incorrect_aria_attribute_type_tristate(name));
            }
        }
        _ => {}
    }
}

fn is_heading(name: &str) -> bool {
    let b = name.as_bytes();
    b.len() == 2 && b[0] == b'h' && (b'1'..=b'6').contains(&b[1])
}

/// `/^\W*javascript:/i`
fn js_prefix(s: &str) -> bool {
    let rest = s.trim_start_matches(|c: char| !(c.is_ascii_alphanumeric() || c == '_'));
    rest.len() >= 11 && rest[..11].eq_ignore_ascii_case("javascript:")
}

/// `/\b(image|picture|photo)\b/i`
fn redundant_img_alt(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    for word in ["image", "picture", "photo"] {
        let mut from = 0;
        while let Some(i) = lower[from..].find(word) {
            let start = from + i;
            let end = start + word.len();
            let before_ok = start == 0 || !is_word(bytes[start - 1]);
            let after_ok = end == bytes.len() || !is_word(bytes[end]);
            if before_ok && after_ok {
                return true;
            }
            from = start + 1;
        }
    }
    false
}

/// `+value` (JS ToNumber on a string)
pub fn js_to_number(s: &str) -> f64 {
    let t = utils::js_trim(s);
    if t.is_empty() {
        return 0.0;
    }
    let (neg, body) = if let Some(r) = t.strip_prefix('-') {
        (true, r)
    } else if let Some(r) = t.strip_prefix('+') {
        (false, r)
    } else {
        (false, t)
    };
    let radix = |prefix: &str, radix: u32| -> Option<f64> {
        let digits = t.strip_prefix(prefix).or_else(|| t.strip_prefix(&prefix.to_uppercase()))?;
        if digits.is_empty() {
            return Some(f64::NAN);
        }
        let mut v = 0f64;
        for c in digits.chars() {
            v = v * radix as f64 + c.to_digit(radix)? as f64;
        }
        Some(v)
    };
    if let Some(v) = radix("0x", 16).or_else(|| radix("0o", 8)).or_else(|| radix("0b", 2)) {
        return v;
    }
    if t.starts_with("0x") || t.starts_with("0X") || t.starts_with("0o") || t.starts_with("0O") || t.starts_with("0b") || t.starts_with("0B") {
        return f64::NAN;
    }
    if body == "Infinity" {
        return if neg { f64::NEG_INFINITY } else { f64::INFINITY };
    }
    let valid = !body.is_empty()
        && body.chars().all(|c| c.is_ascii_digit() || matches!(c, '.' | 'e' | 'E' | '+' | '-'))
        && body.chars().next().is_some_and(|c| c.is_ascii_digit() || c == '.');
    if !valid {
        return f64::NAN;
    }
    match body.parse::<f64>() {
        Ok(v) => {
            if neg {
                -v
            } else {
                v
            }
        }
        Err(_) => f64::NAN,
    }
}
