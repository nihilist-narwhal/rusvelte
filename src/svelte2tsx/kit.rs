//! Port of svelte2tsx's SvelteKit "kit file" helpers (0.7.61): `upsertKitFile` and friends
//! from `helpers/sveltekit.ts`, with `findExports` from `helpers/typescript.ts`.
//!
//! svelte-check (`--incremental` / `--tsgo`) copies route files (`+page.ts`,
//! `+layout.server.js`, `+server.ts`, ...), hooks files and param matchers into its generated
//! directory, adding the types SvelteKit's "zero-config types" would give them (a `load(event)`
//! gets `: import('./$types.js').PageLoadEvent`, `export const prerender` gets
//! `: boolean | 'auto'`, ...). Every insertion is recorded as an [`AddedCode`] so diagnostics in
//! the generated file can be mapped back with [`to_original_pos`].
//!
//! The TypeScript AST the JS relies on is emulated on oxc's: TS's `node.pos` (the end of the
//! previous token) comes from the token list, and the JSDoc TS attaches to nodes (needed for
//! `.js` files, where an existing `@type`/`@param`/`@satisfies` means "already typed", and for
//! rewriting `import('...')` types inside JSDoc) is found and parsed the way TS's parser does it.
//!
//! **Positions**: everything in [`AddedCode`], [`to_original_pos`] and [`to_virtual_pos`] is in
//! UTF-16 code units, like the JS (and like TypeScript's positions).

use std::collections::HashMap;
use std::path::Path;

use oxc_allocator::Allocator;
use oxc_ast::ast::*;
use oxc_ast_visit::{walk, Visit};
use oxc_parser::{config::TokensParserConfig, Parser};
use oxc_span::{GetSpan, SourceType};
use oxc_syntax::scope::ScopeFlags;

use super::rewrite_imports::{external_import_rewrite, RewriteExternalImports};

/// Where SvelteKit's hooks and param matchers live (`config.kit.files`, without extensions)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KitFilesSettings {
    pub server_hooks_path: String,
    pub client_hooks_path: String,
    pub universal_hooks_path: String,
    pub params_path: String,
}

impl Default for KitFilesSettings {
    /// svelte-check's defaults, used when `svelte.config.js` doesn't set `files`
    fn default() -> Self {
        KitFilesSettings {
            server_hooks_path: "src/hooks.server".into(),
            client_hooks_path: "src/hooks.client".into(),
            universal_hooks_path: "src/hooks".into(),
            params_path: "src/params".into(),
        }
    }
}

/// One insertion into the original text. All fields are UTF-16 units.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddedCode {
    /// Where the inserted text starts in the generated text
    pub generated_pos: usize,
    /// Where it was inserted in the original text
    pub original_pos: usize,
    /// `inserted.length`
    pub length: usize,
    /// The length of this and all previous insertions
    pub total: usize,
    pub inserted: String,
}

/// What `upsertKitFile` returns
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KitFileOutput {
    pub text: String,
    pub added_code: Vec<AddedCode>,
}

const KIT_PAGE_FILES: [&str; 5] = ["+page", "+layout", "+page.server", "+layout.server", "+server"];

/// `path.basename` (posix)
fn basename(p: &str) -> &str {
    let t = p.trim_end_matches('/');
    t.rsplit('/').next().unwrap_or(t)
}

/// `path.extname` (posix) of a basename
fn extname(base: &str) -> &str {
    match base.rfind('.') {
        // `.ts` and `..` have no extension
        Some(dot) if dot > 0 && base != ".." => &base[dot..],
        _ => "",
    }
}

/// JS `s.slice(0, -n)`: empty when `n` is 0
fn drop_end(s: &str, n: usize) -> &str {
    if n == 0 || n >= s.len() {
        ""
    } else {
        &s[..s.len() - n]
    }
}

/// `isKitFile`: a route file, hooks file or params file
pub fn is_kit_file(file_name: &str, settings: &KitFilesSettings) -> bool {
    let base = basename(file_name);
    is_kit_route_file(base)
        || is_hooks_file(file_name, base, &settings.server_hooks_path)
        || is_hooks_file(file_name, base, &settings.client_hooks_path)
        || is_hooks_file(file_name, base, &settings.universal_hooks_path)
        || is_params_file(file_name, base, &settings.params_path)
}

/// `isKitRouteFile`: `+page`, `+layout`, `+page.server`, `+layout.server`, `+server` (also
/// `+page@foo...`)
pub fn is_kit_route_file(basename: &str) -> bool {
    let name = if basename.contains('@') {
        basename.split('@').next().unwrap_or("")
    } else {
        drop_end(basename, extname(basename).len())
    };
    KIT_PAGE_FILES.contains(&name)
}

/// `isKitErrorFile`: `+error.svelte`
pub fn is_kit_error_file(basename: &str) -> bool {
    drop_end(basename, extname(basename).len()) == "+error"
}

/// `isHooksFile`: `<hooksPath>.ext` or `<hooksPath>/index.{ts,js}`
pub fn is_hooks_file(file_name: &str, basename: &str, hooks_path: &str) -> bool {
    ((basename == "index.ts" || basename == "index.js") && drop_end(file_name, basename.len() + 1).ends_with(hooks_path))
        || drop_end(file_name, extname(basename).len()).ends_with(hooks_path)
}

/// `isParamsFile`: a file directly in the params directory that isn't a test
pub fn is_params_file(file_name: &str, basename: &str, params_path: &str) -> bool {
    drop_end(file_name, basename.len() + 1).ends_with(params_path) && !basename.contains(".test") && !basename.contains(".spec")
}

/// `getKitTypeImportPath`: `@sveltejs/kit/hooks` if the `@sveltejs/kit` that `file_name`
/// resolves (node10 resolution: the nearest `node_modules/@sveltejs/kit/package.json` up the
/// directory tree) is version 3 or later, `@sveltejs/kit` otherwise.
pub fn resolve_kit_type_import_path(file_name: &Path) -> &'static str {
    let Some(dir) = file_name.parent() else { return "@sveltejs/kit" };
    for ancestor in dir.ancestors() {
        if ancestor.file_name().is_some_and(|n| n == "node_modules") {
            continue;
        }
        let package_json = ancestor.join("node_modules/@sveltejs/kit/package.json");
        if !package_json.is_file() {
            continue;
        }
        let major = std::fs::read_to_string(&package_json)
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .and_then(|v| match &v["version"] {
                serde_json::Value::String(s) => js_parse_int(s),
                serde_json::Value::Number(n) => n.as_f64(),
                _ => None,
            });
        return if major.is_some_and(|m| m >= 3.0) { "@sveltejs/kit/hooks" } else { "@sveltejs/kit" };
    }
    "@sveltejs/kit"
}

