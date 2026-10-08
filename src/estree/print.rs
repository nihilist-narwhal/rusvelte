//! A port of esrap 2.4.0 (`print` from `esrap`, with the visitors of `esrap/languages/ts`), as
//! Svelte calls it: `print(program, ts({ comments }))`.
//!
//! The structure follows esrap. Visitors write *commands* into a [`Ctx`] (strings, newlines,
//! indentation changes, source locations, and nested contexts, which are shared by reference:
//! a context appended somewhere and written to later shows the later writes there too).
//! Contexts measure their content to decide on line breaks, then the command tree is run
//! once to produce the code and the source map segments.
//!
//! Lengths are measured in UTF-16 code units and source map columns are UTF-16 columns, as in
//! JS. Comments are placed by their `loc` relative to the `loc` of the nodes being printed;
//! nodes without `loc` (made by builders) get no comments of their own.
//!
//! Not ported: the `tokens` option (mapping punctuation to source tokens, which Svelte doesn't
//! use), and visitors for node types [`Node`] doesn't have (TypeScript types, JSX,
//! decorators). Identity-based state (`BINDINGS`, `parenthesized_sequences`) is keyed by node
//! address for the duration of one `print` call; esrap's `BINDINGS` is a module-level
//! `WeakSet`, so a node object shared between a binding position and an expression position
//! (impossible in an owned tree) could print differently.

use std::borrow::Cow;

use rustc_hash::FxHashSet;

use super::*;

/// Options of `print` and of the `ts` language
pub struct PrintOptions<'a> {
    /// `ts({ comments })`: comments to place by position
    pub comments: &'a [Comment],
    /// `indent` (default `"\t"`)
    pub indent: &'a str,
    /// `quotes: 'double'` (default single quotes)
    pub double_quotes: bool,
    /// Print each node's `leadingComments`/`trailingComments` (esrap's `getLeadingComments` /
    /// `getTrailingComments` returning them). Svelte's compiler doesn't, its `print` API does.
    pub node_comments: bool,
    /// Record source map mappings
    pub source_map: bool,
}

impl Default for PrintOptions<'_> {
    fn default() -> Self {
        PrintOptions { comments: &[], indent: "\t", double_quotes: false, node_comments: false, source_map: false }
    }
}

/// `[generatedColumn, sourceIndex, originalLine, originalColumn]`, all 0-based
pub type Segment = [u32; 4];

pub struct Printed {
    pub code: String,
    /// Source map segments per generated line (empty unless `source_map` was set)
    pub mappings: Vec<Vec<Segment>>,
}

impl Printed {
    /// The `mappings` string of the source map (`@jridgewell/sourcemap-codec`'s `encode`)
    pub fn encode_mappings(&self) -> String {
        encode_mappings(&self.mappings)
    }
}

/// `print(node, ts(options), options)`
pub fn print(node: &Node, options: &PrintOptions) -> Printed {
    fn count_nodes(node: &Node) -> usize {
        let mut n = 1;
        node.for_each_child(&mut |c| n += count_nodes(c));
        n
    }
    let count = count_nodes(node);
    let mut p = Printer {
        bufs: Vec::with_capacity(count / 2 + 16),
        cmds: Vec::with_capacity(count * if options.source_map { 6 } else { 3 } + 16),
        comments: options.comments,
        comment_index: 0,
        quote: if options.double_quotes { '"' } else { '\'' },
        bindings: FxHashSet::default(),
        parenthesized_sequences: FxHashSet::default(),
        node_comments: options.node_comments,
        locations: options.source_map,
    };
    let mut cx = p.new_ctx();
    p.visit(&mut cx, node);
    p.run(cx.buf, options.indent, options.source_map)
}

// -------------------------------------------------------------------------------------------
// Commands and contexts (esrap's `context.js` and `index.js`)

enum Cmd<'a> {
    Str(Cow<'a, str>),
    Margin,
    Newline,
    Indent,
    Dedent,
    Space,
    Location(Position),
    /// another context's commands
    Buf(u32),
}

const NONE: u32 = u32::MAX;

struct Entry<'a> {
    cmd: Cmd<'a>,
    next: u32,
}

/// A `Context`: its command list lives in the printer's arena
#[derive(Clone, Copy)]
struct Ctx {
    buf: u32,
    multiline: bool,
    has_newline: bool,
}

/// A context's commands, with a cache of `measure()`. A write marks the context and every
/// context it is appended to as dirty (stopping at contexts that already are), and `measure`
/// only recomputes dirty contexts.
///
/// The commands of all contexts live in one arena (`Printer::cmds`), each context's as a
/// linked list, so creating a context doesn't allocate.
struct Buf {
    /// first and last command in `Printer::cmds` (`NONE` when empty)
    head: u32,
    tail: u32,
    /// the UTF-16 length of the strings written directly into this context
    own: usize,
    /// `measure()`, valid unless `dirty`
    total: usize,
    dirty: bool,
    /// the contexts this one is appended to
    parents: smallvec::SmallVec<[u32; 1]>,
}

struct Printer<'a> {
    bufs: Vec<Buf>,
    cmds: Vec<Entry<'a>>,
    comments: &'a [Comment],
    comment_index: usize,
    quote: char,
    bindings: FxHashSet<usize>,
    parenthesized_sequences: FxHashSet<usize>,
    node_comments: bool,
    locations: bool,
}

#[inline]
fn addr(node: &Node) -> usize {
    node as *const Node as usize
}

/// The length of `s` in UTF-16 code units
#[inline]
fn utf16_len(s: &str) -> usize {
    if s.is_ascii() {
        return s.len();
    }
    s.bytes().filter(|&b| b & 0xC0 != 0x80).count() + s.bytes().filter(|&b| b >= 0xF0).count()
}

/// `a` is before `b`
#[inline]
fn before(a: Position, b: Position) -> bool {
    a.line < b.line || (a.line == b.line && a.column < b.column)
}

fn start(node: Option<&Node>) -> Option<Position> {
    node.and_then(|n| n.loc).map(|l| l.start)
}

fn end(node: Option<&Node>) -> Option<Position> {
    node.and_then(|n| n.loc).map(|l| l.end)
}

impl<'a> Printer<'a> {
    fn new_ctx(&mut self) -> Ctx {
        self.bufs.push(Buf { head: NONE, tail: NONE, own: 0, total: 0, dirty: false, parents: smallvec::SmallVec::new() });
        Ctx { buf: (self.bufs.len() - 1) as u32, multiline: false, has_newline: false }
    }

    #[inline]
    fn push(&mut self, cx: &Ctx, cmd: Cmd<'a>) {
        let i = self.cmds.len() as u32;
        self.cmds.push(Entry { cmd, next: NONE });
        let b = &mut self.bufs[cx.buf as usize];
        if b.tail == NONE {
            b.head = i;
        } else {
            self.cmds[b.tail as usize].next = i;
        }
        b.tail = i;
    }

    #[inline]
    fn push_str(&mut self, cx: &Ctx, s: Cow<'a, str>) {
        let len = utf16_len(&s);
        self.push(cx, Cmd::Str(s));
        if len > 0 {
            self.bufs[cx.buf as usize].own += len;
            self.mark(cx.buf);
        }
    }

    /// Invalidate the measurement of `buf` and of the contexts it is appended to
    fn mark(&mut self, buf: u32) {
        let b = &mut self.bufs[buf as usize];
        if b.dirty {
            return;
        }
        b.dirty = true;
        for i in 0..b.parents.len() {
            let p = self.bufs[buf as usize].parents[i];
            self.mark(p);
        }
    }

    fn indent(&mut self, cx: &Ctx) {
        self.push(cx, Cmd::Indent);
    }

    fn dedent(&mut self, cx: &Ctx) {
        self.push(cx, Cmd::Dedent);
    }

    fn margin(&mut self, cx: &Ctx) {
        self.push(cx, Cmd::Margin);
    }

    fn newline(&mut self, cx: &mut Ctx) {
        cx.has_newline = true;
        self.push(cx, Cmd::Newline);
    }

    fn space(&mut self, cx: &Ctx) {
        self.push(cx, Cmd::Space);
    }

    fn append(&mut self, cx: &mut Ctx, other: &Ctx) {
        self.push(cx, Cmd::Buf(other.buf));
        self.bufs[other.buf as usize].parents.push(cx.buf);
        self.mark(cx.buf);
        if cx.has_newline || other.multiline {
            cx.multiline = true;
        }
    }

