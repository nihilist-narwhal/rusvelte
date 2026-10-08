//! A port of Svelte's code generation (`phases/3-transform`): [`compile`] for components,
//! [`compile_module`] for `.svelte.js` modules, and the CSS output (`css/index.js`) through
//! [`compile_css`].

pub mod client;
pub mod const_tags;
pub mod css;
pub mod js;
pub mod options;
pub mod server;

use oxc_allocator::Allocator;

use crate::analyze::{self, Warning};
use crate::error::CompileError;

/// The `css` compile option
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CssMode {
    /// `'external'`: the CSS goes to `result.css`
    #[default]
    External,
    /// `'injected'`: the CSS is embedded in the JS (`result.css` is `null`)
    Injected,
}

/// What a custom `cssHash` function receives (`hash` is [`hash`])
#[derive(Debug, Clone, Copy)]
pub struct CssHashInput<'a> {
    /// The content of the `<style>` element
    pub css: &'a str,
    /// The normalized filename (backslashes turned into slashes, relative to `rootDir`)
    pub filename: &'a str,
    /// The component name derived from the filename
    pub name: &'a str,
}

/// The compile options that affect the CSS output
#[derive(Default)]
pub struct CssOptions {
    /// `dev`: keeps empty rules
    pub dev: bool,
    /// `css`, unless `<svelte:options css>` overrides it
    pub css: CssMode,
    /// `cssHash`; `None` is the default, `svelte-${hash(filename)}`
    pub css_hash: Option<Box<dyn Fn(&CssHashInput) -> String>>,
    /// `rootDir`: the hash uses the filename relative to it
    pub root_dir: Option<String>,
    /// `customElement` (custom elements inject their styles)
    pub custom_element: bool,
    /// `runes` (`None` to infer it)
    pub runes: Option<bool>,
    /// `experimental.async`
    pub experimental_async: bool,
}

/// `result.css` of `compile`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CssOutput {
    pub code: String,
    pub has_global: bool,
    /// the `mappings` of `css.map` (empty where not computed)
    pub mappings: String,
}

/// The stylesheet a component injects at runtime (`css: 'injected'` or custom elements):
/// the `hash` and `code` of the `$$css` object in the generated JS
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InjectedCss {
    pub hash: String,
    pub code: String,
}

/// The CSS side of compiling a component
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CssResult {
    /// `result.css` (`None` without `<style>` or when the styles are injected)
    pub css: Option<CssOutput>,
    /// The styles embedded in the JS, when they're injected
    pub injected: Option<InjectedCss>,
}

/// `hash(str)` from `compiler/utils.js`: djb2 (xor variant) over the UTF-16 code units,
/// from the end, ignoring `\r`, in base 36
pub fn hash(s: &str) -> String {
    let units: Vec<u16> = s.encode_utf16().filter(|&c| c != u16::from(b'\r')).collect();
    let mut h: i32 = 5381;
    for &c in units.iter().rev() {
        // `((hash << 5) - hash) ^ c` with JS number semantics
        let shifted = h.wrapping_shl(5) as i64;
        let diff = (shifted - h as i64) as i32; // ToInt32 wraps modulo 2^32
        h = diff ^ c as i32;
    }
    to_base36(h as u32)
}

fn to_base36(mut n: u32) -> String {
    if n == 0 {
        return "0".into();
    }
    let mut digits = Vec::new();
    while n > 0 {
        digits.push(std::char::from_digit(n % 36, 36).unwrap());
        n /= 36;
    }
    digits.iter().rev().collect()
}

/// The CSS output of `compile(source, { filename, ...options })`: `result.css` (`code` and
/// `hasGlobal`), or the error `compile` throws. Pass `"(unknown)"` as `filename` when there is
/// none. Source maps aren't produced yet.
pub fn compile_css(source: &str, filename: &str, options: &CssOptions) -> Result<Option<CssOutput>, CompileError> {
    Ok(compile_styles(source, filename, options)?.css)
}

