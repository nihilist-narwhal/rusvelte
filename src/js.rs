//! The JS side of the parser: a port of `phases/1-parse/acorn.js` on top of oxc.
//!
//! JS is parsed into oxc's AST, allocated in the component's arena, with spans rebased to
//! byte offsets in the template. Nothing is converted to ESTree JSON while parsing: the
//! parser records what Svelte's JS code does to the acorn AST (which comments were candidates
//! for attachment, parentheses removal, a few node rewrites), and [`ToJson`] replays that when
//! JSON in the shape of `svelte/compiler`'s `parse` is wanted.

use oxc_allocator::{Allocator, Vec as ArenaVec};
use oxc_ast::ast::{CommentKind, Expression, Program, Statement};
use oxc_ast_visit::VisitMut;
use oxc_estree::{CompactSerializer, ESTree};
use oxc_parser::{ParseOptions, Parser};
use oxc_span::{GetSpan, SourceType, Span};
use serde_json::{Map, Value};

use crate::error::Result;
use crate::errors as e;
use crate::locator::Locator;

/// A comment as stored in `root.comments`.
#[derive(Debug, Clone)]
pub struct JsComment {
    pub block: bool,
    pub value: String,
    pub start: usize,
    pub end: usize,
    /// Comments read by Svelte itself (`read_comment` in attributes) get `loc` from Svelte's
    /// locator, which includes `character`; the others get acorn's
    pub svelte_loc: bool,
}

impl JsComment {
    pub fn to_json(&self, locator: &Locator) -> Value {
        let mut m = Map::new();
        m.insert("type".into(), (if self.block { "Block" } else { "Line" }).into());
        m.insert("value".into(), self.value.clone().into());
        m.insert("start".into(), self.start.into());
        m.insert("end".into(), self.end.into());
        let (s, e) = if self.svelte_loc {
            (locator.locate(self.start), locator.locate(self.end))
        } else {
            (locator.acorn_position(self.start), locator.acorn_position(self.end))
        };
        let mut loc = Map::new();
        loc.insert("start".into(), s);
        loc.insert("end".into(), e);
        m.insert("loc".into(), Value::Object(loc));
        Value::Object(m)
    }

    /// The `{ type, value, start, end }` copy that `add_comments` attaches to nodes
    fn attached(&self) -> Value {
        let mut m = Map::new();
        m.insert("type".into(), (if self.block { "Block" } else { "Line" }).into());
        m.insert("value".into(), self.value.clone().into());
        m.insert("start".into(), self.start.into());
        m.insert("end".into(), self.end.into());
        Value::Object(m)
    }
}

/// Text to parse JS from: the template, or a piece of a modified copy of it (Svelte parses
/// things like `<pattern> = 1`). `text[0]` is at template offset `base`, so positions are
/// template offsets throughout.
#[derive(Debug, Clone, Copy)]
pub struct Src<'a> {
    pub text: &'a str,
    pub base: usize,
}

impl<'a> Src<'a> {
    pub fn new(text: &'a str, base: usize) -> Self {
        Src { text, base }
    }
    /// The offset just past the end, like `source.length` in the JS
    pub fn len(&self) -> usize {
        self.base + self.text.len()
    }
    pub fn slice(&self, start: usize, end: usize) -> &'a str {
        &self.text[start - self.base..end - self.base]
    }
    pub fn from(&self, start: usize) -> &'a str {
        &self.text[start - self.base..]
    }
    pub fn byte(&self, i: usize) -> Option<u8> {
        i.checked_sub(self.base).and_then(|i| self.text.as_bytes().get(i).copied())
    }
}

/// Which comments acorn's `add_comments` would have considered for a parse: those in
/// `root.comments[..upto]` that start at or after `index`
#[derive(Debug, Clone, Copy)]
pub struct CommentCtx {
    pub index: u32,
    pub upto: u32,
}

/// Rewrites Svelte applies to an expression after parsing (`{#each}` and patterns)
#[derive(Debug, Clone, Default)]
pub struct ExprFix {
    /// take `expressions[0]` of a SequenceExpression
    pub seq_first: bool,
    /// replace the TSAsExpression ending here by its expression
    pub strip_as_end: Option<u32>,
    /// overwrite `end`
    pub set_end: Option<u32>,
}

/// A parsed JS expression
#[derive(Debug)]
pub struct JsExpr<'a> {
    pub expr: Expression<'a>,
    /// The text it was parsed from, for comment attachment
    pub source: Src<'a>,
    /// `None` for expressions Svelte builds itself (no comment attachment pass)
    pub comments: Option<CommentCtx>,
    /// parsed despite an oxc error that acorn doesn't raise (see `acorn_accepts`)
    pub lenient: bool,
    /// whether Svelte runs `remove_parens` on the result (everywhere except snippet parameters)
    pub remove_parens: bool,
    pub fix: Option<Box<ExprFix>>,
}

impl<'a> JsExpr<'a> {
    pub fn start(&self) -> usize {
        self.expr.span().start as usize
    }
    pub fn end(&self) -> usize {
        self.expr.span().end as usize
    }
    /// The expression with outer parentheses removed (what Svelte's code sees after `remove_parens`)
    pub fn inner(&self) -> &Expression<'a> {
        self.expr.without_parentheses()
    }
}

#[derive(Debug)]
pub struct JsProgram<'a> {
    pub program: Program<'a>,
    pub comments: CommentCtx,
    /// where the script content starts and ends in the template
    pub start: usize,
    pub end: usize,
}

