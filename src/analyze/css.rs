//! CSS analysis: ports of `css/css-analyze.js` (validation and metadata), `css/css-prune.js`
//! (which selectors can match which elements) and `css/css-warn.js` (`css_unused_selector`).
//!
//! Svelte keeps metadata on the CSS nodes; here it lives in side tables keyed by node address.

use oxc_ast::AstKind;
use oxc_ast::ast::*;
use rustc_hash::{FxHashMap, FxHashSet};

use super::nodes::{self, P};
use super::utils::{is_js_whitespace, text_value};
use super::{Analyzer, warnings as w};
use crate::ast::{Attr, AttrValue, Chunk, Expr, Node, NodeId, StyleSheet};
use crate::css::{Atrule, Block, BlockChild, ComplexSelector, RelativeSelector, Rule, SelectorList, SimpleSelector};
use crate::error::Result;
use crate::errors as e;

fn addr<T>(r: &T) -> usize {
    r as *const T as usize
}

/// The metadata the CSS analysis puts on the CSS nodes (and `analysis.css.keyframes` /
/// `has_global`), for `render_stylesheet`. Node sets are keyed by node address.
#[derive(Default)]
pub struct Meta<'c> {
    pub parent_rule: FxHashMap<usize, &'c Rule>,
    pub is_global_block: FxHashSet<usize>,
    pub has_global_selectors: FxHashSet<usize>,
    /// Rule `metadata.has_local_selectors`
    pub has_local_selectors: FxHashSet<usize>,
    /// ComplexSelector → its rule
    pub complex_rule: FxHashMap<usize, &'c Rule>,
    /// ComplexSelector `metadata.is_global`
    pub complex_is_global: FxHashSet<usize>,
    /// ComplexSelector `metadata.used`
    pub used: FxHashSet<usize>,
    /// RelativeSelector `metadata.is_global` / `is_global_like`
    pub rel_is_global: FxHashSet<usize>,
    pub rel_is_global_like: FxHashSet<usize>,
    /// RelativeSelector `metadata.scoped`
    pub rel_scoped: FxHashSet<usize>,
    /// elements with `metadata.scoped`
    pub scoped_elements: FxHashSet<NodeId>,
    /// `analysis.css.keyframes`
    pub keyframes: Vec<String>,
    /// `analysis.css.has_global` as the CSS analysis sets it (exported snippets also set it)
    pub has_global: bool,
}

impl<'c> Meta<'c> {
    pub fn parent_rule(&self, rule: &Rule) -> Option<&'c Rule> {
        self.parent_rule.get(&addr(rule)).copied()
    }
}

pub fn analyze<'s>(an: &mut Analyzer<'s>, sheet: &'s StyleSheet<'s>) -> Result<Meta<'s>> {
    let css = &sheet.css;
    let mut meta = Meta::default();

    // analyze_css
    {
        let mut walker = AnalyzeWalker { meta: &mut meta, path: Vec::new(), rules: Vec::new() };
        for child in &css.children {
            walker.block_child(child, None)?;
        }
    }

    // prune
    let elements = an.elements.clone();
    {
        let mut pruner = Pruner { an, meta: &mut meta, elements: &elements, in_head: Vec::new() };
        pruner.in_head = elements.iter().map(|&e| pruner.is_inside_svelte_head(e)).collect();
        for child in &css.children {
            pruner.prune_child(child);
        }
    }

    // svelte-ignore css_unused_selector
    let should_ignore_unused = match &css.comment {
        Some((start, _, data)) => {
            let data: &'s str = an.alloc.alloc_str(data);
            an.extract_svelte_ignore(*start, data).contains(&"css_unused_selector")
        }
        None => false,
    };
    if !should_ignore_unused {
        let source = an.source;
        let mut warner = Warner { an, meta: &meta, path: Vec::new(), source };
        for child in &css.children {
            warner.block_child(child);
        }
    }
    Ok(meta)
}

// ---------------------------------------------------------------------------------------
// shared helpers (css/utils.js)

fn is_keyframes(a: &Atrule) -> bool {
    let name = a.name.as_str();
    let stripped = ["-webkit-", "-moz-", "-o-", "-ms-"].iter().find_map(|p| name.strip_prefix(p)).unwrap_or(name);
    stripped == "keyframes"
}

fn is_global_relative(r: &RelativeSelector) -> bool {
    match r.selectors.first() {
        Some(SimpleSelector::PseudoClass { name, args, .. }) if name == "global" => {
            args.is_none()
                || r.selectors
                    .iter()
                    .all(|s| is_unscoped_pseudo_class(s) || matches!(s, SimpleSelector::PseudoElement { .. }))
        }
        _ => false,
    }
}

fn is_unscoped_pseudo_class(s: &SimpleSelector) -> bool {
    let SimpleSelector::PseudoClass { name, args, .. } = s else { return false };
    let scoping = matches!(name.as_str(), "has" | "is" | "where");
    let not_ok = name != "not" || args.as_ref().is_none_or(|a| a.children.iter().all(|c| c.children.len() == 1));
    (!scoping && not_ok)
        || args.is_none()
        || args.as_ref().is_some_and(|a| a.children.iter().all(|c| c.children.iter().all(is_global_relative)))
}


// ---------------------------------------------------------------------------------------
// css-analyze.js

/// The kinds of node in a CSS path (only `PseudoClass` is ever looked at)
enum CssP {
    Rule,
    Atrule,
    Block,
    SelectorList,
    Complex,
    Relative,
    PseudoClass,
}

struct AnalyzeWalker<'m, 'c> {
    meta: &'m mut Meta<'c>,
    path: Vec<CssP>,
    /// the Rules in `path`
    rules: Vec<&'c Rule>,
}

fn is_global_block_selector(s: &SimpleSelector) -> bool {
    matches!(s, SimpleSelector::PseudoClass { name, args: None, .. } if name == "global")
}

impl<'m, 'c> AnalyzeWalker<'m, 'c> {
    fn block_child(&mut self, child: &'c BlockChild, rule: Option<&'c Rule>) -> Result<()> {
        match child {
            BlockChild::Rule(r) => self.rule(r, rule),
            BlockChild::Atrule(a) => self.atrule(a, rule),
            BlockChild::Declaration(_) => Ok(()),
        }
    }

    /// `is_unscoped(path)`: every rule in the path has global selectors
    fn is_unscoped(&self) -> bool {
        self.rules.iter().all(|r| self.meta.has_global_selectors.contains(&addr(*r)))
    }

    fn atrule(&mut self, a: &'c Atrule, rule: Option<&'c Rule>) -> Result<()> {
        if is_keyframes(a) {
            let is_global_name = a.prelude.starts_with("-global-");
            if !is_global_name && !self.rules.iter().any(|r| self.meta.is_global_block.contains(&addr(*r))) {
                self.meta.keyframes.push(a.prelude.clone());
            } else if is_global_name {
                self.meta.has_global |= self.is_unscoped();
            }
        }
        if let Some(block) = &a.block {
            self.path.push(CssP::Atrule);
            self.block(block, rule)?;
            self.path.pop();
        }
        Ok(())
    }

    fn block(&mut self, b: &'c Block, rule: Option<&'c Rule>) -> Result<()> {
        self.path.push(CssP::Block);
        for child in &b.children {
            self.block_child(child, rule)?;
        }
        self.path.pop();
        Ok(())
    }

