//! Port of vscode-css-languageservice's `parser/cssScanner.ts` plus the SCSS and LESS scanner
//! overrides. Works on UTF-16 code units like the JS version.

use super::Dialect;

/// `TokenType` (complete, like the JS enum)
#[allow(dead_code)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TT {
    Ident,
    AtKeyword,
    String,
    BadString,
    UnquotedString,
    Hash,
    Num,
    Percentage,
    Dimension,
    UnicodeRange,
    CDO,
    CDC,
    Colon,
    SemiColon,
    CurlyL,
    CurlyR,
    ParenthesisL,
    ParenthesisR,
    BracketL,
    BracketR,
    Whitespace,
    Includes,
    Dashmatch,
    SubstringOperator,
    PrefixOperator,
    SuffixOperator,
    Delim,
    EMS,
    EXS,
    Length,
    Angle,
    Time,
    Freq,
    Exclamation,
    Resolution,
    Comma,
    Charset,
    EscapedJavaScript,
    BadEscapedJavaScript,
    Comment,
    SingleLineComment,
    EOF,
    ContainerQueryLength,
    /// `staticUnitTable['constructor']` / `staticUnitTable['__proto__']`: an `Object.prototype`
    /// member ends up as the token type, which equals no `TokenType`
    ObjectPrototypeUnit,
    // SCSS
    VariableName,
    InterpolationFunction,
    EqualsOperator,
    NotEqualsOperator,
    GreaterEqualsOperator,
    SmallerEqualsOperator,
    // SCSS and LESS
    Ellipsis,
}

/// Where a token's `text` lives: the source range of the token, or a range of the scanner's
/// text buffer (when escapes made the text differ from the source)
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TextRef {
    Src,
    Buf(u32, u32),
}

#[derive(Clone, Copy, Debug)]
pub struct Token {
    pub ty: TT,
    pub offset: i32,
    pub len: i32,
    text: TextRef,
    /// identity of the JS token object
    pub id: u32,
}

impl Token {
    pub fn initial() -> Token {
        Token { ty: TT::EOF, offset: -1, len: 0, text: TextRef::Buf(0, 0), id: 0 }
    }
}

const BSL: u16 = b'\\' as u16;
const NWL: u16 = b'\n' as u16;
const CAR: u16 = b'\r' as u16;
const LFD: u16 = 0x0c;
const WSP: u16 = b' ' as u16;
const TAB: u16 = b'\t' as u16;
const SQO: u16 = b'\'' as u16;
const DQO: u16 = b'"' as u16;
const MIN: u16 = b'-' as u16;
const USC: u16 = b'_' as u16;
const DOT: u16 = b'.' as u16;
const PLS: u16 = b'+' as u16;
const LPA: u16 = b'(' as u16;
const RPA: u16 = b')' as u16;
const FSL: u16 = b'/' as u16;
const MUL: u16 = b'*' as u16;
const EQS: u16 = b'=' as u16;
const QSM: u16 = b'?' as u16;

fn c(b: u8) -> u16 {
    b as u16
}

fn is_digit(ch: u16) -> bool {
    (c(b'0')..=c(b'9')).contains(&ch)
}

fn is_hex(ch: u16) -> bool {
    is_digit(ch) || (c(b'a')..=c(b'f')).contains(&ch) || (c(b'A')..=c(b'F')).contains(&ch)
}

pub struct Scanner<'a> {
    src: &'a [u16],
    /// may exceed `src.len()` (escapes at EOF advance past the end, like the JS stream)
    pos: usize,
    pub in_url: bool,
    dialect: Dialect,
    /// the `content` array of the token being scanned
    buf: Vec<u16>,
    /// texts of tokens that differ from their source
    texts: Vec<u16>,
    next_id: u32,
}

impl<'a> Scanner<'a> {
    pub fn new(src: &'a [u16], dialect: Dialect) -> Self {
        Scanner { src, pos: 0, in_url: false, dialect, buf: Vec::new(), texts: Vec::new(), next_id: 1 }
    }