#[derive(Debug)]
pub struct JsStatement<'a> {
    pub stmt: Statement<'a>,
    pub source: Src<'a>,
    pub comments: CommentCtx,
}

/// Shift every span by `delta`
/// Shifts spans by `.0`, measuring the deepest nesting on the way (`.2`, see
/// `JsParser::max_depth`) and noting strings with lone surrogates (`.3`)
struct Rebase(i64, usize, usize, bool);

impl<'a> VisitMut<'a> for Rebase {
    fn visit_span(&mut self, span: &mut Span) {
        span.start = (span.start as i64 + self.0) as u32;
        span.end = (span.end as i64 + self.0) as u32;
    }

    fn enter_node(&mut self, _: oxc_ast::AstType) {
        self.1 += 1;
        self.2 = self.2.max(self.1);
    }

    fn leave_node(&mut self, _: oxc_ast::AstType) {
        self.1 -= 1;
    }

    fn visit_string_literal(&mut self, it: &mut oxc_ast::ast::StringLiteral<'a>) {
        self.3 |= it.lone_surrogates;
        oxc_ast_visit::walk_mut::walk_string_literal(self, it);
    }

    fn visit_template_element(&mut self, it: &mut oxc_ast::ast::TemplateElement<'a>) {
        self.3 |= it.lone_surrogates;
        oxc_ast_visit::walk_mut::walk_template_element(self, it);
    }
}

