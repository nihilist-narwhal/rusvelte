//! Ports of `svelte2tsx/createRenderFunction.ts` and `addComponentExport.ts`.

use indexmap::IndexMap;

use super::events::ComponentEvents;
use super::exported::ExportedNames;
use super::generics::Generics;
use super::*;
use crate::svelte2tsx::htmlx::Verbatim;
use crate::svelte2tsx::transform::{char_at, last_index_of, surround_with_ignore_comments, IGNORE_END_COMMENT, IGNORE_START_COMMENT};

const RENDER_NAME: &str = "$$render";
/// Prevents class name clashes (sveltejs/language-tools#294)
const COMPONENT_SUFFIX: &str = "__SvelteComponent_";

pub type Slots = IndexMap<String, IndexMap<String, String>>;

pub struct RenderParams<'p, 'r, 'a> {
    pub script_tag: Option<&'p Verbatim<'p>>,
    pub script_destination: usize,
    pub slots: &'p Slots,
    pub events: &'p ComponentEvents,
    pub exported: &'p ExportedNames<'r, 'a>,
    pub uses_props: bool,
    pub uses_rest_props: bool,
    pub uses_slots: bool,
    pub uses_slots_interface: bool,
    pub generics: &'p Generics,
    pub has_top_level_await: bool,
    pub is_ts_file: bool,
    pub mode_ts: bool,
    pub emit_jsdoc: bool,
}

