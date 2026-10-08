//! How svelte-language-server finds a component's `<style>` tag: its `parseHtml` (an adapted
//! copy of vscode-html-languageservice's parser on top of that package's scanner) and
//! `extractStyleTag` (`lib/documents/utils.ts`). Works on UTF-16 code units.

use super::data::VOID_ELEMENTS;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    WithinContent,
    AfterOpeningStartTag,
    AfterOpeningEndTag,
    WithinDoctype,
    WithinTag,
    WithinEndTag,
    WithinComment,
    WithinScriptContent,
    WithinStyleContent,
    AfterAttributeName,
    BeforeAttributeValue,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tok {
    StartCommentTag,
    Comment,
    EndCommentTag,
    StartTagOpen,
    StartTagClose,
    StartTagSelfClose,
    StartTag,
    EndTagOpen,
    EndTagClose,
    EndTag,
    DelimiterAssign,
    AttributeName,
    AttributeValue,
    StartDoctypeTag,
    Doctype,
    EndDoctypeTag,
    Content,
    Whitespace,
    Unknown,
    Script,
    Styles,
    EOS,
}

const LAN: u16 = b'<' as u16;
const RAN: u16 = b'>' as u16;
const FSL: u16 = b'/' as u16;
const EQS: u16 = b'=' as u16;
const SQO: u16 = b'\'' as u16;
const DQO: u16 = b'"' as u16;
const BNG: u16 = b'!' as u16;
const LBR: u16 = b'{' as u16;
const RBR: u16 = b'}' as u16;

/// JS `\s` (and the characters `String.prototype.trim` removes)
pub fn is_js_ws(c: u16) -> bool {
    matches!(
        c,
        0x09 | 0x0a | 0x0b | 0x0c | 0x0d | 0x20 | 0xa0 | 0x1680 | 0x2000..=0x200a | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000 | 0xfeff
    )
}

fn is_html_ws(c: u16) -> bool {
    matches!(c, 0x20 | 0x09 | 0x0a | 0x0c | 0x0d)
}

fn is_ascii_word(c: u16) -> bool {
    c < 0x80 && ((c as u8).is_ascii_alphanumeric() || c == b'_' as u16)
}

fn eq_ignore_ascii_case(t: &[u16], s: &[u8]) -> bool {
    t.len() == s.len() && t.iter().zip(s).all(|(&c, &k)| c < 0x80 && (c as u8).eq_ignore_ascii_case(&k))
}

fn starts_with_ci(t: &[u16], at: usize, s: &[u8]) -> bool {
    at + s.len() <= t.len() && eq_ignore_ascii_case(&t[at..at + s.len()], s)
}

fn starts_with(t: &[u16], at: usize, s: &[u8]) -> bool {
    at + s.len() <= t.len() && t[at..at + s.len()].iter().zip(s).all(|(&c, &k)| c == k as u16)
}

/// vscode-html-languageservice's `createScanner`
struct HtmlScanner<'a> {
    src: &'a [u16],
    pos: usize,
    state: State,
    emit_pseudo_close_tags: bool,
    token_offset: usize,
    has_space_after_tag: bool,
    /// lowercased
    last_tag: Vec<u16>,
    /// lowercased
    last_attribute_name: Option<Vec<u16>>,
    last_type_value: Option<Vec<u16>>,
}

impl<'a> HtmlScanner<'a> {
    fn new(src: &'a [u16], offset: usize, state: State, emit_pseudo_close_tags: bool) -> Self {
        HtmlScanner {
            src,
            pos: offset,
            state,
            emit_pseudo_close_tags,
            token_offset: 0,
            has_space_after_tag: false,
            last_tag: Vec::new(),
            last_attribute_name: None,
            last_type_value: None,
        }
    }

    fn eos(&self) -> bool {
        self.src.len() <= self.pos
    }

    fn peek(&self, n: isize) -> u16 {
        let i = self.pos as isize + n;
        if i < 0 {
            return 0;
        }
        self.src.get(i as usize).copied().unwrap_or(0)
    }

