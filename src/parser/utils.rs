//! Small helpers ported from `svelte/src/utils.js`, `html-tree-validation.js`,
//! `phases/1-parse/utils/{whitespace,bracket,html}.js`.

use std::collections::HashMap;
use std::sync::LazyLock;

use crate::error::Result;
use crate::errors as e;

/// `is_whitespace` from `utils/whitespace.js` (JS's WhiteSpace + LineTerminator)
#[inline]
pub fn is_whitespace_char(c: char) -> bool {
    let cc = c as u32;
    if cc == 32 || (9..=13).contains(&cc) {
        return true;
    }
    if cc < 160 {
        return false;
    }
    matches!(cc, 160 | 5760 | 8192..=8202 | 8232 | 8233 | 8239 | 8287 | 12288 | 65279)
}

/// The char at byte `i` of `s`, if any
#[inline]
pub fn char_at(s: &str, i: usize) -> Option<char> {
    s.get(i..).and_then(|rest| rest.chars().next())
}

/// `String.prototype.trimEnd`
pub fn js_trim_end(s: &str) -> &str {
    s.trim_end_matches(is_whitespace_char)
}

/// `String.prototype.trim`
pub fn js_trim(s: &str) -> &str {
    s.trim_matches(is_whitespace_char)
}

/// UTF-16 length, i.e. JS's `.length`
pub fn js_len(s: &str) -> usize {
    if s.is_ascii() {
        s.len()
    } else {
        s.chars().map(char::len_utf16).sum()
    }
}

/// acorn's `isIdentifierStart(code, true)`
#[inline]
pub fn is_identifier_start(c: char) -> bool {
    c == '$' || c == '_' || c.is_ascii_alphabetic() || (c as u32 >= 0xaa && unicode_id_start::is_id_start(c))
}

/// acorn's `isIdentifierChar(code, true)`
#[inline]
pub fn is_identifier_char(c: char) -> bool {
    c == '$'
        || c == '_'
        || c.is_ascii_alphanumeric()
        || c == '\u{200c}'
        || c == '\u{200d}'
        || (c as u32 >= 0xaa && unicode_id_start::is_id_continue(c))
}

const VOID_ELEMENT_NAMES: &[&str] = &[
    "area", "base", "br", "col", "command", "embed", "hr", "img", "input", "keygen", "link", "meta", "param",
    "source", "track", "wbr",
];

pub fn is_void(name: &str) -> bool {
    VOID_ELEMENT_NAMES.contains(&name) || name.eq_ignore_ascii_case("!doctype")
}

const RESERVED_WORDS: &[&str] = &[
    "arguments", "await", "break", "case", "catch", "class", "const", "continue", "debugger", "default", "delete",
    "do", "else", "enum", "eval", "export", "extends", "false", "finally", "for", "function", "if", "implements",
    "import", "in", "instanceof", "interface", "let", "new", "null", "package", "private", "protected", "public",
    "return", "static", "super", "switch", "this", "throw", "true", "try", "typeof", "var", "void", "while",
    "with", "yield",
];

pub fn is_reserved(word: &str) -> bool {
    RESERVED_WORDS.contains(&word)
}

/// `closing_tag_omitted(current, next)` from `html-tree-validation.js`
pub fn closing_tag_omitted(current: &str, next: &str) -> bool {
    let list: &[&str] = match current {
        "li" => &["li"],
        "dt" | "dd" => &["dt", "dd"],
        "p" => &[
            "address", "article", "aside", "blockquote", "div", "dl", "fieldset", "footer", "form", "h1", "h2", "h3",
            "h4", "h5", "h6", "header", "hgroup", "hr", "main", "menu", "nav", "ol", "p", "pre", "section", "table",
            "ul",
        ],
        "rt" | "rp" => &["rt", "rp"],
        "optgroup" => &["optgroup"],
        "option" => &["option", "optgroup"],
        "thead" | "tbody" => &["tbody", "tfoot"],
        "tfoot" => &["tbody"],
        "tr" => &["tr", "tbody"],
        "td" | "th" => &["td", "th", "tr"],
        _ => return false,
    };
    list.contains(&next)
}

// --- brackets (utils/bracket.js) -------------------------------------------------------

