//! A port of [svelte2tsx](https://github.com/sveltejs/language-tools/tree/master/packages/svelte2tsx)
//! 0.7.61, which turns a Svelte component into TypeScript for type checking.

mod elements;
mod eswalk;
pub mod htmlx;
pub mod rewrite_imports;
pub mod script;
mod periscope;
mod slots;
mod template;
pub mod transform;

use std::fmt;

use oxc_allocator::Allocator;

use crate::error::CompileError;
use crate::magic_string::MagicStringError;
pub use template::Options;

#[derive(Debug)]
pub enum Error {
    /// The template doesn't parse
    Parse(CompileError),
    /// A MagicString edit failed (svelte2tsx throws in these cases too)
    Edit(MagicStringError),
    /// A file starting with a byte order mark: svelte2tsx's positions are off by one there,
    /// which makes it throw (or produce garbage)
    Bom,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Parse(e) => write!(f, "{e}"),
            Error::Edit(e) => write!(f, "{e}"),
            Error::Bom => f.write_str("file starts with a byte order mark"),
        }
    }
}

impl std::error::Error for Error {}

impl From<CompileError> for Error {
    fn from(e: CompileError) -> Self {
        Error::Parse(e)
    }
}

impl From<MagicStringError> for Error {
    fn from(e: MagicStringError) -> Self {
        Error::Edit(e)
    }
}

/// `htmlx2jsx` (svelte2tsx's test entry point for the template converter): the template
/// part of the transformation alone.
pub fn htmlx2jsx(source: &str, opts: &Options) -> Result<String, Error> {
    if source.starts_with('\u{feff}') {
        return Err(Error::Bom);
    }
    let verbatim = htmlx::find_verbatim_elements(source);
    let blanked = htmlx::blank_verbatim_content(source, &verbatim);
    let alloc = Allocator::default();
    let component = crate::parse(&alloc, &blanked, false)?;
    let legacy = crate::legacy::convert(&component.ast, &component.root, &blanked);
    let mut conv = template::Converter::new(source, opts, &component.ast, &component.root.comments);
    conv.convert(&legacy, &verbatim)?;
    Ok(conv.str.to_string())
}

/// svelte2tsx's options (the ones the port supports)
pub struct Svelte2TsxOptions {
    pub filename: Option<String>,
    pub is_ts_file: bool,
    /// `mode: 'ts'` (`'dts'` isn't supported)
    pub mode_ts: bool,
    pub accessors: bool,
    pub typings_namespace: String,
    /// `namespace: 'foreign'`
    pub namespace_foreign: bool,
    pub emit_jsdoc: bool,
    pub svelte5_plus: bool,
    pub rewrite_external_imports: Option<rewrite_imports::RewriteExternalImports>,
}

impl Default for Svelte2TsxOptions {
    fn default() -> Self {
        Svelte2TsxOptions {
            filename: None,
            is_ts_file: false,
            mode_ts: true,
            accessors: false,
            typings_namespace: "svelteHTML".into(),
            namespace_foreign: false,
            emit_jsdoc: false,
            svelte5_plus: true,
            rewrite_external_imports: None,
        }
    }
}

/// What `svelte2tsx` returns besides the code
pub struct Svelte2TsxOutput {
    pub code: String,
    /// `generateMap({ hires: true })`'s decoded mappings (UTF-16 columns), if asked for
    pub mappings: Option<Vec<Vec<[u32; 4]>>>,
    /// `exportedNames`' keys
    pub exported_names: Vec<String>,
}

/// `svelte2tsx(svelte, options).code`
pub fn svelte2tsx(source: &str, o: &Svelte2TsxOptions) -> Result<String, Error> {
    svelte2tsx_full(source, o, false).map(|r| r.code)
}

