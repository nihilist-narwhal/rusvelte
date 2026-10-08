//! Port of vscode-css-languageservice's `services/cssValidation.ts` + `services/lint.ts` with the
//! default lint settings (svelte-check never configures them). Rules whose default level is
//! `ignore` produce nothing and aren't ported.

use super::data::{AT_DIRECTIVES, PROPERTIES};
use super::nodes::{Ast, Class, Field, NodeId, NodeType};
use super::parser::{eq_str, node_text};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Level {
    Warning,
    Error,
}

/// A diagnostic in the stylesheet (UTF-16 offsets into the style content)
#[derive(Clone, Debug)]
pub struct Marker {
    pub code: &'static str,
    pub message: String,
    pub level: Level,
    pub offset: i32,
    pub length: i32,
}

/// The lint threw (a `NodesByRootMap` key that is an `Object.prototype` member); the language
/// server then reports no diagnostics for the file.
pub struct Crash;

/// Members of `Object.prototype`: `{}[key]` is truthy for these
const OBJECT_PROTOTYPE_KEYS: [&str; 12] = [
    "constructor",
    "__defineGetter__",
    "__defineSetter__",
    "hasOwnProperty",
    "__lookupGetter__",
    "__lookupSetter__",
    "isPrototypeOf",
    "propertyIsEnumerable",
    "toString",
    "valueOf",
    "__proto__",
    "toLocaleString",
];

/// `CSSDataManager.isKnownProperty`: `name.toLowerCase() in this._propertySet` (the `in`
/// operator also finds `Object.prototype` members)
fn property_status(name: &str) -> Option<bool> {
    let lower = name.to_lowercase();
    if let Ok(i) = PROPERTIES.binary_search_by(|(n, _)| (*n).cmp(lower.as_str())) {
        return Some(PROPERTIES[i].1);
    }
    if lower == "constructor" || lower == "__proto__" {
        // `this._propertySet[name].status` is undefined
        return Some(true);
    }
    None
}

fn is_known_property(name: &str) -> bool {
    property_status(name).is_some()
}

fn is_standard_property(name: &str) -> bool {
    property_status(name) == Some(true)
}

/// A plain JS object used as a map (`NodesByRootMap.data`): `for...in` yields integer-like keys
/// first (ascending), then the others in insertion order.
struct JsKeyedMap<V> {
    entries: Vec<(String, V)>,
}

impl<V> JsKeyedMap<V> {
    fn new() -> Self {
        JsKeyedMap { entries: Vec::new() }
    }

    fn get_or_insert(&mut self, key: &str, make: impl FnOnce() -> V) -> Result<&mut V, Crash> {
        if let Some(i) = self.entries.iter().position(|(k, _)| k == key) {
            return Ok(&mut self.entries[i].1);
        }
        if OBJECT_PROTOTYPE_KEYS.contains(&key) {
            // `entry = this.data[root]` is truthy, `entry.names.push` throws
            return Err(Crash);
        }
        self.entries.push((key.to_string(), make()));
        Ok(&mut self.entries.last_mut().unwrap().1)
    }

    fn in_for_in_order(&self) -> Vec<usize> {
        fn array_index(k: &str) -> Option<u32> {
            if k.is_empty() || (k.len() > 1 && k.starts_with('0')) || !k.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            k.parse::<u32>().ok().filter(|&n| n != u32::MAX)
        }
        let mut ints: Vec<(u32, usize)> =
            self.entries.iter().enumerate().filter_map(|(i, (k, _))| array_index(k).map(|n| (n, i))).collect();
        ints.sort();
        let mut order: Vec<usize> = ints.into_iter().map(|(_, i)| i).collect();
        order.extend(self.entries.iter().enumerate().filter(|(_, (k, _))| array_index(k).is_none()).map(|(i, _)| i));
        order
    }
}

struct NodesByRoot {
    names: Vec<String>,
    nodes: Vec<NodeId>,
}

struct LintVisitor<'a> {
    ast: &'a Ast,
    src: &'a [u16],
    warnings: Vec<Marker>,
    keyframes: JsKeyedMap<NodesByRoot>,
}