/// `Number.parseInt(s, 10)` (`None` for NaN)
fn js_parse_int(s: &str) -> Option<f64> {
    let s = s.trim_start();
    let (neg, rest) = match s.as_bytes().first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    let n: f64 = digits.parse().ok()?;
    Some(if neg { -n } else { n })
}

/// `toVirtualPos`: an original position to a generated one
pub fn to_virtual_pos(pos: usize, added_code: &[AddedCode]) -> usize {
    let mut total = 0;
    for added in added_code {
        if pos < added.original_pos {
            break;
        }
        total += added.length;
    }
    pos + total
}

/// `toOriginalPos`: a generated position to an original one, and whether it was inside
/// inserted code (then the position is where the code was inserted)
pub fn to_original_pos(pos: usize, added_code: &[AddedCode]) -> (usize, bool) {
    let mut total = 0;
    let mut idx = 0;
    while idx < added_code.len() {
        if pos < added_code[idx].generated_pos {
            break;
        }
        total += added_code[idx].length;
        idx += 1;
    }
    if idx > 0 {
        let prev = &added_code[idx - 1];
        if pos > prev.generated_pos && pos < prev.generated_pos + prev.length {
            return (prev.original_pos, true);
        }
    }
    (pos.wrapping_sub(total), false)
}

/// `internalHelpers.upsertKitFile(ts, fileName, settings, getSource, undefined, rewrite)`.
///
/// `file_name` is the path as svelte-check passes it (absolute, `/`-separated). `rewrite` is
/// the `rewriteExternalImports` argument (its `source_path` is ignored: `file_name` is used,
/// like the JS). `kit_type_import_path` is `getKitTypeImportPath`'s answer for hooks files
/// (`@sveltejs/kit` or `@sveltejs/kit/hooks`); `None` resolves it with
/// [`resolve_kit_type_import_path`].
///
/// Returns `None` for files that aren't kit files, and in the one case where the JS throws
/// (an untyped `export let load;` without initializer in a `.ts` file). A file oxc can't parse
/// at all comes back unchanged.
pub fn upsert_kit_file(
    file_name: &str,
    text: &str,
    settings: &KitFilesSettings,
    rewrite: Option<&RewriteExternalImports>,
    kit_type_import_path: Option<&str>,
) -> Option<KitFileOutput> {
    let base = basename(file_name);
    let kind = if is_kit_route_file(base) {
        FileKind::Route
    } else if is_hooks_file(file_name, base, &settings.server_hooks_path) {
        FileKind::ServerHooks
    } else if is_hooks_file(file_name, base, &settings.client_hooks_path) {
        FileKind::ClientHooks
    } else if is_hooks_file(file_name, base, &settings.universal_hooks_path) {
        FileKind::UniversalHooks
    } else if is_params_file(file_name, base, &settings.params_path) {
        FileKind::Params
    } else {
        return None;
    };
    let is_ts_file = base.ends_with(".ts");
    let alloc = Allocator::default();
    let Some(src) = Source::parse(&alloc, text, is_ts_file) else {
        return Some(KitFileOutput { text: text.to_string(), added_code: Vec::new() });
    };

    let kit_import = || kit_type_import_path.map(str::to_string).unwrap_or_else(|| resolve_kit_type_import_path(Path::new(file_name)).to_string());
    let mut up = Upserter { is_ts_file, inserts: Vec::new(), exports: src.find_exports(is_ts_file) };
    match kind {
        FileKind::Route => up.route_file(base)?,
        FileKind::ServerHooks => {
            let k = kit_import();
            up.add_type_to_function("handleError", &format!("import('{k}').HandleServerError"), None, None);
            up.add_type_to_function("handle", &format!("import('{k}').Handle"), None, None);
            up.add_type_to_function("handleFetch", &format!("import('{k}').HandleFetch"), None, None);
        }
        FileKind::ClientHooks => {
            let k = kit_import();
            up.add_type_to_function("handleError", &format!("import('{k}').HandleClientError"), None, None);
        }
        FileKind::UniversalHooks => {
            let k = kit_import();
            up.add_type_to_function("reroute", &format!("import('{k}').Reroute"), None, None);
        }
        FileKind::Params => up.add_type_to_function("match", "string", Some("boolean"), None),
    }
    let mut inserts = up.inserts;

    if let Some(r) = rewrite {
        let options = RewriteExternalImports { source_path: file_name.into(), generated_path: r.generated_path.clone(), workspace_path: r.workspace_path.clone() };
        let mut collector = ImportCollector { src: &src, specifiers: Vec::new() };
        collector.visit_program(&src.program);
        for (pos, specifier) in collector.specifiers {
            if let Some(prefix) = inserted_prefix(&specifier, &options) {
                inserts.push((pos, prefix));
            }
        }
    }

    // `insertCode` keeps the list sorted by position, later insertions after earlier ones at
    // the same position
    inserts.sort_by_key(|(pos, _)| *pos);
    let mut out = String::with_capacity(text.len() + inserts.iter().map(|(_, s)| s.len()).sum::<usize>());
    let mut added_code = Vec::with_capacity(inserts.len());
    let mut last = 0;
    let mut total = 0;
    let mut utf16 = Utf16Index::new(text);
    for (pos, inserted) in inserts {
        out.push_str(&text[last..pos]);
        out.push_str(&inserted);
        last = pos;
        let original_pos = utf16.at(pos);
        let length = inserted.encode_utf16().count();
        added_code.push(AddedCode { generated_pos: original_pos + total, original_pos, length, total: total + length, inserted });
        total += length;
    }
    out.push_str(&text[last..]);
    Some(KitFileOutput { text: out, added_code })
}

/// `rewrite.insertedPrefix` of `getExternalImportRewrite`, computed with JS string semantics
/// (UTF-16 lengths, `slice` with a negative end)
fn inserted_prefix(specifier: &str, options: &RewriteExternalImports) -> Option<String> {
    let rewrite = external_import_rewrite(specifier, options)?;
    let cut = match (specifier.find('?'), specifier.find('#')) {
        (Some(q), Some(h)) => Some(q.min(h)),
        (q, h) => q.or(h),
    };
    let (path_part, suffix) = cut.map_or((specifier, ""), |i| specifier.split_at(i));
    let relative: Vec<u16> = rewrite.rewritten[..rewrite.rewritten.len() - suffix.len()].encode_utf16().collect();
    let end = relative.len() as isize - path_part.encode_utf16().count() as isize;
    let end = if end < 0 { (relative.len() as isize + end).max(0) as usize } else { end as usize };
    let prefix = String::from_utf16_lossy(&relative[..end]);
    (!prefix.is_empty()).then_some(prefix)
}

/// Byte offsets to UTF-16 offsets, for increasing byte offsets
struct Utf16Index<'s> {
    text: &'s str,
    byte: usize,
    utf16: usize,
}