    fn advance_if_char(&mut self, ch: u16) -> bool {
        if self.src.get(self.pos) == Some(&ch) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn advance_if_chars(&mut self, s: &[u8]) -> bool {
        if starts_with(self.src, self.pos, s) {
            self.pos += s.len();
            true
        } else {
            false
        }
    }

    fn advance_until_char(&mut self, ch: u16) -> bool {
        while self.pos < self.src.len() {
            if self.src[self.pos] == ch {
                return true;
            }
            self.pos += 1;
        }
        false
    }

    fn advance_until_chars(&mut self, s: &[u8]) -> bool {
        while self.pos + s.len() <= self.src.len() {
            if starts_with(self.src, self.pos, s) {
                return true;
            }
            self.pos += 1;
        }
        self.pos = self.src.len();
        false
    }

    fn skip_whitespace(&mut self) -> bool {
        let start = self.pos;
        while self.pos < self.src.len() && is_html_ws(self.src[self.pos]) {
            self.pos += 1;
        }
        self.pos > start
    }

    /// `/^[_:\w][_:\w-.\d]*/`, lowercased
    fn next_element_name(&mut self) -> Vec<u16> {
        let start = self.pos;
        let first = self.peek(0);
        if !(first == b'_' as u16 || first == b':' as u16 || is_ascii_word(first)) {
            return Vec::new();
        }
        self.pos += 1;
        while self.pos < self.src.len() {
            let c = self.src[self.pos];
            if c == b'_' as u16 || c == b':' as u16 || c == b'-' as u16 || c == b'.' as u16 || is_ascii_word(c) {
                self.pos += 1;
            } else {
                break;
            }
        }
        self.src[start..self.pos].iter().map(|&c| (c as u8).to_ascii_lowercase() as u16).collect()
    }

    /// `/^[^\s"'></=\x00-\x0F\x7F\x80-\x9F]*/`, lowercased
    fn next_attribute_name(&mut self) -> Vec<u16> {
        let start = self.pos;
        while self.pos < self.src.len() {
            let c = self.src[self.pos];
            if is_js_ws(c)
                || matches!(c, DQO | SQO | RAN | LAN | FSL | EQS)
                || c <= 0x0f
                || c == 0x7f
                || (0x80..=0x9f).contains(&c)
            {
                break;
            }
            self.pos += 1;
        }
        js_to_lower(&self.src[start..self.pos])
    }

    fn token_end(&self) -> usize {
        self.pos
    }

    fn token_text(&self) -> &'a [u16] {
        &self.src[self.token_offset.min(self.src.len())..self.pos.min(self.src.len())]
    }

    fn finish(&mut self, offset: usize, ty: Tok) -> Tok {
        self.token_offset = offset;
        ty
    }

    fn scan(&mut self) -> Tok {
        let offset = self.pos;
        let token = self.internal_scan();
        if token != Tok::EOS
            && offset == self.pos
            && !(self.emit_pseudo_close_tags && (token == Tok::StartTagClose || token == Tok::EndTagClose))
        {
            // 'Scanner.scan has not advanced'
            self.pos += 1;
            return self.finish(offset, Tok::Unknown);
        }
        token
    }

