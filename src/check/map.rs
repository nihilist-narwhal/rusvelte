//! Mapping TypeScript diagnostics on generated files back to `.svelte` sources, with the
//! language server's filtering (`mapSvelteCheckDiagnostics` → `mapAndFilterDiagnostics`).
//!
//! Positions are LSP positions: 0-based lines, UTF-16 characters. Texts are handled as UTF-16
//! here so the index arithmetic matches the JS.

use super::tsc::{CliDiagnostic, Severity};
use crate::legacy::{LAttr, LAttrValue, LChunk, LElement, LNode};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    pub line: i64,
    pub character: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    pub start: Position,
    pub end: Position,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Code {
    Num(u32),
    Str(String),
}

#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub range: Range,
    pub severity: Severity,
    pub source: &'static str,
    pub message: String,
    pub code: Option<Code>,
    pub code_description: Option<String>,
    /// no file position (e.g. a tsconfig error)
    pub position_unknown: bool,
}

/// A text as UTF-16 units, with the line tables the JS uses
pub struct U16Text {
    pub units: Vec<u16>,
    lsp_lines: std::cell::OnceCell<Vec<usize>>,
}

impl U16Text {
    pub fn new(s: &str) -> Self {
        U16Text { units: s.encode_utf16().collect(), lsp_lines: Default::default() }
    }

    pub fn len(&self) -> usize {
        self.units.len()
    }

    pub fn is_empty(&self) -> bool {
        self.units.is_empty()
    }

    /// `getLineOffsets` (svelte-language-server): breaks on `\r\n`, `\n` and `\r`
    fn lsp_line_offsets(&self) -> &[usize] {
        self.lsp_lines.get_or_init(|| {
            let t = &self.units;
            let mut offsets = Vec::new();
            let mut is_line_start = true;
            let mut i = 0;
            while i < t.len() {
                if is_line_start {
                    offsets.push(i);
                }
                let ch = t[i];
                is_line_start = ch == b'\r' as u16 || ch == b'\n' as u16;
                if ch == b'\r' as u16 && i + 1 < t.len() && t[i + 1] == b'\n' as u16 {
                    i += 1;
                }
                i += 1;
            }
            if is_line_start && !t.is_empty() {
                offsets.push(t.len());
            }
            offsets
        })
    }

    /// `positionAt`
    pub fn position_at(&self, offset: i64) -> Position {
        let offset = offset.clamp(0, self.len() as i64) as usize;
        let lines = self.lsp_line_offsets();
        if lines.is_empty() {
            return Position { line: 0, character: offset as i64 };
        }
        let (mut low, mut high) = (0i64, lines.len() as i64);
        while low <= high {
            let mid = (low + high) / 2;
            let line_offset = lines.get(mid as usize).copied().unwrap_or(usize::MAX);
            if line_offset == offset {
                return Position { line: mid, character: 0 };
            } else if offset > line_offset {
                low = mid + 1;
            } else {
                high = mid - 1;
            }
        }
        let line = low - 1;
        Position { line, character: offset as i64 - lines[line as usize] as i64 }
    }

    /// `offsetAt`
    pub fn offset_at(&self, p: Position) -> usize {
        let lines = self.lsp_line_offsets();
        if p.line >= lines.len() as i64 {
            return self.len();
        } else if p.line < 0 {
            return 0;
        }
        let line_offset = lines[p.line as usize];
        let next = lines.get(p.line as usize + 1).copied().unwrap_or(self.len());
        (line_offset as i64 + p.character).clamp(line_offset as i64, next as i64) as usize
    }

    /// TS's `getPositionOfLineAndCharacter` (line breaks: `\r\n`, `\r`, `\n`, U+2028, U+2029)
    pub fn ts_position(&self, line: usize, character: usize) -> usize {
        let t = &self.units;
        let mut starts = vec![0usize];
        let mut i = 0;
        while i < t.len() && starts.len() <= line {
            let c = t[i];
            i += 1;
            if c == b'\r' as u16 {
                if i < t.len() && t[i] == b'\n' as u16 {
                    i += 1;
                }
                starts.push(i);
            } else if c == b'\n' as u16 || c == 0x2028 || c == 0x2029 {
                starts.push(i);
            }
        }
        starts.get(line).map_or(t.len(), |s| (s + character).min(t.len()))
    }