impl<'s> Utf16Index<'s> {
    fn new(text: &'s str) -> Self {
        Utf16Index { text, byte: 0, utf16: 0 }
    }

    fn at(&mut self, byte: usize) -> usize {
        if byte < self.byte {
            self.byte = 0;
            self.utf16 = 0;
        }
        self.utf16 += self.text[self.byte..byte].chars().map(char::len_utf16).sum::<usize>();
        self.byte = byte;
        self.utf16
    }
}

#[derive(Clone, Copy)]
enum FileKind {
    Route,
    ServerHooks,
    ClientHooks,
    UniversalHooks,
    Params,
}

/// A parsed kit file
struct Source<'a> {
    text: &'a str,
    program: Program<'a>,
    /// Token spans, in order
    tokens: Vec<(u32, u32)>,
}

impl<'a> Source<'a> {
    /// `ts.createSourceFile(..., isTsFile ? ScriptKind.TS : ScriptKind.JS)`. TS's JS mode
    /// accepts JSX, and recovers from TypeScript syntax, so a `.js` file that doesn't parse as
    /// JS is tried as TS.
    fn parse(alloc: &'a Allocator, text: &'a str, is_ts_file: bool) -> Option<Self> {
        let attempt = |source_type: SourceType| {
            let ret = Parser::new(alloc, text, source_type).with_config(TokensParserConfig).parse();
            if ret.fatal_error {
                return None;
            }
            let tokens = ret.tokens.iter().map(|t| (t.start(), t.end())).filter(|(s, e)| e > s).collect();
            Some(Source { text, program: ret.program, tokens })
        };
        if is_ts_file {
            attempt(SourceType::ts().with_module(true))
        } else {
            attempt(SourceType::mjs().with_jsx(true)).or_else(|| attempt(SourceType::ts().with_module(true)))
        }
    }

    /// TS's `node.pos`: the end of the token before `start` (0 if there is none)
    fn full_start(&self, start: u32) -> usize {
        let i = self.tokens.partition_point(|&(_, e)| e <= start);
        if i == 0 { 0 } else { self.tokens[i - 1].1 as usize }
    }

    /// `getJSDocCommentRanges(node)`: the JSDoc comments TS attaches to a node starting at
    /// `start`. `with_trailing` for the kinds that also take comments on the previous
    /// token's line (parameters, function/arrow/parenthesized expressions, variable
    /// declarations, export specifiers).
    fn jsdoc_ranges(&self, start: u32, end: u32, with_trailing: bool) -> Vec<(usize, usize)> {
        let pos = self.full_start(start);
        let mut ranges = if with_trailing { comment_ranges(self.text, pos, true) } else { Vec::new() };
        ranges.extend(comment_ranges(self.text, pos, false));
        let b = self.text.as_bytes();
        ranges.retain(|&(s, e)| e <= end as usize && b.get(s + 1) == Some(&b'*') && b.get(s + 2) == Some(&b'*') && b.get(s + 3) != Some(&b'/'));
        ranges
    }

    /// The tags of the last JSDoc comment of a node (`filterOwnedJSDocTags` only keeps
    /// those)
    fn last_jsdoc_tags(&self, start: u32, end: u32, with_trailing: bool) -> Vec<JsDocTag> {
        match self.jsdoc_ranges(start, end, with_trailing).last() {
            Some(&(s, e)) => parse_jsdoc(self.text, s, e),
            None => Vec::new(),
        }
    }

    /// The first token at or after `pos` with this text
    fn find_token(&self, pos: u32, text: &str) -> Option<u32> {
        let i = self.tokens.partition_point(|&(s, _)| s < pos);
        self.tokens[i..].iter().find(|&&(s, e)| &self.text[s as usize..e as usize] == text).map(|&(s, _)| s)
    }

    /// `findExports(ts, source, isTsFile)`
    fn find_exports(&self, is_ts_file: bool) -> HashMap<String, Export> {
        let mut exports = HashMap::new();
        for stmt in &self.program.body {
            let (stmt_start, stmt_end) = (stmt.span().start, stmt.span().end);
            match stmt {
                // `export function x`, `export default function x` (the first modifier is `export`)
                Statement::ExportDeclaration(e) => match &e.declaration {
                    Declaration::FunctionDeclaration(f) => {
                        if let Some(id) = &f.id {
                            let mut func = self.func_info(FuncNode::Function(f), stmt_start);
                            let tags = if is_ts_file { Vec::new() } else { self.last_jsdoc_tags(stmt_start, stmt_end, false) };
                            func.typed = self.has_typed_parameter(&func, &tags, is_ts_file);
                            exports.insert(id.name.to_string(), Export::Function(func));
                        }
                    }
                    Declaration::VariableDeclaration(v) if v.declarations.len() == 1 => {
                        self.var_export(&v.declarations[0], stmt_start, stmt_end, is_ts_file, &mut exports);
                    }
                    _ => {}
                },
                Statement::ExportDefaultDeclaration(e) => {
                    if let ExportDefaultDeclarationKind::FunctionDeclaration(f) = &e.declaration
                        && let Some(id) = &f.id
                    {
                        let mut func = self.func_info(FuncNode::Function(f), stmt_start);
                        let tags = if is_ts_file { Vec::new() } else { self.last_jsdoc_tags(stmt_start, stmt_end, false) };
                        func.typed = self.has_typed_parameter(&func, &tags, is_ts_file);
                        exports.insert(id.name.to_string(), Export::Function(func));
                    }
                }
                _ => {}
            }
        }
        exports
    }