enum Parsed<'a> {
    Expr(Expression<'a>, bool),
    Program(Program<'a>),
    Statement(Statement<'a>),
}

#[derive(Clone, Copy, PartialEq)]
enum Goal {
    Expression,
    Program,
    Statement,
}

pub struct JsParser<'a> {
    pub ts: bool,
    pub loc: std::rc::Rc<Locator<'a>>,
    alloc: &'a Allocator,
    /// The deepest nesting of any JS parsed so far (the passes after parsing recurse over it)
    pub max_depth: std::cell::Cell<usize>,
    /// A string or template literal with a lone surrogate was parsed: oxc keeps its value in an
    /// encoded form, and Rust strings can't hold the real one, so `compile` declines
    pub lone_surrogates: std::cell::Cell<bool>,
    /// Report acorn's errors where oxc's parser reports a different one (see
    /// `Parser::acorn_checks`)
    pub acorn_checks: bool,
}

impl<'a> JsParser<'a> {
    pub fn new(ts: bool, loc: std::rc::Rc<Locator<'a>>, alloc: &'a Allocator) -> Self {
        JsParser {
            ts,
            loc,
            alloc,
            max_depth: std::cell::Cell::new(0),
            lone_surrogates: std::cell::Cell::new(false),
            acorn_checks: false,
        }
    }

    pub fn alloc_str(&self, s: &str) -> &'a str {
        self.alloc.alloc_str(s)
    }

    fn source_type(&self) -> SourceType {
        if self.ts {
            SourceType::ts().with_module(true)
        } else {
            SourceType::mjs()
        }
    }

    /// Parse `text` (which starts at byte `base` of the template) with oxc, rebasing spans
    /// to template offsets. On failure returns the position and message of the first error.
    fn oxc_parse(
        &self,
        text: &'a str,
        base: usize,
        goal: Goal,
        preserve_parens: bool,
    ) -> std::result::Result<(Parsed<'a>, Vec<JsComment>), (usize, String)> {
        let options = ParseOptions { preserve_parens, ..ParseOptions::default() };
        let parser = Parser::new(self.alloc, text, self.source_type()).with_options(options);
        let mut rebase = Rebase(base as i64, 0, 0, false);

        match goal {
            Goal::Expression => {
                let (mut expr, lenient) = match parser.parse_expression() {
                    Ok(expr) => (expr, false),
                    Err(errors) => {
                        // oxc rejects a few things acorn accepts; parse those through a program
                        // (which keeps the AST alongside the diagnostics) and ignore the diagnostics
                        if !errors.iter().all(|e| acorn_accepts(&e.message)) {
                            return Err(first_error(&errors, base));
                        }
                        match self.lenient_expression(text) {
                            Some(expr) => (expr, true),
                            None => return Err(first_error(&errors, base)),
                        }
                    }
                };
                rebase.visit_expression(&mut expr);
                self.max_depth.set(self.max_depth.get().max(rebase.2));
                self.lone_surrogates.set(self.lone_surrogates.get() || rebase.3);
                // `parse_expression` doesn't hand out comments; if there might be any, get them
                // from a program parse of `(<text>\n)`
                let comments = if text.contains("//") || text.contains("/*") {
                    self.expression_comments(text, base)
                } else {
                    Vec::new()
                };
                Ok((Parsed::Expr(expr, lenient), comments))
            }
            Goal::Program | Goal::Statement => {
                // acorn-typescript keeps parenthesized types; oxc only emits them with
                // `preserve_parens`, so keep parens and drop ParenthesizedExpressions in ToJson
                let parser = if self.ts {
                    Parser::new(self.alloc, text, self.source_type())
                        .with_options(ParseOptions { preserve_parens: true, ..ParseOptions::default() })
                } else {
                    parser
                };
                let ret = parser.parse();
                // (TypeScript grammar rules oxc checks and acorn-typescript doesn't)
                if ret.fatal_error || !ret.diagnostics.iter().all(|e| acorn_accepts(&e.message)) {
                    if let Some(err) = ret.diagnostics.first() {
                        let oxc = first_error(std::slice::from_ref(err), base);
                        // acorn raises its errors as it reads: one it raises before oxc's wins
                        if self.acorn_checks && goal == Goal::Program {
                            let earlier = if ret.fatal_error {
                                self.earlier_acorn_error(text, base, oxc.0)
                            } else {
                                let mut program = ret.program;
                                rebase.visit_program(&mut program);
                                crate::analyze::acorn::first_error(&program, self.loc.source(), self.ts)
                            };
                            if let Some(earlier) = earlier.filter(|e| e.0 <= oxc.0) {
                                return Err(earlier);
                            }
                        }
                        return Err(oxc);
                    }
                }
                let comments: Vec<JsComment> = ret
                    .program
                    .comments
                    .iter()
                    .map(|c| self.make_comment(text, base, c.span.start as usize, c.span.end as usize, c.kind != CommentKind::Line))
                    .collect();
                let mut program = ret.program;
                if goal == Goal::Program {
                    rebase.visit_program(&mut program);
                    self.max_depth.set(self.max_depth.get().max(rebase.2));
                self.lone_surrogates.set(self.lone_surrogates.get() || rebase.3);
                    Ok((Parsed::Program(program), comments))
                } else {
                    if program.body.is_empty() {
                        return Err((base + text.len(), "Unexpected token".into()));
                    }
                    let mut stmt = program.body.remove(0);
                    rebase.visit_statement(&mut stmt);
                    self.max_depth.set(self.max_depth.get().max(rebase.2));
                self.lone_surrogates.set(self.lone_surrogates.get() || rebase.3);
                    Ok((Parsed::Statement(stmt), comments))
                }
            }
        }
    }

    /// After oxc gave up on a script at `pos` (with no AST), the error acorn raises before
    /// reaching `pos` (one oxc leaves to semantic analysis): in the statements before the one
    /// that fails (the longest prefix of complete statements that parses), or a strict mode
    /// reserved word opening the failing statement
    fn earlier_acorn_error(&self, text: &'a str, base: usize, pos: usize) -> Option<(usize, String)> {
        let rel = pos.checked_sub(base)?.min(text.len());
        let bytes = text.as_bytes();
        let boundaries = (1..=rel).rev().filter(|&i| matches!(bytes[i - 1], b';' | b'}' | b'\n')).take(16);
        for end in boundaries.chain(std::iter::once(0)) {
            let alloc = Allocator::default();
            let ret = Parser::new(&alloc, &text[..end], self.source_type()).parse();
            if ret.fatal_error || !ret.diagnostics.is_empty() {
                continue;
            }
            let mut program = ret.program;
            Rebase(base as i64, 0, 0, false).visit_program(&mut program);
            if let Some(error) = crate::analyze::acorn::first_error(&program, self.loc.source(), self.ts) {
                return Some(error);
            }
            return crate::analyze::acorn::reserved_statement_start(text, end, self.ts).map(|(p, m)| (p + base, m));
        }
        None
    }

    /// Parse `(<text>\n)` as a program and return the expression inside, with spans relative
    /// to `text`
    fn lenient_expression(&self, text: &str) -> Option<Expression<'a>> {
        let wrapped = self.alloc.alloc_str(&format!("({text}\n)"));
        let options = ParseOptions { preserve_parens: true, ..ParseOptions::default() };
        let ret = Parser::new(self.alloc, wrapped, self.source_type()).with_options(options).parse();
        if ret.fatal_error || !ret.diagnostics.iter().all(|e| acorn_accepts(&e.message)) {
            return None;
        }
        let mut body: ArenaVec<'a, Statement<'a>> = ret.program.body;
        if body.is_empty() {
            return None;
        }
        let Statement::ExpressionStatement(stmt) = body.remove(0) else { return None };
        let Expression::ParenthesizedExpression(paren) = stmt.unbox().expression else { return None };
        let mut expr = paren.unbox().expression;
        Rebase(-1, 0, 0, false).visit_expression(&mut expr);
        Some(expr)
    }

    fn expression_comments(&self, text: &str, base: usize) -> Vec<JsComment> {
        let wrapped = format!("({text}\n)");
        let spans: Vec<(usize, usize, bool)> = {
            let alloc = Allocator::default();
            let ret = Parser::new(&alloc, &wrapped, self.source_type()).parse();
            ret.program
                .comments
                .iter()
                .map(|c| (c.span.start as usize - 1, c.span.end as usize - 1, c.kind != CommentKind::Line))
                .collect()
        };
        spans.into_iter().map(|(s, e, block)| self.make_comment(text, base, s, e, block)).collect()
    }

    fn make_comment(&self, text: &str, base: usize, start: usize, end: usize, block: bool) -> JsComment {
        let mut value = if block { text[start + 2..end - 2].to_string() } else { text[start + 2..end].to_string() };
        let (start, end) = (start + base, end + base);

        if block && value.contains('\n') {
            // strip the indentation of the line the comment starts on from every line
            let src = self.loc.source();
            let bytes = src.as_bytes();
            let mut a = start;
            while a > 0 && bytes[a - 1] != b'\n' {
                a -= 1;
            }
            let mut b = a;
            while b < bytes.len() && (bytes[b] == b' ' || bytes[b] == b'\t') {
                b += 1;
            }
            let indentation = &src[a..b];
            if !indentation.is_empty() {
                value = dedent(&value, indentation);
            }
        }

        JsComment {
            block,
            value,
            start,
            end,
            svelte_loc: false,
        }
    }

    /// `acorn.parse` for a `<script>`'s contents. `content` starts at byte `base`.
    /// `root_comments` is the template-wide comment list (comments are appended to it).
    pub fn parse_program(
        &self,
        content: &'a str,
        base: usize,
        root_comments: &mut Vec<JsComment>,
    ) -> Result<JsProgram<'a>> {
        self.loc.add_script(base, base + content.len());
        let (parsed, comments) = self
            .oxc_parse(content, base, Goal::Program, false)
            .map_err(|(pos, msg)| e::js_parse_error(pos, &msg))?;
        let Parsed::Program(program) = parsed else { unreachable!() };
        root_comments.extend(comments);
        Ok(JsProgram {
            program,
            comments: CommentCtx { index: 0, upto: root_comments.len() as u32 },
            start: base,
            end: base + content.len(),
        })
    }

    /// `acorn.parseExpressionAt(source, index)`: parse the longest expression starting at
    /// `index`
    pub fn parse_expression_at(
        &self,
        source: Src<'a>,
        index: usize,
        root_comments: &mut Vec<JsComment>,
    ) -> Result<JsExpr<'a>> {
        self.loc.add_expression(index);
        let text = source.from(index);
        // Usually the expression runs up to the `}` closing the tag: try that first, so the
        // common case is a single parse. Only accept it if what follows is a token that can't
        // continue an expression, so the result is the same as the longest-prefix parse.
        if let Some(close) = crate::parser::utils::find_matching_bracket(text, 0, b'{') {
            if let Ok(parsed) = self.oxc_parse(&text[..close], index, Goal::Expression, true) {
                if self.ts || find_ts_operator(expr_of(&parsed.0), source).is_none() {
                    return Ok(self.finish_expression(parsed, source, index, root_comments));
                }
            }
        }
        let parsed = match self.oxc_parse(text, index, Goal::Expression, true) {
            Ok(p) => p,
            Err((pos, msg)) if !self.ts && msg.contains("TypeScript files") => {
                // a TS-only construct in a JS file: acorn would have stopped at the keyword
                let cut = find_keyword(source, pos, &["as", "satisfies"]).unwrap_or(pos);
                match self.oxc_parse(source.slice(index, cut), index, Goal::Expression, true) {
                    Ok(p) => p,
                    Err(_) => return Err(e::js_parse_error(pos, &msg)),
                }
            }
            Err((pos, msg)) => {
                // oxc wants the whole input to be the expression; acorn stops at the first
                // token that can't continue it. Retry with everything before that token.
                if pos > index && pos <= source.len() {
                    match self.oxc_parse(source.slice(index, pos), index, Goal::Expression, true) {
                        Ok(p) => p,
                        Err(_) => return Err(e::js_parse_error(pos, &msg)),
                    }
                } else {
                    return Err(e::js_parse_error(pos, &msg));
                }
            }
        };

        let mut parsed = parsed;
        if !self.ts {
            // acorn (without the TS plugin) stops at `as`/`satisfies`, while oxc reads a TS
            // assertion and complains later. Cut the expression off before the keyword.
            if let Some(cut) = find_ts_operator(expr_of(&parsed.0), source) {
                if let Ok(p) = self.oxc_parse(source.slice(index, cut), index, Goal::Expression, true) {
                    parsed = p;
                }
            }
        }

        Ok(self.finish_expression(parsed, source, index, root_comments))
    }

    fn finish_expression(
        &self,
        (parsed, mut comments): (Parsed<'a>, Vec<JsComment>),
        source: Src<'a>,
        index: usize,
        root_comments: &mut Vec<JsComment>,
    ) -> JsExpr<'a> {
        let Parsed::Expr(expr, lenient) = parsed else { unreachable!() };

        // acorn has also consumed comments between the expression and the next token
        let end = expr.span().end as usize;
        for (s, e, block) in scan_comments(source.text, end - source.base, source.text.len()) {
            if comments.iter().any(|c| c.start == s + source.base) {
                continue;
            }
            comments.push(self.make_comment(source.text, source.base, s, e, block));
        }
        comments.sort_by_key(|c| c.start);

        root_comments.extend(comments);
        JsExpr {
            expr,
            source,
            comments: Some(CommentCtx { index: index as u32, upto: root_comments.len() as u32 }),
            lenient,
            remove_parens: true,
            fix: None,
        }
    }

    /// Like `parse_expression_at`, but for a statement (used by declaration tags)
    pub fn parse_statement_at(
        &self,
        source: Src<'a>,
        index: usize,
        root_comments: &mut Vec<JsComment>,
    ) -> Result<JsStatement<'a>> {
        self.loc.add_expression(index);
        // find the end of the statement: the `}` closing the tag
        let rest = source.from(index);
        let end = crate::parser::utils::find_matching_bracket(rest, 0, b'{').map_or(source.len(), |e| e + index);
        let (parsed, comments) = match self.oxc_parse(source.slice(index, end), index, Goal::Statement, false) {
            Ok(p) => p,
            Err((pos, msg)) => {
                if pos >= source.len() || end == source.len() {
                    return Err(e::unexpected_eof(source.len()));
                }
                return Err(e::js_parse_error(pos, &msg));
            }
        };
        let Parsed::Statement(stmt) = parsed else { unreachable!() };
        root_comments.extend(comments);
        Ok(JsStatement {
            stmt,
            source,
            comments: CommentCtx { index: index as u32, upto: root_comments.len() as u32 },
        })
    }
}

