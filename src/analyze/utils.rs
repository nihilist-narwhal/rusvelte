//! Small helpers from `svelte/src/utils.js`, `compiler/utils/*`, `html-tree-validation.js`,
//! `phases/bindings.js` and `phases/patterns.js`.

use crate::ast::{Attr, AttrValue, Chunk, Expr};

const RUNES: &[&str] = &[
    "$state",
    "$state.raw",
    "$derived",
    "$derived.by",
    "$state.eager",
    "$state.snapshot",
    "$props",
    "$props.id",
    "$bindable",
    "$effect",
    "$effect.pre",
    "$effect.tracking",
    "$effect.root",
    "$effect.pending",
    "$inspect",
    "$inspect().with",
    "$inspect.trace",
    "$host",
];

/// `is_rune(name)`, returning the static name
pub fn is_rune(name: &str) -> Option<&'static str> {
    if !name.starts_with('$') {
        return None;
    }
    RUNES.iter().find(|r| **r == name).copied()
}

pub fn is_state_creation_rune(name: &str) -> bool {
    matches!(name, "$state" | "$state.raw" | "$derived" | "$derived.by")
}

const RESERVED_WORDS: &[&str] = &[
    "arguments", "await", "break", "case", "catch", "class", "const", "continue", "debugger", "default", "delete", "do",
    "else", "enum", "eval", "export", "extends", "false", "finally", "for", "function", "if", "implements", "import",
    "in", "instanceof", "interface", "let", "new", "null", "package", "private", "protected", "public", "return",
    "static", "super", "switch", "this", "throw", "true", "try", "typeof", "var", "void", "while", "with", "yield",
];

pub fn is_reserved(word: &str) -> bool {
    RESERVED_WORDS.contains(&word)
}

const VOID_ELEMENT_NAMES: &[&str] = &[
    "area", "base", "br", "col", "command", "embed", "hr", "img", "input", "keygen", "link", "meta", "param", "source",
    "track", "wbr",
];

pub fn is_void(name: &str) -> bool {
    VOID_ELEMENT_NAMES.contains(&name) || name.eq_ignore_ascii_case("!doctype")
}

const SVG_ELEMENTS: &[&str] = &[
    "altGlyph", "altGlyphDef", "altGlyphItem", "animate", "animateColor", "animateMotion", "animateTransform", "circle",
    "clipPath", "color-profile", "cursor", "defs", "desc", "discard", "ellipse", "feBlend", "feColorMatrix",
    "feComponentTransfer", "feComposite", "feConvolveMatrix", "feDiffuseLighting", "feDisplacementMap",
    "feDistantLight", "feDropShadow", "feFlood", "feFuncA", "feFuncB", "feFuncG", "feFuncR", "feGaussianBlur",
    "feImage", "feMerge", "feMergeNode", "feMorphology", "feOffset", "fePointLight", "feSpecularLighting",
    "feSpotLight", "feTile", "feTurbulence", "filter", "font", "font-face", "font-face-format", "font-face-name",
    "font-face-src", "font-face-uri", "foreignObject", "g", "glyph", "glyphRef", "hatch", "hatchpath", "hkern", "image",
    "line", "linearGradient", "marker", "mask", "mesh", "meshgradient", "meshpatch", "meshrow", "metadata",
    "missing-glyph", "mpath", "path", "pattern", "polygon", "polyline", "radialGradient", "rect", "set", "solidcolor",
    "stop", "svg", "switch", "symbol", "text", "textPath", "tref", "tspan", "unknown", "use", "view", "vkern",
];

pub fn is_svg(name: &str) -> bool {
    SVG_ELEMENTS.contains(&name)
}

const MATHML_ELEMENTS: &[&str] = &[
    "annotation", "annotation-xml", "maction", "math", "merror", "mfrac", "mi", "mmultiscripts", "mn", "mo", "mover",
    "mpadded", "mphantom", "mprescripts", "mroot", "mrow", "ms", "mspace", "msqrt", "mstyle", "msub", "msubsup", "msup",
    "mtable", "mtd", "mtext", "mtr", "munder", "munderover", "semantics",
];