    /// The `export const x = ...` part of `findExports`
    fn var_export(&self, decl: &VariableDeclarator<'a>, stmt_start: u32, stmt_end: u32, is_ts_file: bool, exports: &mut HashMap<String, Export>) {
        let init = decl.init.as_ref();
        let is_satisfies = matches!(init, Some(Expression::TSSatisfiesExpression(_)));
        let mut has_type_definition = decl.type_annotation.is_some() || is_satisfies;
        if !is_ts_file && !has_type_definition {
            // `getJSDocType(declaration)` / `getJSDocTags(declaration)`: the initializer's
            // JSDoc (where a parenthesized expression's `@type`/`@satisfies` belong to it,
            // not the declaration), the declaration's, the statement's
            let mut tags = Vec::new();
            if let Some(init) = init {
                let (s, e) = (init.span().start, init.span().end);
                match init {
                    Expression::ParenthesizedExpression(_) => {
                        tags.extend(self.last_jsdoc_tags(s, e, true).into_iter().filter(|t| t.name != "type" && t.name != "satisfies"))
                    }
                    Expression::ArrowFunctionExpression(_) | Expression::FunctionExpression(_) => tags.extend(self.last_jsdoc_tags(s, e, true)),
                    Expression::ClassExpression(_) => tags.extend(self.last_jsdoc_tags(s, e, false)),
                    _ => {}
                }
            }
            tags.extend(self.last_jsdoc_tags(decl.span.start, decl.span.end, true));
            tags.extend(self.last_jsdoc_tags(stmt_start, stmt_end, false));
            has_type_definition = tags.iter().any(|t| t.name == "type" || t.name == "satisfies");
        }

        let name = &self.text[decl.id.span().start as usize..decl.id.span().end as usize];
        // `export const x = function/arrow`, `= (function/arrow)`, `= (function/arrow) satisfies T`
        let func = init.and_then(|init| {
            let (inner, direct) = match init {
                Expression::TSSatisfiesExpression(s) => (unwrap_paren(&s.expression)?, false),
                Expression::ParenthesizedExpression(p) => (&p.expression, false),
                e => (e, true),
            };
            match inner {
                Expression::FunctionExpression(f) => Some((FuncNode::Function(f), direct)),
                Expression::ArrowFunctionExpression(a) => Some((FuncNode::Arrow(a), direct)),
                _ => None,
            }
        });
        if let Some((node, direct)) = func {
            let mut func = self.func_info(node, node.span().start);
            let tags = if is_ts_file {
                Vec::new()
            } else {
                // the function's own JSDoc, and the statement's if it's the initializer itself
                let mut tags = self.last_jsdoc_tags(node.span().start, node.span().end, true);
                if direct {
                    tags.extend(self.last_jsdoc_tags(stmt_start, stmt_end, false));
                }
                tags
            };
            func.typed = has_type_definition || self.has_typed_parameter(&func, &tags, is_ts_file);
            exports.insert(name.to_string(), Export::Function(func));
        } else if let BindingPattern::BindingIdentifier(id) = &decl.id {
            exports.insert(
                name.to_string(),
                Export::Var(VarInfo {
                    name_end: id.span.end,
                    init: init.map(|i| (i.span().start, i.span().end)),
                    typed: has_type_definition,
                }),
            );
        }
    }