fn expr_of<'b, 'a>(parsed: &'b Parsed<'a>) -> &'b Expression<'a> {
    match parsed {
        Parsed::Expr(e, _) => e,
        _ => unreachable!(),
    }
}

/// If the outermost expression is a TS `as`/`satisfies`, the offset of that keyword
fn find_ts_operator(expr: &Expression, source: Src) -> Option<usize> {
    let mut node = expr;
    loop {
        let inner = match node {
            Expression::TSAsExpression(e) => &e.expression,
            Expression::TSSatisfiesExpression(e) => &e.expression,
            _ => return None,
        };
        // the leftmost one is where acorn stops
        if matches!(inner, Expression::TSAsExpression(_) | Expression::TSSatisfiesExpression(_)) {
            node = inner;
            continue;
        }
        return find_keyword(source, inner.span().end as usize, &["as", "satisfies"]);
    }
}

// ---------------------------------------------------------------------------------------
// Conversion to ESTree JSON in acorn's shape

/// Everything needed to turn JS nodes into the JSON `svelte/compiler` produces
pub struct ToJson<'c> {
    pub ts: bool,
    pub loc: &'c Locator<'c>,
    pub comments: &'c [JsComment],
}

impl ToJson<'_> {
    fn estree<T: ESTree>(&self, node: &T) -> Value {
        let mut s = CompactSerializer::new(self.ts, false);
        node.serialize(&mut s);
        let mut value: Value = serde_json::from_str(&s.into_string()).expect("oxc produced invalid JSON");
        self.fix_node(&mut value);
        value
    }

    /// An expression as `read_expression` returns it: comments attached, parentheses removed,
    /// and Svelte's rewrites applied
    pub fn expr(&self, e: &JsExpr) -> Value {
        let mut value = self.estree(&e.expr);
        if e.lenient {
            mark_optional_defaults(&mut value, e.source);
        }
        if let Some(ctx) = e.comments {
            add_comments(&mut value, e.source, &self.comments[..ctx.upto as usize], ctx.index as usize);
        }
        if e.remove_parens {
            remove_parens(&mut value);
        }
        if let Some(fix) = &e.fix {
            if fix.seq_first && value.get("type").and_then(Value::as_str) == Some("SequenceExpression") {
                value = value["expressions"][0].take();
            }
            if let Some(end) = fix.strip_as_end {
                strip_trailing_as(&mut value, end as usize);
            }
            if let Some(end) = fix.set_end {
                value["end"] = end.into();
            }
        }
        value
    }

    pub fn program(&self, p: &JsProgram) -> Value {
        let mut value = self.estree(&p.program);
        if let Value::Object(m) = &mut value {
            m.shift_remove("hashbang");
            // acorn's Program spans the whole (padded) source from 0
            m.insert("start".into(), 0.into());
            m.insert("end".into(), p.end.into());
        }
        if self.ts {
            remove_parens(&mut value);
        }
        add_comments(&mut value, Src::new(self.loc.source(), 0), &self.comments[..p.comments.upto as usize], 0);
        value
    }

    pub fn statement(&self, s: &JsStatement) -> Value {
        let mut value = self.estree(&s.stmt);
        if self.ts {
            remove_parens(&mut value);
        }
        add_comments(&mut value, s.source, &self.comments[..s.comments.upto as usize], s.comments.index as usize);
        value
    }

    /// acorn-typescript records a trailing comma in `<T,>` as `extra.trailingComma`
    fn mark_trailing_comma(&self, map: &mut Map<String, Value>) {
        let last_end = map.get("params").and_then(Value::as_array).and_then(|p| p.last()).and_then(|p| p.get("end")).and_then(Value::as_u64);
        let end = map.get("end").and_then(Value::as_u64);
        let (Some(last_end), Some(end)) = (last_end, end) else { return };
        let source = self.loc.source().as_bytes();
        let (from, to) = (last_end as usize, (end as usize).min(source.len()));
        if from >= to {
            return;
        }
        if let Some(p) = source[from..to].iter().position(|&b| b == b',') {
            let mut extra = Map::new();
            extra.insert("trailingComma".into(), (from + p).into());
            map.insert("extra".into(), Value::Object(extra));
        }
    }

    /// Add `loc`, and smooth over differences between oxc's ESTree and acorn's
    fn fix_node(&self, node: &mut Value) {
        match node {
            Value::Array(items) => {
                for item in items {
                    self.fix_node(item);
                }
            }
            Value::Object(map) => {
                for (_, v) in map.iter_mut() {
                    if v.is_object() || v.is_array() {
                        self.fix_node(v);
                    }
                }
                if !map.contains_key("type") {
                    return;
                }
                fix_shape(map, self.ts);
                if map.get("type").and_then(Value::as_str) == Some("TSTypeParameterDeclaration") {
                    self.mark_trailing_comma(map);
                }
                if let (Some(s), Some(e)) = (map.get("start").and_then(Value::as_u64), map.get("end").and_then(Value::as_u64)) {
                    let mut loc = Map::new();
                    loc.insert("start".into(), self.loc.acorn_position(s as usize));
                    loc.insert("end".into(), self.loc.acorn_position(e as usize));
                    map.insert("loc".into(), Value::Object(loc));
                }
            }
            _ => {}
        }
    }
}