pub fn lint(ast: &Ast, src: &[u16], root: NodeId) -> Result<Vec<Marker>, Crash> {
    let mut v = LintVisitor { ast, src, warnings: Vec::new(), keyframes: JsKeyedMap::new() };
    v.accept(root)?;
    v.validate_keyframes();
    Ok(v.warnings)
}

impl<'a> LintVisitor<'a> {
    fn text(&self, node: NodeId) -> String {
        let n = self.ast.get(node);
        String::from_utf16_lossy(node_text(self.src, n.offset, n.length))
    }

    fn matches(&self, node: NodeId, s: &str) -> bool {
        let n = self.ast.get(node);
        n.length == s.encode_utf16().count() as i32 && eq_str(node_text(self.src, n.offset, n.length), s)
    }

    fn add_entry(&mut self, node: NodeId, code: &'static str, level: Level, message: String) {
        let n = self.ast.get(node);
        self.warnings.push(Marker { code, message, level, offset: n.offset, length: n.length });
    }

    fn accept(&mut self, node: NodeId) -> Result<(), Crash> {
        if self.visit_node(node)? {
            for &child in &self.ast.get(node).children {
                self.accept(child)?;
            }
        }
        Ok(())
    }

    fn visit_node(&mut self, node: NodeId) -> Result<bool, Crash> {
        Ok(match self.ast.ty(node) {
            NodeType::UnknownAtRule => self.visit_unknown_at_rule(node),
            NodeType::Keyframe => self.visit_keyframe(node)?,
            NodeType::FontFace => self.visit_font_face(node),
            NodeType::Ruleset => self.visit_rule_set(node)?,
            NodeType::HexColorValue => self.visit_hex_color_value(node),
            // visitSimpleSelector, visitFunction, visitNumericValue, visitImport, visitPrio and
            // visitIdentifierSelector only report rules that are ignored by default (and the
            // color function check never matches: `getName()` has no `(`)
            _ => true,
        })
    }

    fn visit_unknown_at_rule(&mut self, node: NodeId) -> bool {
        let Some(name) = self.ast.child(node, 0) else { return false };
        let text = self.text(name);
        if AT_DIRECTIVES.binary_search(&text.as_str()).is_ok() {
            return false;
        }
        self.add_entry(name, "unknownAtRules", Level::Warning, format!("Unknown at rule {text}"));
        true
    }

    fn visit_keyframe(&mut self, node: NodeId) -> Result<bool, Crash> {
        let Some(keyword) = self.ast.field(node, Field::Keyword) else { return Ok(false) };
        let text = self.text(keyword);
        let name = match self.ast.field(node, Field::Identifier) {
            Some(i) => self.text(i),
            None => String::new(),
        };
        let entry = self.keyframes.get_or_insert(&name, || NodesByRoot { names: Vec::new(), nodes: Vec::new() })?;
        entry.names.push(text.clone());
        if text != "@keyframes" {
            entry.nodes.push(keyword);
        }
        Ok(true)
    }

    fn validate_keyframes(&mut self) {
        let order = self.keyframes.in_for_in_order();
        let mut out = Vec::new();
        for i in order {
            let entry = &self.keyframes.entries[i].1;
            let needs_standard = !entry.names.iter().any(|n| n == "@keyframes");
            if needs_standard {
                out.extend(entry.nodes.iter().copied());
            }
        }
        for node in out {
            self.add_entry(
                node,
                "vendorPrefix",
                Level::Warning,
                "Always define standard rule '@keyframes' when defining keyframes.".to_string(),
            );
        }
    }

    /// `isCSSDeclaration`
    fn is_css_declaration(&self, node: NodeId) -> bool {
        if !self.ast.class(node).is_declaration() {
            return false;
        }
        if self.ast.field(node, Field::Value).is_none() {
            return false;
        }
        let Some(property) = self.ast.field(node, Field::Property) else { return false };
        let Some(identifier) = self.ast.field(property, Field::Identifier) else { return false };
        !self.ast.has_children(identifier)
    }

    /// `Property.getName()`: the text without trailing `_` / `+`
    fn property_name(&self, property: NodeId) -> String {
        let t = self.text(property);
        t.trim_end_matches(['_', '+']).to_string()
    }