    fn func_info(&self, node: FuncNode<'_, 'a>, start: u32) -> FuncInfo {
        let (this_param, params, return_type, body_start, is_async) = match node {
            FuncNode::Function(f) => (f.this_param.as_deref(), &*f.params, f.return_type.is_some(), f.body.as_ref().map(|b| b.span.start), f.r#async),
            FuncNode::Arrow(a) => (None, &*a.params, a.return_type.is_some(), Some(a.body.span().start), a.r#async),
        };
        let first_param = if let Some(t) = this_param {
            Some(ParamInfo { end: t.span.end, has_type: t.type_annotation.is_some(), name: Some("this".to_string()) })
        } else if let Some(p) = params.items.first() {
            Some(ParamInfo { end: p.span.end, has_type: p.type_annotation.is_some(), name: binding_name(&p.pattern) })
        } else {
            params.rest.as_ref().map(|r| ParamInfo { end: r.span.end, has_type: r.type_annotation.is_some(), name: binding_name(&r.rest.argument) })
        };
        let param_count = this_param.is_some() as usize + params.items.len() + params.rest.is_some() as usize;
        let return_pos = match node {
            FuncNode::Function(_) => body_start,
            FuncNode::Arrow(a) => {
                let after = a.return_type.as_ref().map_or(a.params.span.end, |t| t.span.end.max(a.params.span.end));
                self.find_token(after, "=>")
            }
        };
        FuncInfo { start, param_count, first_param, has_return_type: return_type, has_body: body_start.is_some(), is_async, return_pos, typed: false }
    }

    /// `hasTypedParameter`
    fn has_typed_parameter(&self, func: &FuncInfo, tags: &[JsDocTag], is_ts_file: bool) -> bool {
        if func.first_param.as_ref().is_some_and(|p| p.has_type) {
            return true;
        }
        if is_ts_file {
            return false;
        }
        if tags.iter().any(|t| t.name == "type") {
            return true;
        }
        // `getJSDocParameterTags(parameters[0])`: `@param`s with its name, or any `@param` for
        // a destructured first parameter
        func.first_param.as_ref().is_some_and(|p| {
            tags.iter().filter_map(|t| t.param.as_ref()).any(|name| match &p.name {
                Some(n) => name.as_deref() == Some(n.as_str()),
                None => true,
            })
        })
    }
}

fn unwrap_paren<'r, 'a>(e: &'r Expression<'a>) -> Option<&'r Expression<'a>> {
    match e {
        Expression::ParenthesizedExpression(p) => Some(&p.expression),
        _ => None,
    }
}

/// `ts.isIdentifier(parameter.name) ? parameter.name.text : undefined`
fn binding_name(p: &BindingPattern) -> Option<String> {
    match p {
        BindingPattern::BindingIdentifier(id) => Some(id.name.to_string()),
        _ => None,
    }
}

#[derive(Clone, Copy)]
enum FuncNode<'r, 'a> {
    Function(&'r Function<'a>),
    Arrow(&'r ArrowFunctionExpression<'a>),
}

impl GetSpan for FuncNode<'_, '_> {
    fn span(&self) -> oxc_span::Span {
        match self {
            FuncNode::Function(f) => f.span,
            FuncNode::Arrow(a) => a.span,
        }
    }
}

struct ParamInfo {
    /// `parameters[0].getEnd()`
    end: u32,
    has_type: bool,
    /// The name if it's an identifier
    name: Option<String>,
}

struct FuncInfo {
    /// `node.getStart()`
    start: u32,
    /// `node.parameters.length` (counting a `this` parameter, like TS)
    param_count: usize,
    first_param: Option<ParamInfo>,
    has_return_type: bool,
    has_body: bool,
    is_async: bool,
    /// The `=>` of an arrow function, the body of others
    return_pos: Option<u32>,
    /// `hasTypeDefinition`
    typed: bool,
}

struct VarInfo {
    /// `node.name.getEnd()`
    name_end: u32,
    /// The initializer's `getStart()` and `getEnd()`
    init: Option<(u32, u32)>,
    /// `hasTypeDefinition`
    typed: bool,
}

enum Export {
    Function(FuncInfo),
    Var(VarInfo),
}

struct Upserter {
    is_ts_file: bool,
    exports: HashMap<String, Export>,
    /// (byte position, inserted text), in insertion order
    inserts: Vec<(usize, String)>,
}

impl Upserter {
    fn insert(&mut self, pos: u32, text: String) {
        self.inserts.push((pos as usize, text));
    }

    /// `upsertKitRouteFile` (`None` where the JS throws)
    fn route_file(&mut self, base: &str) -> Option<()> {
        let load_type = format!(
            "import('./$types.js').{}{}Load",
            if base.contains("layout") { "Layout" } else { "Page" },
            if base.contains("server") { "Server" } else { "" }
        );
        match self.exports.get("load") {
            Some(Export::Function(f)) if f.param_count == 1 && !f.typed => {
                let (start, param) = (f.start, f.first_param.as_ref().map(|p| (p.end, p.name.clone())));
                let (end, name) = param?;
                if self.is_ts_file {
                    self.insert(end, format!(": {load_type}Event"));
                } else {
                    let name = name.unwrap_or_else(|| "arg0".into());
                    self.insert(start, format!("/** @param {{{load_type}Event}} {name} */ "));
                }
            }
            Some(Export::Var(v)) if !v.typed => {
                let init = v.init;
                if self.is_ts_file {
                    // `load.node.initializer.getStart()` throws without an initializer
                    let (s, e) = init?;
                    self.insert(s, "(".into());
                    self.insert(e, format!(") satisfies {load_type}"));
                } else if let Some((s, e)) = init {
                    self.insert(s, format!("/** @satisfies {{{load_type}}} */ ("));
                    self.insert(e, ")".into());
                }
            }
            _ => {}
        }

        if let Some(Export::Function(f)) = self.exports.get("entries")
            && f.param_count == 0
            && !f.typed
            && !base.contains("layout")
        {
            let ty = "import('./$types.js').EntryGenerator";
            if self.is_ts_file && !f.has_return_type && f.has_body {
                if let Some(pos) = f.return_pos {
                    self.insert(pos, format!(": ReturnType<{ty}> "));
                }
            } else if !self.is_ts_file {
                let start = f.start;
                self.insert(start, format!("/** @type {{{ty}}} */ "));
            }
        }

        if let Some(Export::Var(v)) = self.exports.get("actions")
            && !v.typed
            && let Some((s, e)) = v.init
        {
            if self.is_ts_file {
                self.insert(e, " satisfies import('./$types.js').Actions".into());
            } else {
                self.insert(s, "/** @satisfies {import('./$types.js').Actions} */ (".into());
                self.insert(e, ")".into());
            }
        }

        self.add_type_to_variable("prerender", "boolean | 'auto'");
        self.add_type_to_variable("trailingSlash", "'never' | 'always' | 'ignore'");
        self.add_type_to_variable("ssr", "boolean");
        self.add_type_to_variable("csr", "boolean");

        for method in ["GET", "PUT", "POST", "PATCH", "DELETE", "OPTIONS", "HEAD", "fallback"] {
            self.add_type_to_function(method, "import('./$types.js').RequestEvent", Some("Response | Promise<Response>"), Some("Promise<Response>"));
        }
        Some(())
    }

    /// `addTypeToVariable`
    fn add_type_to_variable(&mut self, name: &str, ty: &str) {
        let Some(Export::Var(v)) = self.exports.get(name) else { return };
        let (Some((s, e)), false) = (v.init, v.typed) else { return };
        if self.is_ts_file {
            let end = v.name_end;
            self.insert(end, format!(" : {ty}"));
        } else {
            self.insert(s, format!("/** @type {{{ty}}} */ ("));
            self.insert(e, ")".into());
        }
    }

    /// `addTypeToFunction`
    fn add_type_to_function(&mut self, name: &str, ty: &str, return_type: Option<&str>, async_return_type: Option<&str>) {
        let Some(Export::Function(f)) = self.exports.get(name) else { return };
        if f.param_count != 1 || f.typed {
            return;
        }
        if self.is_ts_file {
            let Some(param_end) = f.first_param.as_ref().map(|p| p.end) else { return };
            let (has_return_type, has_body, is_async, return_pos) = (f.has_return_type, f.has_body, f.is_async, f.return_pos);
            self.insert(param_end, if return_type.is_none() { format!(": Parameters<{ty}>[0]") } else { format!(": {ty}") });
            if !has_return_type && has_body {
                let effective = if is_async && async_return_type.is_some() { async_return_type } else { return_type };
                if let Some(pos) = return_pos {
                    self.insert(pos, match effective {
                        None => format!(": ReturnType<{ty}> "),
                        Some(r) => format!(": {r} "),
                    });
                }
            }
        } else {
            let start = f.start;
            let jsdoc_type = match return_type {
                None => ty.to_string(),
                Some(r) => format!("(arg0: {ty}) => {r}"),
            };
            self.insert(start, format!("/** @type {{{jsdoc_type}}} */ "));
        }
    }
}

/// Finds the module specifiers `applyExternalImportRewritesToAddedCode` looks at
/// (`forEachExternalImportRewrite`): import/export declarations, `import('...')` and
/// `require('...')` calls, import types, and import types in the JSDoc TS attaches to nodes.
/// Collects (position after the opening quote, specifier text).
struct ImportCollector<'s, 'a> {
    src: &'s Source<'a>,
    specifiers: Vec<(usize, String)>,
}

impl ImportCollector<'_, '_> {
    fn string_literal(&mut self, s: &StringLiteral) {
        self.specifiers.push((s.span.start as usize + 1, s.value.to_string()));
    }

    /// `ts.isStringLiteralLike`: a string or a template without substitutions
    fn string_like(&mut self, e: &Expression) {
        match e {
            Expression::StringLiteral(s) => self.string_literal(s),
            Expression::TemplateLiteral(t) if t.expressions.is_empty() => {
                if let Some(q) = t.quasis.first() {
                    let value = q.value.cooked.as_ref().map_or_else(|| q.value.raw.to_string(), |c| c.to_string());
                    self.specifiers.push((t.span.start as usize + 1, value));
                }
            }
            _ => {}
        }
    }

    /// A JSDoc host node: the import types in all its JSDoc comments
    fn host(&mut self, span: oxc_span::Span, with_trailing: bool) {
        for (s, e) in self.src.jsdoc_ranges(span.start, span.end, with_trailing) {
            for tag in parse_jsdoc(self.src.text, s, e) {
                self.specifiers.extend(tag.imports);
            }
        }
    }
}

impl<'a> Visit<'a> for ImportCollector<'_, 'a> {
    fn visit_program(&mut self, it: &Program<'a>) {
        walk::walk_program(self, it);
        // the end-of-file token takes the JSDoc after the last statement
        let end = self.src.text.len() as u32;
        self.host(oxc_span::Span::new(end, end), false);
    }

    fn visit_statement(&mut self, it: &Statement<'a>) {
        // blocks are hosts wherever they appear (see `visit_block_statement`), and an
        // expression statement starting with `(` gets no JSDoc (its expression does)
        let paren_statement = matches!(it, Statement::ExpressionStatement(_)) && self.src.text.as_bytes().get(it.span().start as usize) == Some(&b'(');
        if !matches!(it, Statement::BlockStatement(_)) && !paren_statement {
            self.host(it.span(), false);
        }
        walk::walk_statement(self, it);
    }

    fn visit_directive(&mut self, it: &Directive<'a>) {
        self.host(it.span, false);
        walk::walk_directive(self, it);
    }

    fn visit_block_statement(&mut self, it: &BlockStatement<'a>) {
        self.host(it.span, false);
        walk::walk_block_statement(self, it);
    }

    fn visit_function_body(&mut self, it: &FunctionBody<'a>) {
        self.host(it.span, false);
        walk::walk_function_body(self, it);
    }

    fn visit_variable_declarator(&mut self, it: &VariableDeclarator<'a>) {
        self.host(it.span, true);
        walk::walk_variable_declarator(self, it);
    }

    fn visit_formal_parameter(&mut self, it: &FormalParameter<'a>) {
        self.host(it.span, true);
        walk::walk_formal_parameter(self, it);
    }

    fn visit_formal_parameter_rest(&mut self, it: &FormalParameterRest<'a>) {
        self.host(it.span, true);
        walk::walk_formal_parameter_rest(self, it);
    }

    fn visit_ts_this_parameter(&mut self, it: &TSThisParameter<'a>) {
        self.host(it.span, true);
        walk::walk_ts_this_parameter(self, it);
    }

    fn visit_expression(&mut self, it: &Expression<'a>) {
        match it {
            Expression::ArrowFunctionExpression(_) | Expression::FunctionExpression(_) | Expression::ParenthesizedExpression(_) => self.host(it.span(), true),
            Expression::ClassExpression(_) => self.host(it.span(), false),
            _ => {}
        }
        walk::walk_expression(self, it);
    }

    fn visit_object_property_kind(&mut self, it: &ObjectPropertyKind<'a>) {
        self.host(it.span(), false);
        walk::walk_object_property_kind(self, it);
    }

    fn visit_object_property(&mut self, it: &ObjectProperty<'a>) {
        // a method or accessor is one node in TS, not a property with a function expression
        if (it.method || it.kind != PropertyKind::Init)
            && let Expression::FunctionExpression(f) = &it.value
        {
            self.visit_property_key(&it.key);
            self.visit_function(f, ScopeFlags::Function);
            return;
        }
        walk::walk_object_property(self, it);
    }

    fn visit_class_element(&mut self, it: &ClassElement<'a>) {
        self.host(it.span(), false);
        walk::walk_class_element(self, it);
    }

    fn visit_ts_signature(&mut self, it: &TSSignature<'a>) {
        self.host(it.span(), false);
        walk::walk_ts_signature(self, it);
    }

    fn visit_ts_enum_member(&mut self, it: &TSEnumMember<'a>) {
        self.host(it.span, false);
        walk::walk_ts_enum_member(self, it);
    }

    fn visit_ts_named_tuple_member(&mut self, it: &TSNamedTupleMember<'a>) {
        self.host(it.span, false);
        walk::walk_ts_named_tuple_member(self, it);
    }

    fn visit_ts_function_type(&mut self, it: &TSFunctionType<'a>) {
        self.host(it.span, false);
        walk::walk_ts_function_type(self, it);
    }

    fn visit_ts_constructor_type(&mut self, it: &TSConstructorType<'a>) {
        self.host(it.span, false);
        walk::walk_ts_constructor_type(self, it);
    }

    fn visit_switch_case(&mut self, it: &SwitchCase<'a>) {
        self.host(it.span, false);
        walk::walk_switch_case(self, it);
    }

    fn visit_export_specifier(&mut self, it: &ExportSpecifier<'a>) {
        self.host(it.span, true);
        walk::walk_export_specifier(self, it);
    }

    fn visit_import_declaration(&mut self, it: &ImportDeclaration<'a>) {
        self.string_literal(&it.source);
        walk::walk_import_declaration(self, it);
    }

    fn visit_export_from_declaration(&mut self, it: &ExportFromDeclaration<'a>) {
        self.string_literal(&it.source);
        walk::walk_export_from_declaration(self, it);
    }

    fn visit_export_all_declaration(&mut self, it: &ExportAllDeclaration<'a>) {
        self.string_literal(&it.source);
        walk::walk_export_all_declaration(self, it);
    }

    fn visit_import_expression(&mut self, it: &ImportExpression<'a>) {
        self.string_like(&it.source);
        walk::walk_import_expression(self, it);
    }

    fn visit_call_expression(&mut self, it: &CallExpression<'a>) {
        if let Expression::Identifier(id) = &it.callee
            && id.name == "require"
            && let Some(arg) = it.arguments.first()
            && let Some(e) = arg.as_expression()
        {
            self.string_like(e);
        }
        walk::walk_call_expression(self, it);
    }

    fn visit_ts_import_type(&mut self, it: &TSImportType<'a>) {
        self.string_literal(&it.source);
        walk::walk_ts_import_type(self, it);
    }
}

fn is_line_break(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

/// TS's `isWhiteSpaceSingleLine`
fn is_white_space_single_line(c: char) -> bool {
    matches!(
        c,
        ' ' | '\t' | '\u{0B}' | '\u{0C}' | '\u{85}' | '\u{A0}' | '\u{1680}' | '\u{2000}'..='\u{200B}' | '\u{202F}' | '\u{205F}' | '\u{3000}' | '\u{FEFF}'
    )
}

/// TS's `getLeadingCommentRanges(text, pos)` / `getTrailingCommentRanges(text, pos)`
/// (`iterateCommentRanges`), as (pos, end) pairs
fn comment_ranges(text: &str, mut pos: usize, trailing: bool) -> Vec<(usize, usize)> {
    let b = text.as_bytes();
    let mut collecting = trailing || pos == 0;
    if pos == 0 {
        collecting = true;
        if text.starts_with("#!") {
            pos = text.find(['\n', '\r', '\u{2028}', '\u{2029}']).unwrap_or(text.len());
        }
    }
    let mut out = Vec::new();
    while pos < b.len() {
        let c = text[pos..].chars().next().unwrap();
        match c {
            '\r' | '\n' => {
                if c == '\r' && b.get(pos + 1) == Some(&b'\n') {
                    pos += 1;
                }
                pos += 1;
                if trailing {
                    break;
                }
                collecting = true;
            }
            '\t' | '\u{0B}' | '\u{0C}' | ' ' => pos += 1,
            '/' if matches!(b.get(pos + 1), Some(b'/' | b'*')) => {
                let start = pos;
                if b[pos + 1] == b'/' {
                    pos += 2;
                    while pos < b.len() {
                        let ch = text[pos..].chars().next().unwrap();
                        if is_line_break(ch) {
                            break;
                        }
                        pos += ch.len_utf8();
                    }
                } else {
                    pos += 2;
                    while pos < b.len() {
                        if b[pos] == b'*' && b.get(pos + 1) == Some(&b'/') {
                            pos += 2;
                            break;
                        }
                        pos += 1;
                    }
                    pos = pos.min(b.len());
                }
                if collecting {
                    out.push((start, pos));
                }
            }
            c if (c as u32) > 0x7f && (is_white_space_single_line(c) || is_line_break(c)) => pos += c.len_utf8(),
            _ => break,
        }
    }
    out
}

/// What the port needs of a JSDoc tag
#[derive(Debug)]
struct JsDocTag {
    name: String,
    /// For a top-level `@param`/`@arg`/`@argument`: its name if it's an identifier
    param: Option<Option<String>>,
    /// The import types in the tag's type expressions: (position after the quote, specifier)
    imports: Vec<(usize, String)>,
}

fn is_ident_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_' || c == b'$' || c >= 0x80
}

fn is_ident_part(c: u8) -> bool {
    is_ident_start(c) || c.is_ascii_digit()
}

/// A JSDoc identifier (which may contain `-`) at `i`: its end
fn jsdoc_ident(b: &[u8], i: usize, end: usize) -> usize {
    if i >= end || !is_ident_start(b[i]) {
        return i;
    }
    let mut j = i + 1;
    while j < end && (is_ident_part(b[j]) || b[j] == b'-') {
        j += 1;
    }
    j
}

/// Parses the JSDoc comment `text[start..end]` (`/** ... */`) like TS's
/// `parseJSDocCommentWorker`, as far as tags, `@param` names and import types go
fn parse_jsdoc(text: &str, start: usize, end: usize) -> Vec<JsDocTag> {
    let b = text.as_bytes();
    let (s, e) = (start + 3, end.saturating_sub(2).max(start + 3));
    let mut tags = Vec::new();
    let mut i = s;
    // only whitespace (and the one leading `*`) so far on this line
    let mut line_start = true;
    let mut seen_star = true;
    let mut in_tag = false;
    let mut backticks = false;
    // after `@callback`/`@overload`: following `@param`s belong to its signature
    let mut in_signature = false;
    while i < e {
        let c = b[i];
        if c == b'\n' || c == b'\r' {
            line_start = true;
            seen_star = false;
            backticks = false;
            i += 1;
            continue;
        }
        if line_start {
            if c == b' ' || c == b'\t' || c == 0x0B || c == 0x0C {
                i += 1;
                continue;
            }
            if c == b'*' && !seen_star {
                seen_star = true;
                i += 1;
                continue;
            }
            line_start = false;
            if c == b'@' {
                i = parse_tag(text, i, e, &mut tags, &mut in_signature);
                in_tag = true;
                backticks = false;
                line_start = true;
                seen_star = true;
                continue;
            }
        }
        if in_tag && c == b'`' {
            backticks = !backticks;
            i += 1;
            continue;
        }
        if backticks {
            i += 1;
            continue;
        }
        if c == b'{' {
            i = link_end(b, i, e).unwrap_or(i + 1);
            continue;
        }
        if c == b'@' && i > 0 && matches!(b[i - 1], b' ' | b'\t' | 0x0B | 0x0C) && !(i + 1 < e && matches!(b[i + 1], b' ' | b'\t' | b'\n' | b'\r' | 0x0B | 0x0C)) {
            i = parse_tag(text, i, e, &mut tags, &mut in_signature);
            in_tag = true;
            backticks = false;
            line_start = true;
            seen_star = true;
            continue;
        }
        i += 1;
    }
    tags
}

/// `{@link ...}` (`{@linkcode`, `{@linkplain`) at `i`: where it ends (after `}`, or at the
/// line break)
fn link_end(b: &[u8], i: usize, e: usize) -> Option<usize> {
    if b.get(i + 1) != Some(&b'@') {
        return None;
    }
    let name_end = jsdoc_ident(b, i + 2, e);
    if !matches!(&b[i + 2..name_end], b"link" | b"linkcode" | b"linkplain") {
        return None;
    }
    let mut j = name_end;
    while j < e && b[j] != b'}' && b[j] != b'\n' && b[j] != b'\r' {
        j += 1;
    }
    Some(if j < e && b[j] == b'}' { j + 1 } else { j })
}

/// `skipWhitespaceOrAsterisk`: whitespace, line breaks, and the `*` starting a line
fn skip_ws_or_asterisk(b: &[u8], mut i: usize, e: usize) -> usize {
    let mut after_line_break = false;
    while i < e {
        match b[i] {
            b' ' | b'\t' | 0x0B | 0x0C => {}
            b'\n' | b'\r' => after_line_break = true,
            b'*' if after_line_break => after_line_break = false,
            _ => break,
        }
        i += 1;
    }
    i
}

/// A `{...}` type expression at `i`: its end, and the import types in it
fn type_expression(text: &str, i: usize, e: usize, imports: &mut Vec<(usize, String)>) -> usize {
    let b = text.as_bytes();
    let mut depth = 0;
    let mut j = i;
    while j < e {
        match b[j] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    j += 1;
                    break;
                }
            }
            q @ (b'\'' | b'"' | b'`') => {
                j += 1;
                while j < e && b[j] != q {
                    if b[j] == b'\\' {
                        j += 1;
                    }
                    j += 1;
                }
            }
            _ => {}
        }
        j += 1;
    }
    let j = j.min(e);
    find_import_types(text, i, j, imports);
    j
}