    fn internal_scan(&mut self) -> Tok {
        let offset = self.pos;
        if self.eos() {
            return self.finish(offset, Tok::EOS);
        }
        match self.state {
            State::WithinComment => {
                if self.advance_if_chars(b"-->") {
                    self.state = State::WithinContent;
                    return self.finish(offset, Tok::EndCommentTag);
                }
                self.advance_until_chars(b"-->");
                return self.finish(offset, Tok::Comment);
            }
            State::WithinDoctype => {
                if self.advance_if_char(RAN) {
                    self.state = State::WithinContent;
                    return self.finish(offset, Tok::EndDoctypeTag);
                }
                self.advance_until_char(RAN);
                return self.finish(offset, Tok::Doctype);
            }
            State::WithinContent => {
                if self.advance_if_char(LAN) {
                    if !self.eos() && self.peek(0) == BNG {
                        if self.advance_if_chars(b"!--") {
                            self.state = State::WithinComment;
                            return self.finish(offset, Tok::StartCommentTag);
                        }
                        if starts_with_ci(self.src, self.pos, b"!doctype") {
                            self.pos += 8;
                            self.state = State::WithinDoctype;
                            return self.finish(offset, Tok::StartDoctypeTag);
                        }
                    }
                    if self.advance_if_char(FSL) {
                        self.state = State::AfterOpeningEndTag;
                        return self.finish(offset, Tok::EndTagOpen);
                    }
                    self.state = State::AfterOpeningStartTag;
                    return self.finish(offset, Tok::StartTagOpen);
                }
                self.advance_until_char(LAN);
                return self.finish(offset, Tok::Content);
            }
            State::AfterOpeningEndTag => {
                let tag_name = self.next_element_name();
                if !tag_name.is_empty() {
                    self.state = State::WithinEndTag;
                    return self.finish(offset, Tok::EndTag);
                }
                if self.skip_whitespace() {
                    return self.finish(offset, Tok::Whitespace);
                }
                self.state = State::WithinEndTag;
                self.advance_until_char(RAN);
                if offset < self.pos {
                    return self.finish(offset, Tok::Unknown);
                }
                return self.internal_scan();
            }
            State::WithinEndTag => {
                if self.skip_whitespace() {
                    return self.finish(offset, Tok::Whitespace);
                }
                if self.advance_if_char(RAN) {
                    self.state = State::WithinContent;
                    return self.finish(offset, Tok::EndTagClose);
                }
                if self.emit_pseudo_close_tags && self.peek(0) == LAN {
                    self.state = State::WithinContent;
                    return self.finish(offset, Tok::EndTagClose);
                }
            }
            State::AfterOpeningStartTag => {
                self.last_tag = self.next_element_name();
                self.last_type_value = None;
                self.last_attribute_name = None;
                if !self.last_tag.is_empty() {
                    self.has_space_after_tag = false;
                    self.state = State::WithinTag;
                    return self.finish(offset, Tok::StartTag);
                }
                if self.skip_whitespace() {
                    return self.finish(offset, Tok::Whitespace);
                }
                self.state = State::WithinTag;
                self.advance_until_char(RAN);
                if offset < self.pos {
                    return self.finish(offset, Tok::Unknown);
                }
                return self.internal_scan();
            }
            State::WithinTag => {
                if self.skip_whitespace() {
                    self.has_space_after_tag = true;
                    return self.finish(offset, Tok::Whitespace);
                }
                if self.has_space_after_tag {
                    let name = self.next_attribute_name();
                    let found = !name.is_empty();
                    self.last_attribute_name = Some(name);
                    if found {
                        self.state = State::AfterAttributeName;
                        self.has_space_after_tag = false;
                        return self.finish(offset, Tok::AttributeName);
                    }
                }
                if self.advance_if_chars(b"/>") {
                    self.state = State::WithinContent;
                    return self.finish(offset, Tok::StartTagSelfClose);
                }
                if self.advance_if_char(RAN) {
                    if is(&self.last_tag, b"script") {
                        let html_content = self
                            .last_type_value
                            .as_deref()
                            .is_some_and(|v| is(v, b"text/x-handlebars-template") || is(v, b"text/html"));
                        self.state = if html_content { State::WithinContent } else { State::WithinScriptContent };
                    } else if is(&self.last_tag, b"style") {
                        self.state = State::WithinStyleContent;
                    } else {
                        self.state = State::WithinContent;
                    }
                    return self.finish(offset, Tok::StartTagClose);
                }
                if self.emit_pseudo_close_tags && self.peek(0) == LAN {
                    self.state = State::WithinContent;
                    return self.finish(offset, Tok::StartTagClose);
                }
                self.pos += 1;
                return self.finish(offset, Tok::Unknown);
            }
            State::AfterAttributeName => {
                if self.skip_whitespace() {
                    self.has_space_after_tag = true;
                    return self.finish(offset, Tok::Whitespace);
                }
                if self.advance_if_char(EQS) {
                    self.state = State::BeforeAttributeValue;
                    return self.finish(offset, Tok::DelimiterAssign);
                }
                self.state = State::WithinTag;
                return self.internal_scan();
            }
            State::BeforeAttributeValue => {
                if self.skip_whitespace() {
                    return self.finish(offset, Tok::Whitespace);
                }
                // `/^[^\s"'`=<>]+/`
                let start = self.pos;
                while self.pos < self.src.len() {
                    let c = self.src[self.pos];
                    if is_js_ws(c) || matches!(c, DQO | SQO | EQS | LAN | RAN) || c == b'`' as u16 {
                        break;
                    }
                    self.pos += 1;
                }
                let mut value_end = self.pos;
                if value_end > start {
                    if self.peek(0) == RAN && self.peek(-1) == FSL {
                        self.pos -= 1;
                        value_end -= 1;
                    }
                    if self.last_attribute_name.as_deref().is_some_and(|n| is(n, b"type")) {
                        self.last_type_value = Some(self.src[start..value_end].to_vec());
                    }
                    if value_end > start {
                        self.state = State::WithinTag;
                        self.has_space_after_tag = false;
                        return self.finish(offset, Tok::AttributeValue);
                    }
                }
                let ch = self.peek(0);
                if ch == SQO || ch == DQO {
                    self.pos += 1;
                    if self.advance_until_char(ch) {
                        self.pos += 1;
                    }
                    if self.last_attribute_name.as_deref().is_some_and(|n| is(n, b"type")) {
                        let (a, b) = (offset + 1, self.pos.saturating_sub(1));
                        self.last_type_value = Some(js_substring(self.src, a, b).to_vec());
                    }
                    self.state = State::WithinTag;
                    self.has_space_after_tag = false;
                    return self.finish(offset, Tok::AttributeValue);
                }
                self.state = State::WithinTag;
                self.has_space_after_tag = false;
                return self.internal_scan();
            }
            State::WithinScriptContent => {
                let mut script_state = 1;
                while !self.eos() {
                    match find_script_token(self.src, self.pos) {
                        None => {
                            self.pos = self.src.len();
                            return self.finish(offset, Tok::Script);
                        }
                        Some((m_start, m_end)) => {
                            self.pos = m_end;
                            let m = &self.src[m_start..m_end];
                            if starts_with(m, 0, b"<!--") {
                                if script_state == 1 {
                                    script_state = 2;
                                }
                            } else if starts_with(m, 0, b"-->") {
                                script_state = 1;
                            } else if m[1] != FSL {
                                if script_state == 2 {
                                    script_state = 3;
                                }
                            } else if script_state == 3 {
                                script_state = 2;
                            } else {
                                self.pos = m_start;
                                break;
                            }
                        }
                    }
                }
                self.state = State::WithinContent;
                if offset < self.pos {
                    return self.finish(offset, Tok::Script);
                }
                return self.internal_scan();
            }
            State::WithinStyleContent => {
                // `advanceUntilRegExp(/<\/style/i)`
                let mut i = self.pos;
                let mut found = false;
                while i + 7 <= self.src.len() {
                    if self.src[i] == LAN && self.src[i + 1] == FSL && starts_with_ci(self.src, i + 2, b"style") {
                        found = true;
                        break;
                    }
                    i += 1;
                }
                self.pos = if found { i } else { self.src.len() };
                self.state = State::WithinContent;
                if offset < self.pos {
                    return self.finish(offset, Tok::Styles);
                }
                return self.internal_scan();
            }
        }
        // WithinEndTag fallthrough: 'Closing bracket expected.'
        self.pos += 1;
        self.state = State::WithinContent;
        self.finish(offset, Tok::Unknown)
    }
}