    fn write(&mut self, cx: &mut Ctx, content: impl Into<Cow<'a, str>>) {
        self.push_str(cx, content.into());
        if cx.has_newline {
            cx.multiline = true;
        }
    }

    /// `context.write(content, node)`: mapped to the node's location
    fn write_node(&mut self, cx: &mut Ctx, content: impl Into<Cow<'a, str>>, node: &Node) {
        if let Some(loc) = node.loc {
            self.location(cx, loc.start);
            self.push_str(cx, content.into());
            self.location(cx, loc.end);
        } else {
            self.push_str(cx, content.into());
        }
        if cx.has_newline {
            cx.multiline = true;
        }
    }

    #[inline]
    fn location(&mut self, cx: &Ctx, pos: Position) {
        if self.locations {
            self.push(cx, Cmd::Location(pos));
        }
    }

    /// `context.empty()`: no non-empty string anywhere in it
    fn empty(&mut self, buf: u32) -> bool {
        self.measure(buf) == 0
    }

    /// `context.measure()`
    fn measure(&mut self, buf: u32) -> usize {
        let b = &self.bufs[buf as usize];
        if !b.dirty {
            return b.total;
        }
        let mut total = b.own;
        let mut i = b.head;
        while i != NONE {
            let entry = &self.cmds[i as usize];
            let next = entry.next;
            if let Cmd::Buf(child) = entry.cmd {
                total += self.measure(child);
            }
            i = next;
        }
        let b = &mut self.bufs[buf as usize];
        b.total = total;
        b.dirty = false;
        total
    }

    /// esrap's `print` after visiting: run the commands
    fn run(mut self, root: u32, indent: &str, source_map: bool) -> Printed {
        let length = self.measure(root);
        let mut state = RunState {
            code: String::with_capacity(length + length / 4 + 64),
            current_column: 0,
            mappings: Vec::new(),
            current_line: Vec::new(),
            current_newline: String::from("\n"),
            indent,
            needs_newline: false,
            needs_margin: false,
            needs_space: false,
            pending: Vec::new(),
            source_map,
        };
        state.run(&self.bufs, &self.cmds, root);
        state.flush_locations();
        if source_map {
            let line = std::mem::take(&mut state.current_line);
            state.mappings.push(line);
        }
        Printed { code: state.code, mappings: state.mappings }
    }
}

struct RunState<'i> {
    code: String,
    current_column: u32,
    mappings: Vec<Vec<Segment>>,
    current_line: Vec<Segment>,
    current_newline: String,
    indent: &'i str,
    needs_newline: bool,
    needs_margin: bool,
    needs_space: bool,
    pending: Vec<Position>,
    source_map: bool,
}

impl RunState<'_> {
    fn append(&mut self, s: &str) {
        self.code.push_str(s);
        if !self.source_map {
            return;
        }
        if s.is_ascii() {
            match s.rfind('\n') {
                None => self.current_column += s.len() as u32,
                Some(_) => {
                    for (i, part) in s.split('\n').enumerate() {
                        if i > 0 {
                            let line = std::mem::take(&mut self.current_line);
                            self.mappings.push(line);
                            self.current_column = 0;
                        }
                        self.current_column += part.len() as u32;
                    }
                }
            }
        } else {
            for c in s.chars() {
                if c == '\n' {
                    let line = std::mem::take(&mut self.current_line);
                    self.mappings.push(line);
                    self.current_column = 0;
                } else {
                    self.current_column += c.len_utf16() as u32;
                }
            }
        }
    }

    fn add_location(&mut self, pos: Position) {
        let segment = [self.current_column, 0, pos.line.saturating_sub(1), pos.column];
        match self.current_line.last() {
            Some(prev) if prev[0] == segment[0] && prev[2] == segment[2] && prev[3] == segment[3] => {}
            _ => self.current_line.push(segment),
        }
    }

    fn flush_locations(&mut self) {
        for pos in std::mem::take(&mut self.pending) {
            self.add_location(pos);
        }
    }

    fn run(&mut self, bufs: &[Buf], cmds: &[Entry], buf: u32) {
        let mut i = bufs[buf as usize].head;
        while i != NONE {
            let entry = &cmds[i as usize];
            i = entry.next;
            match &entry.cmd {
                Cmd::Buf(b) => self.run(bufs, cmds, *b),
                Cmd::Newline => self.needs_newline = true,
                Cmd::Margin => self.needs_margin = true,
                Cmd::Space => self.needs_space = true,
                Cmd::Indent => self.current_newline.push_str(self.indent),
                Cmd::Dedent => {
                    let len = self.current_newline.len().saturating_sub(self.indent.len());
                    self.current_newline.truncate(len);
                }
                Cmd::Str(s) => {
                    if self.needs_newline {
                        if self.needs_margin {
                            self.append("\n");
                        }
                        let nl = std::mem::take(&mut self.current_newline);
                        self.append(&nl);
                        self.current_newline = nl;
                    } else if self.needs_space {
                        self.append(" ");
                    }
                    self.needs_margin = false;
                    self.needs_newline = false;
                    self.needs_space = false;
                    self.flush_locations();
                    self.append(s);
                }
                Cmd::Location(pos) => {
                    if self.needs_newline || self.needs_space {
                        self.pending.push(*pos);
                    } else {
                        self.add_location(*pos);
                    }
                }
            }
        }
    }
}

/// `@jridgewell/sourcemap-codec`'s `encode` for 4-field segments
pub fn encode_mappings(lines: &[Vec<Segment>]) -> String {
    fn vlq(out: &mut String, value: i64) {
        const CHARS: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut v: u64 = if value < 0 { ((-value as u64) << 1) | 1 } else { (value as u64) << 1 };
        loop {
            let mut digit = (v & 31) as usize;
            v >>= 5;
            if v > 0 {
                digit |= 32;
            }
            out.push(CHARS[digit] as char);
            if v == 0 {
                break;
            }
        }
    }
    let mut out = String::new();
    let (mut source, mut line, mut column) = (0i64, 0i64, 0i64);
    for (i, segments) in lines.iter().enumerate() {
        if i > 0 {
            out.push(';');
        }
        let mut generated = 0i64;
        for (j, s) in segments.iter().enumerate() {
            if j > 0 {
                out.push(',');
            }
            vlq(&mut out, s[0] as i64 - generated);
            generated = s[0] as i64;
            vlq(&mut out, s[1] as i64 - source);
            source = s[1] as i64;
            vlq(&mut out, s[2] as i64 - line);
            line = s[2] as i64;
            vlq(&mut out, s[3] as i64 - column);
            column = s[3] as i64;
        }
    }
    out
}

// -------------------------------------------------------------------------------------------
// Precedence

/// `EXPRESSIONS_PRECEDENCE[node.type]` (`None` for `undefined`)
fn precedence(node: &Node) -> Option<u8> {
    use NodeKind::*;
    Some(match &node.kind {
        ArrayPattern(_) | ObjectPattern(_) | ArrayExpression(_) | TaggedTemplateExpression(_) | ThisExpression
        | Identifier(_) | TemplateLiteral(_) | Super | SequenceExpression(_) => 20,
        MemberExpression(_) | MetaProperty(_) | CallExpression(_) | ChainExpression(_) | ImportExpression(_)
        | NewExpression(_) => 19,
        Literal(_) => 18,
        AwaitExpression(_) | ClassExpression(_) | FunctionExpression(_) | ObjectExpression(_) => 17,
        UpdateExpression(_) => 16,
        UnaryExpression(_) => 15,
        BinaryExpression(_) => 14,
        LogicalExpression(_) => 12,
        ConditionalExpression(_) => 4,
        ArrowFunctionExpression(_) | AssignmentExpression(_) => 3,
        YieldExpression(_) => 2,
        RestElement(_) => 1,
        _ => return None,
    })
}

/// `EXPRESSIONS_PRECEDENCE[a] < b` (false for `undefined`)
#[inline]
fn prec_lt(node: &Node, b: u8) -> bool {
    precedence(node).is_some_and(|a| a < b)
}

fn operator_precedence(op: &str) -> u8 {
    match op {
        "||" => 2,
        "&&" => 3,
        "??" => 4,
        "|" => 5,
        "^" => 6,
        "&" => 7,
        "==" | "!=" | "===" | "!==" => 8,
        "<" | ">" | "<=" | ">=" | "in" | "instanceof" => 9,
        "<<" | ">>" | ">>>" => 10,
        "+" | "-" => 11,
        "*" | "%" | "/" => 12,
        "**" => 13,
        _ => 0,
    }
}