/// `import('...')` types in `text[from..to]`
fn find_import_types(text: &str, from: usize, to: usize, imports: &mut Vec<(usize, String)>) {
    let b = text.as_bytes();
    let mut j = from;
    while j < to {
        let c = b[j];
        if c == b'\'' || c == b'"' {
            // a string literal type
            j += 1;
            while j < to && b[j] != c {
                if b[j] == b'\\' {
                    j += 1;
                }
                j += 1;
            }
            j += 1;
            continue;
        }
        if c == b'i' && text[j..to].starts_with("import") && (j == 0 || !(is_ident_part(b[j - 1]) || b[j - 1] == b'.')) && !(j + 6 < to && is_ident_part(b[j + 6])) {
            let mut k = skip_ws_or_asterisk(b, j + 6, to);
            if k < to && b[k] == b'(' {
                k = skip_ws_or_asterisk(b, k + 1, to);
                if k < to && matches!(b[k], b'\'' | b'"' | b'`') {
                    let quote = b[k];
                    let mut value = String::new();
                    let mut m = k + 1;
                    let mut ok = false;
                    while m < to {
                        let ch = text[m..].chars().next().unwrap();
                        if ch as u32 == quote as u32 {
                            ok = true;
                            break;
                        }
                        if quote == b'`' && text[m..].starts_with("${") {
                            break;
                        }
                        if ch == '\\' && m + 1 < to {
                            let next = text[m + 1..].chars().next().unwrap();
                            value.push(match next {
                                'n' => '\n',
                                't' => '\t',
                                'r' => '\r',
                                c => c,
                            });
                            m += 1 + next.len_utf8();
                            continue;
                        }
                        value.push(ch);
                        m += ch.len_utf8();
                    }
                    if ok {
                        imports.push((k + 1, value));
                    }
                    j = m + 1;
                    continue;
                }
            }
            j += 6;
            continue;
        }
        j += 1;
    }
}

