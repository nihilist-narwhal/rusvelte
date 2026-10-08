//! A port of [svelte2tsx](https://github.com/sveltejs/language-tools/tree/master/packages/svelte2tsx)
//! 0.7.61, which turns a Svelte component into TypeScript for type checking.

mod elements;
mod eswalk;
pub mod htmlx;
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