/// The operator of a BinaryExpression or LogicalExpression
fn binary_operator_str(node: &Node) -> Option<&'static str> {
    match &node.kind {
        NodeKind::BinaryExpression(b) => Some(b.operator.as_str()),
        NodeKind::LogicalExpression(l) => Some(l.operator.as_str()),
        _ => None,
    }
}

/// `` ` ${operator} ` `` without allocating
fn spaced_operator(op: &str) -> Cow<'static, str> {
    Cow::Borrowed(match op {
        "||" => " || ",
        "&&" => " && ",
        "??" => " ?? ",
        "|" => " | ",
        "^" => " ^ ",
        "&" => " & ",
        "==" => " == ",
        "!=" => " != ",
        "===" => " === ",
        "!==" => " !== ",
        "<" => " < ",
        ">" => " > ",
        "<=" => " <= ",
        ">=" => " >= ",
        "in" => " in ",
        "instanceof" => " instanceof ",
        "<<" => " << ",
        ">>" => " >> ",
        ">>>" => " >>> ",
        "+" => " + ",
        "-" => " - ",
        "*" => " * ",
        "%" => " % ",
        "/" => " / ",
        "**" => " ** ",
        _ => return Cow::Owned(format!(" {op} ")),
    })
}

fn spaced_assignment_operator(op: AssignmentOperator) -> Cow<'static, str> {
    use AssignmentOperator::*;
    Cow::Borrowed(match op {
        Assign => " = ",
        Addition => " += ",
        Subtraction => " -= ",
        Multiplication => " *= ",
        Division => " /= ",
        Remainder => " %= ",
        Exponential => " **= ",
        ShiftLeft => " <<= ",
        ShiftRight => " >>= ",
        ShiftRightZeroFill => " >>>= ",
        BitwiseOR => " |= ",
        BitwiseXOR => " ^= ",
        BitwiseAnd => " &= ",
        LogicalOr => " ||= ",
        LogicalAnd => " &&= ",
        LogicalNullish => " ??= ",
    })
}

fn arrow_concise_body_needs_wrap(body: &Node) -> bool {
    match &body.kind {
        NodeKind::ObjectExpression(_) => true,
        NodeKind::AssignmentExpression(a) => a.left.is("ObjectPattern"),
        NodeKind::LogicalExpression(l) => l.left.is("ObjectExpression"),
        NodeKind::ConditionalExpression(c) => c.test.is("ObjectExpression"),
        _ => false,
    }
}

fn operand_needs_wrap(node: &Node, parent: &Node, is_right: bool) -> bool {
    if node.is("PrivateIdentifier") {
        return false;
    }
    let parent_op = binary_operator_str(parent).unwrap_or("");

    if let (NodeKind::LogicalExpression(n), NodeKind::LogicalExpression(p)) = (&node.kind, &parent.kind) {
        let (n, p) = (n.operator.as_str(), p.operator.as_str());
        if (p == "??" && n != "??") || (p != "??" && n == "??") {
            return true;
        }
    }

    if !is_right && parent_op == "**" && matches!(node.kind, NodeKind::UnaryExpression(_) | NodeKind::AwaitExpression(_)) {
        return true;
    }

    let p = precedence(node);
    let pp = precedence(parent);
    if p != pp {
        return matches!((p, pp), (Some(a), Some(b)) if a < b);
    }
    let operator = binary_operator_str(node).unwrap_or("");
    if operator == "**" && parent_op == "**" {
        return !is_right;
    }
    if is_right {
        return operator_precedence(operator) <= operator_precedence(parent_op);
    }
    operator_precedence(operator) < operator_precedence(parent_op)
}

fn has_call_expression(mut node: &Node) -> bool {
    loop {
        match &node.kind {
            NodeKind::CallExpression(_) => return true,
            NodeKind::MemberExpression(m) => node = &m.object,
            _ => return false,
        }
    }
}

fn leads_with_curly_or_keyword(mut node: &Node) -> bool {
    loop {
        match &node.kind {
            NodeKind::ObjectExpression(_) | NodeKind::ObjectPattern(_) | NodeKind::FunctionExpression(_) | NodeKind::ClassExpression(_) => {
                return true;
            }
            NodeKind::BinaryExpression(BinaryExpression { left, .. }) | NodeKind::LogicalExpression(LogicalExpression { left, .. }) => {
                if operand_needs_wrap(left, node, false) {
                    return false;
                }
                node = left;
            }
            NodeKind::AssignmentExpression(a) => node = &a.left,
            NodeKind::ConditionalExpression(c) => {
                if precedence(&c.test).is_some_and(|p| p <= 4) {
                    return false;
                }
                node = &c.test;
            }
            NodeKind::MemberExpression(m) => {
                if m.object.is("ChainExpression") || prec_lt(&m.object, 19) {
                    return false;
                }
                node = &m.object;
            }
            NodeKind::CallExpression(c) => {
                if c.callee.is("ChainExpression") || prec_lt(&c.callee, 19) {
                    return false;
                }
                node = &c.callee;
            }
            NodeKind::TaggedTemplateExpression(t) => {
                if t.tag.is("ChainExpression") || prec_lt(&t.tag, 19) {
                    return false;
                }
                node = &t.tag;
            }
            NodeKind::UpdateExpression(u) => {
                if u.prefix {
                    return false;
                }
                node = &u.argument;
            }
            _ => return false,
        }
    }
}

fn statement_ends_with_unmatched_if(node: &Node) -> bool {
    match &node.kind {
        NodeKind::IfStatement(i) => match &i.alternate {
            None => true,
            Some(a) => statement_ends_with_unmatched_if(a),
        },
        NodeKind::ForStatement(f) => statement_ends_with_unmatched_if(&f.body),
        NodeKind::ForInStatement(f) | NodeKind::ForOfStatement(f) => statement_ends_with_unmatched_if(&f.body),
        NodeKind::LabeledStatement(l) => statement_ends_with_unmatched_if(&l.body),
        NodeKind::WhileStatement(w) => statement_ends_with_unmatched_if(&w.body),
        NodeKind::WithStatement(w) => statement_ends_with_unmatched_if(&w.body),
        _ => false,
    }
}

fn contains_in_operator(node: &Node) -> bool {
    match &node.kind {
        NodeKind::BinaryExpression(BinaryExpression { left, right, .. })
        | NodeKind::LogicalExpression(LogicalExpression { left, right, .. }) => {
            binary_operator_str(node) == Some("in")
                || (!operand_needs_wrap(left, node, false) && contains_in_operator(left))
                || (!operand_needs_wrap(right, node, true) && contains_in_operator(right))
        }
        NodeKind::ConditionalExpression(c) => {
            (precedence(&c.test).is_some_and(|p| p > 4) && contains_in_operator(&c.test)) || contains_in_operator(&c.alternate)
        }
        NodeKind::AssignmentExpression(a) => contains_in_operator(&a.right),
        NodeKind::ArrowFunctionExpression(a) => !arrow_concise_body_needs_wrap(&a.body) && contains_in_operator(&a.body),
        NodeKind::YieldExpression(y) => y.argument.as_ref().is_some_and(|a| contains_in_operator(a)),
        _ => false,
    }
}

fn same_module_name(a: &Node, b: &Node) -> bool {
    match (&a.kind, &b.kind) {
        (NodeKind::Identifier(a), NodeKind::Identifier(b)) => a.name == b.name,
        (NodeKind::Literal(a), NodeKind::Literal(b)) => a.value == b.value,
        _ => false,
    }
}

fn has_object_or_array_value(node: Option<&Node>) -> bool {
    let Some(NodeKind::Property(p)) = node.map(|n| &n.kind) else { return false };
    let value = match &p.value.kind {
        NodeKind::AssignmentPattern(a) => &a.left,
        _ => &p.value,
    };
    value.is("ObjectExpression") || value.is("ArrayExpression")
}

/// JS's `\s`
fn is_js_whitespace(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'..='\u{200a}' | '\u{2028}'
            | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}'
    )
}