pub fn is_mathml(name: &str) -> bool {
    MATHML_ELEMENTS.contains(&name)
}

pub fn is_content_editable_binding(name: &str) -> bool {
    matches!(name, "textContent" | "innerHTML" | "innerText")
}

// ---------------------------------------------------------------------------------------
// attributes

/// `is_text_attribute(attribute)`: the text of a value that is a single Text chunk
pub fn text_value<'a>(value: &'a AttrValue<'a>) -> Option<&'a str> {
    match value {
        AttrValue::Sequence(chunks) if chunks.len() == 1 => match &chunks[0] {
            Chunk::Text { data, .. } => Some(data),
            _ => None,
        },
        _ => None,
    }
}

/// `is_expression_attribute(attribute)`
pub fn is_expression_value(value: &AttrValue) -> bool {
    match value {
        AttrValue::True => false,
        AttrValue::Expression(_) => true,
        AttrValue::Sequence(chunks) => chunks.len() == 1 && matches!(chunks[0], Chunk::Expression { .. }),
    }
}

/// `get_attribute_expression(attribute)` (for expression attributes)
pub fn value_expression<'a>(value: &'a AttrValue<'a>) -> Option<&'a Expr<'a>> {
    match value {
        AttrValue::Expression(c) => match &**c {
            Chunk::Expression { expression, .. } => Some(expression),
            _ => None,
        },
        AttrValue::Sequence(chunks) if chunks.len() == 1 => match &chunks[0] {
            Chunk::Expression { expression, .. } => Some(expression),
            _ => None,
        },
        _ => None,
    }
}

/// `get_attribute_chunks(value)`
pub fn chunks<'a>(value: &'a AttrValue<'a>) -> &'a [Chunk<'a>] {
    match value {
        AttrValue::True => &[],
        AttrValue::Expression(c) => std::slice::from_ref(&**c),
        AttrValue::Sequence(chunks) => chunks,
    }
}

/// `is_event_attribute(attribute)`
pub fn is_event_attribute(a: &Attr) -> bool {
    match a {
        Attr::Attribute { name, value, .. } => is_expression_value(value) && name.starts_with("on"),
        _ => false,
    }
}

/// `list(strings, conjunction)`
pub fn list(strings: &[String], conjunction: &str) -> String {
    match strings.len() {
        1 => strings[0].clone(),
        2 => format!("{} {} {}", strings[0], conjunction, strings[1]),
        n => format!("{} {} {}", strings[..n - 1].join(", "), conjunction, strings[n - 1]),
    }
}

/// JS `\s`
pub fn is_js_whitespace(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'..='\u{200a}'
            | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}'
    )
}

/// `str.trim()`
pub fn js_trim(s: &str) -> &str {
    s.trim_matches(is_js_whitespace)
}

/// `regex_not_whitespace.test(s)`: `/[^ \t\r\n]/`
pub fn has_non_whitespace(s: &str) -> bool {
    s.bytes().any(|b| !matches!(b, b' ' | b'\t' | b'\r' | b'\n'))
}

/// `str.split(/\s+/)` (JS semantics: empty strings at the ends are kept)
pub fn split_whitespace_js(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut chars = s.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if is_js_whitespace(c) {
            let mut end = i + c.len_utf8();
            while let Some(&(j, d)) = chars.peek() {
                if !is_js_whitespace(d) {
                    break;
                }
                end = j + d.len_utf8();
                chars.next();
            }
            out.push(&s[start..i]);
            start = end;
        }
    }
    out.push(&s[start..]);
    out
}

// ---------------------------------------------------------------------------------------
// fuzzymatch

