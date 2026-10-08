//! `client/transform-template/*`: the `Template` class (the HTML/tree of a fragment) and
//! `transform_template`, which hoists it as `$.from_html(...)` & co.

use crate::estree::builders as b;
use crate::estree::{LiteralValue, Node, NodeKind};

use super::{Client, State, TEMPLATE_USE_MATHML, TEMPLATE_USE_SVG};

/// A text node of the template: `data` and `raw`
#[derive(Clone, Debug)]
pub struct TextPart {
    pub data: String,
    pub raw: String,
}

#[derive(Clone, Debug)]
pub enum Item {
    Element {
        name: String,
        /// in JS object key order (see [`ordered_keys`])
        attributes: Vec<(String, Option<String>)>,
        children: Vec<usize>,
        is_html: bool,
        start: usize,
    },
    Text(Vec<TextPart>),
    Comment(Option<String>),
}

/// `new Template()`
#[derive(Default, Debug)]
pub struct Template {
    pub contains_script_tag: bool,
    pub needs_import_node: bool,
    items: Vec<Item>,
    /// the top-level nodes
    pub nodes: Vec<usize>,
    /// the open elements
    stack: Vec<usize>,
    /// `#element`: the last element pushed
    element: Option<usize>,
}

impl Template {
    fn fragment(&mut self) -> &mut Vec<usize> {
        match self.stack.last() {
            Some(&e) => match &mut self.items[e] {
                Item::Element { children, .. } => children,
                _ => unreachable!(),
            },
            None => &mut self.nodes,
        }
    }

    fn push_item(&mut self, item: Item) -> usize {
        self.items.push(item);
        let i = self.items.len() - 1;
        self.fragment().push(i);
        i
    }

    pub fn push_element(&mut self, name: &str, start: usize, is_html: bool) {
        let i = self.push_item(Item::Element { name: name.to_string(), attributes: Vec::new(), children: Vec::new(), is_html, start });
        self.element = Some(i);
        self.stack.push(i);
    }

    pub fn push_comment(&mut self, data: Option<String>) {
        self.push_item(Item::Comment(data));
    }

    pub fn push_text(&mut self, nodes: Vec<TextPart>) {
        self.push_item(Item::Text(nodes));
    }

    pub fn pop_element(&mut self) {
        self.stack.pop();
    }

    pub fn set_prop(&mut self, key: &str, value: Option<String>) {
        let Some(e) = self.element else { return };
        if let Item::Element { attributes, .. } = &mut self.items[e] {
            match attributes.iter_mut().find(|(k, _)| k == key) {
                Some(entry) => entry.1 = value,
                None => attributes.push((key.to_string(), value)),
            }
        }
    }

    /// Whether the template is a lone comment (`$.comment` creates it more cheaply)
    pub fn is_lone_anchor(&self) -> bool {
        self.nodes.len() == 1 && matches!(self.items[self.nodes[0]], Item::Comment(_))
    }

    pub fn as_html(&self) -> Node {
        let mut s = String::new();
        for &n in &self.nodes {
            self.stringify(n, &mut s);
        }
        b::template(vec![b::quasi_with(&s, true)], vec![])
    }

    pub fn as_tree(&mut self) -> Node {
        if let Some(&first) = self.nodes.first() {
            if matches!(self.items[first], Item::Comment(_)) {
                self.items.push(Item::Comment(None));
                let i = self.items.len() - 1;
                self.nodes.insert(0, i);
            }
        }
        b::array(self.nodes.iter().map(|&n| self.objectify(n)).collect::<Vec<_>>())
    }

    fn stringify(&self, item: usize, out: &mut String) {
        match &self.items[item] {
            Item::Text(nodes) => {
                for n in nodes {
                    out.push_str(&n.raw);
                }
            }
            Item::Comment(data) => match data.as_deref() {
                Some(d) if !d.is_empty() => {
                    out.push_str("<!--");
                    out.push_str(d);
                    out.push_str("-->");
                }
                _ => out.push_str("<!>"),
            },
            Item::Element { name, attributes, children, is_html, .. } => {
                out.push('<');
                out.push_str(name);
                for (key, value) in ordered_keys(attributes) {
                    out.push(' ');
                    if *is_html {
                        out.push_str(&key.to_lowercase());
                    } else {
                        out.push_str(key);
                    }
                    if let Some(v) = value {
                        out.push_str("=\"");
                        out.push_str(&escape_html(v, true));
                        out.push('"');
                    }
                }
                if crate::analyze::utils::is_void(name) {
                    out.push_str("/>");
                } else {
                    out.push('>');
                    for &c in children {
                        self.stringify(c, out);
                    }
                    out.push_str("</");
                    out.push_str(name);
                    out.push('>');
                }
            }
        }
    }