/// `svelte2tsx(svelte, options)`, with the source map if `with_map`
pub fn svelte2tsx_full(source: &str, o: &Svelte2TsxOptions, with_map: bool) -> Result<Svelte2TsxOutput, Error> {
    use script::events::ComponentEvents;
    use script::exported::ExportedNames;
    use script::generics::Generics;
    use script::render::{add_component_export, create_render_function, ExportParams, RenderParams};
    use script::stores::ImplicitStoreValues;
    use script::ts::ScriptAst;
    use script::Out;

    if source.starts_with('\u{feff}') {
        return Err(Error::Bom);
    }
    let opts = Options {
        typings_namespace: o.typings_namespace.clone(),
        preserve_attribute_case: o.namespace_foreign,
        svelte5_plus: o.svelte5_plus,
        emit_jsdoc: o.emit_jsdoc,
        is_ts_file: o.is_ts_file,
        mode_ts: o.mode_ts,
        accessors: o.accessors,
        rewrite_external_imports: o.rewrite_external_imports.clone(),
    };
    let verbatim = htmlx::find_verbatim_elements(source);
    let blanked = htmlx::blank_verbatim_content(source, &verbatim);
    let alloc = Allocator::default();
    let component = crate::parse(&alloc, &blanked, false)?;
    let legacy = crate::legacy::convert(&component.ast, &component.root, &blanked);
    let mut conv = template::Converter::new(source, &opts, &component.ast, &component.root.comments);
    conv.convert(&legacy, &verbatim)?;

    let (script_tag, module_tag) = template::script_tags(&legacy, &verbatim);
    let strict_events = verbatim.iter().any(|t| t.attributes.iter().any(|a| a.name == "strictEvents"));
    let resolved_stores = conv.resolved_stores();
    let uses_accessors = conv.uses_accessors;
    let is_runes = conv.is_runes;
    let mut uses_props = conv.uses_props;
    let mut uses_rest_props = conv.uses_rest_props;
    let mut uses_slots = conv.uses_slots;
    let doc = format_component_documentation(&conv.component_documentation);
    let root_snippets = std::mem::take(&mut conv.root_snippets);
    let slots = std::mem::take(&mut conv.slots.slots);
    let event_handler = std::mem::take(&mut conv.event_handler);
    let mut out = Out::new(conv.into_str());

    let basename = o.filename.as_deref().map(|f| f.rsplit(['/', '\\']).next().unwrap_or(f)).unwrap_or("");
    let script_error = |what: &str| Error::Edit(crate::magic_string::MagicStringError(format!("can't parse the {what} script")));

    // module first, then instance, then the template
    let mut instance_target = 0;
    let module_alloc = Allocator::default();
    let module_ast = match module_tag {
        Some(m) => Some(ScriptAst::parse(&module_alloc, &source[m.content_start..m.content_end], m.content_start).ok_or_else(|| script_error("module"))?),
        None => None,
    };
    if let Some(m) = module_tag {
        if m.start != 0 {
            out.ms.move_(m.start, m.end, 0)?;
        } else {
            // already at 0: move the instance script after it
            instance_target = m.end;
        }
    }

    let render_function_start = match script_tag {
        Some(s) => transform::last_index_of(source, ">", s.content_start).map_or(0, |i| i + 1),
        None => instance_target,
    };
    let mut implicit = ImplicitStoreValues::new(resolved_stores, render_function_start, o.svelte5_plus, false);
    let mut events = ComponentEvents::new(event_handler, strict_events);
    let instance_alloc = Allocator::default();
    let instance_ast = match script_tag {
        Some(s) => Some(ScriptAst::parse(&instance_alloc, &source[s.content_start..s.content_end], s.content_start).ok_or_else(|| script_error("instance"))?),
        None => None,
    };
    let mut exported = ExportedNames::new(0, basename, o.is_ts_file, o.svelte5_plus, is_runes, o.emit_jsdoc);
    let mut generics = Generics::default();
    let mut uses_slots_interface = false;
    let mut has_top_level_await = false;
    if let (Some(s), Some(ast)) = (script_tag, &instance_ast) {
        // between the module script and the template
        if s.start != instance_target {
            out.ms.move_(s.start, s.end, instance_target)?;
        }
        exported = ExportedNames::new(s.content_start, basename, o.is_ts_file, o.svelte5_plus, is_runes, o.emit_jsdoc);
        let res = script::instance::process_instance_script_content(
            &mut out,
            ast,
            s,
            &mut exported,
            &mut events,
            &mut implicit,
            o.mode_ts,
            module_ast.as_ref(),
            o.svelte5_plus,
            o.rewrite_external_imports.as_ref(),
        )?;
        uses_props |= res.uses_props;
        uses_rest_props |= res.uses_rest_props;
        uses_slots |= res.uses_slots;
        generics = res.generics;
        uses_slots_interface = res.uses_slots_interface;
        has_top_level_await = res.has_top_level_await;
    }

    exported.uses_accessors = uses_accessors;
    if o.svelte5_plus {
        exported.check_globals_for_runes(&implicit.globals());
        if has_top_level_await {
            exported.enter_runes_mode();
        }
    }

    create_render_function(
        &mut out,
        &RenderParams {
            script_tag,
            script_destination: instance_target,
            slots: &slots,
            events: &events,
            exported: &exported,
            uses_props,
            uses_rest_props,
            uses_slots,
            uses_slots_interface,
            generics: &generics,
            has_top_level_await,
            is_ts_file: o.is_ts_file,
            mode_ts: o.mode_ts,
            emit_jsdoc: o.emit_jsdoc,
        },
    )?;

    // the module script is processed after the instance script moved, so nothing moves edited text
    if let (Some(m), Some(ast)) = (module_tag, &module_ast) {
        let mut module_implicit =
            ImplicitStoreValues::new(implicit.accessed_stores(), render_function_start, o.svelte5_plus, script_tag.is_none() && !o.mode_ts);
        script::module::process_module_script_tag(&mut out, ast, m, &mut module_implicit, o.rewrite_external_imports.as_ref())?;
        if script_tag.is_none() {
            for stmt in &ast.program.body {
                exported.hoistable.analyze_module_script_node(stmt);
            }
        }
    }

    if module_tag.is_some() && !root_snippets.is_empty() {
        exported.hoistable.analyze_snippets(&root_snippets);
    }

    if module_tag.is_some() || script_tag.is_some() {
        let mut target = 0;
        if !root_snippets.is_empty() {
            if let Some(s) = script_tag {
                // +1 because imports are also moved there, and the snippets go after them
                target = s.start + 1;
            } else if let Some(ast) = &module_ast {
                let last_import_end = ast
                    .program
                    .body
                    .iter()
                    .filter_map(|s| match s {
                        oxc_ast::ast::Statement::ImportDeclaration(i) => Some(i.span.end),
                        _ => None,
                    })
                    .max();
                target = last_import_end.map_or(ast.offset, |e| e as usize + ast.offset);
                out.ms.append_left(target, "\n")?;
            }
        }
        for snippet in &root_snippets {
            let hoist_to_module = module_tag.is_some()
                && (snippet.globals.is_empty() || snippet.globals.iter().all(|id| exported.hoistable.is_allowed_reference(id)));
            if hoist_to_module {
                out.ms.move_(snippet.start, snippet.end, target)?;
            } else if script_tag.is_some() {
                out.ms.move_(snippet.start, snippet.end, render_function_start)?;
            }
        }
    }

    add_component_export(
        &mut out,
        &ExportParams {
            can_have_any_prop: !exported.uses_props_type && (uses_props || uses_rest_props),
            events: &events,
            is_ts_file: o.is_ts_file,
            uses_accessors,
            exported: &exported,
            file_name: o.filename.as_deref(),
            doc,
            mode_ts: o.mode_ts,
            generics: &generics,
            uses_slots: !slots.is_empty(),
            is_svelte5: o.svelte5_plus,
            has_top_level_await,
            no_svelte_component_typed: false,
            emit_jsdoc: o.emit_jsdoc,
        },
    )?;

    out.ms.prepend("///<reference types=\"svelte\" />\n");
    Ok(Svelte2TsxOutput {
        code: out.ms.to_string(),
        mappings: with_map.then(|| out.ms.decoded_map_hires()),
        exported_names: exported.exports.keys().cloned().collect(),
    })
}