pub fn create_render_function(out: &mut Out, p: &RenderParams) -> Result<()> {
    let original = out.original();
    let mut props_decl = String::new();
    let use_ts_syntax = p.is_ts_file || !p.emit_jsdoc;
    let template_names: &[String] = if use_ts_syntax { &[] } else { p.generics.references() };
    let template_comment =
        if !use_ts_syntax && !template_names.is_empty() { format!("\n/** @template {} */\n", template_names.join(", ")) } else { String::new() };
    if p.uses_props {
        props_decl += " let $$props = __sveltets_2_allPropsType();";
    }
    if p.uses_rest_props {
        props_decl += " let $$restProps = __sveltets_2_restPropsType();";
    }
    if p.uses_slots {
        props_decl += &format!(
            " let $$slots = __sveltets_2_slotsType({{{}}});",
            p.slots.keys().map(|n| format!("'{n}': ''")).collect::<Vec<_>>().join(", ")
        );
    }
    let slot_type_reference = if p.uses_slots_interface { "<$$Slots>" } else { "" };
    let slot_declaration = if use_ts_syntax || slot_type_reference.is_empty() {
        format!(";const __sveltets_createSlot = __sveltets_2_createCreateSlot{slot_type_reference}();")
    } else {
        format!("/** @type {{ReturnType<typeof __sveltets_2_createCreateSlot{slot_type_reference}>}} */ const __sveltets_createSlot = __sveltets_2_createCreateSlot();")
    };
    let slots_declaration =
        if !p.slots.is_empty() && p.mode_ts { format!("\n{}", surround_with_ignore_comments(&slot_declaration)) } else { String::new() };
    let async_ = if p.has_top_level_await { "async " } else { "" };

    if let Some(script) = p.script_tag {
        let script_tag_end = last_index_of(original, ">", script.content_start).map_or(0, |i| i + 1);
        out.ms.overwrite(script.start, script.start + 1, ";", false)?;
        match p.generics.generics_attr {
            Some((mut start, mut end)) => {
                if matches!(char_at(original, start), "\"" | "'") {
                    start += 1;
                    end -= 1;
                }
                out.ms.overwrite(script.start + 1, start - 1, &format!("{template_comment}{async_}function {RENDER_NAME}"), false)?;
                if use_ts_syntax {
                    // if the generics are unused, only this char is colored opaque
                    let open = if p.is_ts_file { "<".to_string() } else { format!("<{IGNORE_START_COMMENT}") };
                    out.ms.overwrite(start - 1, start, &open, false)?;
                    let close = if p.is_ts_file { "" } else { IGNORE_END_COMMENT };
                    out.ms.overwrite(end, script_tag_end, &format!(">{close}() {{{props_decl}\n"), false)?;
                } else {
                    out.ms.overwrite(start - 1, start, "", false)?;
                    out.ms.overwrite(start, end, "", false)?;
                    out.ms.overwrite(end, script_tag_end, &format!("() {{{props_decl}\n"), false)?;
                }
            }
            None => {
                let defs = if use_ts_syntax { p.generics.to_definition_string(true) } else { String::new() };
                out.ms.overwrite(
                    script.start + 1,
                    script_tag_end,
                    &format!("{template_comment}{async_}function {RENDER_NAME}{defs}() {{{props_decl}\n"),
                    false,
                )?;
            }
        }
        let script_end_tag_start = last_index_of(original, "<", script.end - 1).unwrap_or(usize::MAX);
        // wrap template with callback
        out.ms.overwrite(script_end_tag_start, script.end, &format!("{slots_declaration};\nasync () => {{"), true)?;
    } else {
        let defs = if use_ts_syntax { p.generics.to_definition_string(true) } else { String::new() };
        out.ms.prepend_right(
            p.script_destination,
            &format!(";{template_comment}{async_}function {RENDER_NAME}{defs}() {{{props_decl}{slots_declaration}\nasync () => {{"),
        )?;
    }

    let slots_as_def = if p.uses_slots_interface {
        "{} as unknown as $$Slots".to_string()
    } else {
        format!(
            "{{{}}}",
            p.slots
                .iter()
                .map(|(name, attrs)| format!("'{name}': {{{}}}", slot_attributes_to_string(attrs)))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    let return_string = format!(
        "\nreturn {{ props: {}{}, slots: {slots_as_def}, events: {} }}}}",
        p.exported.create_props_str(p.uses_props || p.uses_rest_props),
        p.exported.create_exports_str(),
        p.events.to_def_string()
    );
    out.ms.append("};");
    out.ms.append(&return_string);
    Ok(())
}

fn slot_attributes_to_string(attrs: &IndexMap<String, String>) -> String {
    attrs
        .iter()
        .map(|(name, expr)| if name.starts_with("__spread__") { format!("...{expr}") } else { format!("{name}:{expr}") })
        .collect::<Vec<_>>()
        .join(", ")
}

pub struct ExportParams<'p, 'r, 'a> {
    pub can_have_any_prop: bool,
    pub events: &'p ComponentEvents,
    pub is_ts_file: bool,
    pub uses_accessors: bool,
    pub exported: &'p ExportedNames<'r, 'a>,
    pub file_name: Option<&'p str>,
    /// `componentDocumentation.getFormatted()`
    pub doc: String,
    pub mode_ts: bool,
    pub generics: &'p Generics,
    pub uses_slots: bool,
    pub is_svelte5: bool,
    pub has_top_level_await: bool,
    pub no_svelte_component_typed: bool,
    pub emit_jsdoc: bool,
}

pub fn add_component_export(out: &mut Out, p: &ExportParams) -> Result<()> {
    if !p.mode_ts {
        return Err(MagicStringError("dts mode is not supported".into()));
    }
    let statement = if p.generics.has() { generics_component_export(p) } else { simple_component_export(p) };
    out.ms.append(&statement);
    Ok(())
}

fn generics_component_export(p: &ExportParams) -> String {
    let generics_def = p.generics.to_definition_string(false);
    let generics_ref = p.generics.to_references_string();
    let use_ts_syntax = p.is_ts_file || !p.emit_jsdoc;
    let template_names: Vec<String> = if !use_ts_syntax && !generics_def.is_empty() {
        generics_def[1..generics_def.len() - 1]
            .split(',')
            .map(|part| part.trim())
            .map(|part| part.split(|c: char| c.is_whitespace() || c == '=').next().unwrap_or("").to_string())
            .filter(|s| !s.is_empty())
            .collect()
    } else {
        Vec::new()
    };
    let template_comment = if template_names.is_empty() { String::new() } else { format!("/** @template {} */\n", template_names.join(", ")) };
    let doc = &p.doc;
    let class_name = p.file_name.map(|f| class_name_from_filename(f, p.mode_ts));
    let return_type = |part: &str| format!("ReturnType<__sveltets_Render{generics_ref}['{part}']>");
    let g_ref = if use_ts_syntax { generics_ref.as_str() } else { "" };
    let render_call = if p.has_top_level_await { format!("(await {RENDER_NAME}{g_ref}())") } else { format!("{RENDER_NAME}{g_ref}()") };
    let runes = p.exported.is_runes_mode();

    let mut statement = format!(
        "\n{template_comment}class __sveltets_Render{} {{\n    props() {{\n        return {}.props;\n    }}\n    events() {{\n        return {}.events;\n    }}\n    slots() {{\n        return {render_call}.slots;\n    }}\n",
        if use_ts_syntax { generics_def.as_str() } else { "" },
        props(true, p.can_have_any_prop, p.exported, &render_call),
        events_str(p.events.has_strict_events() || runes, &render_call),
    );
    if p.is_svelte5 && runes {
        let render_type = if p.has_top_level_await {
            format!("Awaited<ReturnType<typeof {RENDER_NAME}{generics_ref}>>")
        } else {
            format!("ReturnType<typeof {RENDER_NAME}{generics_ref}>")
        };
        statement = if use_ts_syntax {
            format!(
                "\nclass __sveltets_Render{generics_def} {{\n    props(): {render_type}['props'] {{ return null as any; }}\n    events(): {render_type}['events'] {{ return null as any; }}\n    slots(): {render_type}['slots'] {{ return null as any; }}\n"
            )
        } else {
            format!(
                "\n{template_comment}class __sveltets_Render {{\n    /** @returns {{{render_type}['props']}} */\n    props() {{ return /** @type {{any}} */ (null); }}\n    /** @returns {{{render_type}['events']}} */\n    events() {{ return /** @type {{any}} */ (null); }}\n    /** @returns {{{render_type}['slots']}} */\n    slots() {{ return /** @type {{any}} */ (null); }}\n"
            )
        };
    }
    if p.is_svelte5 {
        statement += &format!(
            "    bindings() {{ return {}; }}\n    {}exports() {{ return {}; }}\n}}\n",
            p.exported.create_bindings_str(),
            if p.has_top_level_await { "async " } else { "" },
            if p.exported.has_exports() { format!("{render_call}.exports") } else { "{}".into() }
        );
    } else {
        statement += "}\n";
    }

    let svelte_component_class = if p.no_svelte_component_typed { "SvelteComponent" } else { "SvelteComponentTyped" };
    if p.is_svelte5 {
        let mut events_slots_type = Vec::new();
        if p.events.has_events() || !runes {
            events_slots_type.push(format!("$$events?: {}", return_type("events")));
        }
        if p.uses_slots {
            events_slots_type.push(format!("$$slots?: {}", return_type("slots")));
            events_slots_type.push("children?: any".to_string());
        }
        let props_type = if !p.can_have_any_prop && p.exported.has_no_props() {
            format!("{{{}}}", events_slots_type.join(", "))
        } else {
            format!("{} & {{{}}}", return_type("props"), events_slots_type.join(", "))
        };
        let bindings_type = format!("ReturnType<__sveltets_Render{}['bindings']>", p.generics.to_references_any_string());
        let children = if p.uses_slots { "& {children?: any}" } else { "" };
        let name = class_name.clone().unwrap_or_else(|| "$$Component".into());
        if use_ts_syntax {
            statement += &format!(
                "\ninterface $$IsomorphicComponent {{\n    new {generics_def}(options: import('svelte').ComponentConstructorOptions<{}{children}>): import('svelte').SvelteComponent<{}, {}, {}> & {{ $$bindings?: {} }} & {};\n    {generics_def}(internal: unknown, props: {props_type}): {};\n    z_$$bindings?: {bindings_type};\n}}\n",
                return_type("props"),
                return_type("props"),
                return_type("events"),
                return_type("slots"),
                return_type("bindings"),
                return_type("exports"),
                return_type("exports"),
            );
            statement += &format!("{doc}const {name}: $$IsomorphicComponent = null as any;\n");
            statement += &surround_with_ignore_comments(&format!("type {name}{generics_def} = InstanceType<typeof {name}{generics_ref}>;\n"));
            statement += &format!("export default {name};");
        } else {
            let component_type = format!(
                "(new {generics_def}(options: import('svelte').ComponentConstructorOptions<{}{children}>) => import('svelte').SvelteComponent<{}, {}, {}> & {{ $$bindings?: {} }} & {}) & ({generics_def}(internal: unknown, props: {props_type}) => {}) & {{z_$$bindings?: {bindings_type}}}",
                return_type("props"),
                return_type("props"),
                return_type("events"),
                return_type("slots"),
                return_type("bindings"),
                return_type("exports"),
                return_type("exports"),
            );
            statement += &format!(
                "\n{template_comment}/** @typedef {{{component_type}}} $$IsomorphicComponent */\n{doc}/** @type {{$$IsomorphicComponent}} */ export const {name} = /** @type {{any}} */(null);\n/** @typedef {{InstanceType<typeof {name}>}} {name} */\nexport default {name};"
            );
        }
    } else if use_ts_syntax {
        statement += &format!(
            "\n\nimport {{ {svelte_component_class} as __SvelteComponentTyped__ }} from \"svelte\" \n{doc}export default class{}{generics_def} extends __SvelteComponentTyped__<{}, {}, {}> {{{}{}\n}}",
            class_name.as_ref().map(|c| format!(" {c}")).unwrap_or_default(),
            return_type("props"),
            return_type("events"),
            return_type("slots"),
            p.exported.create_class_getters(&generics_ref),
            if p.uses_accessors { p.exported.create_class_accessors() } else { String::new() }
        );
    } else {
        statement += &format!(
            "\n\nimport {{ {svelte_component_class} as __SvelteComponentTyped__ }} from \"svelte\" \n/**{} * @extends {{__SvelteComponentTyped__<{}, {}, {}>}}\n */\nexport default class{} extends __SvelteComponentTyped__ {{{}{}\n}}",
            template_names.iter().map(|t| format!(" * @template {t}\n")).collect::<String>(),
            return_type("props"),
            return_type("events"),
            return_type("slots"),
            class_name.as_ref().map(|c| format!(" {c}")).unwrap_or_default(),
            p.exported.create_class_getters(&generics_ref),
            if p.uses_accessors { p.exported.create_class_accessors() } else { String::new() }
        );
    }
    statement
}

fn simple_component_export(p: &ExportParams) -> String {
    let render_call = if p.has_top_level_await { format!("${RENDER_NAME}") } else { format!("{RENDER_NAME}()") };
    let await_declaration = if p.has_top_level_await {
        // tsconfig could disallow top-level await, so wrap it in ignore comments
        format!("{}\n", surround_with_ignore_comments(&format!("const ${RENDER_NAME} = await {RENDER_NAME}();")))
    } else {
        String::new()
    };
    let prop_def = props(p.is_ts_file, p.can_have_any_prop, p.exported, &events_str(p.events.has_strict_events(), &render_call));
    let doc = &p.doc;
    let class_name = p.file_name.map(|f| class_name_from_filename(f, p.mode_ts));
    let component_name = class_name.clone().unwrap_or_else(|| "$$Component".into());

    if p.is_svelte5 {
        let use_ts_syntax = p.is_ts_file || !p.emit_jsdoc;
        let export = if use_ts_syntax { "" } else { "export " };
        if p.exported.is_runes_mode() && !p.uses_slots && !p.events.has_events() {
            format!(
                "\n{await_declaration}{doc}{export}const {component_name} = __sveltets_2_fn_component({render_call});\n{}export default {component_name};",
                surround_with_ignore_comments(&if use_ts_syntax {
                    format!("type {component_name} = ReturnType<typeof {component_name}>;\n")
                } else {
                    format!("/** @typedef {{ReturnType<typeof {component_name}>}} {component_name} */\n")
                })
            )
        } else {
            format!(
                "\n{await_declaration}{doc}{export}const {component_name} = __sveltets_2_isomorphic_component{}({prop_def});\n{}export default {component_name};",
                if p.uses_slots { "_slots" } else { "" },
                surround_with_ignore_comments(&if use_ts_syntax {
                    format!("type {component_name} = InstanceType<typeof {component_name}>;\n")
                } else {
                    format!("/** @typedef {{InstanceType<typeof {component_name}>}} {component_name} */\n")
                })
            )
        }
    } else {
        format!(
            "\n\n{doc}export default class{} extends __sveltets_2_createSvelte2TsxComponent({prop_def}) {{{}{}\n}}",
            class_name.as_ref().map(|c| format!(" {c}")).unwrap_or_default(),
            p.exported.create_class_getters(""),
            if p.uses_accessors { p.exported.create_class_accessors() } else { String::new() }
        )
    }
}

fn events_str(strict_events: bool, render: &str) -> String {
    if strict_events {
        render.to_string()
    } else {
        format!("__sveltets_2_with_any_event({render})")
    }
}

fn props(is_ts_file: bool, can_have_any_prop: bool, exported: &ExportedNames, render: &str) -> String {
    if exported.is_runes_mode() {
        render.to_string()
    } else if is_ts_file {
        if can_have_any_prop {
            format!("__sveltets_2_with_any({render})")
        } else {
            render.to_string()
        }
    } else {
        let optional = exported.create_optional_props_array();
        let partial = if can_have_any_prop { "__sveltets_2_partial_with_any" } else { "__sveltets_2_partial" };
        if optional.is_empty() {
            format!("{partial}({render})")
        } else {
            format!("{partial}([{}], {render})", optional.join(","))
        }
    }
}

/// A component class name from the file name (`classNameFromFilename`)
pub fn class_name_from_filename(filename: &str, append_suffix: bool) -> String {
    let base = filename.rsplit(['/', '\\']).next().unwrap_or(filename);
    let without_extensions = base.split('.').next().unwrap_or("");
    let valid: String = without_extensions.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-').collect();
    let first_valid = valid.chars().position(|c| c.is_ascii_alphabetic());
    let rest = match first_valid {
        Some(i) => &valid[i..],
        // `substr(-1)`: the last char
        None => &valid[valid.len().saturating_sub(1)..],
    };
    let pascal = pascal_case(rest);
    let name = if first_valid.is_none() { format!("A{pascal}") } else { pascal };
    format!("{name}{}", if append_suffix { COMPONENT_SUFFIX } else { "" })
}

/// `scule`'s `pascalCase` (ASCII input)
fn pascal_case(s: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut buff = String::new();
    let mut previous_upper: Option<bool> = None;
    let mut seen_non_splitter = false;
    for c in s.chars() {
        if matches!(c, '-' | '_' | '/' | '.') {
            parts.push(std::mem::take(&mut buff));
            previous_upper = None;
            continue;
        }
        let is_upper = if c.is_ascii_digit() { None } else { Some(c.is_ascii_uppercase()) };
        if seen_non_splitter {
            if previous_upper == Some(false) && is_upper == Some(true) {
                parts.push(std::mem::replace(&mut buff, c.to_string()));
                previous_upper = is_upper;
                continue;
            }
            if previous_upper == Some(true) && is_upper == Some(false) && buff.len() > 1 {
                let last = buff.pop().unwrap();
                parts.push(std::mem::take(&mut buff));
                buff.push(last);
                buff.push(c);
                previous_upper = is_upper;
                continue;
            }
        }
        buff.push(c);
        previous_upper = is_upper;
        seen_non_splitter = true;
    }
    parts.push(buff);
    parts
        .iter()
        .map(|p| {
            let mut chars = p.chars();
            match chars.next() {
                Some(f) => f.to_ascii_uppercase().to_string() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect()
}