/// `/(?:^|\n)\s*\*\s*@type\s*{/.test(value)`
fn is_jsdoc_type(value: &str) -> bool {
    let try_at = |s: &str| -> bool {
        let s = s.trim_start_matches(is_js_whitespace);
        let Some(s) = s.strip_prefix('*') else { return false };
        let s = s.trim_start_matches(is_js_whitespace);
        let Some(s) = s.strip_prefix("@type") else { return false };
        s.trim_start_matches(is_js_whitespace).starts_with('{')
    };
    if try_at(value) {
        return true;
    }
    value.match_indices('\n').any(|(i, _)| try_at(&value[i + 1..]))
}

/// `quote(string, char)`
fn quote(s: &str, q: char) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push(q);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c if c == q => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out.push(q);
    out
}

/// JS's `String(number)`
pub fn js_number_to_string(v: f64) -> String {
    if v.is_nan() {
        return "NaN".into();
    }
    if v.is_infinite() {
        return if v > 0.0 { "Infinity".into() } else { "-Infinity".into() };
    }
    if v == 0.0 {
        return "0".into();
    }
    if v < 0.0 {
        return format!("-{}", js_number_to_string(-v));
    }
    // shortest round-trip digits, as `d.ddde±x`
    let sci = format!("{v:e}");
    let (mantissa, exp) = sci.split_once('e').unwrap();
    let exp: i32 = exp.parse().unwrap();
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let k = digits.len() as i32;
    let n = exp + 1;
    if k <= n && n <= 21 {
        let mut s = digits;
        s.extend(std::iter::repeat_n('0', (n - k) as usize));
        s
    } else if 0 < n && n <= 21 {
        format!("{}.{}", &digits[..n as usize], &digits[n as usize..])
    } else if -6 < n && n <= 0 {
        format!("0.{}{}", "0".repeat((-n) as usize), digits)
    } else {
        let e = n - 1;
        let sign = if e < 0 { '-' } else { '+' };
        if k == 1 {
            format!("{digits}e{sign}{}", e.abs())
        } else {
            format!("{}.{}e{sign}{}", &digits[..1], &digits[1..], e.abs())
        }
    }
}

// -------------------------------------------------------------------------------------------
// The `ts` language

impl<'a> Printer<'a> {
    fn write_comment(&mut self, cx: &mut Ctx, comment: &'a Comment) {
        match comment.kind {
            CommentKind::Line => self.write(cx, format!("//{}", comment.value)),
            CommentKind::Block => {
                let lines: Vec<&str> = comment.value.split('\n').collect();
                let n = lines.len();
                for (i, line) in lines.iter().enumerate() {
                    if i > 0 {
                        self.newline(cx);
                    }
                    let mut s = String::with_capacity(line.len() + 4);
                    if i == 0 {
                        s.push_str("/*");
                    }
                    s.push_str(line);
                    if i == n - 1 {
                        s.push_str("*/");
                    }
                    self.write(cx, s);
                }
                if n > 1 {
                    self.newline(cx);
                }
            }
        }
    }

    fn write_additional_comments(&mut self, cx: &mut Ctx, comments: &'a [Comment], leading: bool) {
        for (i, comment) in comments.iter().enumerate() {
            if !leading && i == 0 {
                self.write(cx, " ");
            }
            self.write_comment(cx, comment);
            if leading {
                if comment.kind == CommentKind::Line {
                    self.newline(cx);
                } else if !comment.value.contains('\n') {
                    self.write(cx, " ");
                }
            }
        }
    }

    fn comment_loc(&self, i: usize) -> Option<SourceLocation> {
        self.comments.get(i).and_then(|c| c.loc)
    }

    fn reset_comment_index(&mut self, node: &Node) {
        let Some(loc) = node.loc else {
            self.comment_index = self.comments.len();
            return;
        };
        let i = self.comment_index;
        if let Some(c) = self.comment_loc(i) {
            let prev_ok = i == 0 || self.comment_loc(i - 1).is_some_and(|p| before(p.start, loc.start));
            if !before(c.start, loc.start) && prev_ok {
                return;
            }
        }
        self.comment_index = self
            .comments
            .iter()
            .position(|c| c.loc.is_some_and(|l| !before(l.start, loc.start)))
            .unwrap_or(self.comments.len());
    }

    fn flush_trailing_comments(&mut self, cx: &mut Ctx, prev: Option<Position>, next: Option<Position>) {
        while self.comment_index < self.comments.len() {
            let comment = &self.comments[self.comment_index];
            let Some(loc) = comment.loc else { break };
            if let Some(prev) = prev {
                if loc.start.line == prev.line && next.is_none_or(|next| !before(next, loc.end)) {
                    self.space(cx);
                    self.write_comment(cx, comment);
                    self.comment_index += 1;
                    if comment.kind == CommentKind::Line {
                        self.newline(cx);
                    } else {
                        continue;
                    }
                }
            }
            break;
        }
    }

    fn flush_comments_until(
        &mut self,
        cx: &mut Ctx,
        from: Option<Position>,
        to: Option<Position>,
        pad: bool,
        is_next_to_expression: bool,
    ) -> usize {
        let mut first = true;
        let mut casts = 0;
        let Some(to) = to else { return 0 };
        while self.comment_index < self.comments.len() {
            let comment = &self.comments[self.comment_index];
            let Some(loc) = comment.loc else { break };
            if !before(loc.start, to) {
                break;
            }
            if first {
                if let Some(from) = from {
                    if loc.start.line > from.line {
                        self.margin(cx);
                        self.newline(cx);
                    }
                }
            }
            first = false;
            self.write_comment(cx, comment);
            let is_cast = is_next_to_expression && comment.kind == CommentKind::Block && is_jsdoc_type(&comment.value);
            if is_cast {
                self.write(cx, " (");
                casts += 1;
            }
            if comment.kind == CommentKind::Line || loc.end.line < to.line {
                self.newline(cx);
            } else if pad && !is_cast {
                self.space(cx);
            }
            self.comment_index += 1;
        }
        casts
    }

    fn token(&mut self, cx: &mut Ctx, token: &'static str, node: &Node) {
        self.write(cx, token);
        if let Some(loc) = node.loc {
            self.location(cx, Position::new(loc.start.line, loc.start.column + token.len() as u32));
        }
    }

    /// `token(context, string, node)` for a non-static string
    fn token_str(&mut self, cx: &mut Ctx, token: &'a str, node: &Node) {
        self.write(cx, token);
        if let Some(loc) = node.loc {
            self.location(cx, Position::new(loc.start.line, loc.start.column + utf16_len(token) as u32));
        }
    }

    fn track_binding(&mut self, node: &Node) {
        self.bindings.insert(addr(node));
        match &node.kind {
            NodeKind::AssignmentPattern(a) => self.track_binding(&a.left),
            NodeKind::RestElement(r) => self.track_binding(&r.argument),
            NodeKind::ArrayPattern(a) => {
                for e in a.elements.iter().flatten() {
                    self.track_binding(e);
                }
            }
            NodeKind::ObjectPattern(o) => {
                for p in &o.properties {
                    match &p.kind {
                        NodeKind::Property(prop) => self.track_binding(&prop.value),
                        _ => self.track_binding(p),
                    }
                }
            }
            _ => {}
        }
    }

    fn track_bindings(&mut self, nodes: &[Node]) {
        for n in nodes {
            self.track_binding(n);
        }
    }

    fn maybe_wrap(&mut self, cx: &mut Ctx, node: &'a Node, wrap: bool) {
        if wrap {
            if let Some(loc) = node.loc {
                self.location(cx, loc.start);
            }
            self.write(cx, "(");
            self.visit(cx, node);
            self.write(cx, ")");
            if let Some(loc) = node.loc {
                self.location(cx, loc.end);
            }
        } else {
            self.visit(cx, node);
        }
    }