fn utf16(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

fn levenshtein(a: &[u16], b: &[u16]) -> usize {
    let mut current: Vec<usize> = vec![0; a.len() + 1];
    let mut prev = 0;
    for i in 0..=b.len() {
        for j in 0..=a.len() {
            let value = if i > 0 && j > 0 {
                if a[j - 1] == b[i - 1] { prev } else { current[j].min(current[j - 1]).min(prev) + 1 }
            } else {
                i + j
            };
            prev = current[j];
            current[j] = value;
        }
    }
    current[a.len()]
}

fn distance(a: &str, b: &str) -> f64 {
    let (a, b) = (utf16(a), utf16(b));
    let d = levenshtein(&a, &b);
    1.0 - d as f64 / a.len().max(b.len()) as f64
}

/// `'-' + value.toLowerCase().replace(/[^\w, ]+/, '') + '-'`, split into grams
fn grams(value: &str, gram_size: usize) -> Vec<Vec<u16>> {
    let lower = utf16(&value.to_lowercase());
    // remove the first run of characters that aren't word characters, commas or spaces
    let is_kept = |c: u16| {
        (c >= b'a' as u16 && c <= b'z' as u16)
            || (c >= b'A' as u16 && c <= b'Z' as u16)
            || (c >= b'0' as u16 && c <= b'9' as u16)
            || c == b'_' as u16
            || c == b',' as u16
            || c == b' ' as u16
    };
    let mut simplified = vec![b'-' as u16];
    let mut i = 0;
    let mut removed = false;
    while i < lower.len() {
        if !removed && !is_kept(lower[i]) {
            while i < lower.len() && !is_kept(lower[i]) {
                i += 1;
            }
            removed = true;
            continue;
        }
        simplified.push(lower[i]);
        i += 1;
    }
    simplified.push(b'-' as u16);
    let mut out = Vec::new();
    if simplified.len() >= gram_size {
        for i in 0..simplified.len() - gram_size + 1 {
            out.push(simplified[i..i + gram_size].to_vec());
        }
    }
    out
}

/// key=gram, value=occurrences, in insertion order
fn gram_counter(value: &str, gram_size: usize) -> Vec<(Vec<u16>, usize)> {
    let mut result: Vec<(Vec<u16>, usize)> = Vec::new();
    for g in grams(value, gram_size) {
        match result.iter_mut().find(|(k, _)| *k == g) {
            Some(entry) => entry.1 += 1,
            None => result.push((g, 1)),
        }
    }
    result
}

struct FuzzySet {
    exact_set: Vec<(String, String)>,
    exact_index: rustc_hash::FxHashMap<String, usize>,
    match_dict: [Vec<(Vec<u16>, Vec<(usize, usize)>)>; 4],
    dict_index: [rustc_hash::FxHashMap<Vec<u16>, usize>; 4],
    items: [Vec<(f64, String)>; 4],
}

impl FuzzySet {
    fn new(arr: &[&str]) -> Self {
        let mut set = FuzzySet {
            exact_set: Vec::new(),
            exact_index: Default::default(),
            match_dict: Default::default(),
            dict_index: Default::default(),
            items: Default::default(),
        };
        for v in arr {
            set.add(v);
        }
        set
    }

    fn exact(&self, normalized: &str) -> Option<&str> {
        self.exact_index.get(normalized).map(|&i| self.exact_set[i].1.as_str())
    }

    fn add(&mut self, value: &str) {
        let normalized = value.to_lowercase();
        if self.exact(&normalized).is_some() {
            return;
        }
        for gram_size in 2..=3 {
            let index = self.items[gram_size].len();
            let counts = gram_counter(&normalized, gram_size);
            let mut sum = 0.0;
            for (gram, count) in counts {
                sum += (count * count) as f64;
                match self.dict_index[gram_size].get(&gram) {
                    Some(&i) => self.match_dict[gram_size][i].1.push((index, count)),
                    None => {
                        self.dict_index[gram_size].insert(gram.clone(), self.match_dict[gram_size].len());
                        self.match_dict[gram_size].push((gram, vec![(index, count)]));
                    }
                }
            }
            self.items[gram_size].push((sum.sqrt(), normalized.clone()));
            match self.exact_index.get(&normalized) {
                Some(&i) => self.exact_set[i].1 = value.to_string(),
                None => {
                    self.exact_index.insert(normalized.clone(), self.exact_set.len());
                    self.exact_set.push((normalized.clone(), value.to_string()));
                }
            }
        }
    }

    fn get(&self, value: &str) -> Option<Vec<(f64, String)>> {
        let normalized = value.to_lowercase();
        if let Some(result) = self.exact(&normalized) {
            if !result.is_empty() {
                return Some(vec![(1.0, result.to_string())]);
            }
        }
        for gram_size in (2..=3).rev() {
            let results = self.get_n(value, gram_size);
            if !results.is_empty() {
                return Some(results);
            }
        }
        None
    }

    fn get_n(&self, value: &str, gram_size: usize) -> Vec<(f64, String)> {
        let normalized = value.to_lowercase();
        // integer keys: iterated in ascending order
        let mut matches: std::collections::BTreeMap<usize, usize> = Default::default();
        let counts = gram_counter(&normalized, gram_size);
        let items = &self.items[gram_size];
        let mut sum = 0.0;
        for (gram, count) in counts {
            sum += (count * count) as f64;
            if let Some(&i) = self.dict_index[gram_size].get(&gram) {
                for &(index, other) in &self.match_dict[gram_size][i].1 {
                    *matches.entry(index).or_insert(0) += count * other;
                }
            }
        }
        let vector_normal = sum.sqrt();
        let mut results: Vec<(f64, String)> = matches
            .iter()
            .map(|(&index, &score)| (score as f64 / (vector_normal * items[index].0), items[index].1.clone()))
            .collect();
        results.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        let mut new_results: Vec<(f64, String)> =
            results.iter().take(50).map(|(_, s)| (distance(s, &normalized), s.clone())).collect();
        new_results.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        let mut out = Vec::new();
        if let Some(best) = new_results.first().map(|r| r.0) {
            for (score, s) in &new_results {
                if *score == best {
                    out.push((*score, self.exact(s).unwrap_or("").to_string()));
                }
            }
        }
        out
    }
}

/// `fuzzymatch(name, names)`. The sets are cached per list of names (always static here).
pub fn fuzzymatch(name: &str, names: &[&str]) -> Option<String> {
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<Vec<(usize, usize, &'static FuzzySet)>>> = OnceLock::new();
    if names.is_empty() {
        return None;
    }
    let key = (names.as_ptr() as usize, names.len());
    let cache = CACHE.get_or_init(Default::default);
    let set: &'static FuzzySet = {
        let mut cache = cache.lock().unwrap();
        match cache.iter().find(|(p, l, _)| (*p, *l) == key) {
            Some(&(_, _, set)) => set,
            None => {
                let set: &'static FuzzySet = Box::leak(Box::new(FuzzySet::new(names)));
                cache.push((key.0, key.1, set));
                set
            }
        }
    };
    let matches = set.get(name)?;
    if matches[0].0 > 0.7 { Some(matches[0].1.clone()) } else { None }
}

// ---------------------------------------------------------------------------------------
// html-tree-validation

enum Disallowed {
    Direct(&'static [&'static str]),
    Descendant(&'static [&'static str], Option<&'static [&'static str]>),
    Only(&'static [&'static str]),
}