/// `ComponentDocumentation.getFormatted()`
fn format_component_documentation(doc: &str) -> String {
    if doc.is_empty() {
        return String::new();
    }
    if !doc.contains('\n') {
        return format!("/** {doc} */\n");
    }
    let lines = dedent(doc).split('\n').map(|line| if line.is_empty() { " *".to_string() } else { format!(" * {line}") }).collect::<Vec<_>>().join("\n");
    format!("/**\n{lines}\n */\n")
}

/// `dedent-js` on a plain string
fn dedent(s: &str) -> String {
    static TRAILING: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| regex::Regex::new(r"\r?\n([\t ]*)$").unwrap());
    static INDENTS: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| regex::Regex::new(r"\n[\t ]+").unwrap());
    let mut s = TRAILING.replace(s, "").into_owned();
    if let Some(size) = INDENTS.find_iter(&s).map(|m| m.len() - 1).min() {
        let pattern = regex::Regex::new(&format!(r"\n[\t ]{{{size}}}")).unwrap();
        s = pattern.replace_all(&s, "\n").into_owned();
    }
    if let Some(rest) = s.strip_prefix("\r\n") {
        rest.to_string()
    } else if let Some(rest) = s.strip_prefix('\n') {
        rest.to_string()
    } else {
        s
    }
}
pub use template::{lnode_end, lnode_start};
