//! A Rust port of the Svelte 5 compiler. So far: the parser (`parse(source, { modern: true })`).

pub mod ast;
pub mod error;
#[allow(clippy::all)]
pub mod errors;
pub mod js;
pub mod locator;
pub mod parser;

use serde_json::Value;

pub use error::CompileError;
use locator::Locator;

/// `svelte/compiler`'s `parse(source, { modern: true, loose })`, as JSON.
/// Offsets in the result (and in errors) are UTF-16 offsets, like the JS version.
pub fn parse_modern(source: &str, loose: bool) -> Result<Value, CompileError> {
    let source = source.strip_prefix('\u{feff}').unwrap_or(source);
    let loc = Locator::new(source);
    let result = parser::parse(source, &loc, loose).map(|(ast, root)| root.into_json(ast));
    match result {
        Ok(mut json) => {
            if !source.is_ascii() {
                to_utf16(&mut json, &loc);
            }
            Ok(json)
        }
        Err(mut err) => {
            if let Some((s, e)) = err.position {
                err.position = Some((loc.utf16(s), loc.utf16(e)));
            }
            Err(err)
        }
    }
}

/// Convert every `start`/`end` offset from bytes to UTF-16 units
fn to_utf16(node: &mut Value, loc: &Locator) {
    match node {
        Value::Object(map) => {
            for (k, v) in map.iter_mut() {
                match v {
                    Value::Number(n) if k == "start" || k == "end" => {
                        if let Some(b) = n.as_u64() {
                            *v = loc.utf16(b as usize).into();
                        }
                    }
                    Value::Object(_) | Value::Array(_) => to_utf16(v, loc),
                    _ => {}
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                to_utf16(item, loc);
            }
        }
        _ => {}
    }
}
