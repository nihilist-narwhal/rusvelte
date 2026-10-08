//! The errors `remove_typescript_nodes` throws for TypeScript Svelte can't strip.

use crate::ast::{Ast, Root};
use crate::error::Result;

pub fn check(_ast: &Ast, _root: &Root) -> Result<()> {
    Ok(())
}
