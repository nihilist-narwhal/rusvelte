//! CSS analysis: `css-analyze.js`, `css-prune.js`, `css-warn.js`.

use super::Analyzer;
use crate::ast::StyleSheet;
use crate::error::Result;

pub fn analyze(_an: &mut Analyzer, _css: &StyleSheet) -> Result<()> {
    Ok(())
}