/// Like [`compile_css`], also returning the styles injected into the JS when the component
/// injects them (`css: 'injected'`, custom elements)
pub fn compile_styles(source: &str, filename: &str, options: &CssOptions) -> Result<CssResult, CompileError> {
    let source = source.strip_prefix('\u{feff}').unwrap_or(source);
    let alloc = Allocator::default();
    let mut warnings: Vec<Warning> = Vec::new();
    let component = crate::parse_with_warnings(&alloc, source, &mut warnings)
        .map_err(|err| analyze::acorn::reword_parse_error(err, source))?;
    let root = &component.root;
    let mut scripts: Vec<&crate::ast::Script> = [&root.instance, &root.module].into_iter().flatten().collect();
    scripts.sort_by_key(|s| s.start);
    if let Some(err) = scripts.iter().find_map(|s| analyze::acorn::check(&s.content.program, source, root.ts)) {
        return Err(err);
    }
    let analyze_options = analyze::CompileOptions {
        runes: options.runes,
        custom_element: options.custom_element,
        experimental_async: options.experimental_async,
        ..Default::default()
    };
    let analysis = analyze::analyze_component(&alloc, &component, source, filename, &analyze_options, &mut warnings)?;

    let (Some(sheet), Some(meta)) = (&root.css, &analysis.css) else {
        return Ok(CssResult::default());
    };

    // `css: 'css' in parsed_options ? parsed_options.css ?? 'external' : options.css`
    let css_mode = match root.options.as_ref().and_then(|o| o.values.get("css")) {
        Some(v) => match v.as_str() {
            Some("injected") => CssMode::Injected,
            _ => CssMode::External,
        },
        None => options.css,
    };
    let inject_styles = css_mode == CssMode::Injected || analysis.custom_element;

    // `state.filename`: backslashes replaced, made relative to `rootDir`
    let mut state_filename = filename.replace('\\', "/");
    if let Some(root_dir) = &options.root_dir {
        let root_dir = root_dir.replace('\\', "/");
        if state_filename.starts_with(&root_dir) {
            let rest = state_filename.replacen(&root_dir, "", 1);
            state_filename = rest.strip_prefix(['/', '\\']).unwrap_or(&rest).to_string();
        }
    }
    let styles = &source[sheet.css.content_start..sheet.css.content_end];
    let css_hash = match &options.css_hash {
        Some(f) => f(&CssHashInput { css: styles, filename: &state_filename, name: &analysis.component_name }),
        None => format!("svelte-{}", hash(if state_filename == "(unknown)" { styles } else { &state_filename })),
    };

    let render = |minify: bool| {
        css::render_stylesheet(source, &sheet.css, meta, &css::RenderOptions { hash: &css_hash, minify, dev: options.dev })
            .map_err(|e| CompileError { code: "magic_string", message: e.0, position: None })
    };
    if inject_styles {
        let code = render(!options.dev)?;
        Ok(CssResult { css: None, injected: Some(InjectedCss { hash: css_hash.clone(), code }) })
    } else {
        let code = render(false)?;
        Ok(CssResult { css: Some(CssOutput { code, has_global: analysis.css_has_global, mappings: String::new() }), injected: None })
    }
}

/// The output of [`compile`]: `js.code` with its source map's `mappings`, `css`, the
/// warnings and `metadata.runes`
#[derive(Debug, Clone)]
pub struct CompileOutput {
    pub js: String,
    /// the `mappings` of `js.map` (its source is the input, see `get_source_name`)
    pub js_mappings: String,
    pub css: Option<CssOutput>,
    pub warnings: Vec<Warning>,
    pub runes: bool,
}

/// Templates nested deeper than this are declined (`unsupported`): the analysis and transform
/// recurse per level, and Svelte's own compiler already overflows its stack well before it
pub const MAX_TEMPLATE_DEPTH: usize = 1000;

/// `compile(source, options)`
pub fn compile(source: &str, options: &options::CompileOptions) -> Result<CompileOutput, CompileError> {
    analyze::evaluate::take_unrepresentable();
    let output = compile_component(source, options)?;
    if analyze::evaluate::take_unrepresentable() {
        return Err(unrepresentable());
    }
    Ok(output)
}

/// A constant the JS compiler folds exactly can't be represented (see `evaluate::UNREPRESENTABLE`)
fn unrepresentable() -> CompileError {
    CompileError { code: "unsupported", message: "a constant expression can't be represented exactly".into(), position: None }
}