    fn rule(&mut self, node: &'c Rule, parent: Option<&'c Rule>) -> Result<()> {
        if let Some(p) = parent {
            self.meta.parent_rule.insert(addr(node), p);
        }
        let key = addr(node);
        for complex in &node.prelude.children {
            let mut is_global_block = false;
            for (selector_idx, child) in complex.children.iter().enumerate() {
                let idx = child.selectors.iter().position(is_global_block_selector);
                if is_global_block {
                    self.meta.rel_is_global_like.insert(addr(child));
                }
                if idx == Some(0) {
                    if child.selectors.len() > 1 && selector_idx == 0 && parent.is_none() {
                        return Err(e::css_global_block_invalid_modifier_start(sel_span(&child.selectors[1])));
                    }
                    self.meta.is_global_block.insert(key);
                    is_global_block = true;
                    for s in &child.selectors[1..] {
                        mark_nested_used(s, self.meta, false);
                    }
                    if let Some(c) = &child.combinator {
                        if c.name != " " {
                            return Err(e::css_global_block_invalid_combinator((child.start, child.end), c.name));
                        }
                    }
                    let declaration = node.block.children.iter().find_map(|c| match c {
                        BlockChild::Declaration(d) => Some(d),
                        _ => None,
                    });
                    let is_lone_global = complex.children.len() == 1 && complex.children[0].selectors.len() == 1;
                    if is_lone_global && node.prelude.children.len() > 1 {
                        return Err(e::css_global_block_invalid_list((node.prelude.start, node.prelude.end)));
                    }
                    if let Some(d) = declaration {
                        if node.prelude.children.len() == 1 && is_lone_global {
                            return Err(e::css_global_block_invalid_declaration((d.start, d.end)));
                        }
                    }
                } else if let Some(idx) = idx {
                    return Err(e::css_global_block_invalid_modifier(sel_span(&child.selectors[idx])));
                }
            }
            if self.meta.is_global_block.contains(&key) && !is_global_block {
                return Err(e::css_global_block_invalid_list((node.prelude.start, node.prelude.end)));
            }
        }

        self.path.push(CssP::Rule);
        self.selector_list(&node.prelude, Some(node))?;
        for complex in &node.prelude.children {
            if self.meta.complex_is_global.contains(&addr(complex)) {
                self.meta.has_global_selectors.insert(key);
            } else {
                self.meta.has_local_selectors.insert(key);
            }
        }
        if self.meta.has_global_selectors.contains(&key)
            && node.block.children.iter().any(|c| matches!(c, BlockChild::Declaration(_)))
            && self.is_unscoped()
        {
            self.meta.has_global = true;
        }
        self.rules.push(node);
        self.block(&node.block, Some(node))?;
        self.rules.pop();
        self.path.pop();
        Ok(())
    }

    fn selector_list(&mut self, list: &'c SelectorList, rule: Option<&'c Rule>) -> Result<()> {
        self.path.push(CssP::SelectorList);
        for c in &list.children {
            self.complex(c, rule)?;
        }
        self.path.pop();
        Ok(())
    }

    fn complex(&mut self, node: &'c ComplexSelector, rule: Option<&'c Rule>) -> Result<()> {
        self.path.push(CssP::Complex);
        for r in &node.children {
            self.relative(r, node, rule)?;
        }
        self.path.pop();

        if let Some(global) = node.children.iter().find(|r| is_global_relative(r)) {
            let n = self.path.len();
            let is_nested = n >= 2 && matches!(self.path[n - 2], CssP::PseudoClass);
            let SimpleSelector::PseudoClass { args, .. } = &global.selectors[0] else { unreachable!() };
            if is_nested && args.is_none() {
                return Err(e::css_global_block_invalid_placement(sel_span(&global.selectors[0])));
            }
            let idx = node.children.iter().position(|r| std::ptr::eq(r, global)).unwrap();
            if args.is_some() && idx != 0 && idx != node.children.len() - 1 {
                for r in &node.children[idx + 1..] {
                    if !is_global_relative(r) {
                        return Err(e::css_global_invalid_placement(sel_span(&global.selectors[0])));
                    }
                }
            }
        }

        for relative in &node.children {
            for (i, selector) in relative.selectors.iter().enumerate() {
                let SimpleSelector::PseudoClass { name, args, .. } = selector else { continue };
                if name != "global" {
                    continue;
                }
                let child = args.as_ref().and_then(|a| a.children.first()).and_then(|c| c.children.first());
                if child.is_some_and(|c| matches!(c.selectors.first(), Some(SimpleSelector::Type { .. }))) && i != 0 {
                    return Err(e::css_global_invalid_selector_list(sel_span(selector)));
                }
                if let Some(next @ SimpleSelector::Type { .. }) = relative.selectors.get(i + 1) {
                    return Err(e::css_type_selector_invalid_placement(sel_span(next)));
                }
                if let Some(args) = args {
                    if args.children.len() > 1 && (node.children.len() > 1 || relative.selectors.len() > 1) {
                        return Err(e::css_global_invalid_selector(sel_span(selector)));
                    }
                }
            }
        }

        let key = addr(node);
        if let Some(r) = rule {
            self.meta.complex_rule.insert(key, r);
        }
        let is_global = node.children.iter().all(|r| {
            self.meta.rel_is_global.contains(&addr(r)) || self.meta.rel_is_global_like.contains(&addr(r))
        });
        if is_global {
            self.meta.complex_is_global.insert(key);
            self.meta.used.insert(key);
        }

        if let Some(rule) = rule {
            if let Some(parent_rule) = self.meta.parent_rule(rule) {
                if matches!(node.children.first().and_then(|r| r.selectors.first()), Some(SimpleSelector::Nesting { .. })) {
                    let first = node.children[0].selectors.get(1);
                    let no_nesting_scope = !matches!(first, Some(SimpleSelector::PseudoClass { .. }))
                        || first.is_some_and(is_unscoped_pseudo_class);
                    let parent_is_global = parent_rule.prelude.children.iter().any(|child| {
                        child.children.len() == 1 && self.meta.rel_is_global.contains(&addr(&child.children[0]))
                    });
                    if no_nesting_scope && parent_is_global {
                        self.meta.used.insert(key);
                    }
                }
            }
        }
        Ok(())
    }

    fn relative(&mut self, node: &'c RelativeSelector, parent: &'c ComplexSelector, rule: Option<&'c Rule>) -> Result<()> {
        let n = self.path.len();
        if let Some(c) = &node.combinator {
            let has_parent_rule = rule.is_some_and(|r| self.meta.parent_rule(r).is_some());
            let in_pseudo = n >= 3 && matches!(self.path[n - 3], CssP::PseudoClass);
            if !has_parent_rule && std::ptr::eq(&parent.children[0], node) && !in_pseudo {
                return Err(e::css_selector_invalid((c.start, c.end)));
            }
        }
        let key = addr(node);
        if !node.selectors.is_empty() && is_global_relative(node) {
            self.meta.rel_is_global.insert(key);
        }
        if !node.selectors.is_empty()
            && node.selectors.iter().all(|s| matches!(s, SimpleSelector::PseudoClass { .. } | SimpleSelector::PseudoElement { .. }))
        {
            let global_like = match &node.selectors[0] {
                SimpleSelector::PseudoClass { name, .. } => name == "host",
                SimpleSelector::PseudoElement { name, .. } => matches!(
                    name.as_str(),
                    "view-transition"
                        | "view-transition-group"
                        | "view-transition-old"
                        | "view-transition-new"
                        | "view-transition-image-pair"
                ),
                _ => false,
            };
            if global_like {
                self.meta.rel_is_global_like.insert(key);
            }
        }
        if node.selectors.iter().any(|s| matches!(s, SimpleSelector::PseudoClass { name, .. } if name == "root"))
            && !node.selectors.iter().any(|s| matches!(s, SimpleSelector::PseudoClass { name, .. } if name == "has"))
        {
            self.meta.rel_is_global_like.insert(key);
        }
        if self.meta.rel_is_global_like.contains(&key) || self.meta.rel_is_global.contains(&key) {
            for s in &node.selectors {
                mark_nested_used(s, self.meta, true);
            }
        }

        self.path.push(CssP::Relative);
        for s in &node.selectors {
            self.simple(s, rule)?;
        }
        self.path.pop();
        Ok(())
    }