/// Remove a trailing `as T` that the TS parser read into an `{#each}` expression
fn strip_trailing_as(node: &mut Value, target_end: usize) -> bool {
    if node.get("type").and_then(Value::as_str) == Some("TSAsExpression") && node_end(node) == target_end {
        let inner = node["expression"].take();
        *node = inner;
        return true;
    }
    if let Value::Object(map) = node {
        for (k, v) in map.iter_mut() {
            if k == "loc" {
                continue;
            }
            match v {
                Value::Object(_) if v.get("type").is_some_and(Value::is_string) => {
                    if strip_trailing_as(v, target_end) {
                        return true;
                    }
                }
                Value::Array(items) => {
                    for item in items {
                        if item.get("type").is_some_and(Value::is_string) && strip_trailing_as(item, target_end) {
                            return true;
                        }
                    }
                }
                _ => {}
            }
        }
    }
    false
}

/// The first of `keywords` (as a whole word) at or after `from`
fn find_keyword(source: Src, from: usize, keywords: &[&str]) -> Option<usize> {
    let bytes = source.text.as_bytes();
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'$';
    let mut i = from.checked_sub(source.base)?;
    while i < bytes.len() {
        for kw in keywords {
            if bytes[i..].starts_with(kw.as_bytes())
                && (i == 0 || !is_word(bytes[i - 1]))
                && !bytes.get(i + kw.len()).copied().is_some_and(is_word)
            {
                return Some(i + source.base);
            }
        }
        i += 1;
    }
    None
}

