//! svelte-check's `css` diagnostics source: svelte-language-server's CSS plugin
//! (`CSSPlugin.getDiagnostics`), which runs vscode-css-languageservice's `doValidation` (its
//! CSS/SCSS/LESS parser plus the default lint rules) on a component's `<style>` tag and maps the
//! ranges back to the `.svelte` file.
//!
//! Everything works on UTF-16 code units like the JS original, so offsets, `toLowerCase` and
//! escapes behave the same.

pub mod data;
mod html;
mod less;
mod lint;
mod nodes;
mod parser;
mod scanner;
mod scss;

/// The parser/scanner flavour
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dialect {
    Css,
    Scss,
    Less,
}

/// An LSP position: 0-based line and UTF-16 character
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Position {
    pub line: u32,
    pub character: u32,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Range {
    pub start: Position,
    pub end: Position,
}

/// LSP `DiagnosticSeverity`
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Severity {
    Error = 1,
    Warning = 2,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CssDiagnostic {
    /// in the `.svelte` file
    pub range: Range,
    pub severity: Severity,
    pub message: String,
    /// e.g. `unknownAtRules`, `css-rcurlyexpected`
    pub code: String,
    /// `css`, `scss` or `less`
    pub source: String,
}

/// `getLanguage(kind)` in svelte-language-server's `plugins/css/service.ts`
fn get_language(kind: &str) -> Dialect {
    match kind {
        "scss" | "text/scss" => Dialect::Scss,
        "less" | "text/less" => Dialect::Less,
        _ => Dialect::Css,
    }
}

fn dialect_name(d: Dialect) -> &'static str {
    match d {
        Dialect::Css => "css",
        Dialect::Scss => "scss",
        Dialect::Less => "less",
    }
}

/// The diagnostics svelte-check reports with source `css`/`scss`/`less` for a component.
/// `svelte_source` is the file's text as read (a BOM, if any, counts as a character).
pub fn style_diagnostics(svelte_source: &str) -> Vec<CssDiagnostic> {
    // a root `<style>` element needs `<style` in the source
    if !svelte_source.contains("<style") {
        return Vec::new();
    }
    let text: Vec<u16> = if svelte_source.is_ascii() {
        svelte_source.bytes().map(u16::from).collect()
    } else {
        svelte_source.encode_utf16().collect()
    };
    style_diagnostics_utf16(&text)
}

/// [`style_diagnostics`] for a UTF-16 source
pub fn style_diagnostics_utf16(text: &[u16]) -> Vec<CssDiagnostic> {
    let Some(style) = html::extract_style_tag(text) else { return Vec::new() };
    // `extractLanguage`: the `lang`/`type` attribute without a leading `text/`
    let kind = style.lang.strip_prefix("text/").unwrap_or(&style.lang);
    // `shouldExcludeValidation`
    if matches!(kind, "postcss" | "sass" | "stylus" | "styl") {
        return Vec::new();
    }
    // the stylesheet is parsed by the service for `languageId`, validated by the one for `kind`
    // (they only differ in the parser), and `source` is `getLanguage(kind)`
    let parse_dialect = get_language(&style.lang);
    let source = dialect_name(get_language(kind));

    let start = style.start.min(text.len());
    let end = style.end.clamp(start, text.len());
    let fragment = &text[start..end];

    let markers = match validate(fragment, parse_dialect) {
        Some(m) => m,
        None => return Vec::new(),
    };
    if markers.is_empty() {
        return Vec::new();
    }
    // positions never go past the style content, so the line starts up to there suffice (the
    // content ends before `</style` or at the end of the file, never inside a `\r\n`)
    let parent_lines = LineOffsets::new(&text[..end]);
    markers
        .into_iter()
        .map(|m| {
            // `positionAt` clamps to the fragment
            let s = m.offset.clamp(0, fragment.len() as i32) as usize;
            let e = (m.offset + m.length).clamp(0, fragment.len() as i32) as usize;
            // `getOriginalPosition`: parent offset = style start + fragment offset.
            // (`mapRangeToOriginal`'s `checkRangeLength` never applies: the fragment is a slice
            // of the parent that doesn't start or end inside a `\r\n`, so a single-line range has
            // the same length in both.)
            let range = Range { start: parent_lines.position_at(start + s), end: parent_lines.position_at(start + e) };
            CssDiagnostic {
                range,
                severity: match m.level {
                    lint::Level::Warning => Severity::Warning,
                    lint::Level::Error => Severity::Error,
                },
                message: m.message,
                code: m.code.to_string(),
                source: source.to_string(),
            }
        })
        .collect()
}

/// `doValidation(document, parseStylesheet(document))`: parse errors, then lint warnings.
/// `None` when the JS would throw.
fn validate(css: &[u16], dialect: Dialect) -> Option<Vec<lint::Marker>> {
    let mut p = parser::Parser::new(css, dialect);
    let root = p.parse_stylesheet();
    let ast = p.ast;
    let mut markers = collect_parse_errors(&ast, root);
    let lint = lint::lint(&ast, css, root);
    ast.recycle();
    markers.extend(lint.ok()?);
    Some(markers)
}

/// `ParseErrorCollector`: the issues of the nodes in the tree, in tree order (pre-order, and
/// insertion order within a node). Issues of nodes that were dropped (failed attempts) don't
/// count.
fn collect_parse_errors(ast: &nodes::Ast, root: nodes::NodeId) -> Vec<lint::Marker> {
    let mut keyed: Vec<(Vec<u32>, &nodes::Issue)> = Vec::new();
    for (node, issue) in ast.all_issues() {
        if let Some(path) = ast.tree_path(root, *node) {
            keyed.push((path, issue));
        }
    }
    // stable, so the issues of one node keep their order
    keyed.sort_by(|a, b| a.0.cmp(&b.0));
    keyed
        .into_iter()
        .map(|(_, issue)| lint::Marker {
            code: issue.error.id(),
            message: issue.error.message().to_string(),
            level: lint::Level::Error,
            offset: issue.offset,
            length: issue.length,
        })
        .collect()
}

/// svelte-language-server's `getLineOffsets` / `positionAt`
struct LineOffsets {
    offsets: Vec<usize>,
    len: usize,
}

impl LineOffsets {
    fn new(text: &[u16]) -> Self {
        let mut offsets = Vec::new();
        let mut is_line_start = true;
        let mut i = 0;
        while i < text.len() {
            if is_line_start {
                offsets.push(i);
            }
            let ch = text[i];
            is_line_start = ch == b'\r' as u16 || ch == b'\n' as u16;
            if ch == b'\r' as u16 && i + 1 < text.len() && text[i + 1] == b'\n' as u16 {
                i += 1;
            }
            i += 1;
        }
        if is_line_start && !text.is_empty() {
            offsets.push(text.len());
        }
        LineOffsets { offsets, len: text.len() }
    }

    fn position_at(&self, offset: usize) -> Position {
        let offset = offset.min(self.len);
        if self.offsets.is_empty() {
            return Position { line: 0, character: offset as u32 };
        }
        // the last line whose start is <= offset
        let line = self.offsets.partition_point(|&o| o <= offset) - 1;
        Position { line: line as u32, character: (offset - self.offsets[line]) as u32 }
    }
}