const HEADINGS: &[&str] = &["h1", "h2", "h3", "h4", "h5", "h6"];

fn disallowed_children(tag: &str) -> Option<Disallowed> {
    use Disallowed::*;
    Some(match tag {
        "li" => Direct(&["li"]),
        "dt" | "dd" => Descendant(&["dt", "dd"], Some(&["dl"])),
        "p" => Descendant(
            &[
                "address", "article", "aside", "blockquote", "div", "dl", "fieldset", "footer", "form", "h1", "h2", "h3",
                "h4", "h5", "h6", "header", "hgroup", "hr", "main", "menu", "nav", "ol", "p", "pre", "section", "table",
                "ul",
            ],
            None,
        ),
        "rt" | "rp" => Descendant(&["rt", "rp"], None),
        "optgroup" => Descendant(&["optgroup"], None),
        "option" => Descendant(&["option", "optgroup"], None),
        "td" | "th" => Direct(&["td", "th", "tr"]),
        "form" => Descendant(&["form"], None),
        "a" => Descendant(&["a"], None),
        "button" => Descendant(&["button"], None),
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => Descendant(HEADINGS, None),
        "tr" => Only(&["th", "td", "style", "script", "template"]),
        "tbody" | "thead" | "tfoot" => Only(&["tr", "style", "script", "template"]),
        "colgroup" => Only(&["col", "template"]),
        "table" => Only(&["caption", "colgroup", "tbody", "thead", "tfoot", "style", "script", "template"]),
        "head" => Only(&[
            "base", "basefont", "bgsound", "link", "meta", "title", "noscript", "noframes", "style", "script", "template",
        ]),
        "html" => Only(&["head", "body", "frameset"]),
        "frameset" => Only(&["frame"]),
        "#document" => Only(&["html"]),
        _ => return None,
    })
}