    /// `objectify(item)`: `None` is the JS's `null` (an array hole)
    fn objectify(&self, item: usize) -> Option<Node> {
        match &self.items[item] {
            Item::Text(nodes) => Some(b::literal(nodes.iter().map(|n| n.data.as_str()).collect::<String>())),
            Item::Comment(data) => match data.as_deref() {
                Some(d) if !d.is_empty() => Some(b::array(vec![b::literal(format!("// {d}"))])),
                _ => None,
            },
            Item::Element { name, attributes, children, .. } => {
                let mut elements: Vec<Option<Node>> = vec![Some(b::literal(name.as_str()))];
                let mut props = Vec::new();
                for (key, value) in ordered_keys(attributes) {
                    props.push(b::prop(
                        "init",
                        b::key(&fix_attribute_casing(key)),
                        match value {
                            Some(v) => b::literal(v.as_str()),
                            None => b::void0(),
                        },
                    ));
                }
                if !props.is_empty() || !children.is_empty() {
                    elements.push(Some(if props.is_empty() { b::null() } else { b::object(props) }));
                }
                if !children.is_empty() {
                    let mut kids: Vec<Option<Node>> = children.iter().map(|&c| self.objectify(c)).collect();
                    if name == "pre" || name == "textarea" {
                        if let Some(Some(first)) = kids.first_mut() {
                            if let NodeKind::Literal(l) = &mut first.kind {
                                if let LiteralValue::String(s) = &l.value {
                                    let stripped = s.strip_prefix("\r\n").or_else(|| s.strip_prefix('\n')).unwrap_or(s);
                                    l.value = LiteralValue::String(stripped.into());
                                }
                            }
                        }
                    }
                    elements.extend(kids);
                }
                Some(b::array(elements))
            }
        }
    }

    /// `(start, children)` of the elements, for `build_locations`
    fn locations(&self, nodes: &[usize], c: &Client) -> Node {
        let mut out = Vec::new();
        for &n in nodes {
            if let Item::Element { start, children, .. } = &self.items[n] {
                let (line, column) = c.locate(*start);
                let mut expression = vec![b::literal(line as f64), b::literal(column as f64)];
                let kids = self.locations(children, c);
                if matches!(&kids.kind, NodeKind::ArrayExpression(a) if !a.elements.is_empty()) {
                    expression.push(kids);
                }
                out.push(b::array(expression));
            }
        }
        b::array(out)
    }
}

/// The keys of a JS object in iteration order: array indices first (ascending), then the
/// other keys in insertion order
fn ordered_keys(attributes: &[(String, Option<String>)]) -> Vec<(&String, &Option<String>)> {
    let index = |k: &str| -> Option<u32> {
        if k == "0" {
            return Some(0);
        }
        if k.starts_with('0') || k.is_empty() || !k.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        k.parse::<u32>().ok().filter(|&n| n != u32::MAX)
    };
    let mut indices: Vec<(u32, &String, &Option<String>)> = Vec::new();
    let mut rest = Vec::new();
    for (k, v) in attributes {
        match index(k) {
            Some(i) => indices.push((i, k, v)),
            None => rest.push((k, v)),
        }
    }
    indices.sort_by_key(|x| x.0);
    let mut out: Vec<(&String, &Option<String>)> = indices.into_iter().map(|(_, k, v)| (k, v)).collect();
    out.extend(rest);
    out
}

/// `escape_html(value, is_attr)`
pub fn escape_html(s: &str, is_attr: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '"' if is_attr => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
    out
}