/// `/<!--|-->|<\/?script\s*\/?>?/i`, searched from `from`: (start, end) of the first match
fn find_script_token(src: &[u16], from: usize) -> Option<(usize, usize)> {
    let mut i = from;
    while i < src.len() {
        if starts_with(src, i, b"<!--") {
            return Some((i, i + 4));
        }
        if starts_with(src, i, b"-->") {
            return Some((i, i + 3));
        }
        if src[i] == LAN {
            let mut j = i + 1;
            if src.get(j) == Some(&FSL) {
                j += 1;
            }
            if starts_with_ci(src, j, b"script") {
                j += 6;
                while j < src.len() && is_js_ws(src[j]) {
                    j += 1;
                }
                if src.get(j) == Some(&FSL) {
                    j += 1;
                }
                if src.get(j) == Some(&RAN) {
                    j += 1;
                }
                return Some((i, j));
            }
        }
        i += 1;
    }
    None
}

fn is(t: &[u16], s: &[u8]) -> bool {
    t.len() == s.len() && t.iter().zip(s).all(|(&c, &k)| c == k as u16)
}

/// JS `toLowerCase` on UTF-16
fn js_to_lower(t: &[u16]) -> Vec<u16> {
    if t.iter().all(|&c| c < 0x80) {
        return t.iter().map(|&c| (c as u8).to_ascii_lowercase() as u16).collect();
    }
    String::from_utf16_lossy(t).to_lowercase().encode_utf16().collect()
}