/// `deprecate(...)` in `validate-options.js`: each deprecated option is warned about once per
/// process (`warn_once`), before anything else
fn deprecated_option_warnings(options: &options::CompileOptions, warnings: &mut Vec<Warning>) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static ACCESSORS: AtomicBool = AtomicBool::new(false);
    static IMMUTABLE: AtomicBool = AtomicBool::new(false);
    for &key in &options.deprecated {
        let (warned, w) = match key {
            "accessors" => (&ACCESSORS, analyze::warnings::options_deprecated_accessors()),
            "immutable" => (&IMMUTABLE, analyze::warnings::options_deprecated_immutable()),
            _ => continue,
        };
        if !warned.swap(true, Ordering::Relaxed) {
            warnings.push(Warning { code: w.code, message: w.message, position: None });
        }
    }
}

fn compile_component(source: &str, options: &options::CompileOptions) -> Result<CompileOutput, CompileError> {
    let alloc = Allocator::default();
    let mut warnings: Vec<Warning> = Vec::new();
    deprecated_option_warnings(options, &mut warnings);
    let component = crate::parse_with_warnings(&alloc, source, &mut warnings)
        .map_err(|err| analyze::acorn::reword_parse_error(err, source))?;
    if component.ast.max_depth > MAX_TEMPLATE_DEPTH {
        return Err(CompileError {
            code: "unsupported",
            message: format!("the template is nested more than {MAX_TEMPLATE_DEPTH} levels deep"),
            position: None,
        });
    }
    let root = &component.root;
    let mut scripts: Vec<&crate::ast::Script> = [&root.instance, &root.module].into_iter().flatten().collect();
    scripts.sort_by_key(|s| s.start);
    if let Some(err) = scripts.iter().find_map(|s| analyze::acorn::check(&s.content.program, source, root.ts)) {
        return Err(err);
    }

    // `<svelte:options>` overrides the options
    let parsed = root.options.as_ref().map(|o| &o.values);
    let parsed_str = |k: &str| parsed.and_then(|v| v.get(k)).and_then(|v| v.as_str()).map(str::to_string);
    let parsed_bool = |k: &str| parsed.and_then(|v| v.get(k)).and_then(|v| v.as_bool());
    let runes = match parsed.and_then(|v| v.get("runes")) {
        Some(v) => v.as_bool(),
        None => options.runes,
    };
    let namespace = parsed_str("namespace").unwrap_or_else(|| options.namespace.clone());
    let preserve_whitespace = parsed_bool("preserveWhitespace").unwrap_or(options.preserve_whitespace);
    let combined = options::CompileOptions {
        filename: options.filename.clone(),
        root_dir: options.root_dir.clone(),
        namespace,
        preserve_whitespace,
        runes,
        accessors: parsed_bool("accessors").unwrap_or(options.accessors),
        immutable: parsed_bool("immutable").unwrap_or(options.immutable),
        css_injected: match parsed_str("css") {
            Some(c) => c == "injected",
            None => options.css_injected,
        },
        css_hash: match &options.css_hash {
            options::CssHash::Constant(c) => options::CssHash::Constant(c.clone()),
            _ => options::CssHash::Default,
        },
        ..options::CompileOptions { ..clone_simple(options) }
    };

    let analyze_options = analyze::CompileOptions {
        runes,
        custom_element: options.custom_element,
        experimental_async: options.experimental_async,
        namespace: Some(combined.namespace.clone()),
        name: options.name.clone(),
    };
    let mut analysis = analyze::analyze_component(&alloc, &component, source, &options.filename, &analyze_options, &mut warnings)?;

    // `state.filename`: backslashes replaced, made relative to `rootDir`
    let mut state_filename = options.filename.replace('\\', "/");
    if let Some(root_dir) = &options.root_dir {
        let root_dir = root_dir.replace('\\', "/");
        if state_filename.starts_with(&root_dir) {
            let rest = state_filename.replacen(&root_dir, "", 1);
            state_filename = rest.strip_prefix(['/', '\\']).unwrap_or(&rest).to_string();
        }
    }

    // the CSS hash and scoped elements
    let css_hash = match &root.css {
        Some(sheet) => {
            let styles = &source[sheet.css.content_start..sheet.css.content_end];
            match &combined.css_hash {
                options::CssHash::Constant(c) => c.clone(),
                _ => format!("svelte-{}", hash(if state_filename == "(unknown)" { styles } else { &state_filename })),
            }
        }
        None => String::new(),
    };
    let scoped = analysis.css.as_ref().map(|m| m.scoped_elements.clone()).unwrap_or_default();
    let (synthetic_class, synthetic_style) = server::synthetic_attributes(&analysis.an, &scoped);

    let css = match (&root.css, &analysis.css) {
        (Some(sheet), Some(meta)) if !combined.css_injected && !analysis.custom_element => {
            let (code, mappings) = css::render_stylesheet_with_mappings(source, &sheet.css, meta, &css::RenderOptions { hash: &css_hash, minify: false, dev: options.dev })
                .map_err(|e| CompileError { code: "magic_string", message: e.0, position: None })?;
            Some(CssOutput { code, has_global: analysis.css_has_global, mappings })
        }
        _ => None,
    };

    let locator = component.locator.clone();
    let comments: Vec<crate::estree::Comment> = root
        .comments
        .iter()
        .map(|c| {
            let (l1, c1) = locator.acorn_line_column(c.start);
            let (l2, c2) = locator.acorn_line_column(c.end);
            crate::estree::Comment {
                kind: if c.block { crate::estree::CommentKind::Block } else { crate::estree::CommentKind::Line },
                value: c.value.as_str().into(),
                span: Some(crate::estree::Span::new(c.start as u32, c.end as u32)),
                loc: Some(crate::estree::SourceLocation {
                    start: crate::estree::Position::new(l1 as u32, c1 as u32),
                    end: crate::estree::Position::new(l2 as u32, c2 as u32),
                }),
            }
        })
        .collect();

        // styles injected into the JS (`css: 'injected'`, client custom elements)
        let inject_css = match (&root.css, &analysis.css) {
            (Some(sheet), Some(meta))
            if (options.generate == options::Generate::Client && (combined.css_injected || analysis.custom_element))
                || (options.generate == options::Generate::Server && combined.css_injected && !analysis.custom_element) => {
                let (mut code, mappings) = css::render_stylesheet_with_mappings(source, &sheet.css, meta, &css::RenderOptions { hash: &css_hash, minify: !options.dev, dev: options.dev })
                    .map_err(|e| CompileError { code: "magic_string", message: e.0, position: None })?;
                // in dev, the injected styles carry their source map
                if options.dev && !code.is_empty() {
                    let basename = options.filename.rsplit(['/', '\\']).next().unwrap_or("").to_string();
                    let map = serde_json::json!({
                        "version": 3,
                        "file": basename,
                        "sources": [basename],
                        "sourcesContent": [source],
                        "names": [],
                        "mappings": mappings,
                    });
                    code.push_str(&format!("\n/*# sourceMappingURL=data:application/json;charset=utf-8;base64,{} */", base64(map.to_string().as_bytes())));
                }
                Some((css_hash.clone(), code))
            }
            _ => None,
        };

    let program = match options.generate {
        options::Generate::Server => {
            let conv = crate::estree::convert::Converter::new(&locator, root.ts);
            let mut s = server::Server {
                error: Default::default(),
                an: &mut analysis.an,
                options: &combined,
                conv,
                locator: &locator,
                css_hash,
                scoped,
                path: Vec::new(),
                hoisted: Vec::new(),
                legacy_reactive_statements: Vec::new(),
                filename: state_filename,
                dev: options.dev,
                instance_nodes: Default::default(),
                snippet_fns: Vec::new(),
                synthetic_class,
                synthetic_style,
            };
            let program = server::server_component(&mut s, inject_css);
            if let Some(e) = s.error.take() {
                return Err(e);
            }
            program
        }
        options::Generate::Client => {
            let conv = crate::estree::convert::Converter::new(&locator, root.ts);
            let mut c = client::Client {
                error: Default::default(),
                an: &mut analysis.an,
                options: &combined,
                conv,
                locator: &locator,
                css_hash,
                scoped,
                path: Vec::new(),
                hoisted: Vec::new(),
                templates: Default::default(),
                legacy_reactive_imports: Vec::new(),
                legacy_reactive_statements: Vec::new(),
                events: Vec::new(),
                instance_level_snippets: Vec::new(),
                module_level_snippets: Vec::new(),
                filename: state_filename,
                dev: options.dev,
                synthetic_class,
                synthetic_style,
                js_nodes: Default::default(),
                programs: Vec::new(),
                is_controlled: Default::default(),
                needs_mutation_validation: false,
                needs_props: false,
                memo_names: Default::default(),
                next_memo: 0,
                store_cache: Default::default(),
                immutable: false,
                accessors: false,
            };
            c.needs_props = c.an.needs_props;
            c.immutable = c.an.runes || combined.immutable;
            c.accessors = c.an.custom_element || (!c.an.runes && combined.accessors) || combined.component_api_4;
            let program = client::client_component(&mut c, inject_css);
            if let Some(e) = c.error.take() {
                return Err(e);
            }
            program
        }
        _ => return Err(CompileError { code: "unsupported", message: "generate: false is not supported".into(), position: None }),
    };
    let runes = analysis.an.runes;
    drop(analysis);
    let printed = crate::estree::print::print(&program, &crate::estree::print::PrintOptions { comments: &comments, source_map: true, ..Default::default() });
    Ok(CompileOutput { js_mappings: printed.encode_mappings(), js: printed.code, css, warnings, runes })
}

