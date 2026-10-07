//! A Rust port of the Svelte 5 compiler. So far: the parser (`parse(source, { modern: true })`).

pub mod ast;
pub mod css;
pub mod error;
#[allow(clippy::all)]
pub mod errors;
pub mod js;
pub mod legacy;
pub mod locator;
pub mod magic_string;
pub mod parser;
#[allow(clippy::all)]
mod warning_codes;

use oxc_allocator::Allocator;
use serde_json::Value;

pub use error::CompileError;
use locator::Locator;

/// A parsed component. JS nodes are allocated in the `Allocator` passed to [`parse`].
pub struct Component<'a> {
    pub ast: ast::Ast<'a>,
    pub root: ast::Root<'a>,
    pub locator: std::rc::Rc<Locator<'a>>,
}

/// Parse a component into the Rust AST. `source` should not include a BOM.
/// Positions in the AST are byte offsets.
pub fn parse<'a>(alloc: &'a Allocator, source: &'a str, loose: bool) -> Result<Component<'a>, CompileError> {
    let locator = std::rc::Rc::new(Locator::new(source));
    let (ast, root) = parser::parse(alloc, source, locator.clone(), loose)?;
    Ok(Component { ast, root, locator })
}

impl Component<'_> {
    /// The JSON `svelte/compiler`'s `parse(source, { modern: true })` returns (byte offsets)
    pub fn to_json(&self) -> Value {
        let cx = js::ToJson { ts: self.root.ts, loc: &self.locator, comments: &self.root.comments };
        self.root.to_json(&self.ast, &cx)
    }
}

/// `{ html, _comments }` of `svelte/compiler`'s legacy `parse(source, { loose })`, as JSON.
/// Offsets are UTF-16 offsets, like the JS version.
pub fn parse_legacy(source: &str, loose: bool) -> Result<Value, CompileError> {
    let source = source.strip_prefix('\u{feff}').unwrap_or(source);
    let alloc = Allocator::default();
    let component = parse(&alloc, source, loose).map_err(|err| error_to_utf16(err, source))?;
    let legacy = legacy::convert(&component.ast, &component.root, source);
    let cx = js::ToJson { ts: component.root.ts, loc: &component.locator, comments: &component.root.comments };
    let mut json = legacy.to_json(&component.ast, &component.root, &cx);
    if !source.is_ascii() {
        to_utf16(&mut json, &component.locator);
    }
    Ok(json)
}

/// `svelte/compiler`'s `parse(source, { modern: true, loose })`, as JSON.
/// Offsets in the result (and in errors) are UTF-16 offsets, like the JS version.
pub fn parse_modern(source: &str, loose: bool) -> Result<Value, CompileError> {
    let source = source.strip_prefix('\u{feff}').unwrap_or(source);
    let alloc = Allocator::default();
    match parse(&alloc, source, loose) {
        Ok(component) => {
            let mut json = component.to_json();
            if !source.is_ascii() {
                to_utf16(&mut json, &component.locator);
            }
            Ok(json)
        }
        Err(err) => Err(error_to_utf16(err, source)),
    }
}

fn error_to_utf16(mut err: CompileError, source: &str) -> CompileError {
    if let (Some((s, e)), false) = (err.position, source.is_ascii()) {
        let loc = Locator::new(source);
        err.position = Some((loc.utf16(s), loc.utf16(e)));
    }
    err
}

/// Convert every `start`/`end` offset from bytes to UTF-16 units
fn to_utf16(node: &mut Value, loc: &Locator) {
    match node {
        Value::Object(map) => {
            for (k, v) in map.iter_mut() {
                match v {
                    Value::Number(n) if k == "start" || k == "end" || k == "trailingComma" => {
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