pub fn is_tag_valid_with_ancestor(child_tag: &str, ancestors: &[&str]) -> Option<String> {
    if child_tag.contains('-') {
        return None;
    }
    let ancestor_tag = *ancestors.last()?;
    let disallowed = disallowed_children(ancestor_tag)?;
    if let Disallowed::Descendant(list, reset_by) = disallowed {
        if let Some(reset_by) = reset_by {
            for ancestor in ancestors[..ancestors.len() - 1].iter().rev() {
                if ancestor.contains('-') {
                    return None;
                }
                if reset_by.contains(ancestor) {
                    return None;
                }
            }
        }
        if list.contains(&child_tag) {
            return Some(format!("`<{child_tag}>` cannot be a descendant of `<{ancestor_tag}>`"));
        }
    }
    None
}

pub fn is_tag_valid_with_parent(child_tag: &str, parent_tag: &str) -> Option<String> {
    if child_tag.contains('-') || parent_tag.contains('-') {
        return None;
    }
    if parent_tag == "template" {
        return None;
    }
    let child = || format!("`<{child_tag}>`");
    let parent = || format!("`<{parent_tag}>`");
    match disallowed_children(parent_tag) {
        Some(Disallowed::Direct(list)) if list.contains(&child_tag) => {
            return Some(format!("{} cannot be a direct child of {}", child(), parent()));
        }
        Some(Disallowed::Descendant(list, _)) if list.contains(&child_tag) => {
            return Some(format!("{} cannot be a child of {}", child(), parent()));
        }
        Some(Disallowed::Only(list)) => {
            if list.contains(&child_tag) {
                return None;
            }
            let only: Vec<String> = list.iter().map(|d| format!("`<{d}>`")).collect();
            return Some(format!(
                "{} cannot be a child of {}. `<{parent_tag}>` only allows these children: {}",
                child(),
                parent(),
                only.join(", ")
            ));
        }
        _ => {}
    }
    match child_tag {
        "body" | "caption" | "col" | "colgroup" | "frameset" | "frame" | "head" | "html" => {
            Some(format!("{} cannot be a child of {}", child(), parent()))
        }
        "thead" | "tbody" | "tfoot" => Some(format!("{} must be the child of a `<table>`, not a {}", child(), parent())),
        "td" | "th" => Some(format!("{} must be the child of a `<tr>`, not a {}", child(), parent())),
        "tr" => Some(format!("`<tr>` must be the child of a `<thead>`, `<tbody>`, or `<tfoot>`, not a {}", parent())),
        _ => None,
    }
}

// ---------------------------------------------------------------------------------------
// bindings.js

pub struct BindingProperty {
    pub name: &'static str,
    pub valid_elements: Option<&'static [&'static str]>,
    pub invalid_elements: Option<&'static [&'static str]>,
}

const MEDIA: &[&str] = &["audio", "video"];
const NOT_WINDOW_DOCUMENT: &[&str] = &["svelte:window", "svelte:document"];

macro_rules! bp {
    ($name:literal) => {
        BindingProperty { name: $name, valid_elements: None, invalid_elements: None }
    };
    ($name:literal, valid $v:expr) => {
        BindingProperty { name: $name, valid_elements: Some($v), invalid_elements: None }
    };
    ($name:literal, invalid $v:expr) => {
        BindingProperty { name: $name, valid_elements: None, invalid_elements: Some($v) }
    };
}