/// `parseTag` at the `@` at `i`: pushes the tag, returns where the tag's comment starts
fn parse_tag(text: &str, i: usize, e: usize, tags: &mut Vec<JsDocTag>, in_signature: &mut bool) -> usize {
    let b = text.as_bytes();
    let name_end = jsdoc_ident(b, i + 1, e);
    let name = text[i + 1..name_end].to_string();
    let mut j = skip_ws_or_asterisk(b, name_end, e);
    let mut tag = JsDocTag { name, param: None, imports: Vec::new() };
    match tag.name.as_str() {
        "type" | "this" | "enum" | "satisfies" => {
            if j < e && b[j] == b'{' {
                j = type_expression(text, j, e, &mut tag.imports);
            } else {
                // a type without braces, up to the end of the line
                let line_end = text[j..e].find(['\n', '\r']).map_or(e, |k| j + k);
                find_import_types(text, j, line_end, &mut tag.imports);
            }
        }
        "param" | "arg" | "argument" | "prop" | "property" => {
            let mut has_type = false;
            if j < e && b[j] == b'{' {
                j = type_expression(text, j, e, &mut tag.imports);
                has_type = true;
                j = skip_ws_or_asterisk(b, j, e);
            }
            // `parseBracketNameInPropertyAndParamTag`
            let bracketed = j < e && b[j] == b'[';
            if bracketed {
                j = skip_ws_or_asterisk(b, j + 1, e);
            }
            let backquoted = j < e && b[j] == b'`';
            if backquoted {
                j += 1;
            }
            let id_end = jsdoc_ident(b, j, e);
            let mut param_name = Some(text[j..id_end].to_string());
            j = id_end;
            if b.get(j) == Some(&b'[') && b.get(j + 1) == Some(&b']') {
                j += 2;
            }
            while j < e && b[j] == b'.' {
                param_name = None;
                j = jsdoc_ident(b, j + 1, e);
                if b.get(j) == Some(&b'[') && b.get(j + 1) == Some(&b']') {
                    j += 2;
                }
            }
            if backquoted && j < e && b[j] == b'`' {
                j += 1;
            }
            if bracketed {
                while j < e && b[j] != b']' && b[j] != b'\n' {
                    j += 1;
                }
                if j < e && b[j] == b']' {
                    j += 1;
                }
            }
            j = skip_ws_or_asterisk(b, j, e);
            if !has_type && j < e && b[j] == b'{' && link_end(b, j, e).is_none() {
                j = type_expression(text, j, e, &mut tag.imports);
            }
            if matches!(tag.name.as_str(), "param" | "arg" | "argument") && !*in_signature {
                tag.param = Some(param_name);
            }
        }
        "returns" | "return" | "throws" | "exception" | "typedef" | "template" => {
            if j < e && b[j] == b'{' {
                j = type_expression(text, j, e, &mut tag.imports);
            }
        }
        _ => {}
    }
    *in_signature = match tag.name.as_str() {
        "callback" | "overload" => true,
        "param" | "arg" | "argument" | "returns" | "return" => *in_signature,
        _ => false,
    };
    tags.push(tag);
    j
}