    pub fn slice(&self, start: usize, end: usize) -> String {
        let end = end.min(self.len());
        String::from_utf16_lossy(&self.units[start.min(end)..end])
    }

    pub fn starts_with_at(&self, at: usize, needle: &str) -> bool {
        let n: Vec<u16> = needle.encode_utf16().collect();
        self.units.get(at..at + n.len()) == Some(&n[..])
    }

    /// `lastIndexOf(needle, from)`
    pub fn last_index_of(&self, needle: &[u16], from: usize) -> Option<usize> {
        let max = from.min(self.len().checked_sub(needle.len())?);
        (0..=max).rev().find(|&i| &self.units[i..i + needle.len()] == needle)
    }

    /// `indexOf(needle, from)`
    pub fn index_of(&self, needle: &[u16], from: usize) -> Option<usize> {
        let last = self.len().checked_sub(needle.len())?;
        (from..=last).find(|&i| &self.units[i..i + needle.len()] == needle)
    }
}

/// trace-mapping's `originalPositionFor` (greatest lower bound) on decoded mappings
fn original_position_for(mappings: &[Vec<[u32; 4]>], line: i64, column: i64) -> Option<(u32, u32)> {
    if line < 0 || column < 0 {
        return None;
    }
    let segs = mappings.get(line as usize)?;
    let column = column as u32;
    let mut index = match segs.binary_search_by(|s| s[0].cmp(&column)) {
        Ok(i) => i,
        Err(0) => return None,
        Err(i) => i - 1,
    };
    while index > 0 && segs[index - 1][0] == segs[index][0] {
        index -= 1;
    }
    let s = segs[index];
    Some((s[2], s[3]))
}

/// The generated code of a component, with what mapping needs
pub struct SvelteFile<'f> {
    /// the `.svelte` source
    pub source: &'f str,
    /// the code tsgo saw
    pub generated: &'f str,
    pub mappings: &'f [Vec<[u32; 4]>],
    pub exported_names: &'f [String],
    /// `ts` or `js`
    pub source_kind: &'static str,
    /// `// @ts-check` / `// @ts-nocheck` the language server prepends to its snapshot
    pub ts_check: Option<String>,
    /// the parsed template, for the checks that look at nodes
    pub nodes: Option<&'f [LNode<'f, 'f>]>,
}

const IGNORE_START: &str = "/*Ωignore_startΩ*/";
const IGNORE_END: &str = "/*Ωignore_endΩ*/";

/// `isInGeneratedCode`
fn is_in_generated_code(text: &U16Text, start: usize, end: usize) -> bool {
    let s: Vec<u16> = IGNORE_START.encode_utf16().collect();
    let e: Vec<u16> = IGNORE_END.encode_utf16().collect();
    let last_start = text.last_index_of(&s, start).map_or(-1, |i| i as i64);
    let last_end = text.last_index_of(&e, start).map_or(-1, |i| i as i64);
    let next_end = text.index_of(&e, end).map_or(-1, |i| i as i64);
    (last_start > last_end || last_end == next_end) && last_start < next_end
}

