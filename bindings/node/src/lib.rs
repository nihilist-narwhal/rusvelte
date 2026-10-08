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
use std::sync::mpsc::{Sender, channel};
use std::sync::{Mutex, Once, OnceLock};

use napi_derive::napi;
use rusvelte::transform::{self, options::CompileOptions};
use serde_json::{Value, json};

/// `compile(source, options)`
#[napi]
pub fn compile(source: String, options: String) -> String {
    on_worker(source, options, false)
}

/// `compileModule(source, options)`
#[napi(js_name = "compileModule")]
pub fn compile_module(source: String, options: String) -> String {
    on_worker(source, options, true)
}

/// The analysis and transform recurse over the template and the scripts' syntax trees, and a
/// stack overflow aborts the whole process (it can't be caught like a panic). So compilations
/// run on one long-lived thread with a large stack; its memory is only committed as it's used.
const WORKER_STACK: usize = 256 << 20;

type Job = (String, String, bool, Sender<String>);

fn on_worker(source: String, options: String, module: bool) -> String {
    static WORKER: OnceLock<Mutex<Sender<Job>>> = OnceLock::new();
    let worker = WORKER.get_or_init(|| {
        let (tx, rx) = channel::<Job>();
        std::thread::Builder::new()
            .name("rusvelte".into())
            .stack_size(WORKER_STACK)
            .spawn(move || {
                for (source, options, module, reply) in rx {
                    let _ = reply.send(run(&source, &options, module));
                }
            })
            .expect("spawn the rusvelte compile thread");
        Mutex::new(tx)
    });
    let (reply, result) = channel();
    let sent = worker.lock().map(|tx| tx.send((source, options, module, reply)).is_ok()).unwrap_or(false);
    match (sent, result.recv()) {
        (true, Ok(json)) => json,
        _ => json!({ "unsupported": "the compile thread is unavailable" }).to_string(),
    }
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

/// Byte offsets → UTF-16 offsets, through one table built per non-ASCII source
struct Utf16Offsets {
    /// the UTF-16 offset of each byte offset (empty for ASCII sources, where they're equal)
    table: Vec<u32>,
}

impl Utf16Offsets {
    fn new(source: &str) -> Self {
        if source.is_ascii() {
            return Utf16Offsets { table: Vec::new() };
        }
        let mut table = Vec::with_capacity(source.len() + 1);
        let mut utf16 = 0u32;
        for c in source.chars() {
            // bytes inside a character map to its start
            for _ in 0..c.len_utf8() {
                table.push(utf16);
            }
            utf16 += c.len_utf16() as u32;
        }
        table.push(utf16);
        Utf16Offsets { table }
    }

    fn of(&self, byte: usize) -> usize {
        if self.table.is_empty() {
            return byte;
        }
        self.table[byte.min(self.table.len() - 1)] as usize
    }
}