/// oxc errors for code that acorn(-typescript) parses without complaint
fn acorn_accepts(message: &str) -> bool {
    matches!(
        message,
        "A parameter cannot have a question mark and an initializer."
            | "A required parameter cannot follow an optional parameter."
            | "Import declarations in a namespace cannot reference a module."
    ) || message.starts_with("Type parameter name cannot be '")
}

/// `x?: T = 1`: oxc rejects it and loses the `?`, acorn-typescript marks `x` optional
fn mark_optional_defaults(node: &mut Value, text: Src) {
    match node {
        Value::Object(map) => {
            if map.get("type").and_then(Value::as_str) == Some("AssignmentPattern") {
                if let Some(Value::Object(left)) = map.get_mut("left") {
                    let name_end = left.get("start").and_then(Value::as_u64).unwrap_or(0) as usize
                        + left.get("name").and_then(Value::as_str).map_or(0, str::len);
                    if left.get("type").and_then(Value::as_str) == Some("Identifier") && text.byte(name_end) == Some(b'?') {
                        left.insert("optional".into(), true.into());
                        map.shift_remove("optional");
                    }
                }
            }
            map.values_mut().for_each(|v| mark_optional_defaults(v, text));
        }
        Value::Array(items) => items.iter_mut().for_each(|i| mark_optional_defaults(i, text)),
        _ => {}
    }
}

fn first_error(errors: &[oxc_diagnostics::OxcDiagnostic], base: usize) -> (usize, String) {
    let err = &errors[0];
    let pos = err.labels.iter().map(|l| l.offset()).min().unwrap_or(0);
    (pos as usize + base, err.message.to_string())
}

fn node_end(node: &Value) -> usize {
    node.get("end").and_then(Value::as_u64).unwrap_or(0) as usize
}

fn node_start(node: &Value) -> usize {
    node.get("start").and_then(Value::as_u64).unwrap_or(0) as usize
}

fn dedent(value: &str, indentation: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for (i, line) in value.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(line.strip_prefix(indentation).unwrap_or(line));
    }
    out
}

/// Find JS comments in `text[from..to]` that sit between tokens, i.e. skip whitespace and
/// comments starting at `from` (used for the gap after an expression).
fn scan_comments(text: &str, from: usize, to: usize) -> Vec<(usize, usize, bool)> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = from;
    while i < to {
        let b = bytes[i];
        if b == b'/' && bytes.get(i + 1) == Some(&b'/') {
            let mut j = i + 2;
            while j < bytes.len() && !matches!(bytes[j], b'\n' | b'\r') {
                if bytes[j] == 0xE2 && bytes.get(j + 1) == Some(&0x80) && matches!(bytes.get(j + 2), Some(0xA8 | 0xA9)) {
                    break;
                }
                j += 1;
            }
            out.push((i, j, false));
            i = j;
        } else if b == b'/' && bytes.get(i + 1) == Some(&b'*') {
            let Some(rel) = text[i + 2..].find("*/") else { break };
            let j = i + 2 + rel + 2;
            out.push((i, j, true));
            i = j;
        } else if b.is_ascii_whitespace() {
            i += 1;
        } else if b >= 0x80 {
            let c = text[i..].chars().next().unwrap();
            if crate::parser::utils::is_whitespace_char(c) {
                i += c.len_utf8();
            } else {
                break;
            }
        } else {
            break;
        }
    }
    out
}