pub static BINDING_PROPERTIES: &[BindingProperty] = &[
    bp!("currentTime", valid MEDIA),
    bp!("duration", valid MEDIA),
    bp!("focused"),
    bp!("paused", valid MEDIA),
    bp!("buffered", valid MEDIA),
    bp!("seekable", valid MEDIA),
    bp!("played", valid MEDIA),
    bp!("volume", valid MEDIA),
    bp!("muted", valid MEDIA),
    bp!("playbackRate", valid MEDIA),
    bp!("seeking", valid MEDIA),
    bp!("ended", valid MEDIA),
    bp!("readyState", valid MEDIA),
    bp!("videoHeight", valid &["video"]),
    bp!("videoWidth", valid &["video"]),
    bp!("naturalWidth", valid &["img"]),
    bp!("naturalHeight", valid &["img"]),
    bp!("activeElement", valid &["svelte:document"]),
    bp!("fullscreenElement", valid &["svelte:document"]),
    bp!("pointerLockElement", valid &["svelte:document"]),
    bp!("visibilityState", valid &["svelte:document"]),
    bp!("innerWidth", valid &["svelte:window"]),
    bp!("innerHeight", valid &["svelte:window"]),
    bp!("outerWidth", valid &["svelte:window"]),
    bp!("outerHeight", valid &["svelte:window"]),
    bp!("scrollX", valid &["svelte:window"]),
    bp!("scrollY", valid &["svelte:window"]),
    bp!("online", valid &["svelte:window"]),
    bp!("devicePixelRatio", valid &["svelte:window"]),
    bp!("clientWidth", invalid NOT_WINDOW_DOCUMENT),
    bp!("clientHeight", invalid NOT_WINDOW_DOCUMENT),
    bp!("offsetWidth", invalid NOT_WINDOW_DOCUMENT),
    bp!("offsetHeight", invalid NOT_WINDOW_DOCUMENT),
    bp!("contentRect", invalid NOT_WINDOW_DOCUMENT),
    bp!("contentBoxSize", invalid NOT_WINDOW_DOCUMENT),
    bp!("borderBoxSize", invalid NOT_WINDOW_DOCUMENT),
    bp!("devicePixelContentBoxSize", invalid NOT_WINDOW_DOCUMENT),
    bp!("indeterminate", valid &["input"]),
    bp!("checked", valid &["input"]),
    bp!("group", valid &["input"]),
    bp!("this"),
    bp!("innerText", invalid NOT_WINDOW_DOCUMENT),
    bp!("innerHTML", invalid NOT_WINDOW_DOCUMENT),
    bp!("textContent", invalid NOT_WINDOW_DOCUMENT),
    bp!("open", valid &["details"]),
    bp!("value", valid &["input", "textarea", "select"]),
    bp!("files", valid &["input"]),
];

pub fn binding_property(name: &str) -> Option<&'static BindingProperty> {
    BINDING_PROPERTIES.iter().find(|b| b.name == name)
}

// ---------------------------------------------------------------------------------------
// `svelte/src/utils.js` helpers the transforms use

/// `hash(str)`: djb2 over UTF-16 units, `\r` removed, base 36
pub fn hash(s: &str) -> String {
    let units: Vec<u16> = s.encode_utf16().filter(|&c| c != u16::from(b'\r')).collect();
    let mut h: i32 = 5381;
    for &c in units.iter().rev() {
        h = (h.wrapping_shl(5).wrapping_sub(h)) ^ i32::from(c);
    }
    let mut n = h as u32;
    if n == 0 {
        return "0".into();
    }
    let mut out = Vec::new();
    while n > 0 {
        out.push(b"0123456789abcdefghijklmnopqrstuvwxyz"[(n % 36) as usize]);
        n /= 36;
    }
    out.reverse();
    String::from_utf8(out).unwrap()
}

pub fn is_capture_event(name: &str) -> bool {
    name.ends_with("capture") && name != "gotpointercapture" && name != "lostpointercapture"
}

const DELEGATED_EVENTS: &[&str] = &[
    "beforeinput", "click", "change", "dblclick", "contextmenu", "focusin", "focusout", "input", "keydown", "keyup",
    "mousedown", "mousemove", "mouseout", "mouseover", "mouseup", "pointerdown", "pointermove", "pointerout",
    "pointerover", "pointerup", "touchend", "touchmove", "touchstart",
];

