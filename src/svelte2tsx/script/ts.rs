//! What svelte2tsx asks of TypeScript's AST, answered from oxc's: a script parsed with its
//! tokens, so a node's TS "full start" (`node.pos`, the end of the previous token) can be
//! looked up, plus ports of TS's comment-range scanning (`getLeadingCommentRanges`).

use oxc_allocator::Allocator;
use oxc_ast::ast::Program;
use oxc_parser::{config::TokensParserConfig, Parser};
use oxc_span::SourceType;

pub struct ScriptAst<'a> {
    /// The script's content (positions in the AST are relative to it)
    pub text: &'a str,
    /// Where the content starts in the component (`astOffset`)
    pub offset: usize,
    pub program: Program<'a>,
    /// Token spans, in order
    pub tokens: Vec<(u32, u32)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommentRange {
    pub pos: usize,
    pub end: usize,
    pub multi_line: bool,
    pub has_trailing_new_line: bool,
}

impl<'a> ScriptAst<'a> {
    /// `ts.createSourceFile(..., ScriptKind.TS)`: always parsed as TypeScript
    pub fn parse(alloc: &'a Allocator, text: &'a str, offset: usize) -> Option<Self> {
        let ret = Parser::new(alloc, text, SourceType::ts().with_module(true)).with_config(TokensParserConfig).parse();
        if ret.fatal_error {
            return None;
        }
        let tokens = ret.tokens.iter().map(|t| (t.start(), t.end())).filter(|(s, e)| e > s).collect();
        Some(ScriptAst { text, offset, program: ret.program, tokens })
    }

    /// TS's `node.pos`: the end of the token before `start` (0 if there is none)
    pub fn full_start(&self, start: u32) -> usize {
        let i = self.tokens.partition_point(|&(_, e)| e <= start);
        if i == 0 {
            0
        } else {
            self.tokens[i - 1].1 as usize
        }
    }

    /// The token starting at or after `pos`
    pub fn token_at_or_after(&self, pos: u32) -> Option<(u32, u32)> {
        let i = self.tokens.partition_point(|&(s, _)| s < pos);
        self.tokens.get(i).copied()
    }

    pub fn slice(&self, start: usize, end: usize) -> &'a str {
        &self.text[start.min(self.text.len())..end.min(self.text.len())]
    }

    /// `ts.getLeadingCommentRanges(text, pos)`
    pub fn leading_comment_ranges(&self, pos: usize) -> Vec<CommentRange> {
        leading_comment_ranges(self.text, pos)
    }

    /// `ts.getLeadingCommentRanges(node.getFullText(), 0)` (offsets relative to the script)
    pub fn leading_comments_of_full_text(&self, full_start: usize, start: usize) -> Vec<CommentRange> {
        let mut ranges = leading_comment_ranges(&self.text[full_start..start.max(full_start)], 0);
        for r in &mut ranges {
            r.pos += full_start;
            r.end += full_start;
        }
        ranges
    }

    /// `getLastLeadingDoc(node)`: the last `/* */` comment before the node, without `@typedef` tags
    pub fn last_leading_doc(&self, full_start: usize, start: usize) -> Option<String> {
        let comment = self.leading_comments_of_full_text(full_start, start).into_iter().filter(|c| c.multi_line).last()?;
        let mut text = self.text[comment.pos..comment.end].to_string();
        // remove `@typedef ...` tags: the tag's line, and `@property` lines that belong to it
        static PROP_LINE: std::sync::LazyLock<regex::Regex> =
            std::sync::LazyLock::new(|| regex::Regex::new(r"^\r?\n[ \t]*\*?[ \t]*@(property|prop)\b").unwrap());
        let line_end = |t: &str, from: usize| {
            let rest = &t[from..];
            let nl = rest.find(['\n', '\r']).unwrap_or(rest.len());
            let close = rest.find("*/").unwrap_or(rest.len());
            from + nl.min(close)
        };
        let mut from = 0;
        while let Some(p) = text[from..].find("@typedef") {
            let i = from + p;
            let mut end = line_end(&text, i);
            while let Some(m) = PROP_LINE.find(&text[end..]) {
                end = line_end(&text, end + m.end());
            }
            let end = i + text[i..end].trim_end().len();
            text.replace_range(i..end, "");
            from = i;
        }
        Some(text)
    }

    /// `getLastLeadingDoc(sourceFile)`: the last `/* */` comment at the very start
    pub fn leading_doc_of_file(&self) -> Option<String> {
        let comment = leading_comment_ranges(self.text, 0).into_iter().filter(|c| c.multi_line).last()?;
        Some(self.text[comment.pos..comment.end].to_string())
    }

    /// Whether the JSDoc comments of the statement starting at `stmt_start` have an `@type` tag
    /// (`ts.getJSDocType(declaration)` for the first declaration of a variable statement)
    pub fn has_jsdoc_type(&self, stmt_start: u32) -> bool {
        let pos = self.full_start(stmt_start);
        self.leading_comment_ranges(pos).iter().any(|c| {
            let t = &self.text[c.pos..c.end];
            is_jsdoc(t) && has_tag(t, "type")
        })
    }
}