fn find_unescaped_char(s: &[u8], from: usize, ch: u8) -> usize {
    let mut i = from;
    loop {
        let Some(found) = s.get(i..).and_then(|rest| rest.iter().position(|&b| b == ch)).map(|p| p + i) else {
            return usize::MAX;
        };
        let mut count = 0;
        let mut j = found;
        while j > 0 && s[j - 1] == b'\\' {
            count += 1;
            j -= 1;
        }
        if count % 2 == 0 {
            return found;
        }
        i = found + 1;
    }
}

fn find_string_end(s: &[u8], from: usize, quote: u8) -> usize {
    if quote == b'`' {
        find_unescaped_char(s, from, quote)
    } else {
        let eol = s.get(from..).and_then(|r| r.iter().position(|&b| b == b'\n')).map_or(s.len(), |p| p + from);
        find_unescaped_char(&s[..eol], from, quote)
    }
}

/// `find_matching_bracket`: index of the bracket closing `open`, skipping strings, comments and regexes
pub fn find_matching_bracket(template: &str, index: usize, open: u8) -> Option<usize> {
    let s = template.as_bytes();
    let close = match open {
        b'{' => b'}',
        b'(' => b')',
        b'[' => b']',
        _ => unreachable!(),
    };
    let mut brackets = 1;
    let mut i = index;
    while brackets > 0 && i < s.len() {
        match s[i] {
            q @ (b'\'' | b'"' | b'`') => {
                i = find_string_end(s, i + 1, q).saturating_add(1);
            }
            b'/' => {
                let Some(&next) = s.get(i + 1) else {
                    i += 1;
                    continue;
                };
                if next == b'/' {
                    i = s[i + 1..].iter().position(|&b| b == b'\n').map_or(usize::MAX, |p| p + i + 1).saturating_add(1);
                } else if next == b'*' {
                    i = template[i + 1..].find("*/").map_or(usize::MAX, |p| p + i + 1).saturating_add(2);
                } else {
                    let slash = find_unescaped_char(s, i + 1, b'/');
                    let eol = find_unescaped_char(s, i + 1, b'\n');
                    if slash < eol {
                        i = slash + 1;
                    } else {
                        i += 1;
                    }
                }
            }
            c => {
                if c == open {
                    brackets += 1;
                } else if c == close {
                    brackets -= 1;
                }
                if brackets == 0 {
                    return Some(i);
                }
                i += 1;
            }
        }
    }
    None
}

/// `match_bracket`: index just past the bracket matching the one at `start`
pub fn match_bracket(template: &str, start: usize, pairs: &[(u8, u8)]) -> Result<usize> {
    let s = template.as_bytes();
    let mut stack: Vec<u8> = Vec::new();
    let mut i = start;
    while i < s.len() {
        let ch = s[i];
        i += 1;
        if ch == b'\'' || ch == b'"' || ch == b'`' {
            i = match_quote(template, i, ch)?;
            continue;
        }
        if pairs.iter().any(|&(o, _)| o == ch) {
            stack.push(ch);
        } else if pairs.iter().any(|&(_, c)| c == ch) {
            let popped = stack.pop();
            let expected = popped.and_then(|p| pairs.iter().find(|&&(o, _)| o == p)).map(|&(_, c)| c);
            if Some(ch) != expected {
                let expected = expected.map_or("undefined".to_string(), |c| (c as char).to_string());
                return Err(e::expected_token(i - 1, &expected));
            }
            if stack.is_empty() {
                return Ok(i);
            }
        }
    }
    Err(e::unexpected_eof(template.len()))
}

pub const DEFAULT_BRACKETS: &[(u8, u8)] = &[(b'{', b'}'), (b'(', b')'), (b'[', b']')];

fn match_quote(template: &str, start: usize, quote: u8) -> Result<usize> {
    let s = template.as_bytes();
    let mut escaped = false;
    let mut i = start;
    while i < s.len() {
        let ch = s[i];
        i += 1;
        if escaped {
            escaped = false;
            continue;
        }
        if ch == quote {
            return Ok(i);
        }
        if ch == b'\\' {
            escaped = true;
        }
        if quote == b'`' && ch == b'$' && s.get(i) == Some(&b'{') {
            i = match_bracket(template, i, DEFAULT_BRACKETS)?;
        }
    }
    Err(e::unterminated_string_constant(start))
}

// --- character references (utils/html.js) ----------------------------------------------