    /// The token's `text`
    pub fn text(&self, t: &Token) -> &[u16] {
        match t.text {
            TextRef::Src => self.substring(t.offset.max(0) as usize, (t.offset + t.len).max(0) as usize),
            TextRef::Buf(s, l) => &self.texts[s as usize..(s + l) as usize],
        }
    }

    fn substring(&self, from: usize, to: usize) -> &'a [u16] {
        let len = self.src.len();
        let (from, to) = (from.min(len), to.min(len));
        if from <= to { &self.src[from..to] } else { &self.src[to..from] }
    }

    pub fn pos(&self) -> usize {
        self.pos
    }

    pub fn go_back_to(&mut self, pos: usize) {
        self.pos = pos;
    }

    // --- MultiLineStream ---

    #[inline]
    fn eos(&self) -> bool {
        self.src.len() <= self.pos
    }

    #[inline]
    fn peek(&self, n: usize) -> u16 {
        self.src.get(self.pos + n).copied().unwrap_or(0)
    }

    #[inline]
    fn advance(&mut self, n: usize) {
        self.pos += n;
    }

    fn next_char(&mut self) -> u16 {
        let ch = self.peek(0);
        self.pos += 1;
        ch
    }

    #[inline]
    fn advance_if_char(&mut self, ch: u16) -> bool {
        if self.src.get(self.pos) == Some(&ch) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn advance_if_chars(&mut self, chars: &[u8]) -> bool {
        if self.pos + chars.len() > self.src.len() {
            return false;
        }
        for (i, &ch) in chars.iter().enumerate() {
            if self.src[self.pos + i] != ch as u16 {
                return false;
            }
        }
        self.pos += chars.len();
        true
    }

    fn advance_while_char(&mut self, mut cond: impl FnMut(u16) -> bool) -> usize {
        let start = self.pos;
        while self.pos < self.src.len() && cond(self.src[self.pos]) {
            self.pos += 1;
        }
        self.pos - start
    }

    // --- Scanner ---

    fn finish_token(&mut self, offset: usize, ty: TT, with_text: bool) -> Token {
        let id = self.next_id;
        self.next_id += 1;
        let text = if with_text && !self.buf.is_empty() && self.buf.as_slice() != self.substring(offset, self.pos) {
            let start = self.texts.len() as u32;
            self.texts.extend_from_slice(&self.buf);
            TextRef::Buf(start, self.buf.len() as u32)
        } else {
            TextRef::Src
        };
        Token { ty, offset: offset as i32, len: (self.pos - offset) as i32, text, id }
    }

    pub fn scan_unquoted_string(&mut self) -> Option<Token> {
        let offset = self.pos;
        self.buf.clear();
        if self.unquoted_string() {
            return Some(self.finish_token(offset, TT::UnquotedString, true));
        }
        None
    }

    pub fn scan(&mut self) -> Token {
        // trivia (comments and whitespace are always ignored by the parser)
        loop {
            if self.whitespace() || self.comment() {
                continue;
            }
            break;
        }
        let offset = self.pos;
        self.buf.clear();
        if self.eos() {
            return self.finish_token(offset, TT::EOF, false);
        }
        self.scan_next(offset)
    }

    pub fn try_scan_unicode(&mut self) -> Option<Token> {
        let offset = self.pos;
        if !self.eos() && self.unicode_range() {
            return Some(self.finish_token(offset, TT::UnicodeRange, false));
        }
        self.pos = offset;
        None
    }

    fn scan_next(&mut self, offset: usize) -> Token {
        match self.dialect {
            Dialect::Scss => {
                if self.advance_if_char(c(b'$')) {
                    self.buf.clear();
                    self.buf.push(c(b'$'));
                    if self.ident() {
                        return self.finish_token(offset, TT::VariableName, true);
                    } else {
                        self.pos = offset;
                    }
                }
                if self.advance_if_chars(b"#{") {
                    return self.finish_token(offset, TT::InterpolationFunction, false);
                }
                if self.advance_if_chars(b"==") {
                    return self.finish_token(offset, TT::EqualsOperator, false);
                }
                if self.advance_if_chars(b"!=") {
                    return self.finish_token(offset, TT::NotEqualsOperator, false);
                }
                if self.advance_if_char(c(b'<')) {
                    if self.advance_if_char(EQS) {
                        return self.finish_token(offset, TT::SmallerEqualsOperator, false);
                    }
                    return self.finish_token(offset, TT::Delim, false);
                }
                if self.advance_if_char(c(b'>')) {
                    if self.advance_if_char(EQS) {
                        return self.finish_token(offset, TT::GreaterEqualsOperator, false);
                    }
                    return self.finish_token(offset, TT::Delim, false);
                }
                if self.advance_if_chars(b"...") {
                    return self.finish_token(offset, TT::Ellipsis, false);
                }
            }
            Dialect::Less => {
                if self.peek(0) == c(b'`') {
                    self.advance(1);
                    self.advance_while_char(|ch| ch != c(b'`'));
                    let ty = if self.advance_if_char(c(b'`')) { TT::EscapedJavaScript } else { TT::BadEscapedJavaScript };
                    return self.finish_token(offset, ty, false);
                }
                if self.advance_if_chars(b"...") {
                    return self.finish_token(offset, TT::Ellipsis, false);
                }
            }
            Dialect::Css => {}
        }
        self.css_scan_next(offset)
    }

    fn css_scan_next(&mut self, offset: usize) -> Token {
        if self.advance_if_chars(b"<!--") {
            return self.finish_token(offset, TT::CDO, false);
        }
        if self.advance_if_chars(b"-->") {
            return self.finish_token(offset, TT::CDC, false);
        }
        self.buf.clear();
        if self.ident() {
            return self.finish_token(offset, TT::Ident, true);
        }
        if self.advance_if_char(c(b'@')) {
            self.buf.clear();
            self.buf.push(c(b'@'));
            if self.name() {
                let is_charset = self.buf.len() == 8 && self.buf.iter().copied().eq("@charset".bytes().map(c));
                let ty = if is_charset { TT::Charset } else { TT::AtKeyword };
                return self.finish_token(offset, ty, true);
            } else {
                self.buf.clear();
                return self.finish_token(offset, TT::Delim, false);
            }
        }
        if self.advance_if_char(c(b'#')) {
            self.buf.clear();
            self.buf.push(c(b'#'));
            if self.name() {
                return self.finish_token(offset, TT::Hash, true);
            } else {
                self.buf.clear();
                return self.finish_token(offset, TT::Delim, false);
            }
        }
        if self.advance_if_char(c(b'!')) {
            return self.finish_token(offset, TT::Exclamation, false);
        }
        if self.number() {
            let pos = self.pos;
            self.buf.clear();
            let num = self.substring(offset, pos);
            self.buf.extend_from_slice(num);
            if self.advance_if_char(c(b'%')) {
                self.buf.clear();
                return self.finish_token(offset, TT::Percentage, false);
            } else if self.ident() {
                let dim = self.substring(pos, self.pos);
                let ty = unit_token(dim).unwrap_or(TT::Dimension);
                return self.finish_token(offset, ty, true);
            }
            self.buf.clear();
            return self.finish_token(offset, TT::Num, false);
        }
        self.buf.clear();
        if let Some(ty) = self.string() {
            return self.finish_token(offset, ty, true);
        }
        self.buf.clear();
        let ty = match self.peek(0) {
            0x3b => Some(TT::SemiColon),
            0x3a => Some(TT::Colon),
            0x7b => Some(TT::CurlyL),
            0x7d => Some(TT::CurlyR),
            0x5d => Some(TT::BracketR),
            0x5b => Some(TT::BracketL),
            0x28 => Some(TT::ParenthesisL),
            0x29 => Some(TT::ParenthesisR),
            0x2c => Some(TT::Comma),
            _ => None,
        };
        if let Some(ty) = ty {
            self.advance(1);
            return self.finish_token(offset, ty, false);
        }
        let (p0, p1) = (self.peek(0), self.peek(1));
        if p1 == EQS {
            let ty = match p0 {
                0x7e => Some(TT::Includes),
                0x7c => Some(TT::Dashmatch),
                0x2a => Some(TT::SubstringOperator),
                0x5e => Some(TT::PrefixOperator),
                0x24 => Some(TT::SuffixOperator),
                _ => None,
            };
            if let Some(ty) = ty {
                self.advance(2);
                return self.finish_token(offset, ty, false);
            }
        }
        self.next_char();
        self.finish_token(offset, TT::Delim, false)
    }

    fn comment(&mut self) -> bool {
        if self.advance_if_chars(b"/*") {
            let (mut success, mut hot) = (false, false);
            self.advance_while_char(|ch| {
                if hot && ch == FSL {
                    success = true;
                    return false;
                }
                hot = ch == MUL;
                true
            });
            if success {
                self.advance(1);
            }
            return true;
        }
        if self.dialect != Dialect::Css && !self.in_url && self.advance_if_chars(b"//") {
            self.advance_while_char(|ch| !matches!(ch, NWL | CAR | LFD));
            return true;
        }
        false
    }

    fn number(&mut self) -> bool {
        let mut npeek = 0;
        let mut has_dot = false;
        let first = self.peek(0);
        if first == PLS || first == MIN {
            npeek += 1;
        }
        if self.peek(npeek) == DOT {
            npeek += 1;
            has_dot = true;
        }
        let ch = self.peek(npeek);
        if is_digit(ch) {
            self.advance(npeek + 1);
            self.advance_while_char(|ch| is_digit(ch) || (!has_dot && ch == DOT));
            return true;
        }
        false
    }

    fn newline(&mut self, push: bool) -> bool {
        let ch = self.peek(0);
        match ch {
            CAR | LFD | NWL => {
                self.advance(1);
                if push {
                    self.buf.push(ch);
                }
                if ch == CAR && self.advance_if_char(NWL) && push {
                    self.buf.push(NWL);
                }
                true
            }
            _ => false,
        }
    }

    fn escape(&mut self, include_newlines: bool) -> bool {
        let mut ch = self.peek(0);
        if ch == BSL {
            self.advance(1);
            ch = self.peek(0);
            let mut hex_count = 0;
            while hex_count < 6 && is_hex(ch) {
                self.advance(1);
                ch = self.peek(0);
                hex_count += 1;
            }
            if hex_count > 0 {
                // `parseInt(substring(pos - hexNumCount), 16)`: reads every hex digit from there
                let start = self.pos - hex_count;
                let digits = self.src[start..].iter().take_while(|&&d| is_hex(d));
                let mut value: f64 = 0.0;
                for &d in digits {
                    let v = (d as u8 as char).to_digit(16).unwrap() as f64;
                    value = value * 16.0 + v;
                }
                if value != 0.0 {
                    // `String.fromCharCode` (ToUint16)
                    let unit = if value.is_finite() { (value % 65536.0) as u32 as u16 } else { 0 };
                    self.buf.push(unit);
                }
                if ch == WSP || ch == TAB {
                    self.advance(1);
                } else {
                    self.newline(false);
                }
                return true;
            }
            if ch != CAR && ch != LFD && ch != NWL {
                self.advance(1);
                self.buf.push(ch);
                return true;
            } else if include_newlines {
                return self.newline(true);
            }
        }
        false
    }

    fn string_char(&mut self, close: u16) -> bool {
        let ch = self.peek(0);
        if ch != 0 && ch != close && ch != BSL && ch != CAR && ch != LFD && ch != NWL {
            self.advance(1);
            self.buf.push(ch);
            return true;
        }
        false
    }

    fn string(&mut self) -> Option<TT> {
        let p = self.peek(0);
        if p == SQO || p == DQO {
            let close = self.next_char();
            self.buf.push(close);
            while self.string_char(close) || self.escape(true) {}
            if self.peek(0) == close {
                self.next_char();
                self.buf.push(close);
                return Some(TT::String);
            } else {
                return Some(TT::BadString);
            }
        }
        None
    }

    fn unquoted_char(&mut self) -> bool {
        let ch = self.peek(0);
        if ch != 0 && !matches!(ch, BSL | SQO | DQO | LPA | RPA | WSP | TAB | NWL | LFD | CAR) {
            self.advance(1);
            self.buf.push(ch);
            return true;
        }
        false
    }

    fn unquoted_string(&mut self) -> bool {
        let mut has_content = false;
        while self.unquoted_char() || self.escape(false) {
            has_content = true;
        }
        has_content
    }

    fn whitespace(&mut self) -> bool {
        self.advance_while_char(|ch| matches!(ch, WSP | TAB | NWL | LFD | CAR)) > 0
    }

    fn name(&mut self) -> bool {
        let mut matched = false;
        while self.ident_char() || self.escape(false) {
            matched = true;
        }
        matched
    }

    fn ident(&mut self) -> bool {
        let pos = self.pos;
        let has_minus = self.minus();
        if has_minus {
            if self.minus() || self.ident_first_char() || self.escape(false) {
                while self.ident_char() || self.escape(false) {}
                return true;
            }
        } else if self.ident_first_char() || self.escape(false) {
            while self.ident_char() || self.escape(false) {}
            return true;
        }
        self.pos = pos;
        false
    }

    fn ident_first_char(&mut self) -> bool {
        let ch = self.peek(0);
        if ch == USC || (c(b'a')..=c(b'z')).contains(&ch) || (c(b'A')..=c(b'Z')).contains(&ch) || ch >= 0x80 {
            self.advance(1);
            self.buf.push(ch);
            return true;
        }
        false
    }

    fn minus(&mut self) -> bool {
        if self.peek(0) == MIN {
            self.advance(1);
            self.buf.push(MIN);
            return true;
        }
        false
    }

    fn ident_char(&mut self) -> bool {
        let ch = self.peek(0);
        if ch == USC
            || ch == MIN
            || (c(b'a')..=c(b'z')).contains(&ch)
            || (c(b'A')..=c(b'Z')).contains(&ch)
            || is_digit(ch)
            || ch >= 0x80
        {
            self.advance(1);
            self.buf.push(ch);
            return true;
        }
        false
    }

    fn unicode_range(&mut self) -> bool {
        if self.advance_if_char(PLS) {
            let code_points = self.advance_while_char(is_hex) + self.advance_while_char(|ch| ch == QSM);
            if (1..=6).contains(&code_points) {
                if self.advance_if_char(MIN) {
                    let digits = self.advance_while_char(is_hex);
                    if (1..=6).contains(&digits) {
                        return true;
                    }
                } else {
                    return true;
                }
            }
        }
        false
    }
}