/// The Svelte version `compileModule` names in its header comment
pub const VERSION: &str = "5.57.2";

/// `compileModule(source, options)`: a `.svelte.js` module (JavaScript with runes). Only
/// `filename`, `generate`, `dev`, `rootDir` and `experimental` matter.
pub fn compile_module(source: &str, options: &options::CompileOptions) -> Result<CompileOutput, CompileError> {
    analyze::evaluate::take_unrepresentable();
    let output = compile_module_inner(source, options)?;
    if analyze::evaluate::take_unrepresentable() {
        return Err(unrepresentable());
    }
    Ok(output)
}

fn compile_module_inner(source: &str, options: &options::CompileOptions) -> Result<CompileOutput, CompileError> {
    let source = source.strip_prefix('\u{feff}').unwrap_or(source);
    let alloc = Allocator::default();
    let locator = std::rc::Rc::new(crate::locator::Locator::new(source));

    // `parse(source, comments, false, false)`
    let parser = crate::js::JsParser::new(false, locator.clone(), &alloc);
    let mut js_comments = Vec::new();
    let content = parser.parse_program(source, 0, &mut js_comments)?;
    if let Some(err) = analyze::acorn::check_module(&content.program, source) {
        return Err(err);
    }
    let mut ast = crate::ast::Ast::default();
    let fragment = ast.new_fragment(false);
    let root = crate::ast::Root {
        start: 0,
        end: source.len(),
        fragment,
        css: None,
        instance: None,
        module: Some(crate::ast::Script {
            start: 0,
            end: source.len(),
            context: "module",
            content,
            attributes: Vec::new(),
            leading_comment: None,
        }),
        options: None,
        comments: js_comments,
        ts: false,
    };
    let component = crate::Component { ast, root, locator: locator.clone() };

    let mut warnings: Vec<Warning> = Vec::new();
    let analyze_options = analyze::CompileOptions { runes: Some(true), experimental_async: options.experimental_async, ..Default::default() };
    let mut an = analyze::analyze_module(&alloc, &component, source, &options.filename, &analyze_options, &mut warnings)?;

    // `state.filename`: backslashes replaced, made relative to `rootDir`
    let mut state_filename = options.filename.replace('\\', "/");
    if let Some(root_dir) = &options.root_dir {
        let root_dir = root_dir.replace('\\', "/");
        if state_filename.starts_with(&root_dir) {
            let rest = state_filename.replacen(&root_dir, "", 1);
            state_filename = rest.strip_prefix(['/', '\\']).unwrap_or(&rest).to_string();
        }
    }

    let comments = estree_comments(&component.root.comments, &locator);
    let combined = clone_simple(options);
    let program = match options.generate {
        options::Generate::Server => {
            let conv = crate::estree::convert::Converter::new(&locator, false);
            let mut s = server::Server {
                error: Default::default(),
                an: &mut an,
                options: &combined,
                conv,
                locator: &locator,
                css_hash: String::new(),
                scoped: Default::default(),
                path: Vec::new(),
                hoisted: Vec::new(),
                legacy_reactive_statements: Vec::new(),
                filename: state_filename,
                dev: options.dev,
                instance_nodes: Default::default(),
                snippet_fns: Vec::new(),
                synthetic_class: Default::default(),
                synthetic_style: Default::default(),
            };
            server::server_module(&mut s)
        }
        options::Generate::Client => {
            let conv = crate::estree::convert::Converter::new(&locator, false);
            let mut c = client::Client {
                error: Default::default(),
                an: &mut an,
                options: &combined,
                conv,
                locator: &locator,
                css_hash: String::new(),
                scoped: Default::default(),
                path: Vec::new(),
                hoisted: Vec::new(),
                templates: Default::default(),
                legacy_reactive_imports: Vec::new(),
                legacy_reactive_statements: Vec::new(),
                events: Vec::new(),
                instance_level_snippets: Vec::new(),
                module_level_snippets: Vec::new(),
                filename: state_filename,
                dev: options.dev,
                synthetic_class: Default::default(),
                synthetic_style: Default::default(),
                js_nodes: Default::default(),
                programs: Vec::new(),
                is_controlled: Default::default(),
                needs_mutation_validation: false,
                needs_props: false,
                memo_names: Default::default(),
                next_memo: 0,
                store_cache: Default::default(),
                // `analysis.immutable`, `analysis.accessors`
                immutable: true,
                accessors: false,
            };
            client::client_module(&mut c)
        }
        _ => return Err(CompileError { code: "unsupported", message: "generate: false is not supported".into(), position: None }),
    };
    let printed = crate::estree::print::print(&program, &crate::estree::print::PrintOptions { comments: &comments, source_map: true, ..Default::default() });
    drop(an);
    let basename = options.filename.rsplit(['/', '\\']).next().unwrap_or("");
    // prepend the comment (and an empty line to the mappings)
    Ok(CompileOutput {
        js: format!("/* {basename} generated by Svelte v{VERSION} */\n{}", printed.code),
        js_mappings: format!(";{}", printed.encode_mappings()),
        css: None,
        warnings,
        runes: true,
    })
}