    fn simple(&mut self, s: &'c SimpleSelector, rule: Option<&'c Rule>) -> Result<()> {
        match s {
            SimpleSelector::PseudoElement { .. } => Ok(()),
            SimpleSelector::PseudoClass { args, .. } => {
                if let Some(args) = args {
                    self.path.push(CssP::PseudoClass);
                    self.selector_list(args, rule)?;
                    self.path.pop();
                }
                Ok(())
            }
            SimpleSelector::Nesting { start, end } => {
                let rule = rule.unwrap();
                match self.meta.parent_rule(rule) {
                    None => {
                        let children = &rule.prelude.children;
                        let selectors = &children[0].children[0].selectors;
                        let ok = children.len() == 1
                            && selectors.len() == 1
                            && match &selectors[0] {
                                SimpleSelector::PseudoClass { name, args, .. } if name == "global" => args
                                    .as_ref()
                                    .and_then(|a| a.children.first())
                                    .and_then(|c| c.children.first())
                                    .and_then(|r| r.selectors.first())
                                    .is_some_and(|first| std::ptr::eq(first, s)),
                                _ => false,
                            };
                        if !ok {
                            return Err(e::css_nesting_selector_invalid_placement((*start, *end)));
                        }
                    }
                    Some(parent_rule) => {
                        if self.meta.is_global_block.contains(&addr(parent_rule))
                            && self.meta.parent_rule(parent_rule).is_none()
                            && parent_rule.prelude.children[0].children.len() == 1
                            && parent_rule.prelude.children[0].children[0].selectors.len() == 1
                        {
                            return Err(e::css_global_block_invalid_modifier_start((*start, *end)));
                        }
                    }
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

/// `walk(child, null, { ComplexSelector(node, context) { node.metadata.used = true; context.next() } })`
/// (`deep`: keep going into nested selectors once a ComplexSelector is found)
fn mark_nested_used(s: &SimpleSelector, meta: &mut Meta, deep: bool) {
    if let SimpleSelector::PseudoClass { args: Some(args), .. } | SimpleSelector::PseudoElement { args: Some(args), .. } = s {
        for c in &args.children {
            meta.used.insert(addr(c));
            if deep {
                for r in &c.children {
                    for s in &r.selectors {
                        mark_nested_used(s, meta, true);
                    }
                }
            }
        }
    }
}

fn sel_span(s: &SimpleSelector) -> (usize, usize) {
    match s {
        SimpleSelector::Nesting { start, end }
        | SimpleSelector::Type { start, end, .. }
        | SimpleSelector::Id { start, end, .. }
        | SimpleSelector::Class { start, end, .. }
        | SimpleSelector::PseudoElement { start, end, .. }
        | SimpleSelector::PseudoClass { start, end, .. }
        | SimpleSelector::Attribute { start, end, .. }
        | SimpleSelector::Nth { start, end, .. }
        | SimpleSelector::Percentage { start, end, .. } => (*start, *end),
    }
}

// ---------------------------------------------------------------------------------------
// css-warn.js

enum WarnP {
    PseudoClass,
    Complex(usize),
    Other,
}

struct Warner<'a, 's, 'm, 'c> {
    an: &'a mut Analyzer<'s>,
    meta: &'m Meta<'c>,
    path: Vec<WarnP>,
    source: &'s str,
}

impl<'c> Warner<'_, '_, '_, 'c> {
    fn block_child(&mut self, child: &'c BlockChild) {
        match child {
            BlockChild::Atrule(a) => {
                if !is_keyframes(a) {
                    if let Some(b) = &a.block {
                        self.path.push(WarnP::Other);
                        self.path.push(WarnP::Other);
                        for c in &b.children {
                            self.block_child(c);
                        }
                        self.path.pop();
                        self.path.pop();
                    }
                }
            }
            BlockChild::Rule(r) => {
                self.path.push(WarnP::Other);
                self.selector_list(&r.prelude);
                if !self.meta.is_global_block.contains(&addr(r)) {
                    self.path.push(WarnP::Other);
                    for c in &r.block.children {
                        self.block_child(c);
                    }
                    self.path.pop();
                }
                self.path.pop();
            }
            BlockChild::Declaration(_) => {}
        }
    }

    fn selector_list(&mut self, list: &'c SelectorList) {
        self.path.push(WarnP::Other);
        for c in &list.children {
            self.complex(c);
        }
        self.path.pop();
    }

    fn complex(&mut self, node: &'c ComplexSelector) {
        let key = addr(node);
        let n = self.path.len();
        let parent_is_pseudo = n >= 2 && matches!(self.path[n - 2], WarnP::PseudoClass);
        let outer_used = n >= 4 && matches!(self.path[n - 4], WarnP::Complex(k) if self.meta.used.contains(&k));
        if !self.meta.used.contains(&key) && (!parent_is_pseudo || outer_used) {
            let text = &self.source[node.start..node.end];
            self.an.warn_range(node.start, node.end, w::css_unused_selector(text));
        }
        self.path.push(WarnP::Complex(key));
        for r in &node.children {
            self.path.push(WarnP::Other);
            for s in &r.selectors {
                if let SimpleSelector::PseudoClass { name, args: Some(args), .. } = s {
                    if name == "is" || name == "where" {
                        self.path.push(WarnP::PseudoClass);
                        self.selector_list(args);
                        self.path.pop();
                    }
                }
            }
            self.path.pop();
        }
        self.path.pop();
    }
}

// ---------------------------------------------------------------------------------------
// css-prune.js

const FORWARD: bool = false;
const BACKWARD: bool = true;

#[derive(Clone, Copy)]
enum Sel<'c> {
    Real(&'c SimpleSelector),
    Nesting,
    AnyType,
}

#[derive(Clone, Copy)]
enum SelSource<'c> {
    All(&'c [SimpleSelector]),
    /// `:root.y:has(...)`: only the `:has` selectors
    OnlyHas(&'c [SimpleSelector]),
    Nesting,
    Any,
}

/// A RelativeSelector as the pruning code sees it (possibly a modified copy)
#[derive(Clone, Copy)]
struct Rel<'c> {
    /// the RelativeSelector whose metadata this shares (`None` for synthetic ones)
    node: Option<&'c RelativeSelector>,
    combinator: Option<&'c str>,
    selectors: SelSource<'c>,
}

impl<'c> Rel<'c> {
    fn iter(&self) -> impl Iterator<Item = Sel<'c>> + 'c {
        let (slice, only_has, synthetic): (&'c [SimpleSelector], bool, Option<Sel<'c>>) = match self.selectors {
            SelSource::All(s) => (s, false, None),
            SelSource::OnlyHas(s) => (s, true, None),
            SelSource::Nesting => (&[], false, Some(Sel::Nesting)),
            SelSource::Any => (&[], false, Some(Sel::AnyType)),
        };
        synthetic.into_iter().chain(
            slice
                .iter()
                .filter(move |s| !only_has || matches!(s, SimpleSelector::PseudoClass { name, .. } if name == "has"))
                .map(Sel::Real),
        )
    }
    fn len(&self) -> usize {
        self.iter().count()
    }
}

/// `is_outer_global`: `:global` or `:global(...)` followed only by pseudo classes/elements
fn is_outer_global(r: &Rel) -> bool {
    let mut iter = r.iter();
    match iter.next() {
        Some(Sel::Real(SimpleSelector::PseudoClass { name, args, .. })) if name == "global" => {
            args.is_none()
                || r.iter().all(|s| {
                    matches!(s, Sel::Real(SimpleSelector::PseudoClass { .. } | SimpleSelector::PseudoElement { .. }))
                })
        }
        _ => false,
    }
}

fn real_rel(r: &RelativeSelector) -> Rel<'_> {
    Rel { node: Some(r), combinator: r.combinator.as_ref().map(|c| c.name), selectors: SelSource::All(&r.selectors) }
}

const NESTING_SELECTOR: Rel<'static> = Rel { node: None, combinator: None, selectors: SelSource::Nesting };
const ANY_SELECTOR: Rel<'static> = Rel { node: None, combinator: None, selectors: SelSource::Any };

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Exists {
    Probably = 0,
    Definitely = 1,
}

type SiblingMap = Vec<(NodeId, Exists)>;

fn add_to_map(from: &SiblingMap, to: &mut SiblingMap) {
    for &(el, exist) in from {
        match to.iter_mut().find(|(k, _)| *k == el) {
            Some(entry) => entry.1 = entry.1.max(exist),
            None => to.push((el, exist)),
        }
    }
}

fn has_definite_elements(m: &SiblingMap) -> bool {
    m.iter().any(|(_, e)| *e == Exists::Definitely)
}

/// possible values of an attribute value chunk
#[derive(Clone, Debug, PartialEq)]
enum JsVal {
    Str(String),
    False,
    NaN,
    Zero,
}

impl JsVal {
    fn to_js_string(&self) -> String {
        match self {
            JsVal::Str(s) => s.clone(),
            JsVal::False => "false".into(),
            JsVal::NaN => "NaN".into(),
            JsVal::Zero => "0".into(),
        }
    }
}

struct Values {
    set: Vec<JsVal>,
    unknown: bool,
}

impl Values {
    fn add(&mut self, v: JsVal) {
        if !self.set.contains(&v) {
            self.set.push(v);
        }
    }
}

struct Pruner<'a, 's, 'm, 'c> {
    an: &'a mut Analyzer<'s>,
    meta: &'m mut Meta<'c>,
    elements: &'a [NodeId],
    /// whether each element is inside `<svelte:head>`
    in_head: Vec<bool>,
}

impl<'s, 'c> Pruner<'_, 's, '_, 'c>
where
    's: 'c,
{
    fn prune_child(&mut self, child: &'c BlockChild) {
        match child {
            BlockChild::Atrule(a) => {
                if let Some(b) = &a.block {
                    for c in &b.children {
                        self.prune_child(c);
                    }
                }
            }
            BlockChild::Rule(r) => {
                for complex in &r.prelude.children {
                    self.prune_complex(complex);
                }
                if !self.meta.is_global_block.contains(&addr(r)) {
                    for c in &r.block.children {
                        self.prune_child(c);
                    }
                }
            }
            BlockChild::Declaration(_) => {}
        }
    }

    fn prune_complex(&mut self, node: &'c ComplexSelector) {
        let selectors = self.get_relative_selectors(node);
        let rule = self.meta.complex_rule.get(&addr(node)).copied();
        if self.every_is_global(&selectors, 0, selectors.len(), rule) {
            self.meta.used.insert(addr(node));
        }
        for i in 0..self.elements.len() {
            let element = self.elements[i];
            if !self.in_head[i] && self.apply_selector(&selectors, rule, element, BACKWARD, 0, selectors.len()) {
                self.meta.used.insert(addr(node));
            }
        }
    }

    fn element_path(&self, n: NodeId) -> &[P<'s>] {
        self.an.saved_path(n)
    }

    fn is_inside_svelte_head(&self, n: NodeId) -> bool {
        self.element_path(n).iter().any(|p| self.an.ty(*p) == "SvelteHead")
    }

    fn get_relative_selectors(&self, node: &'c ComplexSelector) -> Vec<Rel<'c>> {
        let mut selectors = self.truncate(node);
        let rule = self.meta.complex_rule.get(&addr(node)).copied();
        if rule.is_some_and(|r| self.meta.parent_rule(r).is_some()) && !selectors.is_empty() {
            let has_explicit_nesting = selectors.iter().any(|r| r.iter().any(|s| sel_has_nesting(s)));
            if !has_explicit_nesting {
                if selectors[0].combinator.is_none() {
                    selectors[0].combinator = Some(" ");
                }
                selectors.insert(0, NESTING_SELECTOR);
            }
        }
        selectors
    }

    fn truncate(&self, node: &'c ComplexSelector) -> Vec<Rel<'c>> {
        let i = node.children.iter().rposition(|r| {
            let first = r.selectors.first();
            !self.meta.rel_is_global_like.contains(&addr(r))
                && !matches!(first, Some(SimpleSelector::PseudoClass { name, args: None, .. }) if name == "global")
                && !self.meta.rel_is_global.contains(&addr(r))
        });
        let end = i.map_or(0, |i| i + 1);
        node.children[..end]
            .iter()
            .map(|child| {
                let has_root = child.selectors.iter().any(|s| matches!(s, SimpleSelector::PseudoClass { name, .. } if name == "root"));
                if !has_root || self.meta.rel_is_global_like.contains(&addr(child)) {
                    real_rel(child)
                } else {
                    Rel { selectors: SelSource::OnlyHas(&child.selectors), ..real_rel(child) }
                }
            })
            .collect()
    }

    fn rel_is_global(&self, r: &Rel<'c>) -> bool {
        r.node.is_some_and(|n| self.meta.rel_is_global.contains(&addr(n)))
    }

    fn rel_is_global_like(&self, r: &Rel<'c>) -> bool {
        r.node.is_some_and(|n| self.meta.rel_is_global_like.contains(&addr(n)))
    }

    fn apply_selector(&mut self, rels: &[Rel<'c>], rule: Option<&'c Rule>, element: NodeId, direction: bool, from: usize, to: usize) -> bool {
        if from >= to {
            return false;
        }
        let selector_index = if direction == FORWARD { from } else { to - 1 };
        let relative = rels[selector_index];
        let (rest_from, rest_to) = if direction == FORWARD { (from + 1, to) } else { (from, to - 1) };
        let matched = self.relative_selector_might_apply_to_node(&relative, rule, element, direction)
            && self.apply_combinator(&relative, rels, rest_from, rest_to, rule, element, direction);
        if matched {
            if let Some(node) = relative.node {
                if !is_outer_global(&relative) {
                    self.meta.rel_scoped.insert(addr(node));
                }
            }
            self.meta.scoped_elements.insert(element);
        }
        matched
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_combinator(
        &mut self,
        relative: &Rel<'c>,
        rels: &[Rel<'c>],
        from: usize,
        to: usize,
        rule: Option<&'c Rule>,
        node: NodeId,
        direction: bool,
    ) -> bool {
        let combinator = if direction == FORWARD {
            if from < to { rels[from].combinator } else { None }
        } else {
            relative.combinator
        };
        let Some(combinator) = combinator else { return true };
        match combinator {
            " " | ">" => {
                let is_adjacent = combinator == ">";
                let parents = if direction == FORWARD {
                    self.get_descendant_elements(node, is_adjacent)
                } else {
                    self.get_ancestor_elements(node, is_adjacent, &mut Vec::new())
                };
                let mut parent_matched = false;
                for &parent in &parents {
                    if self.apply_selector(rels, rule, parent, direction, from, to) {
                        parent_matched = true;
                    }
                }
                parent_matched
                    || (direction == BACKWARD && (!is_adjacent || parents.is_empty()) && self.every_is_global(rels, from, to, rule))
            }
            "+" | "~" => {
                let siblings = self.get_possible_element_siblings(node, direction, combinator == "+", &mut Vec::new());
                let mut sibling_matched = false;
                for &(sibling, _) in &siblings {
                    let is_special = match &self.an.ast.nodes[sibling] {
                        Node::RenderTag { .. } => true,
                        Node::Element(el) => matches!(el.kind, "SlotElement" | "Component"),
                        _ => false,
                    };
                    if is_special {
                        if to - from == 1 && self.rel_is_global(&rels[from]) {
                            sibling_matched = true;
                        }
                    } else if self.apply_selector(rels, rule, sibling, direction, from, to) {
                        sibling_matched = true;
                    }
                }
                sibling_matched
                    || (direction == BACKWARD && self.get_element_parent(node).is_none() && self.every_is_global(rels, from, to, rule))
            }
            _ => true,
        }
    }

    fn every_is_global(&self, rels: &[Rel<'c>], from: usize, to: usize, rule: Option<&'c Rule>) -> bool {
        (from..to).all(|i| self.is_global(&rels[i], rule))
    }

    fn is_global(&self, selector: &Rel<'c>, rule: Option<&'c Rule>) -> bool {
        if self.rel_is_global(selector) || self.rel_is_global_like(selector) {
            return true;
        }
        let mut explicitly_global = false;
        for s in selector.iter() {
            let mut selector_list: Option<&'c SelectorList> = None;
            let mut can_be_global = false;
            let mut owner = rule;
            match s {
                Sel::Real(sp @ SimpleSelector::PseudoClass { name, args, .. }) => {
                    if (name == "is" || name == "where") && args.is_some() {
                        selector_list = args.as_ref();
                    } else {
                        can_be_global = is_unscoped_pseudo_class(sp);
                    }
                }
                Sel::Real(SimpleSelector::Nesting { .. }) | Sel::Nesting => {
                    owner = rule.and_then(|r| self.meta.parent_rule(r));
                    selector_list = owner.map(|o| &o.prelude);
                }
                _ => {}
            }
            let has_global_selectors = selector_list.is_some_and(|list| {
                list.children.iter().any(|complex| complex.children.iter().all(|r| self.is_global(&real_rel(r), owner)))
            });
            explicitly_global |= has_global_selectors;
            if !has_global_selectors && !can_be_global {
                return false;
            }
        }
        explicitly_global || selector.len() == 0
    }

    fn relative_selector_might_apply_to_node(&mut self, relative: &Rel<'c>, rule: Option<&'c Rule>, element: NodeId, direction: bool) -> bool {
        let mut include_self: Option<bool> = None;
        let el = self.an.element(element).unwrap();
        for selector in relative.iter() {
            if let Sel::Real(SimpleSelector::PseudoClass { name, args: Some(args), .. }) = selector {
                if name == "has" {
                    if include_self.is_none() {
                        let rules = self.parent_rules(rule);
                        let a = rules.iter().any(|r| {
                            r.prelude.children.iter().any(|c| c.children.iter().any(|s| self.is_global(&real_rel(s), Some(r))))
                        });
                        let b = rules.last().is_some_and(|last| {
                            last.prelude.children.iter().any(|c| {
                                c.children.iter().any(|r| {
                                    r.selectors.iter().any(|s| {
                                        matches!(s, SimpleSelector::PseudoClass { name, args, .. }
                                            if name == "root" || (name == "global" && args.is_some()))
                                    })
                                })
                            })
                        });
                        include_self = Some(a || b);
                    }
                    let mut matched = false;
                    for complex in &args.children {
                        let truncated = self.truncate(complex);
                        let Some(first) = truncated.first().copied() else {
                            self.meta.used.insert(addr(complex));
                            matched = true;
                            continue;
                        };
                        let rest = &truncated[1..];
                        if include_self == Some(true) {
                            let mut including = vec![if first.combinator.is_some() { Rel { combinator: None, ..first } } else { first }];
                            including.extend_from_slice(rest);
                            let len = including.len();
                            if self.apply_selector(&including, rule, element, FORWARD, 0, len) {
                                self.meta.used.insert(addr(complex));
                                matched = true;
                            }
                        }
                        let mut excluding = vec![ANY_SELECTOR, if first.combinator.is_some() { first } else { Rel { combinator: Some(" "), ..first } }];
                        excluding.extend_from_slice(rest);
                        let len = excluding.len();
                        if self.apply_selector(&excluding, rule, element, FORWARD, 0, len) {
                            self.meta.used.insert(addr(complex));
                            matched = true;
                        }
                    }
                    if !matched {
                        return false;
                    }
                    continue;
                }
            }

            let (kind, raw_name) = match selector {
                Sel::Real(SimpleSelector::Percentage { .. } | SimpleSelector::Nth { .. }) => continue,
                Sel::Real(SimpleSelector::PseudoClass { name, .. }) => ("pc", name.as_str()),
                Sel::Real(SimpleSelector::PseudoElement { name, .. }) => ("pe", name.as_str()),
                Sel::Real(SimpleSelector::Attribute { name, .. }) => ("attr", name.as_str()),
                Sel::Real(SimpleSelector::Class { name, .. }) => ("class", name.as_str()),
                Sel::Real(SimpleSelector::Id { name, .. }) => ("id", name.as_str()),
                Sel::Real(SimpleSelector::Type { name, .. }) => ("type", name.as_str()),
                Sel::AnyType => ("type", "*"),
                Sel::Real(SimpleSelector::Nesting { .. }) | Sel::Nesting => ("nesting", "&"),
            };
            let name_owned;
            let name: &str = if raw_name.contains('\\') {
                name_owned = unescape(raw_name);
                &name_owned
            } else {
                raw_name
            };

            match kind {
                "pc" => {
                    let Sel::Real(SimpleSelector::PseudoClass { args, .. }) = selector else { unreachable!() };
                    if name == "host" || name == "root" {
                        return false;
                    }
                    if name == "global" && args.is_some() && relative.len() == 1 {
                        let complex = &args.as_ref().unwrap().children[0];
                        let rels: Vec<Rel<'c>> = complex.children.iter().map(real_rel).collect();
                        let len = rels.len();
                        return self.apply_selector(&rels, rule, element, BACKWARD, 0, len);
                    }
                    if name == "global" && args.is_none() {
                        return true;
                    }
                    if name == "not" && args.is_some() {
                        for complex in &args.as_ref().unwrap().children {
                            self.meta.used.insert(addr(complex));
                            for r in &complex.children {
                                for s in &r.selectors {
                                    mark_nested_used(s, self.meta, true);
                                }
                            }
                            if complex.children.len() > 1 {
                                for r in self.truncate(complex) {
                                    if let Some(node) = r.node {
                                        self.meta.rel_scoped.insert(addr(node));
                                    }
                                }
                                let mut el = Some(element);
                                while let Some(e) = el {
                                    self.meta.scoped_elements.insert(e);
                                    el = self.get_element_parent(e);
                                }
                            }
                        }
                        continue;
                    }
                    if (name == "is" || name == "where") && args.is_some() {
                        let mut matched = false;
                        for complex in &args.as_ref().unwrap().children {
                            let relative = self.truncate(complex);
                            let is_global = relative.is_empty();
                            let len = relative.len();
                            if is_global {
                                self.meta.used.insert(addr(complex));
                                matched = true;
                            } else if self.apply_selector(&relative, rule, element, BACKWARD, 0, len) {
                                self.meta.used.insert(addr(complex));
                                matched = true;
                            } else if complex.children.len() > 1 {
                                self.meta.used.insert(addr(complex));
                                matched = true;
                                for r in &relative {
                                    if let Some(node) = r.node {
                                        self.meta.rel_scoped.insert(addr(node));
                                    }
                                }
                            }
                        }
                        if !matched {
                            return false;
                        }
                    }
                }
                "pe" => {}
                "attr" => {
                    let Sel::Real(SimpleSelector::Attribute { name: attr_name, value, matcher, flags, .. }) = selector else {
                        unreachable!()
                    };
                    let whitelisted =
                        (lower_eq(el.name, "details") || lower_eq(el.name, "dialog")) && lower_eq(attr_name, "open");
                    let case_insensitive = flags.as_ref().is_some_and(|f| f.contains('i'))
                        || (!flags.as_ref().is_some_and(|f| f.contains('s'))
                            && CASE_INSENSITIVE_ATTRIBUTES.iter().any(|a| lower_eq(attr_name, a)));
                    let expected = value.as_ref().map(|v| unquote(v));
                    if !whitelisted
                        && !self.attribute_matches(element, attr_name, expected.as_deref(), matcher.as_deref(), case_insensitive)
                    {
                        return false;
                    }
                }
                "class" => {
                    if !self.attribute_matches(element, "class", Some(name), Some("~="), false) {
                        return false;
                    }
                }
                "id" => {
                    if !self.attribute_matches(element, "id", Some(name), Some("="), false) {
                        return false;
                    }
                }
                "type" => {
                    if !lower_eq_both(el.name, name) && name != "*" && el.kind != "SvelteElement" {
                        return false;
                    }
                }
                "nesting" => {
                    let mut matched = false;
                    let parent = rule.and_then(|r| self.meta.parent_rule(r));
                    if let Some(parent) = parent {
                        for complex in &parent.prelude.children {
                            let rels = self.get_relative_selectors(complex);
                            let len = rels.len();
                            if self.apply_selector(&rels, Some(parent), element, direction, 0, len)
                                || complex.children.iter().all(|s| self.is_global(&real_rel(s), Some(parent)))
                            {
                                self.meta.used.insert(addr(complex));
                                matched = true;
                            }
                        }
                    }
                    if !matched {
                        return false;
                    }
                }
                _ => {}
            }
        }
        true
    }

    fn parent_rules(&self, rule: Option<&'c Rule>) -> Vec<&'c Rule> {
        let mut rules = Vec::new();
        let mut r = rule;
        while let Some(x) = r {
            rules.push(x);
            r = self.meta.parent_rule(x);
        }
        rules
    }

    fn attribute_matches(&self, n: NodeId, name: &str, expected: Option<&str>, operator: Option<&str>, case_insensitive: bool) -> bool {
        let name_lower: std::borrow::Cow<str> =
            if name.bytes().any(|b| b.is_ascii_uppercase()) || !name.is_ascii() { name.to_lowercase().into() } else { name.into() };
        let name_lower: &str = &name_lower;
        let el = self.an.element(n).unwrap();
        let textarea_value = if self.an.textarea_values.contains(&n) { Some(()) } else { None };
        let attrs = el.attributes.iter().map(AttrView::Attr).chain(textarea_value.map(|_| AttrView::TextareaValue));
        for attribute in attrs {
            let a = match attribute {
                AttrView::Attr(a) => a,
                AttrView::TextareaValue => {
                    if name_lower != "value" {
                        continue;
                    }
                    if expected.is_none() {
                        return true;
                    }
                    // the moved children: Text and ExpressionTag nodes
                    let nodes = &self.an.ast.fragments[el.fragment].nodes;
                    let chunks: Vec<ChunkView> = nodes
                        .iter()
                        .filter_map(|&c| match &self.an.ast.nodes[c] {
                            Node::Text { data, .. } => Some(ChunkView::Text(data)),
                            Node::ExpressionTag { expression, .. } => Some(ChunkView::Expr(expression)),
                            _ => None,
                        })
                        .collect();
                    match self.chunks_match(&chunks, name_lower, expected.unwrap(), operator, case_insensitive) {
                        Some(true) => return true,
                        _ => continue,
                    }
                }
            };
            match a {
                Attr::Spread { .. } => return true,
                Attr::Directive { kind: "BindDirective", name: an, .. } if *an == name => return true,
                Attr::StyleDirective { .. } if name_lower == "style" => return true,
                Attr::Directive { kind: "ClassDirective", name: cn, .. } if name_lower == "class" => {
                    if operator == Some("~=") {
                        if Some(*cn) == expected {
                            return true;
                        }
                    } else {
                        return true;
                    }
                }
                _ => {}
            }
            let Attr::Attribute { name: attr_name, value, .. } = a else { continue };
            if !lower_eq(attr_name, name_lower) {
                continue;
            }
            if matches!(value, AttrValue::True) {
                return operator.is_none();
            }
            let Some(expected) = expected else { return true };
            if let Some(text) = text_value(value) {
                let matches = test_attribute(operator, expected, case_insensitive, text);
                if !matches && (name_lower == "class" || name_lower == "style") {
                    continue;
                }
                return matches;
            }
            let chunks: Vec<ChunkView> = super::utils::chunks(value)
                .iter()
                .map(|c| match c {
                    Chunk::Text { data, .. } => ChunkView::Text(data),
                    Chunk::Expression { expression, .. } => ChunkView::Expr(expression),
                })
                .collect();
            if let Some(true) = self.chunks_match(&chunks, name_lower, expected, operator, case_insensitive) {
                return true;
            }
        }
        false
    }

    /// The possible-values part of `attribute_matches` for a dynamic value: `Some(true)` on a
    /// match (or when it can't tell), `Some(false)` otherwise
    fn chunks_match(&self, chunks: &[ChunkView], name_lower: &str, expected: &str, operator: Option<&str>, case_insensitive: bool) -> Option<bool> {
        let mut possible_values: Vec<String> = Vec::new();
        let add = |set: &mut Vec<String>, v: String| {
            if !set.contains(&v) {
                set.push(v);
            }
        };
        let mut prev_values: Vec<String> = Vec::new();
        for chunk in chunks {
            let Some(current) = get_possible_values(chunk, name_lower == "class") else {
                return Some(true);
            };
            if !prev_values.is_empty() {
                let mut start_with_space = Vec::new();
                let mut remaining = Vec::new();
                for v in &current {
                    if v.chars().next().is_some_and(is_js_whitespace) {
                        start_with_space.push(v.clone());
                    } else {
                        remaining.push(v.clone());
                    }
                }
                if !remaining.is_empty() {
                    if !start_with_space.is_empty() {
                        for p in &prev_values {
                            add(&mut possible_values, p.clone());
                        }
                    }
                    let mut combined = Vec::new();
                    for p in &prev_values {
                        for v in &remaining {
                            combined.push(format!("{p}{v}"));
                        }
                    }
                    prev_values = combined;
                    for v in start_with_space {
                        if v.chars().next_back().is_some_and(is_js_whitespace) {
                            add(&mut possible_values, v);
                        } else {
                            prev_values.push(v);
                        }
                    }
                    continue;
                } else {
                    for p in &prev_values {
                        add(&mut possible_values, p.clone());
                    }
                    prev_values.clear();
                }
            }
            for v in &current {
                if v.chars().next_back().is_some_and(is_js_whitespace) {
                    add(&mut possible_values, v.clone());
                } else {
                    prev_values.push(v.clone());
                }
            }
            if prev_values.len() < current.len() {
                prev_values.push(" ".into());
            }
            if prev_values.len() > 20 {
                return Some(true);
            }
        }
        for p in prev_values {
            add(&mut possible_values, p);
        }
        Some(possible_values.iter().any(|v| test_attribute(operator, expected, case_insensitive, v)))
    }

    // -----------------------------------------------------------------------------------
    // tree navigation

    fn get_element_parent(&self, n: NodeId) -> Option<NodeId> {
        self.element_path(n).iter().rev().find_map(|p| {
            let id = p.node()?;
            let el = self.an.element(id)?;
            matches!(el.kind, "RegularElement" | "SvelteElement").then_some(id)
        })
    }

    fn get_ancestor_elements(&self, n: NodeId, adjacent_only: bool, seen: &mut Vec<NodeId>) -> Vec<NodeId> {
        let mut ancestors = Vec::new();
        let path = self.element_path(n).to_vec();
        let mut i = path.len();
        while i > 0 {
            i -= 1;
            let Some(pid) = path[i].node() else { continue };
            match &self.an.ast.nodes[pid] {
                Node::SnippetBlock { .. } => {
                    if !seen.contains(&pid) {
                        seen.push(pid);
                        for &site in self.an.snippet_sites.get(&pid).map(|v| v.as_slice()).unwrap_or(&[]) {
                            let more = self.get_ancestor_elements(site, adjacent_only, seen);
                            ancestors.extend(more);
                        }
                    }
                    break;
                }
                Node::Element(el) if matches!(el.kind, "RegularElement" | "SvelteElement") => {
                    if el.kind == "RegularElement" && el.name == "option" {
                        let is_direct_child = ancestors.is_empty();
                        let select = path[..i].iter().rev().find_map(|p| {
                            let id = p.node()?;
                            let e = self.an.element(id)?;
                            (e.kind == "RegularElement" && e.name == "select").then_some(id)
                        });
                        if let Some(select) = select {
                            if !adjacent_only || is_direct_child {
                                let selectedcontent = self.find_selectedcontent(P::Node(select));
                                if adjacent_only && is_direct_child {
                                    if let Some(sc) = selectedcontent {
                                        return vec![sc, pid];
                                    }
                                } else if let Some(sc) = selectedcontent {
                                    ancestors.push(sc);
                                }
                            }
                        }
                    }
                    ancestors.push(pid);
                    if adjacent_only {
                        break;
                    }
                }
                _ => {}
            }
        }
        ancestors
    }

    fn find_selectedcontent(&self, p: P<'s>) -> Option<NodeId> {
        let mut found = None;
        self.walk_template(p, &mut |me, child| {
            if found.is_some() {
                return false;
            }
            if let Some(el) = child.node().and_then(|c| me.an.element(c)) {
                if el.kind == "RegularElement" && el.name == "selectedcontent" {
                    found = child.node();
                    return false;
                }
            }
            true
        });
        found
    }

    /// Walk the template below `p` (not into JS), calling `f` for every node; `f` returns
    /// whether to descend
    fn walk_template(&self, p: P<'s>, f: &mut dyn FnMut(&Self, P<'s>) -> bool) {
        for child in template_children(self.an, p) {
            if f(self, child) {
                self.walk_template(child, f);
            }
        }
    }

    fn get_descendant_elements(&self, n: NodeId, adjacent_only: bool) -> Vec<NodeId> {
        let mut descendants = Vec::new();
        let mut seen: Vec<NodeId> = Vec::new();
        let start = match &self.an.ast.nodes[n] {
            Node::RenderTag { .. } => P::Node(n),
            Node::Element(el) => P::Fragment(el.fragment),
            _ => return descendants,
        };
        self.walk_children(start, adjacent_only, &mut descendants, &mut seen);

        if let Node::Element(el) = &self.an.ast.nodes[n] {
            if el.kind == "RegularElement" && el.name == "selectedcontent" {
                let select = self.element_path(n).iter().rev().find_map(|p| {
                    let id = p.node()?;
                    let e = self.an.element(id)?;
                    (e.kind == "RegularElement" && e.name == "select").then_some(id)
                });
                if let Some(select) = select {
                    self.walk_select_options(P::Node(select), false, adjacent_only, &mut descendants, &mut seen);
                }
            }
        }
        descendants
    }

    fn walk_select_options(&self, p: P<'s>, inside_option: bool, adjacent_only: bool, out: &mut Vec<NodeId>, seen: &mut Vec<NodeId>) {
        let is_option = p.node().and_then(|c| self.an.element(c)).is_some_and(|e| e.kind == "RegularElement" && e.name == "option");
        if is_option {
            for child in template_children(self.an, p) {
                self.walk_select_options(child, true, adjacent_only, out, seen);
            }
        } else if inside_option {
            self.walk_children(p, adjacent_only, out, seen);
        } else {
            for child in template_children(self.an, p) {
                self.walk_select_options(child, false, adjacent_only, out, seen);
            }
        }
    }

    /// `walk_children(node)` of `get_descendant_elements` (the `_` visitor runs on `p` itself)
    fn walk_children(&self, p: P<'s>, adjacent_only: bool, out: &mut Vec<NodeId>, seen: &mut Vec<NodeId>) {
        if let Some(id) = p.node() {
            match &self.an.ast.nodes[id] {
                Node::Element(el) if matches!(el.kind, "RegularElement" | "SvelteElement") => {
                    out.push(id);
                    if adjacent_only {
                        return;
                    }
                }
                Node::RenderTag { .. } => {
                    for &snippet in self.an.renderer_snippets.get(&id).map(|v| v.as_slice()).unwrap_or(&[]) {
                        if seen.contains(&snippet) {
                            continue;
                        }
                        seen.push(snippet);
                        if let Node::SnippetBlock { body, .. } = &self.an.ast.nodes[snippet] {
                            self.walk_children(P::Fragment(*body), adjacent_only, out, seen);
                        }
                    }
                    return;
                }
                _ => {}
            }
        }
        for child in template_children(self.an, p) {
            self.walk_children(child, adjacent_only, out, seen);
        }
    }

    fn get_possible_element_siblings(&self, n: NodeId, direction: bool, adjacent_only: bool, seen: &mut Vec<NodeId>) -> SiblingMap {
        let mut result: SiblingMap = Vec::new();
        let path = self.element_path(n).to_vec();
        let mut current = P::Node(n);
        let mut i = path.len() as isize;
        loop {
            i -= 1;
            if i < 0 {
                break;
            }
            let fragment = path[i as usize];
            i -= 1;
            let nodes: Vec<NodeId> = self.an.fragment_nodes(fragment).to_vec();
            let pos = current.node().and_then(|c| nodes.iter().position(|&x| x == c));
            let mut j: isize = match pos {
                Some(p) => p as isize + if direction == FORWARD { 1 } else { -1 },
                None => if direction == FORWARD { 0 } else { -2 },
            };
            while j >= 0 && (j as usize) < nodes.len() {
                let node = nodes[j as usize];
                match &self.an.ast.nodes[node] {
                    Node::Element(el) if el.kind == "RegularElement" => {
                        let has_slot = el.attributes.iter().any(|a| matches!(a, Attr::Attribute { name, .. } if name.eq_ignore_ascii_case("slot")));
                        if !has_slot {
                            set(&mut result, node, Exists::Definitely);
                            if adjacent_only {
                                return result;
                            }
                        }
                    }
                    other if is_block(other) || matches!(other, Node::Element(el) if el.kind == "Component") => {
                        if matches!(other, Node::Element(el) if matches!(el.kind, "SlotElement" | "Component")) {
                            set(&mut result, node, Exists::Probably);
                        }
                        let possible_last_child = self.get_possible_nested_siblings(node, direction, adjacent_only, &mut Vec::new());
                        add_to_map(&possible_last_child, &mut result);
                        let is_component = matches!(other, Node::Element(el) if el.kind == "Component");
                        if adjacent_only && !is_component && has_definite_elements(&possible_last_child) {
                            return result;
                        }
                    }
                    Node::Element(el) if el.kind == "SvelteElement" => set(&mut result, node, Exists::Probably),
                    Node::RenderTag { .. } => {
                        set(&mut result, node, Exists::Probably);
                        for &snippet in self.an.renderer_snippets.get(&node).map(|v| v.as_slice()).unwrap_or(&[]) {
                            let m = self.get_possible_nested_siblings(snippet, direction, adjacent_only, &mut Vec::new());
                            add_to_map(&m, &mut result);
                        }
                    }
                    _ => {}
                }
                j += if direction == FORWARD { 1 } else { -1 };
            }

            if i < 0 {
                break;
            }
            current = path[i as usize];
            let Some(cid) = current.node() else { break };
            let cnode = &self.an.ast.nodes[cid];
            if matches!(cnode, Node::Element(el) if matches!(el.kind, "Component" | "SvelteComponent" | "SvelteSelf")) {
                continue;
            }
            if let Node::SnippetBlock { .. } = cnode {
                if seen.contains(&cid) {
                    break;
                }
                seen.push(cid);
                let sites = self.an.snippet_sites.get(&cid).cloned().unwrap_or_default();
                for &site in &sites {
                    let siblings = self.get_possible_element_siblings(site, direction, adjacent_only, seen);
                    add_to_map(&siblings, &mut result);
                    if adjacent_only && sites.len() == 1 && has_definite_elements(&siblings) {
                        return result;
                    }
                }
            }
            if !is_block(cnode) {
                break;
            }
            if let Node::EachBlock { body, .. } = cnode {
                if fragment == P::Fragment(*body) {
                    let m = self.get_possible_nested_siblings(cid, direction, adjacent_only, &mut Vec::new());
                    add_to_map(&m, &mut result);
                }
            }
        }
        result
    }

    fn get_possible_nested_siblings(&self, n: NodeId, direction: bool, adjacent_only: bool, seen: &mut Vec<NodeId>) -> SiblingMap {
        let node = &self.an.ast.nodes[n];
        let mut fragments: Vec<Option<usize>> = Vec::new();
        match node {
            Node::EachBlock { body, fallback, .. } => fragments.extend([Some(*body), *fallback]),
            Node::IfBlock { consequent, alternate, .. } => fragments.extend([Some(*consequent), *alternate]),
            Node::AwaitBlock { pending, then, catch, .. } => fragments.extend([*pending, *then, *catch]),
            Node::KeyBlock { fragment, .. } => fragments.push(Some(*fragment)),
            Node::Element(el) if el.kind == "SlotElement" => fragments.push(Some(el.fragment)),
            Node::SnippetBlock { body, .. } => {
                if seen.contains(&n) {
                    return Vec::new();
                }
                seen.push(n);
                fragments.push(Some(*body));
            }
            Node::Element(el) if el.kind == "Component" => {
                fragments.push(Some(el.fragment));
                for &s in self.an.renderer_snippets.get(&n).map(|v| v.as_slice()).unwrap_or(&[]) {
                    if let Node::SnippetBlock { body, .. } = &self.an.ast.nodes[s] {
                        fragments.push(Some(*body));
                    }
                }
            }
            _ => {}
        }
        let mut result: SiblingMap = Vec::new();
        let mut exhaustive = !matches!(node, Node::Element(el) if el.kind == "SlotElement") && !matches!(node, Node::SnippetBlock { .. });
        for f in fragments {
            let Some(f) = f else {
                exhaustive = false;
                continue;
            };
            let map = self.loop_child(&self.an.ast.fragments[f].nodes, direction, adjacent_only, seen);
            exhaustive = exhaustive && has_definite_elements(&map);
            add_to_map(&map, &mut result);
        }
        if !exhaustive {
            for entry in result.iter_mut() {
                entry.1 = Exists::Probably;
            }
        }
        result
    }

    fn loop_child(&self, children: &[NodeId], direction: bool, adjacent_only: bool, seen: &mut Vec<NodeId>) -> SiblingMap {
        let mut result: SiblingMap = Vec::new();
        let mut i: isize = if direction == FORWARD { 0 } else { children.len() as isize - 1 };
        while i >= 0 && (i as usize) < children.len() {
            let child = children[i as usize];
            match &self.an.ast.nodes[child] {
                Node::Element(el) if el.kind == "RegularElement" => {
                    set(&mut result, child, Exists::Definitely);
                    if adjacent_only {
                        break;
                    }
                }
                Node::Element(el) if el.kind == "SvelteElement" => set(&mut result, child, Exists::Probably),
                Node::RenderTag { .. } => {
                    for &snippet in self.an.renderer_snippets.get(&child).map(|v| v.as_slice()).unwrap_or(&[]) {
                        let m = self.get_possible_nested_siblings(snippet, direction, adjacent_only, seen);
                        add_to_map(&m, &mut result);
                    }
                }
                other if is_block(other) => {
                    let m = self.get_possible_nested_siblings(child, direction, adjacent_only, seen);
                    add_to_map(&m, &mut result);
                    if adjacent_only && has_definite_elements(&m) {
                        break;
                    }
                }
                _ => {}
            }
            i += if direction == FORWARD { 1 } else { -1 };
        }
        result
    }
}

fn set(map: &mut SiblingMap, n: NodeId, e: Exists) {
    match map.iter_mut().find(|(k, _)| *k == n) {
        Some(entry) => entry.1 = e,
        None => map.push((n, e)),
    }
}

fn is_block(node: &Node) -> bool {
    matches!(node, Node::IfBlock { .. } | Node::EachBlock { .. } | Node::AwaitBlock { .. } | Node::KeyBlock { .. })
        || matches!(node, Node::Element(el) if el.kind == "SlotElement")
}

impl PartialEq for P<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.key() == other.key()
    }
}

/// The template children of a node (fragments, nodes; not attributes or JS)
fn template_children<'s>(an: &Analyzer<'s>, p: P<'s>) -> Vec<P<'s>> {
    let ast = an.ast;
    match p {
        P::Fragment(f) => ast.fragments[f].nodes.iter().map(|&n| P::Node(n)).collect(),
        P::Node(n) => match &ast.nodes[n] {
            Node::IfBlock { consequent, alternate, .. } => {
                std::iter::once(*consequent).chain(*alternate).map(P::Fragment).collect()
            }
            Node::EachBlock { body, fallback, .. } => std::iter::once(*body).chain(*fallback).map(P::Fragment).collect(),
            Node::AwaitBlock { pending, then, catch, .. } => {
                [pending, then, catch].into_iter().flatten().map(|f| P::Fragment(*f)).collect()
            }
            Node::KeyBlock { fragment, .. } => vec![P::Fragment(*fragment)],
            Node::SnippetBlock { body, .. } => vec![P::Fragment(*body)],
            Node::Element(el) => {
                if an.textarea_values.contains(&n) {
                    // children moved into the `value` attribute: no elements there
                    vec![P::Fragment(el.fragment)].into_iter().filter(|_| false).collect()
                } else {
                    vec![P::Fragment(el.fragment)]
                }
            }
            _ => Vec::new(),
        },
        _ => Vec::new(),
    }
}

fn sel_has_nesting(s: Sel) -> bool {
    match s {
        Sel::Nesting | Sel::Real(SimpleSelector::Nesting { .. }) => true,
        Sel::Real(SimpleSelector::PseudoClass { args: Some(args), .. } | SimpleSelector::PseudoElement { args: Some(args), .. }) => args
            .children
            .iter()
            .any(|c| c.children.iter().any(|r| r.selectors.iter().any(|s| sel_has_nesting(Sel::Real(s))))),
        _ => false,
    }
}

/// `name.replace(/\\(.)/g, '$1')`
fn unescape(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some(n) if n != '\n' && n != '\r' && n != '\u{2028}' && n != '\u{2029}' => out.push(n),
                Some(n) => {
                    out.push(c);
                    out.push(n);
                }
                None => out.push(c),
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn unquote(s: &str) -> String {
    let b = s.as_bytes();
    if (!b.is_empty() && b[0] == b[b.len() - 1] && b[0] == b'\'') || b.first() == Some(&b'"') {
        // `str.slice(1, str.length - 1)`
        let chars: Vec<char> = s.chars().collect();
        if chars.len() >= 2 {
            return chars[1..chars.len() - 1].iter().collect();
        }
        return String::new();
    }
    s.to_string()
}

fn test_attribute(operator: Option<&str>, expected: &str, case_insensitive: bool, value: &str) -> bool {
    if !case_insensitive {
        return test_attribute_exact(operator, expected, value);
    }
    test_attribute_exact(operator, &expected.to_lowercase(), &value.to_lowercase())
}

fn test_attribute_exact(operator: Option<&str>, expected: &str, value: &str) -> bool {
    match operator {
        Some("=") => value == expected,
        Some("~=") => {
            if value.is_ascii() {
                value
                    .as_bytes()
                    .split(|b| matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c))
                    .any(|v| v == expected.as_bytes())
            } else {
                value.split(is_js_whitespace).any(|v| v == expected)
            }
        }
        Some("|=") => format!("{value}-").starts_with(&format!("{expected}-")),
        Some("^=") => value.starts_with(expected),
        Some("$=") => value.ends_with(expected),
        Some("*=") => value.contains(expected),
        _ => false,
    }
}

/// `a.toLowerCase() === lower` (`lower` already lowercase)
fn lower_eq(a: &str, lower: &str) -> bool {
    if a.is_ascii() { a.eq_ignore_ascii_case(lower) } else { a.to_lowercase() == lower }
}

/// `a.toLowerCase() === b.toLowerCase()`
fn lower_eq_both(a: &str, b: &str) -> bool {
    if a.is_ascii() && b.is_ascii() { a.eq_ignore_ascii_case(b) } else { a.to_lowercase() == b.to_lowercase() }
}

const CASE_INSENSITIVE_ATTRIBUTES: &[&str] = &[
    "accept-charset", "autocapitalize", "autocomplete", "behavior", "charset", "crossorigin", "decoding", "dir",
    "direction", "draggable", "enctype", "enterkeyhint", "fetchpriority", "formenctype", "formmethod", "formtarget",
    "hidden", "http-equiv", "inputmode", "kind", "loading", "method", "preload", "referrerpolicy", "rel", "rev", "role",
    "rules", "scope", "shape", "spellcheck", "target", "translate", "type", "valign", "wrap",
];

enum AttrView<'s> {
    Attr(&'s Attr<'s>),
    /// the `value` attribute made of a `<textarea>`'s children
    TextareaValue,
}

enum ChunkView<'a, 's> {
    Text(&'a str),
    Expr(&'a Expr<'s>),
}

/// `get_possible_values(chunk, is_class)`
fn get_possible_values(chunk: &ChunkView, is_class: bool) -> Option<Vec<String>> {
    let mut values = Values { set: Vec::new(), unknown: false };
    match chunk {
        ChunkView::Text(t) => values.add(JsVal::Str(t.to_string())),
        ChunkView::Expr(e) => gather(nodes::template_expr(e), is_class, &mut values, false),
    }
    if values.unknown {
        return None;
    }
    Some(values.set.iter().map(JsVal::to_js_string).collect())
}

fn literal_string(p: P) -> Option<String> {
    use AstKind as K;
    Some(match p {
        P::Js(K::StringLiteral(s)) => s.value.to_string(),
        P::Js(K::NumericLiteral(n)) => super::visit::js_number(n.value),
        P::Js(K::BooleanLiteral(b)) => b.value.to_string(),
        P::Js(K::NullLiteral(_)) => "null".into(),
        P::Js(K::BigIntLiteral(b)) => b.raw.as_ref().map_or_else(String::new, |r| r.trim_end_matches('n').replace('_', "")),
        P::Js(K::RegExpLiteral(r)) => format!("/{}/{}", r.regex.pattern.text, r.regex.flags),
        P::TplExpr(Expr::Literal { value, .. }) => value.clone(),
        _ => return None,
    })
}

fn gather(p: P, is_class: bool, set: &mut Values, is_nested: bool) {
    use AstKind as K;
    if set.unknown {
        return;
    }
    if let Some(s) = literal_string(p) {
        set.add(JsVal::Str(s));
        return;
    }
    match p {
        P::Js(K::ConditionalExpression(c)) => {
            gather(nodes::expr(&c.consequent), is_class, set, is_nested);
            gather(nodes::expr(&c.alternate), is_class, set, is_nested);
        }
        P::Js(K::LogicalExpression(l)) => {
            if l.operator == LogicalOperator::And {
                let mut left = Values { set: Vec::new(), unknown: false };
                gather(nodes::expr(&l.left), is_class, &mut left, is_nested);
                if left.unknown {
                    if !is_class || !is_nested {
                        set.add(JsVal::Str(String::new()));
                        set.add(JsVal::False);
                        set.add(JsVal::NaN);
                        set.add(JsVal::Zero);
                    }
                } else {
                    for v in left.set {
                        // `!value && value != undefined`: only falsy strings remain (literals are strings)
                        let falsy = match &v {
                            JsVal::Str(s) => s.is_empty(),
                            _ => true,
                        };
                        if falsy && (!is_class || !is_nested) {
                            set.add(v);
                        }
                    }
                }
                gather(nodes::expr(&l.right), is_class, set, is_nested);
            } else {
                gather(nodes::expr(&l.left), is_class, set, is_nested);
                gather(nodes::expr(&l.right), is_class, set, is_nested);
            }
        }
        P::Js(K::ArrayExpression(a)) if is_class => {
            for el in &a.elements {
                match el {
                    ArrayExpressionElement::Elision(_) => {}
                    ArrayExpressionElement::SpreadElement(s) => gather(P::Js(K::SpreadElement(s)), is_class, set, true),
                    _ => gather(nodes::expr(el.as_expression().unwrap()), is_class, set, true),
                }
            }
        }
        P::Js(K::ObjectExpression(o)) if is_class => {
            for prop in &o.properties {
                match prop {
                    ObjectPropertyKind::ObjectProperty(p) if !p.computed => {
                        let key = nodes::property_key(&p.key);
                        if let Some(id) = super::scope::ident(key) {
                            set.add(JsVal::Str(id.name.to_string()));
                        } else if let Some(s) = literal_string(key) {
                            set.add(JsVal::Str(s));
                        } else {
                            set.unknown = true;
                        }
                    }
                    _ => set.unknown = true,
                }
            }
        }
        _ => set.unknown = true,
    }
}