/// A JSDoc comment: `/**` but not `/**/`
pub fn is_jsdoc(comment: &str) -> bool {
    comment.starts_with("/**") && !comment.starts_with("/**/")
}

/// `@tag` followed by a non-identifier char
pub fn has_tag(comment: &str, tag: &str) -> bool {
    let needle = format!("@{tag}");
    comment.match_indices(&needle).any(|(i, _)| {
        !comment[i + needle.len()..].starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_' || c == '$')
    })
}

fn is_line_break(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

/// Port of TypeScript's `iterateCommentRanges(reduce=false, text, pos, trailing=false)`
pub fn leading_comment_ranges(text: &str, mut pos: usize) -> Vec<CommentRange> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut pending: Option<CommentRange> = None;
    let mut collecting = pos == 0;
    while pos < bytes.len() {
        let c = text[pos..].chars().next().unwrap();
        match c {
            '\r' | '\n' => {
                if c == '\r' && bytes.get(pos + 1) == Some(&b'\n') {
                    pos += 1;
                }
                pos += 1;
                collecting = true;
                if let Some(p) = &mut pending {
                    p.has_trailing_new_line = true;
                }
            }
            '\t' | '\x0B' | '\x0C' | ' ' => pos += 1,
            '/' if matches!(bytes.get(pos + 1), Some(b'/' | b'*')) => {
                let multi_line = bytes[pos + 1] == b'*';
                let start = pos;
                pos += 2;
                let mut has_trailing_new_line = false;
                if !multi_line {
                    while pos < bytes.len() {
                        let ch = text[pos..].chars().next().unwrap();
                        if is_line_break(ch) {
                            has_trailing_new_line = true;
                            break;
                        }
                        pos += ch.len_utf8();
                    }
                } else {
                    while pos < bytes.len() {
                        if bytes[pos] == b'*' && bytes.get(pos + 1) == Some(&b'/') {
                            pos += 2;
                            break;
                        }
                        pos += 1;
                    }
                    pos = pos.min(bytes.len());
                }
                if collecting {
                    if let Some(p) = pending.take() {
                        out.push(p);
                    }
                    pending = Some(CommentRange { pos: start, end: pos, multi_line, has_trailing_new_line });
                }
            }
            c if (c as u32) > 0x7f && (crate::parser::utils::is_whitespace_char(c) || c == '\u{85}') => {
                if is_line_break(c) {
                    if let Some(p) = &mut pending {
                        p.has_trailing_new_line = true;
                    }
                }
                pos += c.len_utf8();
            }
            _ => break,
        }
    }
    if let Some(p) = pending {
        out.push(p);
    }
    out
}