pub fn can_delegate_event(event_name: &str) -> bool {
    DELEGATED_EVENTS.contains(&event_name)
}

const DOM_BOOLEAN_ATTRIBUTES: &[&str] = &[
    "allowfullscreen", "async", "autofocus", "autoplay", "checked", "controls", "default", "disabled",
    "formnovalidate", "indeterminate", "inert", "ismap", "loop", "multiple", "muted", "nomodule", "novalidate",
    "open", "playsinline", "readonly", "required", "reversed", "seamless", "selected", "webkitdirectory", "defer",
    "disablepictureinpicture", "disableremoteplayback",
];

pub fn is_boolean_attribute(name: &str) -> bool {
    DOM_BOOLEAN_ATTRIBUTES.contains(&name)
}

/// `normalize_attribute`: lowercase, then the property name for aliased attributes
pub fn normalize_attribute(name: &str) -> String {
    let name = name.to_lowercase();
    match name.as_str() {
        "formnovalidate" => "formNoValidate",
        "ismap" => "isMap",
        "nomodule" => "noModule",
        "playsinline" => "playsInline",
        "readonly" => "readOnly",
        "defaultvalue" => "defaultValue",
        "defaultchecked" => "defaultChecked",
        "srcobject" => "srcObject",
        "novalidate" => "noValidate",
        "allowfullscreen" => "allowFullscreen",
        "disablepictureinpicture" => "disablePictureInPicture",
        "disableremoteplayback" => "disableRemotePlayback",
        _ => return name,
    }
    .to_string()
}

pub fn is_dom_property(name: &str) -> bool {
    DOM_BOOLEAN_ATTRIBUTES.contains(&name)
        || matches!(
            name,
            "formNoValidate"
                | "isMap"
                | "noModule"
                | "playsInline"
                | "readOnly"
                | "value"
                | "volume"
                | "defaultValue"
                | "defaultChecked"
                | "srcObject"
                | "noValidate"
                | "allowFullscreen"
                | "disablePictureInPicture"
                | "disableRemotePlayback"
        )
}

/// Attributes that can't be set through the template string
pub fn cannot_be_set_statically(name: &str) -> bool {
    matches!(name, "autofocus" | "muted" | "defaultValue" | "defaultChecked")
}

pub fn is_passive_event(name: &str) -> bool {
    matches!(name, "touchstart" | "touchmove")
}

pub fn is_load_error_element(name: &str) -> bool {
    matches!(name, "body" | "embed" | "iframe" | "img" | "link" | "object" | "script" | "style" | "track")
}

pub fn is_raw_text_element(name: &str) -> bool {
    matches!(name, "textarea" | "script" | "style" | "title")
}

/// `REGEX_VALID_TAG_NAME`
pub fn is_valid_tag_name(name: &str) -> bool {
    let mut chars = name.chars();
    if !chars.next().is_some_and(|c| c.is_ascii_alphabetic()) {
        return false;
    }
    let rest = chars.as_str();
    let (head, custom) = match rest.find('-') {
        Some(i) => (&rest[..i], Some(&rest[i + 1..])),
        None => (rest, None),
    };
    if !head.chars().all(|c| c.is_ascii_alphanumeric()) {
        return false;
    }
    custom.is_none_or(|c| {
        c.chars().all(|c| {
            c.is_ascii_alphanumeric()
                || matches!(c, '.' | '-' | '_' | '\u{B7}' | '\u{C0}'..='\u{D6}' | '\u{D8}'..='\u{F6}' | '\u{F8}'..='\u{37D}' | '\u{37F}'..='\u{1FFF}' | '\u{200C}'..='\u{200D}' | '\u{203F}'..='\u{2040}' | '\u{2070}'..='\u{218F}' | '\u{2C00}'..='\u{2FEF}' | '\u{3001}'..='\u{D7FF}' | '\u{F900}'..='\u{FDCF}' | '\u{FDF0}'..='\u{FFFD}' | '\u{10000}'..='\u{EFFFF}')
        })
    })
}

/// `sanitize_location`: a zero-width space after each `/`
pub fn sanitize_location(location: &str) -> String {
    location.replace('/', "/\u{200b}")
}