/// JS `String.prototype.substring`
pub fn js_substring(src: &[u16], a: usize, b: usize) -> &[u16] {
    let (a, b) = (a.min(src.len()), b.min(src.len()));
    if a <= b { &src[a..b] } else { &src[b..a] }
}

pub struct HtmlNode {
    pub start: usize,
    pub end: usize,
    pub tag: Option<Vec<u16>>,
    pub start_tag_end: Option<usize>,
    pub end_tag_start: Option<usize>,
    /// `attributes` (raw names; `None` for a valueless attribute)
    pub attributes: Option<Vec<(Vec<u16>, Option<Vec<u16>>)>>,
    pub children: Vec<usize>,
    pub parent: Option<usize>,
}

impl HtmlNode {
    fn is_same_tag(&self, lower: Option<&[u16]>) -> bool {
        match (&self.tag, lower) {
            (None, l) => l.is_none(),
            (Some(_), None) => false,
            (Some(t), Some(l)) => t.len() == l.len() && js_to_lower(t) == l,
        }
    }

    fn set_attribute(&mut self, name: &[u16], value: Option<Vec<u16>>) {
        // `attributes["__proto__"] = ...` never creates an own property
        if is(name, b"__proto__") {
            return;
        }
        let attrs = self.attributes.get_or_insert_with(Vec::new);
        if let Some(slot) = attrs.iter_mut().find(|(n, _)| n == name) {
            slot.1 = value;
        } else {
            attrs.push((name.to_vec(), value));
        }
    }
}