/// `mapSvelteCheckDiagnostics` for one component
pub fn map_svelte_diagnostics(file: &SvelteFile, diags: &[CliDiagnostic]) -> Vec<Diagnostic> {
    let generated = U16Text::new(file.generated);
    let snapshot_text = match &file.ts_check {
        Some(c) => U16Text::new(&format!("{c}{}", file.generated)),
        None => U16Text::new(file.generated),
    };
    let nr_prepended_lines = i64::from(file.ts_check.is_some());
    let original = U16Text::new(file.source);
    let mut out = Vec::new();

    for d in diags {
        // cliDiagnosticToTsDiagnostic
        let start = generated.ts_position(d.line, d.character);
        let length = d.length;
        // isNotGenerated
        if is_in_generated_code(&snapshot_text, start, start + length) {
            continue;
        }
        // isUnusedReactiveStatementLabel
        if d.code == 7028 && generated.slice(start, start + length) == "$" {
            let after = generated.slice(start + length, (start + length + 20).min(generated.len()));
            if after.trim_start().starts_with(':') {
                continue;
            }
        }
        // expectedTransitionThirdArgument (without a language service)
        if d.code == 2554
            && start > 0
            && snapshot_text.slice(0, start).ends_with("__sveltets_2_ensureTransition(")
            && d.message.contains(" 3")
        {
            continue;
        }

        let mut range = Range { start: snapshot_text.position_at(start as i64), end: snapshot_text.position_at((start + length) as i64) };
        let mut message = d.message.clone();

        // rangeMapper
        let generated_range = range;
        let map = |p: Position| match original_position_for(file.mappings, p.line - nr_prepended_lines, p.character) {
            Some((line, col)) => Position { line: line as i64, character: col as i64 },
            None => Position { line: -1, character: -1 },
        };
        range = Range { start: map(range.start), end: map(range.end) };
        // checkRangeLength
        if range.start.line == range.end.line
            && generated_range.start.line == generated_range.end.line
            && range.end.character - range.start.character == generated_range.end.character - generated_range.start.character - 1
        {
            range.end.character += 1;
        }
        if (d.code == 2741 || d.code == 2739 || message.contains("'Properties<")) && range.start == range.end {
            if let Some(nodes) = file.nodes {
                let offset = original.offset_at(range.start);
                if let Some((start, tag_len)) = node_in_start_tag(file.source, &original, nodes, offset) {
                    range.start = original.position_at(start as i64 + 1);
                    range.end = original.position_at((start + 1 + tag_len.max(1)) as i64);
                }
            }
        }

        // moveBindingErrorMessage
        if d.code == 2322 && start > 0 && snapshot_text.slice(start, start + length).ends_with(".$$bindings") {
            if let Some(nodes) = file.nodes {
                let offset = original.offset_at(range.start);
                if let Some(component) = find_component_at(file.source, &original, nodes, offset) {
                    let after = snapshot_text.slice(start + length, start + length + 100);
                    let name = after.find('\'').and_then(|q| after[q + 1..].find('\'').map(|e| &after[q + 1..q + 1 + e])).unwrap_or("");
                    let binding = component.attributes.iter().find_map(|a| match a {
                        LAttr::Other { attr, legacy_type: "Binding" } if attr.name() == Some(name) => Some((attr.start(), attr.end())),
                        _ => None,
                    });
                    if let Some((bs, be)) = binding {
                        if message.starts_with("Type '") && message.contains("is not assignable to type '") {
                            let idx = message.find("Type '\"").map_or(0, |i| i + "Type '\"".len());
                            let prop = message[idx..].split('"').next().unwrap_or("");
                            message = format!(
                                "Cannot use 'bind:' with this property. It is declared as non-bindable inside the component.\nTo mark a property as bindable: 'let {{ {prop} = $bindable() }} = $props()'"
                            );
                        } else {
                            message = format!(
                                "Cannot use 'bind:' with this property. It is declared as non-bindable inside the component.\nTo mark a property as bindable: 'let {{ prop = $bindable() }} = $props()'\n\n{message}"
                            );
                        }
                        range = Range { start: original.position_at(byte_to_u16(file.source, bs) as i64), end: original.position_at(byte_to_u16(file.source, be) as i64) };
                    }
                }
            }
        }

        // hasNoNegativeLines
        if range.start.line < 0 || range.end.line < 0 {
            continue;
        }
        // isNoFalsePositive
        if d.code == 1117 || d.code == 2300 {
            if let Some(nodes) = file.nodes {
                if is_element_attribute_name_at(file.source, &original, nodes, original.offset_at(range.start)) {
                    continue;
                }
            }
        }
        static GENERATED_VAR: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| regex::Regex::new(r"'\$\$_\w+(\.\$on)?'").unwrap());
        if d.code == 6387 && GENERATED_VAR.is_match(&message) {
            continue;
        }
        if d.code == 2454 {
            let name = original.slice(original.offset_at(range.start), original.offset_at(range.end));
            if file.exported_names.iter().any(|n| *n == name) {
                continue;
            }
        }
        // adjustIfNecessary
        if d.code == 2345 && message.contains("ConstructorOfATypedSvelteComponent") {
            message += "\n\nPossible causes:\n- You use the instance type of a component where you should use the constructor type\n- Type definitions are missing for this Svelte Component. ";
        }
        if d.code == 1184 {
            message += "\nIf this is a declare statement, move it into <script context=\"module\">..</script>";
        }
        // swapDiagRangeStartEndIfNecessary
        if range.end.line < range.start.line || (range.end.line == range.start.line && range.end.character < range.start.character) {
            std::mem::swap(&mut range.start, &mut range.end);
        }
        out.push(Diagnostic {
            range,
            severity: d.severity,
            source: file.source_kind,
            message,
            code: Some(Code::Num(d.code)),
            code_description: None,
            position_unknown: false,
        });
    }
    out
}