    /// `Declaration.getFullPropertyName()`
    fn full_property_name(&self, decl: NodeId) -> String {
        let name = match self.ast.field(decl, Field::Property) {
            Some(p) => self.property_name(p),
            None => "unknown".to_string(),
        };
        if let Some(parent) = self.ast.get(decl).parent
            && self.ast.class(parent) == Class::Declarations
            && let Some(np) = self.ast.get_parent(parent)
            && self.ast.class(np) == Class::NestedProperties
            && let Some(parent_decl) = self.ast.get_parent(np)
            && self.ast.class(parent_decl).is_declaration()
        {
            return self.full_property_name(parent_decl) + &name;
        }
        name
    }

    fn non_prefixed_property_name(&self, decl: NodeId) -> String {
        let name = self.full_property_name(decl);
        if let Some(rest) = name.strip_prefix('-')
            && let Some(i) = rest.find('-')
        {
            return rest[i + 1..].to_string();
        }
        name
    }

    fn visit_font_face(&mut self, node: NodeId) -> bool {
        let Some(declarations) = self.ast.field(node, Field::Declarations) else { return false };
        let (mut defines_src, mut defines_font_family, mut contains_unknowns) = (false, false, false);
        for &decl in &self.ast.get(declarations).children {
            if self.is_css_declaration(decl) {
                let property = self.ast.field(decl, Field::Property).unwrap();
                let name = self.property_name(property).to_lowercase();
                if name == "src" {
                    defines_src = true;
                }
                if name == "font-family" {
                    defines_font_family = true;
                }
            } else {
                contains_unknowns = true;
            }
        }
        if !contains_unknowns && (!defines_src || !defines_font_family) {
            self.add_entry(
                node,
                "fontFaceProperties",
                Level::Warning,
                "@font-face rule must define 'src' and 'font-family' properties".to_string(),
            );
        }
        true
    }

    fn visit_hex_color_value(&mut self, node: NodeId) -> bool {
        let length = self.ast.get(node).length;
        if !matches!(length, 9 | 7 | 5 | 4) {
            self.add_entry(
                node,
                "hexColorLength",
                Level::Error,
                "Hex colors must consist of three, four, six or eight hex numbers".to_string(),
            );
        }
        false
    }

    /// `findValueInExpression`
    fn find_value_in_expression(&self, expression: NodeId, v: &str) -> bool {
        fn walk(this: &LintVisitor, node: NodeId, v: &str, found: &mut bool) {
            if this.ast.ty(node) == NodeType::Identifier && this.matches(node, v) {
                *found = true;
            }
            if !*found {
                for &c in &this.ast.get(node).children {
                    walk(this, c, v, found);
                }
            }
        }
        let mut found = false;
        walk(self, expression, v, &mut found);
        found
    }