/// svelte-language-server's `parseHtml`: returns the nodes, root is index 0
pub fn parse_html(text: &[u16]) -> Vec<HtmlNode> {
    let mut nodes = vec![HtmlNode {
        start: 0,
        end: text.len(),
        tag: None,
        start_tag_end: None,
        end_tag_start: None,
        attributes: None,
        children: Vec::new(),
        parent: None,
    }];
    let mut scanner = HtmlScanner::new(text, 0, State::WithinContent, true);
    let mut curr = 0usize;
    let mut end_tag_start: usize = 0;
    let mut end_tag_name: Option<Vec<u16>> = None;
    let mut pending_attribute: Option<Vec<u16>> = None;
    let mut token = scanner.scan();

    macro_rules! restart_scanner_at {
        ($offset:expr, $state:expr) => {{
            let offset = $offset;
            if offset > scanner.token_end() {
                scanner = HtmlScanner::new(text, offset, $state, true);
            }
        }};
    }
    macro_rules! finish_attribute {
        ($start:expr, $end:expr) => {{
            let (s, e) = ($start, $end);
            if let Some(name) = pending_attribute.clone()
                && nodes[curr].attributes.is_some()
            {
                let v = js_substring(text, s, e).to_vec();
                nodes[curr].set_attribute(&name, Some(v));
                pending_attribute = None;
            }
        }};
    }

    while token != Tok::EOS {
        match token {
            Tok::StartTagOpen => {
                let child = nodes.len();
                nodes.push(HtmlNode {
                    start: scanner.token_offset,
                    end: text.len(),
                    tag: None,
                    start_tag_end: None,
                    end_tag_start: None,
                    attributes: None,
                    children: Vec::new(),
                    parent: Some(curr),
                });
                nodes[curr].children.push(child);
                curr = child;
            }
            Tok::StartTag => {
                nodes[curr].tag = Some(scanner.token_text().to_vec());
            }
            Tok::StartTagClose => {
                if let Some(parent) = nodes[curr].parent {
                    nodes[curr].end = scanner.token_end();
                    if scanner.token_end() > scanner.token_offset {
                        nodes[curr].start_tag_end = Some(scanner.token_end());
                        let is_void = nodes[curr]
                            .tag
                            .as_deref()
                            .is_some_and(|t| VOID_ELEMENTS.iter().any(|v| is(t, v.as_bytes())));
                        if is_void {
                            curr = parent;
                        }
                    } else {
                        curr = parent;
                    }
                }
            }
            Tok::StartTagSelfClose => {
                if let Some(parent) = nodes[curr].parent {
                    nodes[curr].end = scanner.token_end();
                    nodes[curr].start_tag_end = Some(scanner.token_end());
                    curr = parent;
                }
            }
            Tok::EndTagOpen => {
                end_tag_start = scanner.token_offset;
                end_tag_name = None;
            }
            Tok::EndTag => {
                end_tag_name = Some(js_to_lower(scanner.token_text()));
            }
            Tok::EndTagClose => {
                let mut node = curr;
                while !nodes[node].is_same_tag(end_tag_name.as_deref())
                    && let Some(p) = nodes[node].parent
                {
                    node = p;
                }
                if nodes[node].parent.is_some() {
                    while curr != node {
                        nodes[curr].end = end_tag_start;
                        curr = nodes[curr].parent.unwrap();
                    }
                    nodes[curr].end_tag_start = Some(end_tag_start);
                    nodes[curr].end = scanner.token_end();
                    curr = nodes[curr].parent.unwrap();
                }
            }
            Tok::AttributeName => {
                let name = scanner.token_text().to_vec();
                nodes[curr].attributes.get_or_insert_with(Vec::new);
                nodes[curr].set_attribute(&name, None);
                pending_attribute = Some(name);
            }
            Tok::DelimiterAssign => {
                let after_assign = scanner.token_end();
                if text.get(after_assign) == Some(&LBR) {
                    let end = scan_matching_braces(text, after_assign);
                    restart_scanner_at!(end, State::WithinTag);
                    finish_attribute!(after_assign, end);
                }
            }
            Tok::Whitespace => {
                let after_ws = scanner.token_end();
                if text.get(after_ws) == Some(&LBR) {
                    if scanner.state == State::BeforeAttributeValue {
                        let end = scan_matching_braces(text, after_ws);
                        restart_scanner_at!(end, State::WithinTag);
                        finish_attribute!(after_ws, end);
                    } else {
                        // spread or attribute shorthand
                        let end = scan_matching_braces(text, after_ws);
                        restart_scanner_at!(end, State::WithinTag);
                        let rest = &text[(after_ws + 1).min(text.len())..];
                        if !starts_with(rest, 0, b"...") {
                            let expr = js_trim(js_substring(text, after_ws + 1, end.saturating_sub(1)));
                            let value = js_substring(text, after_ws, end).to_vec();
                            nodes[curr].attributes.get_or_insert_with(Vec::new);
                            nodes[curr].set_attribute(expr, Some(value));
                        }
                    }
                }
            }
            Tok::AttributeValue => {
                let start = scanner.token_offset;
                let quote = text.get(start).copied().unwrap_or(0);
                if quote != SQO && quote != DQO {
                    finish_attribute!(start, scanner.token_end());
                } else {
                    let token_end = scanner.token_end();
                    let mut expression_tag_end = skip_expression_in_range(text, start, token_end);
                    if expression_tag_end > token_end {
                        let quote_index = text[expression_tag_end.min(text.len())..].iter().position(|&c| c == quote);
                        expression_tag_end = match quote_index {
                            Some(i) => expression_tag_end + i + 1,
                            None => text.len(),
                        };
                        restart_scanner_at!(expression_tag_end, State::WithinTag);
                    }
                    finish_attribute!(start, expression_tag_end);
                }
            }
            Tok::Unknown => {
                let token_offset = scanner.token_offset;
                let inside_tag = matches!(
                    scanner.state,
                    State::WithinTag | State::AfterAttributeName | State::BeforeAttributeValue | State::AfterOpeningStartTag
                );
                if inside_tag && text.get(token_offset) == Some(&FSL) {
                    let next = text.get(token_offset + 1).copied().unwrap_or(0);
                    if next == FSL {
                        let nl = text[(token_offset + 2).min(text.len())..].iter().position(|&c| c == b'\n' as u16);
                        let end = nl.map_or(text.len(), |i| token_offset + 2 + i);
                        restart_scanner_at!(end, State::WithinTag);
                    } else if next == b'*' as u16 {
                        let from = (token_offset + 2).min(text.len());
                        let close = text[from..].windows(2).position(|w| w[0] == b'*' as u16 && w[1] == FSL);
                        let end = close.map_or(text.len(), |i| from + i + 2);
                        restart_scanner_at!(end, State::WithinTag);
                    }
                }
            }
            Tok::Content => {
                let expression_end = skip_expression_in_range(text, scanner.token_offset, scanner.token_end());
                if expression_end > scanner.token_end() {
                    restart_scanner_at!(expression_end, State::WithinContent);
                }
            }
            _ => {}
        }
        token = scanner.scan();
    }
    while let Some(p) = nodes[curr].parent {
        nodes[curr].end = text.len();
        curr = p;
    }
    nodes
}