/// A diagnostic in a plain TS/JS file (or without a file)
pub fn map_plain_diagnostic(d: &CliDiagnostic, source: &'static str) -> Diagnostic {
    Diagnostic {
        range: Range {
            start: Position { line: d.line as i64, character: d.character as i64 },
            end: Position { line: d.line as i64, character: (d.character + d.length) as i64 },
        },
        severity: d.severity,
        source,
        message: d.message.clone(),
        code: Some(Code::Num(d.code)),
        code_description: None,
        position_unknown: d.file_path.is_none(),
    }
}

pub fn byte_to_u16(s: &str, byte: usize) -> usize {
    let mut b = byte.min(s.len());
    while !s.is_char_boundary(b) {
        b -= 1;
    }
    s[..b].encode_utf16().count()
}

fn u16_to_byte(s: &str, unit: usize) -> usize {
    let mut n = 0;
    for (i, c) in s.char_indices() {
        if n >= unit {
            return i;
        }
        n += c.len_utf16();
    }
    s.len()
}

// --- template lookups --------------------------------------------------------------------

fn lnode_range(n: &LNode) -> (usize, usize) {
    (crate::svelte2tsx::lnode_start(n), crate::svelte2tsx::lnode_end(n))
}

fn element_children<'r, 'm, 'a>(n: &'r LNode<'m, 'a>) -> Vec<&'r [LNode<'m, 'a>]> {
    match n {
        LNode::Element(el) => el.children.as_deref().into_iter().collect(),
        LNode::IfBlock { children, else_block, .. } | LNode::EachBlock { children, else_block, .. } => {
            let mut v: Vec<&[LNode]> = vec![children];
            if let Some(e) = else_block {
                v.push(&e.children);
            }
            v
        }
        LNode::AwaitBlock { pending, then, catch, .. } => vec![&pending.children, &then.children, &catch.children],
        LNode::KeyBlock { children, .. } | LNode::SnippetBlock { children, .. } => vec![children],
        _ => Vec::new(),
    }
}

/// What `svelteNodeAt` finds, as far as the filters care
enum Found<'r, 'm, 'a> {
    Element(&'r LElement<'m, 'a>),
    /// an attribute-like node: its legacy type, and the element it's on
    Attr(&'static str, &'r LElement<'m, 'a>),
    Other,
}