/// Port of `add_comments` from `get_comment_handlers`.
/// `comments` is the template-wide list; like the JS code, every comment at or after
/// `index` is a candidate (including ones from earlier parses).
fn add_comments(ast: &mut Value, source: Src, all: &[JsComment], index: usize) {
    if all.is_empty() {
        return;
    }
    let mut queue: std::collections::VecDeque<Value> =
        all.iter().filter(|c| c.start >= index).map(|c| c.attached()).collect();
    if queue.is_empty() {
        return;
    }

    fn visit(node: &mut Value, parent: Option<(&str, usize, Option<usize>, Option<usize>)>, source: Src, queue: &mut std::collections::VecDeque<Value>) {
        // parent: (type, end, index of node in parent's list, length of that list)
        let start = node_start(node);
        while let Some(c) = queue.front() {
            if node_start(c) < start {
                let c = queue.pop_front().unwrap();
                push_comment(node, "leadingComments", c);
            } else {
                break;
            }
        }

        // next(): visit children in key order
        let ty = node.get("type").and_then(Value::as_str).unwrap_or("").to_string();
        let end = node_end(node);
        if let Value::Object(map) = node {
            let keys: Vec<String> = map.keys().cloned().collect();
            for k in keys {
                if k == "leadingComments" || k == "trailingComments" || k == "loc" {
                    continue;
                }
                let list_key = matches!(
                    (ty.as_str(), k.as_str()),
                    ("BlockStatement" | "Program", "body") | ("ArrayExpression", "elements") | ("ObjectExpression", "properties")
                );
                match map.get_mut(&k).unwrap() {
                    Value::Array(items) => {
                        let len = items.len();
                        for (i, item) in items.iter_mut().enumerate() {
                            if item.get("type").map_or(false, Value::is_string) {
                                let info = if list_key { (Some(i), Some(len)) } else { (None, None) };
                                visit(item, Some((&ty, end, info.0, info.1)), source, queue);
                            }
                        }
                    }
                    child @ Value::Object(_) => {
                        if child.get("type").map_or(false, Value::is_string) {
                            visit(child, Some((&ty, end, None, None)), source, queue);
                        }
                    }
                    _ => {}
                }
            }
        }

        if queue.is_empty() {
            return;
        }
        let parent_end = parent.map(|p| p.1);
        if parent.is_none() || Some(end) != parent_end {
            let first_start = node_start(&queue[0]);
            let is_last_in_body = matches!(parent, Some((_, _, Some(i), Some(len))) if i == len - 1);
            if is_last_in_body {
                while let Some(c) = queue.front() {
                    if let Some(pe) = parent_end {
                        if node_start(c) >= pe {
                            break;
                        }
                    }
                    let c = queue.pop_front().unwrap();
                    push_comment(node, "trailingComments", c);
                }
            } else if end <= first_start {
                let clamp = |i: usize| i.clamp(source.base, source.len());
                let slice = source.slice(clamp(end), clamp(first_start).max(clamp(end)));
                if slice.bytes().all(|b| matches!(b, b',' | b')' | b' ' | b'\t')) {
                    let c = queue.pop_front().unwrap();
                    if let Value::Object(m) = node {
                        m.insert("trailingComments".into(), Value::Array(vec![c]));
                    }
                }
            }
        }
    }

    visit(ast, None, source, &mut queue);

    let is_program = ast.get("type").and_then(Value::as_str) == Some("Program");
    if let Some(first) = queue.front() {
        if node_start(first) >= node_end(ast) || is_program {
            for c in queue.drain(..) {
                push_comment(ast, "trailingComments", c);
            }
        }
    }
}

fn push_comment(node: &mut Value, key: &str, comment: Value) {
    if let Value::Object(m) = node {
        match m.get_mut(key) {
            Some(Value::Array(list)) => list.push(comment),
            _ => {
                m.insert(key.into(), Value::Array(vec![comment]));
            }
        }
    }
}