fn js_trim(t: &[u16]) -> &[u16] {
    let start = t.iter().position(|&c| !is_js_ws(c)).unwrap_or(t.len());
    let end = t.iter().rposition(|&c| !is_js_ws(c)).map_or(start, |i| i + 1);
    &t[start..end.max(start)]
}

fn skip_expression_in_range(text: &[u16], start: usize, end: usize) -> usize {
    let mut index = start;
    while index < end {
        if text.get(index) != Some(&LBR) {
            index += 1;
            continue;
        }
        index = scan_matching_braces(text, index);
    }
    index.max(end)
}

/// `scanMatchingBraces(html, startOffset).endOffset`
pub fn scan_matching_braces(html: &[u16], start: usize) -> usize {
    if html.get(start) != Some(&LBR) {
        return start;
    }
    let len = html.len();
    let at = |i: usize| html.get(i).copied().unwrap_or(0);
    let mut depth: i64 = 0;
    let mut template_stack: Vec<i64> = Vec::new();
    let mut index = start;

    // returns the new index
    fn scan_template_string(html: &[u16], mut index: usize, depth: &mut i64, stack: &mut Vec<i64>) -> usize {
        while index < html.len() {
            let ch = html[index];
            match ch {
                0x60 => return index,
                0x24 => {
                    if html.get(index + 1) == Some(&LBR) {
                        stack.push(*depth);
                        *depth = 0;
                        return index;
                    }
                }
                0x5c => index += 1,
                _ => {}
            }
            index += 1;
        }
        index
    }

    while index < len {
        let ch = html[index];
        match ch {
            LBR => depth += 1,
            RBR => {
                if depth > 0 {
                    depth -= 1;
                }
                if depth == 0 && !template_stack.is_empty() {
                    depth = template_stack.pop().unwrap_or(0);
                    index = scan_template_string(html, index, &mut depth, &mut template_stack);
                }
            }
            SQO | DQO => {
                index += 1;
                // scanString
                while index < len {
                    let c = html[index];
                    if c == ch || c == b'\n' as u16 {
                        break;
                    }
                    if c == b'\\' as u16 {
                        index += 2;
                        continue;
                    }
                    index += 1;
                }
            }
            0x60 => {
                index += 1;
                index = scan_template_string(html, index, &mut depth, &mut template_stack);
            }
            FSL => {
                let next = at(index + 1);
                if next == FSL {
                    while index < len {
                        let c = html[index];
                        if c == b'\r' as u16 || c == b'\n' as u16 {
                            break;
                        }
                        index += 1;
                    }
                } else if next == b'*' as u16 {
                    index += 2;
                    while index < len {
                        if html[index] == b'*' as u16 && at(index + 1) == FSL {
                            index += 2;
                            break;
                        }
                        index += 1;
                    }
                }
            }
            _ => {}
        }
        index += 1;
        if depth == 0 && template_stack.is_empty() {
            return index;
        }
    }
    index
}

/// `TagInformation` of the style tag
pub struct StyleTag {
    pub start: usize,
    pub end: usize,
    /// `attrs.lang || attrs.type || 'css'`
    pub lang: String,
}

/// `extractStyleTag(source, parseHtml(source))`
pub fn extract_style_tag(text: &[u16]) -> Option<StyleTag> {
    let nodes = parse_html(text);
    let roots = &nodes[0].children;
    for (index, &id) in roots.iter().enumerate() {
        let node = &nodes[id];
        if !node.tag.as_deref().is_some_and(|t| is(t, b"style")) {
            continue;
        }
        if !is_not_inside_control_flow_tag(text, &nodes, roots, index) || !is_not_inside_html_tag(text, &nodes, roots, index) {
            continue;
        }
        let start = node.start_tag_end.unwrap_or(node.start);
        let end = node.end_tag_start.unwrap_or(node.end);
        let attr = |name: &[u8]| -> Option<Vec<u16>> {
            let (n, v) = node.attributes.as_ref()?.iter().find(|(n, _)| is(n, name))?;
            Some(match v {
                None => n.clone(),
                Some(v) => remove_outer_quotes(v).to_vec(),
            })
        };
        let lang = [attr(b"lang"), attr(b"type")]
            .into_iter()
            .flatten()
            .find(|v| !v.is_empty())
            .map(|v| String::from_utf16_lossy(&v))
            .unwrap_or_else(|| "css".to_string());
        return Some(StyleTag { start, end, lang });
    }
    None
}