/// The `loc` Svelte gives a `<script>`'s `Program` (`read_script`): from the start of the
/// `<script>` tag to the end of `</script>` (its `start`/`end` stay acorn's)
pub fn script_program_loc(
    conv: &crate::estree::convert::Converter,
    root: &crate::ast::Root,
    program: &oxc_ast::ast::Program,
) -> Option<crate::estree::SourceLocation> {
    [&root.instance, &root.module]
        .into_iter()
        .flatten()
        .find(|s| std::ptr::eq(&s.content.program, program))
        .map(|s| conv.location(oxc_span::Span::new(s.start as u32, s.end as u32)))
}

/// The JS comments, as esrap gets them (`analysis.comments`)
fn estree_comments(comments: &[crate::js::JsComment], locator: &crate::locator::Locator) -> Vec<crate::estree::Comment> {
    comments
        .iter()
        .map(|c| {
            let (l1, c1) = locator.acorn_line_column(c.start);
            let (l2, c2) = locator.acorn_line_column(c.end);
            crate::estree::Comment {
                kind: if c.block { crate::estree::CommentKind::Block } else { crate::estree::CommentKind::Line },
                value: c.value.as_str().into(),
                span: Some(crate::estree::Span::new(c.start as u32, c.end as u32)),
                loc: Some(crate::estree::SourceLocation {
                    start: crate::estree::Position::new(l1 as u32, c1 as u32),
                    end: crate::estree::Position::new(l2 as u32, c2 as u32),
                }),
            }
        })
        .collect()
}