/// `remove_parens`: replace every ParenthesizedExpression by its contents
pub fn remove_parens(node: &mut Value) {
    loop {
        let is_paren = node.get("type").and_then(Value::as_str) == Some("ParenthesizedExpression");
        if !is_paren {
            break;
        }
        let inner = node.get_mut("expression").map(Value::take).unwrap_or(Value::Null);
        *node = inner;
    }
    match node {
        Value::Object(map) => {
            for (k, v) in map.iter_mut() {
                if k == "loc" {
                    continue;
                }
                if v.is_object() || v.is_array() {
                    remove_parens(v);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                remove_parens(item);
            }
        }
        _ => {}
    }
}

/// Differences between oxc's ESTree output and acorn's
fn fix_shape(map: &mut Map<String, Value>, ts: bool) {
    let ty = map.get("type").and_then(Value::as_str).unwrap_or("").to_string();
    if ty == "Program" {
        map.shift_remove("hashbang");
    }
    // acorn only has `phase` for `import source`/`import defer`, `directive` for directives
    for key in ["phase", "directive"] {
        if map.get(key) == Some(&Value::Null) {
            map.shift_remove(key);
        }
    }
    // acorn has no decorators, and acorn-typescript only emits these when non-empty
    for key in ["decorators", "implements"] {
        if matches!(map.get(key), Some(Value::Array(a)) if a.is_empty()) {
            map.shift_remove(key);
        }
    }
    if ty.starts_with("TS") && !ty.starts_with("TSAbstract") && map.get("static") == Some(&Value::Bool(false)) {
        map.shift_remove("static");
    }
    if ty == "ImportDeclaration" && map.get("importKind").and_then(Value::as_str) == Some("type") {
        if matches!(map.get("attributes"), Some(Value::Array(a)) if a.is_empty()) {
            map.shift_remove("attributes");
        }
    }
    if ty == "TSEnumDeclaration" {
        if let Some(mut body) = map.shift_remove("body") {
            if let Some(members) = body.get_mut("members") {
                map.insert("members".into(), members.take());
            }
        }
    }
    if ty == "TSImportType" {
        if let Some(source) = map.shift_remove("source") {
            map.insert("argument".into(), source);
        }
        for key in ["options", "qualifier"] {
            if map.get(key) == Some(&Value::Null) {
                map.shift_remove(key);
            }
        }
    }
    if matches!(
        ty.as_str(),
        "TSFunctionType" | "TSConstructorType" | "TSCallSignatureDeclaration" | "TSConstructSignatureDeclaration" | "TSMethodSignature"
    ) {
        if let Some(params) = map.shift_remove("params") {
            map.insert("parameters".into(), params);
        }
        match map.shift_remove("returnType") {
            Some(Value::Null) | None => {}
            Some(rt) => {
                map.insert("typeAnnotation".into(), rt);
            }
        }
    }
    if ty == "TSTypeParameter" {
        if let Some(name) = map.get("name").and_then(|n| n.get("name")).cloned() {
            map.insert("name".into(), name);
        }
    }
    if ty == "TSTypeParameter" {
        map.retain(|k, v| {
            !matches!((k.as_str(), &*v), ("constraint" | "default", Value::Null) | ("in" | "out" | "const", Value::Bool(false)))
        });
    }
    if (ty == "TSEnumDeclaration" && map.get("const") == Some(&Value::Bool(false)))
        || (ty == "TSModuleDeclaration" && map.get("global") == Some(&Value::Bool(false)))
    {
        map.shift_remove(if ty == "TSEnumDeclaration" { "const" } else { "global" });
    }
    if ts
        && ty == "CallExpression"
        && map.get("typeArguments").is_some_and(|t| !t.is_null())
        && map.get("optional") == Some(&Value::Bool(false))
        && map.get("callee").and_then(|c| c.get("optional")) != Some(&Value::Bool(true))
    {
        // acorn-typescript's call-with-type-arguments path doesn't set `optional`, except
        // inside an optional chain
        map.shift_remove("optional");
    }
    if ts && ty == "ImportExpression" && map.get("options") == Some(&Value::Null) {
        // acorn-typescript parses `import()` itself, without import attributes
        map.shift_remove("options");
    }
    if ty == "Decorator" {
        if let Some(Value::Object(call)) = map.get_mut("expression") {
            if call.get("type").and_then(Value::as_str) == Some("CallExpression") && call.get("optional") == Some(&Value::Bool(false)) {
                call.shift_remove("optional");
            }
        }
    }
    if ty == "TSClassImplements" {
        map.insert("type".into(), "TSExpressionWithTypeArguments".into());
    }
    if ty == "AccessorProperty" {
        map.insert("type".into(), "PropertyDefinition".into());
        map.insert("accessor".into(), true.into());
    }
    // acorn-typescript still uses the old name for `extends Base<T>` type arguments
    if let Some(args) = map.shift_remove("superTypeArguments") {
        if !args.is_null() {
            map.insert("superTypeParameters".into(), args);
        }
    }
    if ty == "TSDeclareFunction" && map.get("body") == Some(&Value::Null) {
        map.shift_remove("body");
    }
    if ty == "TSEmptyBodyFunctionExpression" {
        map.insert("type".into(), "TSDeclareMethod".into());
        if map.get("body") == Some(&Value::Null) {
            map.shift_remove("body");
        }
    }
    if ty == "TSAbstractMethodDefinition" {
        map.insert("type".into(), "MethodDefinition".into());
        map.insert("abstract".into(), true.into());
    }
    if ty == "TSEnumMember" {
        map.shift_remove("computed");
    }
    if ty == "TSModuleDeclaration" {
        map.shift_remove("kind");
    }
    if ty == "RestElement" && map.get("value") == Some(&Value::Null) {
        map.shift_remove("value");
    }
    if ts && ty == "TemplateElement" {
        // TS-ESTree includes the delimiters in a quasi's range, acorn doesn't
        let tail = map.get("tail") == Some(&Value::Bool(true));
        if let (Some(s), Some(e)) = (map.get("start").and_then(Value::as_u64), map.get("end").and_then(Value::as_u64)) {
            map.insert("start".into(), (s + 1).into());
            map.insert("end".into(), (e - if tail { 1 } else { 2 }).into());
        }
    }
    if ts {
        if matches!(ty.as_str(), "ImportDeclaration" | "ExportNamedDeclaration" | "ExportAllDeclaration")
            && matches!(map.get("attributes"), Some(Value::Array(a)) if a.is_empty())
        {
            map.shift_remove("attributes");
        }
        if ty == "TSInterfaceDeclaration" && matches!(map.get("extends"), Some(Value::Array(a)) if a.is_empty()) {
            map.shift_remove("extends");
        }
        // TS-ESTree emits empty TS-specific fields everywhere; acorn-typescript only when set
        map.retain(|k, v| {
            !matches!(
                (k.as_str(), &*v),
                ("decorators", Value::Array(a)) if a.is_empty()
            ) && !matches!(
                (k.as_str(), &*v),
                ("typeAnnotation" | "returnType" | "typeParameters" | "typeArguments" | "superTypeArguments" | "accessibility", Value::Null)
                    | ("definite" | "declare" | "abstract" | "override" | "readonly", Value::Bool(false))
            )
        });
        if ty != "MemberExpression" && ty != "CallExpression" && map.get("optional") == Some(&Value::Bool(false)) {
            map.shift_remove("optional");
        }
    }
}