    fn sequence(
        &mut self,
        cx: &mut Ctx,
        nodes: &[Option<&'a Node>],
        until: Option<Position>,
        pad: bool,
        separator: &'static str,
        trailing_newline: bool,
    ) {
        let mut multiline = false;
        let mut length: i64 = -1;
        let mut multiline_nodes = Vec::with_capacity(nodes.len());
        let mut children = Vec::with_capacity(nodes.len());

        for (i, child) in nodes.iter().enumerate() {
            let mut child_cx = self.new_ctx();
            if let Some(child) = child {
                self.visit(&mut child_cx, child);
            }
            multiline_nodes.push(child_cx.multiline);
            if i < nodes.len() - 1 || child.is_none() {
                self.write(&mut child_cx, separator);
            }
            let next = if i == nodes.len() - 1 { until } else { start(nodes[i + 1]) };
            self.flush_trailing_comments(&mut child_cx, end(*child), next);
            length += self.measure(child_cx.buf) as i64 + 1;
            multiline |= child_cx.multiline;
            children.push(child_cx);
        }

        multiline |= length > 60;

        if multiline {
            self.indent(cx);
            self.newline(cx);
        } else if pad && length > 0 {
            self.write(cx, " ");
        }

        for i in 0..nodes.len() {
            if i > 0 {
                if multiline_nodes[i - 1]
                    && multiline_nodes[i]
                    && (!has_object_or_array_value(nodes[i - 1]) || !has_object_or_array_value(nodes[i]))
                {
                    self.margin(cx);
                }
                if nodes[i].is_some() {
                    if multiline {
                        self.newline(cx);
                    } else {
                        self.write(cx, " ");
                    }
                }
            }
            let child = children[i];
            self.append(cx, &child);
        }

        let last = nodes.last().copied().flatten();
        self.flush_comments_until(cx, end(last), until, false, false);

        if multiline {
            self.dedent(cx);
            if trailing_newline {
                self.newline(cx);
            }
        } else if pad && length > 0 {
            self.write(cx, " ");
        }
    }

    fn sequence_of(&mut self, cx: &mut Ctx, nodes: &'a [Node], until: Option<Position>, pad: bool) {
        let list: Vec<Option<&Node>> = nodes.iter().map(Some).collect();
        self.sequence(cx, &list, until, pad, ",", true);
    }

    /// `body(context, node)`
    fn body(&mut self, cx: &mut Ctx, node: &'a Node, body: &'a [Node]) {
        self.reset_comment_index(node);

        let mut prev_type: Option<&str> = None;
        let mut prev_multiline = false;

        for (i, child) in body.iter().enumerate() {
            if child.is("EmptyStatement") {
                continue;
            }
            let mut child_cx = self.new_ctx();
            self.visit(&mut child_cx, child);

            if let Some(prev_type) = prev_type {
                if child_cx.multiline || prev_multiline || child.type_name() != prev_type {
                    self.margin(cx);
                }
                self.newline(cx);
            }

            self.append(cx, &child_cx);

            let next = end(body.get(i + 1)).or(node.loc.map(|l| l.end));
            self.flush_trailing_comments(cx, end(Some(child)), next);

            prev_type = Some(child.type_name());
            prev_multiline = child_cx.multiline;
        }

        if let Some(loc) = node.loc {
            if !self.empty(cx.buf) {
                self.newline(cx);
            }
            self.flush_comments_until(cx, end(body.last()), Some(loc.end), false, false);
        }
    }

    fn write_import_attributes(&mut self, cx: &mut Ctx, attributes: &'a [Node]) {
        if attributes.is_empty() {
            return;
        }
        self.write(cx, " with ");
        self.write(cx, "{ ");
        for (i, a) in attributes.iter().enumerate() {
            if let NodeKind::ImportAttribute(a) = &a.kind {
                self.visit(cx, &a.key);
                self.write(cx, ": ");
                self.visit(cx, &a.value);
            }
            if i < attributes.len() - 1 {
                self.write(cx, ", ");
            }
        }
        self.write(cx, " }");
    }

    fn handle_var_declarator(&mut self, cx: &mut Ctx, node: &'a Node, no_in: bool) {
        let NodeKind::VariableDeclarator(d) = &node.kind else { return self.visit(cx, node) };
        self.track_binding(&d.id);
        self.visit(cx, &d.id);
        if let Some(init) = &d.init {
            self.write(cx, " = ");
            let wrap = no_in && contains_in_operator(init);
            self.maybe_wrap(cx, init, wrap);
        }
    }

    fn handle_var_declaration(&mut self, cx: &mut Ctx, node: &'a Node, d: &'a VariableDeclaration, no_in: bool) {
        let mut open = self.new_ctx();
        let mut join = self.new_ctx();
        let mut child = self.new_ctx();

        self.append(cx, &child);
        self.token(&mut child, d.kind.as_str(), node);
        self.write(&mut child, " ");
        self.append(&mut child, &open);

        let mut first = true;
        for declarator in &d.declarations {
            if !first {
                self.append(&mut child, &join);
            }
            first = false;
            self.handle_var_declarator(&mut child, declarator, no_in);
        }

        let n = d.declarations.len();
        let length = self.measure(child.buf) as i64 + 2 * (n as i64 - 1);
        let multiline = child.multiline || (n > 1 && length > 50);

        if multiline {
            cx.multiline = true;
            if n > 1 {
                self.indent(&open);
            }
            self.write(&mut join, ",");
            self.newline(&mut join);
            if n > 1 {
                self.dedent(cx);
            }
        } else {
            self.write(&mut join, ", ");
        }
        let _ = &mut open;
    }

    fn write_for_head_declaration(&mut self, cx: &mut Ctx, node: &'a Node, no_in: bool) {
        let NodeKind::VariableDeclaration(d) = &node.kind else { return self.visit(cx, node) };
        if let Some(loc) = node.loc {
            self.location(cx, loc.start);
        }
        self.handle_var_declaration(cx, node, d, no_in);
        if let Some(loc) = node.loc {
            self.location(cx, loc.end);
        }
    }

    /// `context.visit(node)` with the `_` visitor around it
    fn visit(&mut self, cx: &mut Ctx, node: &'a Node) {
        if self.node_comments {
            if let Some(c) = &node.comments {
                self.write_additional_comments(cx, &c.leading, true);
            }
        }

        let mut casts = 0;
        if let Some(loc) = node.loc {
            let is_expression = precedence(node).is_some() && !self.bindings.contains(&addr(node));
            casts = self.flush_comments_until(cx, None, Some(loc.start), true, is_expression);
            self.location(cx, loc.start);
        }

        self.visit_node(cx, node);

        if let Some(loc) = node.loc {
            self.location(cx, loc.end);
        }
        if casts > 0 {
            self.write(cx, ")".repeat(casts));
        }

        if self.node_comments {
            if let Some(c) = &node.comments {
                self.write_additional_comments(cx, &c.trailing, false);
            }
        }
    }

    fn visit_function_params_and_body(&mut self, cx: &mut Ctx, f: &'a Function, until_fallback: Option<Position>) {
        self.track_bindings(&f.params);
        self.write(cx, "(");
        let until = start(Some(&f.body)).or(until_fallback);
        self.sequence_of(cx, &f.params, until, false);
        self.write(cx, ")");
    }

    fn visit_node(&mut self, cx: &mut Ctx, node: &'a Node) {
        use NodeKind as K;
        match &node.kind {
            K::ArrayExpression(a) | K::ArrayPattern(a) => {
                self.write(cx, "[");
                let list: Vec<Option<&Node>> = a.elements.iter().map(Option::as_ref).collect();
                self.sequence(cx, &list, end(Some(node)), false, ",", true);
                self.write(cx, "]");
            }
            K::BinaryExpression(BinaryExpression { left, right, .. })
            | K::LogicalExpression(LogicalExpression { left, right, .. }) => {
                let wrap = operand_needs_wrap(left, node, false);
                self.maybe_wrap(cx, left, wrap);
                let op = binary_operator_str(node).unwrap();
                self.write(cx, spaced_operator(op));
                let wrap = operand_needs_wrap(right, node, true);
                self.maybe_wrap(cx, right, wrap);
            }
            K::BlockStatement(b) | K::ClassBody(b) => {
                self.token(cx, "{", node);
                let mut child = self.new_ctx();
                self.body(&mut child, node, &b.body);
                if !self.empty(child.buf) {
                    self.indent(cx);
                    self.newline(cx);
                    self.append(cx, &child);
                    self.dedent(cx);
                    self.newline(cx);
                }
                if let Some(loc) = node.loc {
                    self.location(cx, Position::new(loc.end.line, loc.end.column.wrapping_sub(1)));
                }
                self.write(cx, "}");
            }
            K::CallExpression(CallExpression { callee, arguments, .. }) | K::NewExpression(NewExpression { callee, arguments }) => {
                let is_new = matches!(node.kind, K::NewExpression(_));
                if is_new {
                    self.token(cx, "new", node);
                    self.write(cx, " ");
                }
                if let Some(loc) = callee.loc {
                    self.location(cx, loc.start);
                }
                let wrap = callee.is("ChainExpression") || prec_lt(callee, 19) || (is_new && has_call_expression(callee));
                self.maybe_wrap(cx, callee, wrap);
                if let K::CallExpression(c) = &node.kind {
                    if c.optional {
                        self.write(cx, "?.");
                    }
                }

                let mut open = self.new_ctx();
                let mut join = self.new_ctx();
                self.write(cx, "(");
                self.append(cx, &open);

                let mut child = self.new_ctx();
                let mut last = self.new_ctx();
                self.append(cx, &child);
                self.append(cx, &last);

                let n = arguments.len();
                for (i, arg) in arguments.iter().enumerate() {
                    let is_last = i == n - 1;
                    if is_last {
                        if let (Some(arg_loc), Some(c)) = (arg.loc, self.comment_loc(self.comment_index)) {
                            if c.start.line < arg_loc.start.line {
                                child.multiline = true;
                            }
                        }
                    }
                    let ctx = if is_last { &mut last } else { &mut child };
                    self.visit(ctx, arg);
                    if !is_last {
                        self.write(ctx, ",");
                    }
                    let next = if is_last { end(Some(node)) } else { start(arguments.get(i + 1)) };
                    self.flush_trailing_comments(ctx, end(Some(arg)), next);
                    if !is_last {
                        self.append(ctx, &join);
                    }
                }

                cx.multiline |= child.multiline || last.multiline;

                if child.multiline {
                    self.indent(&open);
                    self.newline(&mut open);
                    self.newline(&mut join);
                    self.dedent(cx);
                    self.newline(cx);
                } else {
                    self.write(&mut join, " ");
                }
                self.write(cx, ")");
            }
            K::ClassDeclaration(c) | K::ClassExpression(c) => {
                self.write(cx, "class ");
                if let Some(id) = &c.id {
                    self.visit(cx, id);
                    self.write(cx, " ");
                }
                if let Some(super_class) = &c.super_class {
                    self.write(cx, "extends ");
                    let wrap = prec_lt(super_class, 19);
                    self.maybe_wrap(cx, super_class, wrap);
                    self.write(cx, " ");
                }
                self.visit(cx, &c.body);
            }
            K::ForInStatement(f) | K::ForOfStatement(f) => {
                let is_of = matches!(node.kind, K::ForOfStatement(_));
                self.token(cx, "for", node);
                self.write(cx, " ");
                if is_of && f.is_await {
                    self.write(cx, "await ");
                }
                self.write(cx, "(");
                if f.left.is("VariableDeclaration") {
                    self.write_for_head_declaration(cx, &f.left, false);
                } else {
                    self.visit(cx, &f.left);
                }
                self.write(cx, if is_of { " of " } else { " in " });
                self.visit(cx, &f.right);
                self.write(cx, ") ");
                self.visit(cx, &f.body);
            }
            K::FunctionDeclaration(f) | K::FunctionExpression(f) => {
                if f.is_async {
                    self.write(cx, "async ");
                }
                self.write(cx, "function");
                self.write(cx, if f.generator { "* " } else { " " });
                if let Some(id) = &f.id {
                    self.track_binding(id);
                    self.visit(cx, id);
                }
                self.visit_function_params_and_body(cx, f, None);
                self.write(cx, " ");
                self.visit(cx, &f.body);
            }
            K::MethodDefinition(m) => {
                if m.is_static {
                    self.write(cx, "static ");
                }
                if matches!(m.kind, MethodKind::Get | MethodKind::Set) {
                    self.write(cx, if m.kind == MethodKind::Get { "get " } else { "set " });
                }
                let K::FunctionExpression(f) = &m.value.kind else { return };
                if f.is_async {
                    self.write(cx, "async ");
                }
                if f.generator {
                    self.write(cx, "*");
                }
                if m.computed {
                    self.write(cx, "[");
                }
                self.visit(cx, &m.key);
                if m.computed {
                    self.write(cx, "]");
                }
                self.visit_function_params_and_body(cx, f, end(Some(node)));
                self.write(cx, " ");
                self.visit(cx, &f.body);
            }
            K::PropertyDefinition(p) => {
                if p.is_abstract {
                    self.write(cx, "abstract ");
                }
                if p.is_static {
                    self.write(cx, "static ");
                }
                if p.computed {
                    self.write(cx, "[");
                    self.visit(cx, &p.key);
                    self.write(cx, "]");
                } else {
                    self.visit(cx, &p.key);
                }
                if let Some(value) = &p.value {
                    self.write(cx, " = ");
                    self.visit(cx, value);
                }
                self.write(cx, ";");
                let prev = end(Some(p.value.as_deref().unwrap_or(&p.key)));
                self.flush_trailing_comments(cx, prev, None);
            }
            K::RestElement(r) | K::SpreadElement(r) => {
                self.write(cx, "...");
                self.visit(cx, &r.argument);
            }
            K::ArrowFunctionExpression(a) => {
                if a.is_async {
                    self.write(cx, "async ");
                }
                self.track_bindings(&a.params);
                self.write(cx, "(");
                self.sequence_of(cx, &a.params, start(Some(&a.body)), false);
                self.write(cx, ")");
                self.write(cx, " => ");
                let wrap = arrow_concise_body_needs_wrap(&a.body);
                self.maybe_wrap(cx, &a.body, wrap);
            }
            K::AssignmentExpression(a) => {
                self.visit(cx, &a.left);
                self.write(cx, spaced_assignment_operator(a.operator));
                self.visit(cx, &a.right);
            }
            K::AssignmentPattern(a) => {
                self.visit(cx, &a.left);
                self.write(cx, " = ");
                self.visit(cx, &a.right);
            }
            K::AwaitExpression(a) => {
                self.token(cx, "await", node);
                self.write(cx, " ");
                if prec_lt(&a.argument, 17) {
                    self.maybe_wrap(cx, &a.argument, true);
                } else {
                    self.visit(cx, &a.argument);
                }
            }
            K::BreakStatement(j) | K::ContinueStatement(j) => {
                self.token(cx, if matches!(node.kind, K::BreakStatement(_)) { "break" } else { "continue" }, node);
                if let Some(label) = &j.label {
                    self.write(cx, " ");
                    self.visit(cx, label);
                }
                self.write(cx, ";");
            }
            K::ChainExpression(c) => self.visit(cx, &c.expression),
            K::ConditionalExpression(c) => {
                let wrap = precedence(&c.test).is_some_and(|p| p <= 4);
                self.maybe_wrap(cx, &c.test, wrap);

                let mut consequent = self.new_ctx();
                let mut alternate = self.new_ctx();
                self.visit(&mut consequent, &c.consequent);
                self.visit(&mut alternate, &c.alternate);

                if consequent.multiline
                    || alternate.multiline
                    || self.measure(consequent.buf) + self.measure(alternate.buf) > 50
                {
                    self.indent(cx);
                    self.newline(cx);
                    self.write(cx, "? ");
                    self.append(cx, &consequent);
                    self.newline(cx);
                    self.write(cx, ": ");
                    self.append(cx, &alternate);
                    self.dedent(cx);
                } else {
                    self.write(cx, " ? ");
                    self.append(cx, &consequent);
                    self.write(cx, " : ");
                    self.append(cx, &alternate);
                }
            }
            K::DebuggerStatement => {
                self.write_node(cx, "debugger", node);
                self.write(cx, ";");
            }
            K::DoWhileStatement(w) => {
                self.token(cx, "do", node);
                self.write(cx, " ");
                self.visit(cx, &w.body);
                self.write(cx, " while ");
                self.write(cx, "(");
                self.visit(cx, &w.test);
                self.write(cx, ")");
                self.write(cx, ";");
            }
            K::EmptyStatement => self.write(cx, ";"),
            K::ExportAllDeclaration(e) => {
                self.token(cx, "export", node);
                self.write(cx, " * ");
                if let Some(exported) = &e.exported {
                    self.write(cx, "as ");
                    self.visit(cx, exported);
                }
                self.write(cx, " from ");
                self.visit(cx, &e.source);
                self.write_import_attributes(cx, &e.attributes);
                self.write(cx, ";");
            }
            K::ExportDefaultDeclaration(e) => {
                let d = &e.declaration;
                if let Some(loc) = node.loc {
                    self.location(cx, loc.start);
                }
                self.token(cx, "export", node);
                self.write(cx, " default ");
                if let Some(loc) = d.loc {
                    self.flush_comments_until(cx, None, Some(loc.start), true, false);
                }
                self.visit(cx, d);
                if !d.is("FunctionDeclaration") {
                    self.write(cx, ";");
                }
            }
            K::ExportNamedDeclaration(e) => {
                if let Some(d) = &e.declaration {
                    if let Some(loc) = node.loc {
                        self.location(cx, loc.start);
                    }
                    self.token(cx, "export", node);
                    self.write(cx, " ");
                    if let Some(loc) = d.loc {
                        self.flush_comments_until(cx, None, Some(loc.start), true, false);
                    }
                    self.visit(cx, d);
                    return;
                }
                self.token(cx, "export", node);
                self.write(cx, " ");
                self.write(cx, "{");
                let until = start(e.source.as_deref()).or(end(Some(node)));
                self.sequence_of(cx, &e.specifiers, until, true);
                self.write(cx, "}");
                if let Some(source) = &e.source {
                    self.write(cx, " from ");
                    self.visit(cx, source);
                    self.write_import_attributes(cx, &e.attributes);
                }
                self.write(cx, ";");
            }
            K::ExportSpecifier(s) => {
                self.visit(cx, &s.local);
                if !same_module_name(&s.local, &s.exported) {
                    self.write(cx, " as ");
                    self.visit(cx, &s.exported);
                }
            }
            K::ExpressionStatement(s) => {
                let wrap = leads_with_curly_or_keyword(&s.expression);
                self.maybe_wrap(cx, &s.expression, wrap);
                self.write(cx, ";");
            }
            K::ForStatement(f) => {
                self.token(cx, "for", node);
                self.write(cx, " (");
                if let Some(init) = &f.init {
                    if init.is("VariableDeclaration") {
                        self.write_for_head_declaration(cx, init, true);
                    } else {
                        let wrap = contains_in_operator(init);
                        self.maybe_wrap(cx, init, wrap);
                    }
                }
                self.write(cx, "; ");
                if let Some(test) = &f.test {
                    self.visit(cx, test);
                }
                self.write(cx, "; ");
                if let Some(update) = &f.update {
                    self.visit(cx, update);
                }
                self.write(cx, ") ");
                self.visit(cx, &f.body);
            }
            K::Identifier(id) => self.token_str(cx, &id.name, node),
            K::IfStatement(i) => {
                self.token(cx, "if", node);
                self.write(cx, " (");
                self.visit(cx, &i.test);
                self.write(cx, ") ");
                if i.alternate.is_some() && statement_ends_with_unmatched_if(&i.consequent) {
                    let loc = i.consequent.loc;
                    if let Some(loc) = loc {
                        self.location(cx, loc.start);
                    }
                    self.write(cx, "{");
                    self.indent(cx);
                    self.newline(cx);
                    self.visit(cx, &i.consequent);
                    self.dedent(cx);
                    self.newline(cx);
                    self.write(cx, "}");
                    if let Some(loc) = loc {
                        self.location(cx, loc.end);
                    }
                } else {
                    self.visit(cx, &i.consequent);
                }
                if let Some(alternate) = &i.alternate {
                    self.space(cx);
                    self.write(cx, "else ");
                    self.visit(cx, alternate);
                }
            }
            K::ImportDeclaration(d) => {
                self.token(cx, "import", node);
                self.write(cx, " ");
                if d.specifiers.is_empty() {
                    self.visit(cx, &d.source);
                    self.write_import_attributes(cx, &d.attributes);
                    self.write(cx, ";");
                    return;
                }
                let mut namespace = None;
                let mut default = None;
                let mut named: Vec<Option<&Node>> = Vec::new();
                for s in &d.specifiers {
                    match &s.kind {
                        K::ImportNamespaceSpecifier(_) => namespace = Some(s),
                        K::ImportDefaultSpecifier(_) => default = Some(s),
                        _ => named.push(Some(s)),
                    }
                }
                let local_name = |s: &'a Node| -> &'a str {
                    match &s.kind {
                        K::ImportNamespaceSpecifier(l) | K::ImportDefaultSpecifier(l) => {
                            l.local.identifier_name().map_or("", |n| n.as_str())
                        }
                        _ => "",
                    }
                };
                if let Some(s) = default {
                    self.write_node(cx, local_name(s), s);
                    if namespace.is_some() || !named.is_empty() {
                        self.write(cx, ", ");
                    }
                }
                if let Some(s) = namespace {
                    self.write_node(cx, format!("* as {}", local_name(s)), s);
                }
                if !named.is_empty() {
                    self.write(cx, "{");
                    self.sequence(cx, &named, start(Some(&d.source)), true, ",", true);
                    self.write(cx, "}");
                }
                self.write(cx, " from ");
                self.visit(cx, &d.source);
                self.write_import_attributes(cx, &d.attributes);
                self.write(cx, ";");
            }
            K::ImportExpression(i) => {
                self.token(cx, "import", node);
                self.write(cx, "(");
                self.visit(cx, &i.source);
                if let Some(options) = &i.options {
                    self.write(cx, ", ");
                    self.visit(cx, options);
                }
                self.write(cx, ")");
            }
            K::ImportSpecifier(s) => {
                if !same_module_name(&s.imported, &s.local) {
                    self.visit(cx, &s.imported);
                    self.write(cx, " as ");
                    self.visit(cx, &s.local);
                } else {
                    self.visit(cx, &s.local);
                }
            }
            K::LabeledStatement(l) => {
                self.visit(cx, &l.label);
                self.write(cx, ": ");
                self.visit(cx, &l.body);
            }
            K::Literal(l) => {
                let value: Cow<'a, str> = match &l.raw {
                    Some(raw) if !raw.is_empty() => Cow::Borrowed(raw.as_str()),
                    _ => Cow::Owned(match &l.value {
                        LiteralValue::BigInt(b) => format!("{b}n"),
                        LiteralValue::String(s) => quote(s, self.quote),
                        LiteralValue::Number(n) => js_number_to_string(*n),
                        LiteralValue::Boolean(b) => b.to_string(),
                        LiteralValue::Null => "null".into(),
                        LiteralValue::RegExp(r) => format!("/{}/{}", r.pattern, r.flags),
                    }),
                };
                self.write_node(cx, value, node);
            }
            K::MemberExpression(m) => {
                let wrap = m.object.is("ChainExpression") || prec_lt(&m.object, 19);
                self.maybe_wrap(cx, &m.object, wrap);
                if m.computed {
                    if m.optional {
                        self.write(cx, "?.");
                    }
                    self.write(cx, "[");
                    self.visit(cx, &m.property);
                    self.write(cx, "]");
                } else {
                    self.write(cx, if m.optional { "?." } else { "." });
                    self.visit(cx, &m.property);
                }
            }
            K::MetaProperty(m) => {
                self.visit(cx, &m.meta);
                self.write(cx, ".");
                self.visit(cx, &m.property);
            }
            K::ObjectExpression(o) | K::ObjectPattern(o) => {
                self.write(cx, "{");
                self.sequence_of(cx, &o.properties, end(Some(node)), true);
                self.write(cx, "}");
            }
            K::ParenthesizedExpression(p) => {
                if p.expression.is("SequenceExpression") {
                    if let Some(loc) = p.expression.loc {
                        self.location(cx, loc.start);
                    }
                    self.write(cx, "(");
                    let a = addr(&p.expression);
                    self.parenthesized_sequences.insert(a);
                    self.visit(cx, &p.expression);
                    self.parenthesized_sequences.remove(&a);
                } else if node.loc.is_some() {
                    self.write(cx, "(");
                    self.visit(cx, &p.expression);
                    self.write(cx, ")");
                } else {
                    self.maybe_wrap(cx, &p.expression, true);
                }
            }
            K::PrivateIdentifier(id) => {
                self.write(cx, "#");
                self.write_node(cx, id.name.as_str(), node);
            }
            K::Program(p) => self.body(cx, node, &p.body),
            K::Property(p) => {
                let value = match &p.value.kind {
                    K::AssignmentPattern(a) => &a.left,
                    _ => &p.value,
                };
                let shorthand = !p.computed
                    && p.kind == PropertyKind::Init
                    && matches!((&p.key.kind, &value.kind), (K::Identifier(k), K::Identifier(v)) if k.name == v.name);
                if shorthand {
                    self.visit(cx, &p.value);
                    return;
                }
                match &p.value.kind {
                    K::FunctionExpression(f) if p.method || p.kind != PropertyKind::Init => {
                        if p.kind != PropertyKind::Init {
                            self.write(cx, if p.kind == PropertyKind::Get { "get " } else { "set " });
                        }
                        if f.is_async {
                            self.write(cx, "async ");
                        }
                        if f.generator {
                            self.write(cx, "*");
                        }
                        if p.computed {
                            self.write(cx, "[");
                        }
                        self.visit(cx, &p.key);
                        if p.computed {
                            self.write(cx, "]");
                        }
                        self.visit_function_params_and_body(cx, f, None);
                        self.write(cx, " ");
                        self.visit(cx, &f.body);
                    }
                    _ => {
                        if p.computed {
                            self.write(cx, "[");
                        }
                        if p.kind != PropertyKind::Init {
                            self.write(cx, if p.kind == PropertyKind::Get { "get " } else { "set " });
                        }
                        self.visit(cx, &p.key);
                        if p.computed {
                            self.write(cx, "]");
                        }
                        self.write(cx, ": ");
                        self.visit(cx, &p.value);
                    }
                }
            }
            K::ReturnStatement(r) => {
                self.token(cx, "return", node);
                if let Some(argument) = &r.argument {
                    let contains_comment = match (self.comment_loc(self.comment_index), argument.loc) {
                        (Some(c), Some(a)) => before(c.start, a.start),
                        _ => false,
                    };
                    self.write(cx, if contains_comment { " (" } else { " " });
                    self.visit(cx, argument);
                    self.write(cx, if contains_comment { ");" } else { ";" });
                } else {
                    self.write(cx, ";");
                }
            }
            K::SequenceExpression(s) => {
                let wrap = !self.parenthesized_sequences.contains(&addr(node));
                if wrap {
                    self.write(cx, "(");
                }
                self.sequence_of(cx, &s.expressions, end(Some(node)), false);
                self.write(cx, ")");
            }
            K::StaticBlock(b) => {
                self.write(cx, "static ");
                self.write(cx, "{");
                self.indent(cx);
                self.newline(cx);
                self.body(cx, node, &b.body);
                self.dedent(cx);
                self.newline(cx);
                self.write(cx, "}");
            }
            K::Super => self.write_node(cx, "super", node),
            K::SwitchCase(c) => {
                if let Some(test) = &c.test {
                    self.token(cx, "case", node);
                    self.write(cx, " ");
                    self.visit(cx, test);
                    self.write(cx, ":");
                } else {
                    self.token(cx, "default", node);
                    self.write(cx, ":");
                }
                self.indent(cx);
                for statement in &c.consequent {
                    self.newline(cx);
                    self.visit(cx, statement);
                }
                self.dedent(cx);
            }
            K::SwitchStatement(s) => {
                self.token(cx, "switch", node);
                self.write(cx, " (");
                self.visit(cx, &s.discriminant);
                self.write(cx, ") ");
                self.write(cx, "{");
                self.indent(cx);
                let mut first = true;
                for case in &s.cases {
                    if !first {
                        self.margin(cx);
                    }
                    first = false;
                    self.newline(cx);
                    self.visit(cx, case);
                }
                self.dedent(cx);
                self.newline(cx);
                self.write(cx, "}");
            }
            K::TaggedTemplateExpression(t) => {
                let wrap = t.tag.is("ChainExpression") || prec_lt(&t.tag, 19);
                self.maybe_wrap(cx, &t.tag, wrap);
                self.visit(cx, &t.quasi);
            }
            K::TemplateLiteral(t) => {
                self.write(cx, "`");
                let raw = |q: &'a Node| -> &'a str {
                    match &q.kind {
                        K::TemplateElement(e) => e.raw.as_str(),
                        _ => "",
                    }
                };
                for (i, expression) in t.expressions.iter().enumerate() {
                    let r = raw(&t.quasis[i]);
                    self.write(cx, r);
                    self.write(cx, "${");
                    self.visit(cx, expression);
                    self.write(cx, "}");
                    if r.contains('\n') {
                        cx.multiline = true;
                    }
                }
                let r = t.quasis.last().map_or("", raw);
                self.write(cx, r);
                self.write(cx, "`");
                if r.contains('\n') {
                    cx.multiline = true;
                }
            }
            K::ThisExpression => self.write_node(cx, "this", node),
            K::ThrowStatement(t) => {
                self.token(cx, "throw", node);
                self.write(cx, " ");
                self.visit(cx, &t.argument);
                self.write(cx, ";");
            }
            K::CatchClause(c) => {
                self.token(cx, "catch", node);
                if let Some(param) = &c.param {
                    self.write(cx, "(");
                    self.track_binding(param);
                    self.visit(cx, param);
                    self.write(cx, ")");
                }
                self.write(cx, " ");
                self.visit(cx, &c.body);
            }
            K::TryStatement(t) => {
                self.token(cx, "try", node);
                self.write(cx, " ");
                self.visit(cx, &t.block);
                if let Some(handler) = &t.handler {
                    self.write(cx, " ");
                    self.visit(cx, handler);
                }
                if let Some(finalizer) = &t.finalizer {
                    self.write(cx, " finally ");
                    self.visit(cx, finalizer);
                }
            }
            K::UnaryExpression(u) => {
                let op = u.operator.as_str();
                self.token(cx, op, node);
                if op.len() > 1 {
                    self.write(cx, " ");
                } else if (op == "+" || op == "-")
                    && match &u.argument.kind {
                        K::UnaryExpression(a) => a.operator.as_str() == op,
                        K::UpdateExpression(a) => a.prefix && a.operator.as_str().starts_with(op),
                        _ => false,
                    }
                {
                    self.write(cx, " ");
                }
                let wrap = prec_lt(&u.argument, 15);
                self.maybe_wrap(cx, &u.argument, wrap);
            }
            K::UpdateExpression(u) => {
                let wrap = prec_lt(&u.argument, 16);
                if u.prefix {
                    self.write(cx, u.operator.as_str());
                }
                self.maybe_wrap(cx, &u.argument, wrap);
                if !u.prefix {
                    self.write(cx, u.operator.as_str());
                }
            }
            K::VariableDeclaration(d) => {
                self.handle_var_declaration(cx, node, d, false);
                self.write(cx, ";");
            }
            K::VariableDeclarator(_) => self.handle_var_declarator(cx, node, false),
            K::WhileStatement(w) => {
                self.token(cx, "while", node);
                self.write(cx, " (");
                self.visit(cx, &w.test);
                self.write(cx, ") ");
                self.visit(cx, &w.body);
            }
            K::WithStatement(w) => {
                self.token(cx, "with", node);
                self.write(cx, " (");
                self.visit(cx, &w.object);
                self.write(cx, ") ");
                self.visit(cx, &w.body);
            }
            K::YieldExpression(y) => {
                self.token(cx, if y.delegate { "yield*" } else { "yield" }, node);
                if let Some(argument) = &y.argument {
                    self.write(cx, " ");
                    self.visit(cx, argument);
                }
            }
            K::TSImportEqualsDeclaration(d) => {
                self.token(cx, "import", node);
                self.write(cx, " ");
                if d.is_type {
                    self.write(cx, "type ");
                }
                self.visit(cx, &d.id);
                self.write(cx, " = ");
                self.visit(cx, &d.module_reference);
            }
            K::TSExternalModuleReference(r) => {
                self.write(cx, "require");
                self.write(cx, "(");
                self.visit(cx, &r.expression);
                self.write(cx, ")");
                self.write(cx, ";");
            }
            K::TSQualifiedName(q) => {
                self.visit(cx, &q.left);
                self.write(cx, ".");
                self.visit(cx, &q.right);
            }
            K::TSExportAssignment(e) => {
                self.write(cx, "export ");
                self.write(cx, "= ");
                self.visit(cx, &e.expression);
                self.write(cx, ";");
            }
            K::TSNamespaceExportDeclaration(d) => {
                self.token(cx, "export", node);
                self.write(cx, " as ");
                self.write(cx, "namespace ");
                self.visit(cx, &d.id);
                self.write(cx, ";");
            }
            // esrap has no visitor for these: their parents write them
            K::TemplateElement(_) | K::ImportAttribute(_) | K::ImportDefaultSpecifier(_) | K::ImportNamespaceSpecifier(_) => {
                panic!("Not implemented: {}", node.type_name())
            }
        }
    }
}