/// `svelteNodeAt`: the chain of nodes containing `offset` (each one the "parent" of the next)
fn nodes_at<'r, 'm, 'a>(nodes: &'r [LNode<'m, 'a>], offset: usize, out: &mut Vec<Found<'r, 'm, 'a>>) {
    for n in nodes {
        let (s, e) = lnode_range(n);
        if s > offset || e < offset {
            continue;
        }
        match n {
            LNode::Element(el) => {
                out.push(Found::Element(el));
                for a in &el.attributes {
                    let (attr, ty) = match a {
                        LAttr::Attribute { attr, .. } => (*attr, a.legacy_type()),
                        LAttr::Other { attr, legacy_type } => (*attr, *legacy_type),
                    };
                    if attr.start() > offset || attr.end() < offset {
                        continue;
                    }
                    out.push(Found::Attr(ty, el));
                    if let LAttr::Attribute { value: LAttrValue::Chunks(chunks), .. } = a {
                        for c in chunks {
                            let (cs, ce) = match c {
                                LChunk::Text(t) => match t {
                                    crate::ast::Chunk::Text { start, end, .. } | crate::ast::Chunk::Expression { start, end, .. } => (*start, *end),
                                },
                                LChunk::MustacheTag { start, end, .. } | LChunk::AttributeShorthand { start, end, .. } => (*start, *end),
                            };
                            if cs <= offset && offset <= ce {
                                out.push(Found::Other);
                            }
                        }
                    }
                }
            }
            _ => out.push(Found::Other),
        }
        for children in element_children(n) {
            nodes_at(children, offset, out);
        }
    }
}

fn is_element_attribute_name_at(source: &str, text: &U16Text, nodes: &[LNode], offset: usize) -> bool {
    let _ = text;
    let byte = u16_to_byte(source, offset);
    let mut chain = Vec::new();
    nodes_at(nodes, byte, &mut chain);
    matches!(chain.last(), Some(Found::Attr("Attribute" | "EventHandler", el)) if el.kind == "Element")
}

fn find_component_at<'r, 'm, 'a>(source: &str, _text: &U16Text, nodes: &'r [LNode<'m, 'a>], offset: usize) -> Option<&'r LElement<'m, 'a>> {
    let byte = u16_to_byte(source, offset);
    let mut chain = Vec::new();
    nodes_at(nodes, byte, &mut chain);
    chain.iter().rev().find_map(|f| match f {
        Found::Element(el) if el.kind == "InlineComponent" => Some(*el),
        _ => None,
    })
}

/// `getNodeIfIsInStartTag(document.html, offset)`: the innermost tag containing `offset`, if
/// `offset` is inside its start tag. Returns `(start, tag name length)` in UTF-16 units.
fn node_in_start_tag(source: &str, _text: &U16Text, nodes: &[LNode], offset: usize) -> Option<(usize, usize)> {
    let byte = u16_to_byte(source, offset);
    fn find<'r, 'm, 'a>(nodes: &'r [LNode<'m, 'a>], byte: usize) -> Option<&'r LElement<'m, 'a>> {
        let mut found = None;
        for n in nodes {
            let (s, e) = lnode_range(n);
            if !(s < byte && byte <= e) {
                continue;
            }
            if let LNode::Element(el) = n {
                found = Some(el);
            }
            for children in element_children(n) {
                if let Some(inner) = find(children, byte) {
                    found = Some(inner);
                }
            }
        }
        found
    }
    let el = find(nodes, byte)?;
    // the start tag ends at the `>` after the last attribute
    let after_attrs = el.attributes.iter().map(|a| match a {
        LAttr::Attribute { attr, .. } | LAttr::Other { attr, .. } => attr.end(),
    });
    let from = after_attrs.max().unwrap_or(el.start + 1 + el.name.len());
    let tag_end = source[from..].find('>').map_or(source.len(), |p| from + p + 1);
    if byte < tag_end {
        Some((byte_to_u16(source, el.start), el.name.encode_utf16().count()))
    } else {
        None
    }
}

/// `getTsCheckComment`: `// @ts-check` (or nocheck) among the comments the script starts with
pub fn ts_check_comment(script_content: &str) -> Option<String> {
    static COMMENTS: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| regex::Regex::new(r"^(\s*//.*\s*)*").unwrap());
    static TS_CHECK: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"//[ \t\x{a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}]*(@ts-(no)?check)($|\s)").unwrap()
    });
    let comments = COMMENTS.find(script_content)?.as_str();
    if comments.is_empty() {
        return None;
    }
    TS_CHECK.captures(comments).map(|c| format!("// {}\n", &c[1]))
}