const SVG_ATTRIBUTES: &str = "accent-height accumulate additive alignment-baseline allowReorder alphabetic amplitude arabic-form ascent attributeName attributeType autoReverse azimuth baseFrequency baseline-shift baseProfile bbox begin bias by calcMode cap-height class clip clipPathUnits clip-path clip-rule color color-interpolation color-interpolation-filters color-profile color-rendering contentScriptType contentStyleType cursor cx cy d decelerate descent diffuseConstant direction display divisor dominant-baseline dur dx dy edgeMode elevation enable-background end exponent externalResourcesRequired fill fill-opacity fill-rule filter filterRes filterUnits flood-color flood-opacity font-family font-size font-size-adjust font-stretch font-style font-variant font-weight format from fr fx fy g1 g2 glyph-name glyph-orientation-horizontal glyph-orientation-vertical glyphRef gradientTransform gradientUnits hanging height href horiz-adv-x horiz-origin-x id ideographic image-rendering in in2 intercept k k1 k2 k3 k4 kernelMatrix kernelUnitLength kerning keyPoints keySplines keyTimes lang lengthAdjust letter-spacing lighting-color limitingConeAngle local marker-end marker-mid marker-start markerHeight markerUnits markerWidth mask maskContentUnits maskUnits mathematical max media method min mode name numOctaves offset onabort onactivate onbegin onclick onend onerror onfocusin onfocusout onload onmousedown onmousemove onmouseout onmouseover onmouseup onrepeat onresize onscroll onunload opacity operator order orient orientation origin overflow overline-position overline-thickness panose-1 paint-order pathLength patternContentUnits patternTransform patternUnits pointer-events points pointsAtX pointsAtY pointsAtZ preserveAlpha preserveAspectRatio primitiveUnits r radius refX refY rendering-intent repeatCount repeatDur requiredExtensions requiredFeatures restart result rotate rx ry scale seed shape-rendering slope spacing specularConstant specularExponent speed spreadMethod startOffset stdDeviation stemh stemv stitchTiles stop-color stop-opacity strikethrough-position strikethrough-thickness string stroke stroke-dasharray stroke-dashoffset stroke-linecap stroke-linejoin stroke-miterlimit stroke-opacity stroke-width style surfaceScale systemLanguage tabindex tableValues target targetX targetY text-anchor text-decoration text-rendering textLength to transform type u1 u2 underline-position underline-thickness unicode unicode-bidi unicode-range units-per-em v-alphabetic v-hanging v-ideographic v-mathematical values version vert-adv-y vert-origin-x vert-origin-y viewBox viewTarget visibility width widths word-spacing writing-mode x x-height x1 x2 xChannelSelector xlink:actuate xlink:arcrole xlink:href xlink:role xlink:show xlink:title xlink:type xml:base xml:lang xml:space y y1 y2 yChannelSelector z zoomAndPan";

/// `fix_attribute_casing(name)`
pub fn fix_attribute_casing(name: &str) -> String {
    let lower = name.to_lowercase();
    SVG_ATTRIBUTES.split(' ').find(|a| a.to_lowercase() == lower).map(str::to_string).unwrap_or(lower)
}

impl<'a, 's> Client<'a, 's> {
    /// `transform_template(state, name, flags)`: the identifier of the hoisted template
    pub fn transform_template(&mut self, st: &State, name: &str, flags: u32) -> Node {
        let namespace = st.namespace;
        let tree = self.options.fragments_tree;
        if st.template.borrow().is_lone_anchor() {
            return b::id("$.comment");
        }
        let expression = if tree { st.template.borrow_mut().as_tree() } else { st.template.borrow().as_html() };
        let key = if tree || self.dev {
            None
        } else {
            let raw = match &expression.kind {
                NodeKind::TemplateLiteral(t) => match &t.quasis[0].kind {
                    NodeKind::TemplateElement(e) => e.raw.to_string(),
                    _ => String::new(),
                },
                _ => String::new(),
            };
            Some(format!("{namespace} {flags} {raw}"))
        };
        if let Some(k) = &key {
            if let Some(existing) = self.templates.get(k) {
                return b::id(existing.as_str());
            }
        }
        let mut flags = flags;
        if tree {
            if namespace == "svg" {
                flags |= TEMPLATE_USE_SVG;
            }
            if namespace == "mathml" {
                flags |= TEMPLATE_USE_MATHML;
            }
        }
        let callee = if tree { "$.from_tree".to_string() } else { format!("$.from_{namespace}") };
        let mut call = b::call(callee.as_str(), vec![Some(expression), if flags != 0 { Some(b::literal(flags as f64)) } else { None }]);
        if st.template.borrow().contains_script_tag {
            call = b::call("$.with_script", vec![call]);
        }
        if self.dev {
            let template = st.template.borrow();
            let nodes = template.nodes.clone();
            let locations = template.locations(&nodes, self);
            call = b::call(
                "$.add_locations",
                vec![call, b::member_with(b::id(self.an.name.as_str()), b::id("$.FILENAME"), true, false), locations],
            );
        }
        let id = self.an.sc.unique(name);
        self.hoisted.push(b::var(b::id(id.as_str()), call));
        if let Some(k) = key {
            self.templates.insert(k, id.clone());
        }
        b::id(id.as_str())
    }
}