#[cfg(test)]
mod tests {
    use super::*;

    fn upsert(file: &str, source: &str) -> String {
        let settings = KitFilesSettings {
            client_hooks_path: "hooks.client".into(),
            params_path: "params".into(),
            server_hooks_path: "hooks.server".into(),
            universal_hooks_path: "hooks".into(),
        };
        upsert_kit_file(file, source, &settings, None, Some("@sveltejs/kit")).map(|r| r.text).unwrap_or_default()
    }

    #[test]
    fn helper_tests() {
        assert_eq!(upsert("+page.ts", "export function load(e) { return e; }"), "export function load(e: import('./$types.js').PageLoadEvent) { return e; }");
        assert_eq!(
            upsert("+page.js", "export function load(e) { return e; }"),
            "/** @param {import('./$types.js').PageLoadEvent} e */ export function load(e) { return e; }"
        );
        let typed = "/** @type {import('./$types.js').PageLoad} */ export function load(e) { return e; }";
        assert_eq!(upsert("+page.js", typed), typed);
        assert_eq!(
            upsert("hooks.server.ts", "export const handle = async ({ event, resolve }) => {};"),
            "export const handle = async ({ event, resolve }: Parameters<import('@sveltejs/kit').Handle>[0]) : ReturnType<import('@sveltejs/kit').Handle> => {};"
        );
        assert_eq!(
            upsert("+server.ts", "export async function GET(e) {}"),
            "export async function GET(e: import('./$types.js').RequestEvent) : Promise<Response> {}"
        );
        assert_eq!(
            upsert("+page.js", "export const load = (async (e) => {});"),
            "export const load = (/** @param {import('./$types.js').PageLoadEvent} e */ async (e) => {});"
        );
        assert_eq!(upsert("+layout@foo.js", "export const ssr = true;"), "export const ssr = /** @type {boolean} */ (true);");
    }

    #[test]
    fn positions() {
        let r = upsert_kit_file("/a/src/routes/+page.ts", "export const ssr = 'é' && true;", &KitFilesSettings::default(), None, None).unwrap();
        assert_eq!(r.added_code.len(), 1);
        assert_eq!(r.added_code[0].original_pos, 16);
        assert_eq!(to_original_pos(17, &r.added_code), (16, true));
        assert_eq!(to_original_pos(30, &r.added_code), (30 - r.added_code[0].length, false));
        assert_eq!(to_virtual_pos(20, &r.added_code), 20 + r.added_code[0].length);
    }

    #[test]
    fn kit_files() {
        let s = KitFilesSettings::default();
        assert!(is_kit_file("/w/src/routes/+page.server.ts", &s));
        assert!(is_kit_file("/w/src/routes/+page@.js", &s));
        assert!(is_kit_file("/w/src/hooks.server.ts", &s));
        assert!(is_kit_file("/w/src/hooks/index.js", &s));
        assert!(is_kit_file("/w/src/params/slug.ts", &s));
        assert!(!is_kit_file("/w/src/params/slug.test.ts", &s));
        assert!(!is_kit_file("/w/src/routes/+page.svelte.ts", &s));
        assert!(!is_kit_file("/w/src/lib/utils.ts", &s));
    }
}