/// `staticUnitTable[dim.toLowerCase()]`
fn unit_token(dim: &[u16]) -> Option<TT> {
    if dim.len() > 11 || dim.iter().any(|&ch| ch >= 0x80) {
        // non-ASCII units can't lowercase to an ASCII unit except via a few special
        // characters (the Kelvin sign lowercases to `k`)
        let s = String::from_utf16_lossy(dim).to_lowercase();
        return unit_str(&s);
    }
    let mut lower = [0u8; 11];
    for (i, &ch) in dim.iter().enumerate() {
        lower[i] = (ch as u8).to_ascii_lowercase();
    }
    unit_str(std::str::from_utf8(&lower[..dim.len()]).unwrap_or(""))
}

fn unit_str(s: &str) -> Option<TT> {
    Some(match s {
        "em" => TT::EMS,
        "ex" => TT::EXS,
        "px" | "cm" | "mm" | "in" | "pt" | "pc" => TT::Length,
        "deg" | "rad" | "grad" => TT::Angle,
        "ms" | "s" => TT::Time,
        "hz" | "khz" => TT::Freq,
        "%" | "fr" => TT::Percentage,
        "dpi" | "dpcm" => TT::Resolution,
        "cqw" | "cqh" | "cqi" | "cqb" | "cqmin" | "cqmax" => TT::ContainerQueryLength,
        "constructor" | "__proto__" => TT::ObjectPrototypeUnit,
        _ => return None,
    })
}