/// Standard base64 (with padding)
fn base64(bytes: &[u8]) -> String {
    const CHARS: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16) | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8) | u32::from(*chunk.get(2).unwrap_or(&0));
        out.push(CHARS[(n >> 18) as usize & 63] as char);
        out.push(CHARS[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { CHARS[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { CHARS[n as usize & 63] as char } else { '=' });
    }
    out
}

/// The plain (cloneable) fields of the options
fn clone_simple(o: &options::CompileOptions) -> options::CompileOptions {
    options::CompileOptions {
        filename: o.filename.clone(),
        root_dir: o.root_dir.clone(),
        dev: o.dev,
        generate: o.generate,
        experimental_async: o.experimental_async,
        accessors: o.accessors,
        css_injected: o.css_injected,
        css_hash: options::CssHash::Default,
        custom_element: o.custom_element,
        disclose_version: o.disclose_version,
        immutable: o.immutable,
        component_api_4: o.component_api_4,
        name: o.name.clone(),
        namespace: o.namespace.clone(),
        preserve_comments: o.preserve_comments,
        fragments_tree: o.fragments_tree,
        preserve_whitespace: o.preserve_whitespace,
        runes: o.runes,
        hmr: o.hmr,
        deprecated: o.deprecated.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_matches_js() {
        assert_eq!(hash("main.svelte"), "70s021");
        assert_eq!(hash(""), "45h");
        assert_eq!(hash("a\r\nb"), "2nhu6q");
        assert_eq!(hash("ünïcödé 😀 .foo { color: red }"), "yvms2h");
        assert_eq!(hash(&"x".repeat(1000)), "615edh");
    }

    #[test]
    fn compile_css_scopes_and_prunes() {
        let source = "<div class=\"a\">x</div><style>.a { color: red; } .b { color: blue; } @keyframes k {} .a { animation: k 1s; }</style>";
        let out = compile_css(source, "A.svelte", &CssOptions::default()).unwrap().unwrap();
        let h = format!("svelte-{}", hash("A.svelte"));
        assert_eq!(
            out.code,
            format!(".a.{h} {{ color: red; }} /* (unused) .b {{ color: blue; }}*/ @keyframes {h}-k {{}} .a.{h} {{ animation: {h}-k 1s; }}")
        );
        assert!(!out.has_global);
        // (checked against svelte 5.57.2)
        let injected = compile_styles(source, "A.svelte", &CssOptions { css: CssMode::Injected, ..Default::default() }).unwrap();
        assert!(injected.css.is_none());
        assert_eq!(injected.injected.unwrap().code, format!(".a.{h} {{color:red;}} @keyframes {h}-k {{}}.a.{h} {{ animation: {h}-k 1s;}}"));
    }

    #[test]
    fn compile_module_classes() {
        // (checked against svelte 5.57.2)
        let source = "class A { #x = $derived(1); constructor(){ this.#x = 3; this.#x += 1 } get x(){ return this.#x } }";
        let compile = |generate| {
            let options = options::CompileOptions { filename: "a.svelte.js".into(), generate, ..Default::default() };
            compile_module(source, &options).unwrap().js
        };
        assert_eq!(
            compile(options::Generate::Client),
            "/* a.svelte.js generated by Svelte v5.57.2 */\nimport * as $ from 'svelte/internal/client';\n\nclass A {\n\t#x = $.derived(() => 1);\n\n\tconstructor() {\n\t\t$.set(this.#x, 3);\n\t\t$.set(this.#x, $.get(this.#x) + 1);\n\t}\n\n\tget x() {\n\t\treturn $.get(this.#x);\n\t}\n}"
        );
        assert_eq!(
            compile(options::Generate::Server),
            "/* a.svelte.js generated by Svelte v5.57.2 */\nimport * as $ from 'svelte/internal/server';\n\nclass A {\n\t#x = $.derived(() => 1);\n\n\tconstructor() {\n\t\tthis.#x(3);\n\t\tthis.#x(this.#x() + 1);\n\t}\n\n\tget x() {\n\t\treturn this.#x();\n\t}\n}"
        );
    }

    #[test]
    fn compile_module_errors() {
        let options = options::CompileOptions { filename: "a.svelte.js".into(), ..Default::default() };
        let code = |source: &str| compile_module(source, &options).err().map(|e| e.code);
        assert_eq!(code("export { x };"), Some("js_parse_error"));
        assert_eq!(code("export { f }; function f() {}"), None);
        assert_eq!(code("export { y }; { var y = 1 }"), None);
        assert_eq!(code("export default x;"), None);
        assert_eq!(code("let a = $state(0); export default a; a = 2;"), Some("state_invalid_export"));
        assert_eq!(code("export const x = [$state(0)];"), Some("state_invalid_placement"));
        assert_eq!(code("import { writable } from 'svelte/store'; const s = writable(1); export const v = () => $s;"), Some("store_invalid_subscription_module"));
    }
}
