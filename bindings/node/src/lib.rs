//! Node binding: `compile` and `compileModule` take the source and the options as JSON and
//! return the result as JSON (see `packages/rusvelte/compiler.js`, which builds the
//! `CompileResult` objects and falls back to `svelte/compiler` where this can't help):
//!
//! - `{ js: { code, mappings }, css: { code, mappings, hasGlobal } | null, warnings, runes }`
//! - `{ error: { code, message, position } }`: a compile error
//! - `{ unsupported: message }`: options or input this port doesn't handle (or a panic)
//!
//! Positions are UTF-16 offsets, like JavaScript string indices.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Once;

use napi_derive::napi;
use rusvelte::transform::{self, options::CompileOptions};
use serde_json::{Value, json};

/// `compile(source, options)`
#[napi]
pub fn compile(source: String, options: String) -> String {
    run(&source, &options, false)
}

/// `compileModule(source, options)`
#[napi(js_name = "compileModule")]
pub fn compile_module(source: String, options: String) -> String {
    run(&source, &options, true)
}

/// The Svelte version whose output this build reproduces
#[napi(js_name = "svelteVersion")]
pub fn svelte_version() -> String {
    transform::VERSION.to_string()
}

fn run(source: &str, options: &str, module: bool) -> String {
    static QUIET: Once = Once::new();
    // a panic becomes `{ unsupported }` (the caller falls back), without printing a backtrace
    QUIET.call_once(|| std::panic::set_hook(Box::new(|_| {})));

    let options: Value = match serde_json::from_str(options) {
        Ok(v) => v,
        Err(e) => return json!({ "unsupported": format!("options: {e}") }).to_string(),
    };
    let options = CompileOptions::from_json(&options);
    let result = catch_unwind(AssertUnwindSafe(|| {
        if module { transform::compile_module(source, &options) } else { transform::compile(source, &options) }
    }));
    let utf16 = Utf16Offsets::new(source);
    let position = |p: Option<(usize, usize)>| p.map(|(s, e)| json!([utf16.of(s), utf16.of(e)]));
    match result {
        Err(panic) => {
            let message = panic
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| panic.downcast_ref::<String>().cloned())
                .unwrap_or_default();
            json!({ "unsupported": format!("panic: {message}") })
        }
        Ok(Err(e)) if e.code == "unsupported" => json!({ "unsupported": e.message }),
        Ok(Err(e)) => json!({ "error": { "code": e.code, "message": e.message, "position": position(e.position) } }),
        Ok(Ok(out)) => json!({
            "js": { "code": out.js, "mappings": out.js_mappings },
            "css": out.css.map(|c| json!({ "code": c.code, "mappings": c.mappings, "hasGlobal": c.has_global })),
            "warnings": out.warnings.iter().map(|w| json!({ "code": w.code, "message": w.message, "position": position(w.position) })).collect::<Vec<_>>(),
            "runes": out.runes,
        }),
    }
    .to_string()
}

/// Byte offsets → UTF-16 offsets
struct Utf16Offsets<'a> {
    source: &'a str,
    ascii: bool,
}

impl<'a> Utf16Offsets<'a> {
    fn new(source: &'a str) -> Self {
        Utf16Offsets { source, ascii: source.is_ascii() }
    }

    fn of(&self, byte: usize) -> usize {
        if self.ascii {
            return byte;
        }
        let end = byte.min(self.source.len());
        let mut end = end;
        while !self.source.is_char_boundary(end) {
            end -= 1;
        }
        self.source[..end].encode_utf16().count()
    }
}