static ENTITY_INDEX: LazyLock<HashMap<u8, Vec<(&'static str, u32)>>> = LazyLock::new(|| {
    let mut map: HashMap<u8, Vec<(&'static str, u32)>> = HashMap::new();
    for &(name, code) in super::entities::ENTITIES {
        map.entry(name.as_bytes()[0]).or_default().push((name, code));
    }
    map
});

const WINDOWS_1252: [u32; 32] = [
    8364, 129, 8218, 402, 8222, 8230, 8224, 8225, 710, 8240, 352, 8249, 338, 141, 381, 143, 144, 8216, 8217, 8220,
    8221, 8226, 8211, 8212, 732, 8482, 353, 8250, 339, 157, 382, 376,
];

fn validate_code(code: u32, is_attribute_value: bool) -> u32 {
    if code == 10 && !is_attribute_value {
        return 32;
    }
    if code < 128 {
        return code;
    }
    if code <= 159 {
        return WINDOWS_1252[(code - 128) as usize];
    }
    if code < 55296 {
        return code;
    }
    if code <= 57343 {
        return 0;
    }
    if code <= 65535 {
        return code;
    }
    if (65536..=131071).contains(&code) || (131072..=196607).contains(&code) {
        return code;
    }
    if (917504..=917631).contains(&code) || (917760..=917999).contains(&code) {
        return code;
    }
    0
}

/// Length of the reference following `&` at `rest`, and its code point (0 if none)
fn match_reference(rest: &[u8], is_attribute_value: bool) -> Option<(usize, u32)> {
    // numeric: #(?:[xX][a-fA-F\d]+|\d+)(?:;)?
    if rest.first() == Some(&b'#') {
        let (digits_start, hex) = match rest.get(1) {
            Some(b'x' | b'X') if rest.get(2).is_some_and(u8::is_ascii_hexdigit) => (2, true),
            Some(d) if d.is_ascii_digit() => (1, false),
            _ => return named(rest, is_attribute_value),
        };
        let mut j = digits_start;
        while j < rest.len() && (if hex { rest[j].is_ascii_hexdigit() } else { rest[j].is_ascii_digit() }) {
            j += 1;
        }
        let digits = std::str::from_utf8(&rest[digits_start..j]).unwrap();
        // parseInt on a huge number gives a huge float; anything past char range becomes NUL
        let code = u64::from_str_radix(digits, if hex { 16 } else { 10 }).unwrap_or(u64::MAX);
        let code = if code > u32::MAX as u64 { u32::MAX } else { code as u32 };
        if rest.get(j) == Some(&b';') {
            j += 1;
        }
        return Some((j, code));
    }
    named(rest, is_attribute_value)
}

fn named(rest: &[u8], is_attribute_value: bool) -> Option<(usize, u32)> {
    let candidates = ENTITY_INDEX.get(rest.first()?)?;
    for &(name, code) in candidates {
        if rest.starts_with(name.as_bytes()) {
            if is_attribute_value && !name.ends_with(';') {
                // `name\b(?!=)`
                let next = rest.get(name.len()).copied();
                let word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
                let last = name.as_bytes()[name.len() - 1];
                let boundary = match next {
                    None => word(last),
                    Some(n) => word(last) != word(n),
                };
                if !boundary || next == Some(b'=') {
                    continue;
                }
            }
            return Some((name.len(), code));
        }
    }
    None
}

/// `decode_character_references`
pub fn decode_character_references(html: &str, is_attribute_value: bool) -> String {
    if !html.contains('&') {
        return html.to_string();
    }
    let bytes = html.as_bytes();
    let mut out = String::with_capacity(html.len());
    let mut last = 0;
    let mut i = 0;
    while let Some(p) = html[i..].find('&') {
        let amp = i + p;
        match match_reference(&bytes[amp + 1..], is_attribute_value) {
            Some((len, code)) => {
                out.push_str(&html[last..amp]);
                if code == 0 {
                    out.push_str(&html[amp..amp + 1 + len]);
                } else {
                    let c = validate_code(code, is_attribute_value);
                    out.push(char::from_u32(c).unwrap_or('\0'));
                }
                last = amp + 1 + len;
                i = last;
            }
            None => i = amp + 1,
        }
    }
    out.push_str(&html[last..]);
    out
}