fn remove_outer_quotes(v: &[u16]) -> &[u16] {
    if v.len() >= 2 && ((v[0] == DQO && v[v.len() - 1] == DQO) || (v[0] == SQO && v[v.len() - 1] == SQO)) {
        return &v[1..v.len() - 1];
    }
    // `"x"` of length 1 (`'"'`) starts and ends with the quote too
    if v.len() == 1 && (v[0] == DQO || v[0] == SQO) {
        return &v[1..1];
    }
    v
}

fn is_not_inside_control_flow_tag(text: &[u16], nodes: &[HtmlNode], roots: &[usize], tag_index: usize) -> bool {
    let tag = &nodes[roots[tag_index]];
    if tag_index == 0 && js_trim(&text[..tag.start.min(text.len())]).is_empty() {
        return true;
    }
    let after = &roots[tag_index..];
    let mut content: Vec<u16> = Vec::new();
    for (idx, &id) in after.iter().enumerate() {
        let node = &nodes[id];
        let start = if node.start_tag_end.is_some() {
            node.end
        } else {
            node.start + node.tag.as_ref().map_or(0, |t| t.len())
        };
        let end = after.get(idx + 1).map_or(text.len(), |&n| nodes[n].start);
        content.extend_from_slice(js_substring(text, start, end));
    }
    let default = text.len();
    for (open, close) in [(&b"#if"[..], &b"/if"[..]), (b"#each", b"/each"), (b"#await", b"/await")] {
        let start = find_block_open(&content, open).unwrap_or(default);
        let end = find_block_close(&content, close).unwrap_or(default);
        if end < start {
            return false;
        }
    }
    true
}

fn is_not_inside_html_tag(text: &[u16], nodes: &[HtmlNode], roots: &[usize], tag_index: usize) -> bool {
    let before = &roots[..tag_index];
    let mut content: Vec<u16> = Vec::new();
    // `[{ start: 0, end: 0 }, ...nodes].map((node, idx) => text.substring(node.end, nodes[idx]?.start))`
    for idx in 0..=before.len() {
        let start = if idx == 0 { 0 } else { nodes[before[idx - 1]].end };
        let end = before.get(idx).map_or(text.len(), |&n| nodes[n].start);
        content.extend_from_slice(js_substring(text, start, end));
    }
    let last_html = last_html_tag(&content);
    let last_close = content.iter().rposition(|&c| c == RBR);
    let as_i = |o: Option<usize>| o.map_or(-1, |i| i as i64);
    as_i(last_html) <= as_i(last_close)
}

/// skip `\s*` from `i`
fn skip_ws(s: &[u16], mut i: usize) -> usize {
    while i < s.len() && is_js_ws(s[i]) {
        i += 1;
    }
    i
}

/// first match of `/{\s*#kw\s.*?}/s`
fn find_block_open(s: &[u16], kw: &[u8]) -> Option<usize> {
    let last_close = s.iter().rposition(|&c| c == RBR)?;
    for i in 0..s.len() {
        if s[i] != LBR {
            continue;
        }
        let j = skip_ws(s, i + 1);
        if starts_with(s, j, kw) && s.get(j + kw.len()).is_some_and(|&c| is_js_ws(c)) && last_close > j + kw.len() {
            return Some(i);
        }
    }
    None
}

/// first match of `/{\s*\/kw}/`
fn find_block_close(s: &[u16], kw: &[u8]) -> Option<usize> {
    for i in 0..s.len() {
        if s[i] != LBR {
            continue;
        }
        let j = skip_ws(s, i + 1);
        if starts_with(s, j, kw) && s.get(j + kw.len()) == Some(&RBR) {
            return Some(i);
        }
    }
    None
}

/// last match of `/{\s*@html\s/`
fn last_html_tag(s: &[u16]) -> Option<usize> {
    let mut last = None;
    for i in 0..s.len() {
        if s[i] != LBR {
            continue;
        }
        let j = skip_ws(s, i + 1);
        if starts_with(s, j, b"@html") && s.get(j + 5).is_some_and(|&c| is_js_ws(c)) {
            last = Some(i);
        }
    }
    last
}
