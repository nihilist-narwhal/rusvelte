//! A port of `phases/3-transform/css/index.js` (`render_stylesheet`): the component's CSS with
//! selectors scoped by the hash class, `:global` removed, unused rules commented out (or
//! removed when minifying) and keyframes renamed.
//!
//! The JS walks the CSS AST with zimmerframe; the only parts of the walk's `path` the visitors
//! look at are tracked as arguments: whether an ancestor rule is a `:global` block, and whether
//! an ancestor complex selector is unused.
//!
//! The source map comes from `addSourcemapLocation` on every visited node's start and end and
//! `generateMap` (see `render_stylesheet_with_mappings`). Not ported: the preprocessor map merge.

use crate::analyze::css::Meta;
use crate::css::{Atrule, BlockChild, ComplexSelector, Declaration, RelativeSelector, Rule, SelectorList, SimpleSelector, StyleSheet};
use crate::magic_string::{MagicString, MagicStringError};

type Result<T> = std::result::Result<T, MagicStringError>;

fn addr<T>(r: &T) -> usize {
    r as *const T as usize
}

pub struct RenderOptions<'a> {
    /// `analysis.css.hash`
    pub hash: &'a str,
    /// `analysis.inject_styles && !options.dev`
    pub minify: bool,
    /// `options.dev` (empty rules are kept in dev mode)
    pub dev: bool,
}

struct State<'a, 's> {
    code: MagicString<'s>,
    original: &'s str,
    hash: &'a str,
    minify: bool,
    dev: bool,
    selector: String,
    meta: &'a Meta<'s>,
    /// `code.addSourcemapLocation(...)` of the visited nodes' starts and ends
    locations: rustc_hash::FxHashSet<usize>,
}

/// Who a selector list belongs to (`path.at(-1)`)
#[derive(Clone, Copy)]
enum ListParent<'c> {
    Rule(&'c Rule),
    /// the arguments of a pseudo class
    PseudoClass,
}

/// `render_stylesheet(source, analysis, options).code`
pub fn render_stylesheet<'s>(source: &'s str, sheet: &StyleSheet, meta: &Meta<'s>, options: &RenderOptions) -> Result<String> {
    Ok(render(source, sheet, meta, options)?.code.to_string())
}

/// [`render_stylesheet`], with the `mappings` of its source map (`generateMap()`)
pub fn render_stylesheet_with_mappings<'s>(source: &'s str, sheet: &StyleSheet, meta: &Meta<'s>, options: &RenderOptions) -> Result<(String, String)> {
    let state = render(source, sheet, meta, options)?;
    let mappings = crate::magic_string::encode_mappings(&state.code.decoded_map(false, &state.locations));
    Ok((state.code.to_string(), mappings))
}

fn render<'a, 's>(source: &'s str, sheet: &StyleSheet, meta: &'a Meta<'s>, options: &'a RenderOptions) -> Result<State<'a, 's>> {
    let mut state = State {
        code: MagicString::new(source),
        original: source,
        hash: options.hash,
        minify: options.minify,
        dev: options.dev,
        selector: format!(".{}", options.hash),
        meta,
        locations: Default::default(),
    };

    state.locate(sheet.start, sheet.end);
    for child in &sheet.children {
        state.block_child(child, false)?;
    }
    // the walk also visits the stylesheet's `comments`
    for comment in &sheet.comments {
        state.locate(comment.start, comment.end);
    }

    state.code.remove(0, sheet.content_start)?;
    state.code.remove(sheet.content_end, source.len())?;
    if state.minify {
        state.remove_preceding_whitespace(sheet.content_end)?;
    }
    Ok(state)
}

/// `/\s/` (JS whitespace)
fn is_js_whitespace(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'..='\u{200a}'
            | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}'
    )
}

/// `regex_css_name_boundary`: `/^[\s,;}]$/`
fn is_name_boundary(c: char) -> bool {
    is_js_whitespace(c) || matches!(c, ',' | ';' | '}')
}