    fn visit_rule_set(&mut self, node: NodeId) -> Result<bool, Crash> {
        let Some(declarations) = self.ast.field(node, Field::Declarations) else { return Ok(false) };
        let selectors = self.ast.field(node, Field::Selectors);
        if !self.ast.has_children(declarations) {
            // `node.getSelectors()` always exists for a RuleSet
            self.add_entry(selectors.unwrap(), "emptyRules", Level::Warning, "Do not use empty rulesets".to_string());
        }
        // (fullPropertyName, node)
        let property_table: Vec<(String, NodeId)> = self
            .ast
            .get(declarations)
            .children
            .iter()
            .filter(|&&c| self.ast.class(c).is_declaration())
            .map(|&c| (self.full_property_name(c).to_lowercase(), c))
            .collect();

        let has_display = |v: &str| {
            property_table.iter().any(|(name, decl)| {
                name == "display" && self.ast.field(*decl, Field::Value).is_some_and(|e| self.find_value_in_expression(e, v))
            })
        };
        let (display_inline_block, display_block) = (has_display("inline-block"), has_display("block"));
        if display_inline_block {
            for (name, decl) in &property_table {
                if name == "float"
                    && let Some(value) = self.ast.field(*decl, Field::Value)
                    && !self.matches(value, "none")
                {
                    self.add_entry(*decl, "propertyIgnoredDueToDisplay", Level::Warning, "inline-block is ignored due to the float. If 'float' has a value other than 'none', the box is floated and 'display' is treated as 'block'".to_string());
                }
            }
        }
        if display_block {
            for (name, decl) in &property_table {
                if name == "vertical-align" {
                    self.add_entry(*decl, "propertyIgnoredDueToDisplay", Level::Warning, "Property is ignored due to the display. With 'display: block', vertical-align should not be used.".to_string());
                }
            }
        }

        let is_export_block = selectors.is_some_and(|s| self.matches(s, ":export"));
        if !is_export_block {
            let mut properties_by_suffix: JsKeyedMap<NodesByRoot> = JsKeyedMap::new();
            let mut contains_unknowns = false;
            for (full, decl) in &property_table {
                let decl = *decl;
                if !self.is_css_declaration(decl) {
                    contains_unknowns = true;
                    continue;
                }
                let property = self.ast.field(decl, Field::Property).unwrap();
                let mut chars = full.chars();
                let first = chars.next();
                if first == Some('-') {
                    if chars.next() != Some('-') {
                        let non_prefixed = self.non_prefixed_property_name(decl);
                        let entry = properties_by_suffix
                            .get_or_insert(&non_prefixed, || NodesByRoot { names: Vec::new(), nodes: Vec::new() })?;
                        entry.names.push(full.clone());
                        entry.nodes.push(property);
                    }
                } else {
                    let mut name: &str = full;
                    if first == Some('*') || first == Some('_') {
                        name = &full[1..];
                    }
                    if !is_known_property(full) && !is_known_property(name) {
                        let message = format!("Unknown property: '{}'", self.full_property_name(decl));
                        self.add_entry(property, "unknownProperties", Level::Warning, message);
                    }
                    let entry =
                        properties_by_suffix.get_or_insert(name, || NodesByRoot { names: Vec::new(), nodes: Vec::new() })?;
                    entry.names.push(name.to_string());
                }
            }
            if !contains_unknowns {
                let mut pseudo_elements: Option<Vec<Vec<u16>>> = None;
                for i in properties_by_suffix.in_for_in_order() {
                    let (suffix, entry) = &properties_by_suffix.entries[i];
                    let needs_standard = is_standard_property(suffix) && !entry.names.iter().any(|n| n == suffix);
                    if !needs_standard {
                        continue;
                    }
                    let pseudo_elements =
                        pseudo_elements.get_or_insert_with(|| self.contextual_vendor_specific_pseudo_elements(node));
                    let suffix_len = suffix.encode_utf16().count();
                    let mut flagged = Vec::new();
                    for &prop in &entry.nodes {
                        let property_name: Vec<u16> = self.property_name(prop).encode_utf16().collect();
                        let prefix_len = property_name.len().saturating_sub(suffix_len);
                        let prefix = &property_name[..prefix_len];
                        if !pseudo_elements.iter().any(|x| x.starts_with(prefix)) {
                            flagged.push(prop);
                        }
                    }
                    let message = format!("Also define the standard property '{suffix}' for compatibility");
                    for prop in flagged {
                        self.add_entry(prop, "vendorPrefix", Level::Warning, message.clone());
                    }
                }
            }
        }
        Ok(true)
    }

    /// `getContextualVendorSpecificPseudoElements` (as UTF-16, for `startsWith`)
    fn contextual_vendor_specific_pseudo_elements(&self, node: NodeId) -> Vec<Vec<u16>> {
        fn walk_down(this: &LintVisitor, s: &mut Vec<Vec<u16>>, n: NodeId) {
            for &child in &this.ast.get(n).children {
                if this.ast.ty(child) == NodeType::PseudoSelector
                    && let Some(&first) = this.ast.get(child).children.first()
                {
                    let c = this.ast.get(first);
                    let text = node_text(this.src, c.offset, c.length).to_vec();
                    if !text.is_empty() && !s.contains(&text) {
                        s.push(text);
                    }
                }
                walk_down(this, s, child);
            }
        }
        let mut result = Vec::new();
        let mut cur = Some(node);
        while let Some(n) = cur {
            if self.ast.ty(n) == NodeType::Ruleset
                && let Some(selectors) = self.ast.field(n, Field::Selectors)
            {
                for &selector in &self.ast.get(selectors).children {
                    walk_down(self, &mut result, selector);
                }
            }
            cur = self.ast.get(n).parent;
        }
        result
    }
}