/// `remove_css_prefix`
fn remove_css_prefix(name: &str) -> &str {
    for prefix in ["-webkit-", "-moz-", "-o-", "-ms-"] {
        if let Some(rest) = name.strip_prefix(prefix) {
            return rest;
        }
    }
    name
}

fn is_keyframes(a: &Atrule) -> bool {
    remove_css_prefix(&a.name) == "keyframes"
}

impl<'s> State<'_, 's> {
    /// The universal visitor: `addSourcemapLocation(node.start)` and `(node.end)`
    fn locate(&mut self, start: usize, end: usize) {
        self.locations.insert(start);
        self.locations.insert(end);
    }

    fn is_global_block(&self, rule: &Rule) -> bool {
        self.meta.is_global_block.contains(&addr(rule))
    }

    fn is_used(&self, rule: &Rule) -> bool {
        rule.prelude.children.iter().any(|c| self.meta.used.contains(&addr(c)))
    }

    /// The offset of the character before `i`, if it is whitespace
    fn whitespace_before(&self, i: usize) -> Option<usize> {
        let c = self.original[..i].chars().next_back()?;
        is_js_whitespace(c).then(|| i - c.len_utf8())
    }

    /// Walk backwards until we find a non-whitespace character
    fn remove_preceding_whitespace(&mut self, end: usize) -> Result<()> {
        let mut start = end;
        while let Some(prev) = self.whitespace_before(start) {
            start = prev;
        }
        if start < end {
            self.code.remove(start, end)?;
        }
        Ok(())
    }

    fn block_child(&mut self, child: &BlockChild, in_global_block: bool) -> Result<()> {
        match child {
            BlockChild::Rule(r) => self.locate(r.start, r.end),
            BlockChild::Atrule(a) => self.locate(a.start, a.end),
            BlockChild::Declaration(d) => self.locate(d.start, d.end),
        }
        match child {
            BlockChild::Rule(r) => self.rule(r, in_global_block),
            BlockChild::Atrule(a) => self.atrule(a, in_global_block),
            BlockChild::Declaration(d) => self.declaration(d),
        }
    }

    fn atrule(&mut self, node: &Atrule, in_global_block: bool) -> Result<()> {
        if is_keyframes(node) {
            let bytes = self.original.as_bytes();
            let mut start = node.start + node.name.len() + 1;
            while bytes.get(start) == Some(&b' ') {
                start += 1;
            }
            if node.prelude.starts_with("-global-") {
                self.code.remove(start, start + 8)?;
            } else if !in_global_block {
                self.code.prepend_right(start, &format!("{}-", self.hash))?;
            }
            return Ok(()); // don't transform anything within
        }
        if let Some(block) = &node.block {
            self.locate(block.start, block.end);
            for child in &block.children {
                self.block_child(child, in_global_block)?;
            }
        }
        Ok(())
    }

    fn declaration(&mut self, node: &Declaration) -> Result<()> {
        let lower = node.property.to_lowercase();
        let property = remove_css_prefix(&lower);
        if property == "animation" || property == "animation-name" {
            let mut index = node.start + node.property.len() + 1;
            let mut name_start = index;
            while index < self.original.len() {
                let Some(c) = self.original[index..].chars().next() else { break };
                if is_name_boundary(c) {
                    let name = &self.original[name_start..index];
                    if self.meta.keyframes.iter().any(|k| k == name) {
                        self.code.prepend_right(name_start, &format!("{}-", self.hash))?;
                    }
                    if c == ';' || c == '}' {
                        break;
                    }
                    name_start = index + c.len_utf8();
                }
                index += c.len_utf8();
            }
        } else if self.minify {
            self.remove_preceding_whitespace(node.start)?;
            // Don't minify whitespace in custom properties, since some browsers (Chromium < 99)
            // treat --foo: ; and --foo:; differently
            if !node.property.starts_with("--") {
                let start = node.start + node.property.len() + 1;
                let mut end = start;
                while let Some(c) = self.original.get(end..).and_then(|s| s.chars().next()).filter(|c| is_js_whitespace(*c)) {
                    end += c.len_utf8();
                }
                if end > start {
                    self.code.remove(start, end)?;
                }
            }
        }
        Ok(())
    }

    fn rule(&mut self, node: &Rule, in_global_block: bool) -> Result<()> {
        if self.minify {
            self.remove_preceding_whitespace(node.start)?;
            self.remove_preceding_whitespace(node.block.end - 1)?;
        }

        // keep empty rules in dev, because it's convenient to
        // see them in devtools
        if !self.dev && self.is_empty(node, in_global_block) {
            if self.minify {
                self.code.remove(node.start, node.end)?;
            } else {
                self.code.prepend_right(node.start, "/* (empty) ")?;
                self.code.append_left(node.end, "*/")?;
                self.escape_comment_close(node)?;
            }
            return Ok(());
        }

        if !self.is_used(node) && !in_global_block {
            if self.minify {
                self.code.remove(node.start, node.end)?;
            } else {
                self.code.prepend_right(node.start, "/* (unused) ")?;
                self.code.append_left(node.end, "*/")?;
                self.escape_comment_close(node)?;
            }
            return Ok(());
        }

        let is_global_block = self.is_global_block(node);
        if is_global_block {
            let selector = &node.prelude.children[0];
            if node.prelude.children.len() == 1 && selector.children.len() == 1 && selector.children[0].selectors.len() == 1 {
                // `:global {...}`
                if self.minify {
                    self.code.remove(node.start, node.block.start + 1)?;
                    self.code.remove(node.block.end - 1, node.end)?;
                } else {
                    self.code.prepend_right(node.start, "/* ")?;
                    self.code.append_left(node.block.start + 1, "*/")?;

                    self.code.prepend_right(node.block.end - 1, "/*")?;
                    self.code.append_left(node.block.end, "*/")?;
                }

                // don't recurse into selectors but visit the body
                self.locate(node.block.start, node.block.end);
                for child in &node.block.children {
                    self.block_child(child, true)?;
                }
                return Ok(());
            }
        }

        let inner = in_global_block || is_global_block;
        let mut bumped = false;
        self.selector_list(&node.prelude, ListParent::Rule(node), inner, false, &mut bumped)?;
        self.locate(node.block.start, node.block.end);
        for child in &node.block.children {
            self.block_child(child, inner)?;
        }
        Ok(())
    }

    /// `in_unused_complex`: an ancestor ComplexSelector is unused. `bumped` is the
    /// `state.specificity` object the list's selectors share (a rule's list makes its own).
    fn selector_list(
        &mut self,
        node: &SelectorList,
        parent: ListParent,
        in_global_block: bool,
        in_unused_complex: bool,
        bumped: &mut bool,
    ) -> Result<()> {
        self.locate(node.start, node.end);
        let parent_is_global_block = matches!(parent, ListParent::Rule(r) if self.is_global_block(r));

        // Only add comments if we're not inside a complex selector that itself is unused or a global block
        if (!in_global_block || (node.children.len() > 1 && parent_is_global_block)) && !in_unused_complex {
            let children = &node.children;
            let mut pruning = false;
            let mut prune_start = children[0].start;
            let mut last = prune_start;
            let mut has_previous_used = false;

            for (i, selector) in children.iter().enumerate() {
                let used = self.meta.used.contains(&addr(selector));
                if used == pruning {
                    if pruning {
                        let mut j = selector.start;
                        while self.original.as_bytes()[j] != b',' {
                            j -= 1;
                        }
                        let at = if has_previous_used { j } else { j + 1 };
                        if self.minify {
                            self.code.remove(prune_start, at)?;
                        } else {
                            self.code.append_right(at, "*/")?;
                        }
                    } else if i == 0 {
                        if self.minify {
                            prune_start = selector.start;
                        } else {
                            self.code.prepend_right(selector.start, "/* (unused) ")?;
                        }
                    } else if self.minify {
                        prune_start = last;
                    } else {
                        self.code.overwrite(last, selector.start, " /* (unused) ", false)?;
                    }
                    pruning = !pruning;
                }

                if !pruning && used {
                    has_previous_used = true;
                }

                last = selector.end;
            }

            if pruning {
                if self.minify {
                    self.code.remove(prune_start, last)?;
                } else {
                    self.code.append_left(last, "*/")?;
                }
            }
        }

        // if this selector list belongs to a rule, require a specificity bump for the
        // first scoped selector but only if we're at the top level
        let mut own = false;
        let specificity = match parent {
            ListParent::Rule(rule) => {
                let mut r = self.meta.parent_rule(rule);
                while let Some(x) = r {
                    if self.meta.has_local_selectors.contains(&addr(x)) {
                        own = true;
                        break;
                    }
                    r = self.meta.parent_rule(x);
                }
                &mut own
            }
            // if we're in a `:is(...)` or whatever, keep existing specificity bump state
            ListParent::PseudoClass => bumped,
        };

        for complex in &node.children {
            self.complex(complex, in_global_block, in_unused_complex, specificity)?;
        }
        Ok(())
    }

    fn complex(&mut self, node: &ComplexSelector, in_global_block: bool, in_unused_complex: bool, bumped: &mut bool) -> Result<()> {
        let before_bumped = *bumped;
        self.locate(node.start, node.end);
        for relative in &node.children {
            self.locate(relative.start, relative.end);
            if let Some(c) = &relative.combinator {
                self.locate(c.start, c.end);
            }
            for selector in &relative.selectors {
                self.locate(sel_start(selector), sel_end(selector));
            }
        }

        for relative in &node.children {
            if self.meta.rel_is_global.contains(&addr(relative)) {
                let global = &relative.selectors[0];
                self.remove_global_pseudo_class(global, relative.combinator.as_ref().map(|c| c.name))?;

                let parent_rule = self.meta.complex_rule.get(&addr(node)).and_then(|r| self.meta.parent_rule(r));
                if let (Some(_), SimpleSelector::PseudoClass { start, args: None, .. }) = (parent_rule, global) {
                    if relative.combinator.is_none() {
                        // div { :global.x { ... } } becomes div { &.x { ... } }
                        self.code.prepend_right(*start, "&")?;
                    }
                    // (the JS then deletes a comma between multiple `:global` selectors, under a
                    // condition that never holds: `children.length === index - 1`)
                }
                continue;
            } else {
                // for any :global() or :global at the middle of compound selector
                for selector in &relative.selectors {
                    if matches!(selector, SimpleSelector::PseudoClass { name, .. } if name == "global") {
                        self.remove_global_pseudo_class(selector, None)?;
                    }
                }
            }

            if self.meta.rel_scoped.contains(&addr(relative)) {
                self.scope(relative, bumped)?;
            }
        }

        // context.next()
        let in_unused = in_unused_complex || !self.meta.used.contains(&addr(node));
        for relative in &node.children {
            for selector in &relative.selectors {
                if let SimpleSelector::PseudoClass { name, args: Some(args), .. } = selector {
                    if matches!(name.as_str(), "is" | "where" | "has" | "not") {
                        self.selector_list(args, ListParent::PseudoClass, in_global_block, in_unused, bumped)?;
                    }
                }
            }
        }

        *bumped = before_bumped;
        Ok(())
    }

    /// Add the scoping class to a scoped relative selector
    fn scope(&mut self, relative: &RelativeSelector, bumped: &mut bool) -> Result<()> {
        if relative.selectors.len() == 1 {
            // skip standalone :is/:where/& selectors
            if matches!(&relative.selectors[0], SimpleSelector::PseudoClass { name, .. } if name == "is" || name == "where") {
                return Ok(());
            }
        }

        if relative.selectors.iter().any(|s| matches!(s, SimpleSelector::Nesting { .. })) {
            return Ok(());
        }

        // for the first occurrence, we use a classname selector, so that every
        // encapsulated selector gets a +0-1-0 specificity bump. thereafter,
        // we use a `:where` selector, which does not affect specificity
        let modifier = if *bumped { format!(":where({})", self.selector) } else { self.selector.clone() };
        *bumped = true;

        for (i, selector) in relative.selectors.iter().enumerate().rev() {
            match selector {
                SimpleSelector::PseudoElement { name, start, .. } | SimpleSelector::PseudoClass { name, start, .. } => {
                    if name != "root" && name != "host" && i == 0 {
                        self.code.prepend_right(*start, &modifier)?;
                    }
                    continue;
                }
                SimpleSelector::Type { name, namespace: None, start, end } if name == "*" => {
                    self.code.update(*start, *end, &modifier)?;
                }
                _ => {
                    self.code.append_left(sel_end(selector), &modifier)?;
                }
            }
            break;
        }
        Ok(())
    }

    fn remove_global_pseudo_class(&mut self, selector: &SimpleSelector, combinator: Option<&str>) -> Result<()> {
        let SimpleSelector::PseudoClass { start: sel_start, end: sel_end, args, .. } = selector else { return Ok(()) };
        if args.is_none() {
            let mut start = *sel_start;
            if combinator == Some(" ") {
                // div :global.x becomes div.x
                while let Some(prev) = self.whitespace_before(start) {
                    start = prev;
                }
            }
            // update(...), not remove(...) because there could be a closing unused comment at the end
            self.code.update(start, sel_start + ":global".len(), "")?;
        } else {
            self.code.remove(*sel_start, sel_start + ":global(".len())?;
            self.code.remove(sel_end - 1, *sel_end)?;
        }
        Ok(())
    }

    fn is_empty(&self, rule: &Rule, in_global_block: bool) -> bool {
        if self.is_global_block(rule) {
            return rule.block.children.is_empty();
        }
        for child in &rule.block.children {
            match child {
                BlockChild::Declaration(_) => return false,
                BlockChild::Rule(r) => {
                    if (self.is_used(r) || in_global_block) && !self.is_empty(r, in_global_block) {
                        return false;
                    }
                }
                BlockChild::Atrule(a) => {
                    if a.block.as_ref().is_none_or(|b| !b.children.is_empty()) {
                        return false;
                    }
                }
            }
        }
        true
    }

    fn escape_comment_close(&mut self, node: &Rule) -> Result<()> {
        let bytes = self.original.as_bytes();
        let mut escaped = false;
        let mut in_comment = false;
        let mut i = node.start;
        while i < node.end {
            if escaped {
                escaped = false;
            } else {
                let c = bytes[i];
                if in_comment {
                    if c == b'*' && bytes.get(i + 1) == Some(&b'/') {
                        i += 1;
                        self.code.prepend_right(i, "\\")?;
                        in_comment = false;
                    }
                } else if c == b'\\' {
                    escaped = true;
                } else if c == b'/' && {
                    i += 1;
                    bytes.get(i) == Some(&b'*')
                } {
                    in_comment = true;
                }
            }
            i += 1;
        }
        Ok(())
    }
}

fn sel_start(s: &SimpleSelector) -> usize {
    match s {
        SimpleSelector::Nesting { start, .. }
        | SimpleSelector::Type { start, .. }
        | SimpleSelector::Id { start, .. }
        | SimpleSelector::Class { start, .. }
        | SimpleSelector::PseudoElement { start, .. }
        | SimpleSelector::PseudoClass { start, .. }
        | SimpleSelector::Attribute { start, .. }
        | SimpleSelector::Nth { start, .. }
        | SimpleSelector::Percentage { start, .. } => *start,
    }
}

fn sel_end(s: &SimpleSelector) -> usize {
    match s {
        SimpleSelector::Nesting { end, .. }
        | SimpleSelector::Type { end, .. }
        | SimpleSelector::Id { end, .. }
        | SimpleSelector::Class { end, .. }
        | SimpleSelector::PseudoElement { end, .. }
        | SimpleSelector::PseudoClass { end, .. }
        | SimpleSelector::Attribute { end, .. }
        | SimpleSelector::Nth { end, .. }
        | SimpleSelector::Percentage { end, .. } => *end,
    }
}
