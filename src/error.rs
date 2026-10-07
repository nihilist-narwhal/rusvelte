//! Compile errors. Positions are byte offsets into the source while parsing and are
//! converted to UTF-16 offsets (what JS reports) when the error leaves the crate.

use std::fmt;

/// Something an error can point at: a single offset, a `[start, end]` range, or nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Loc {
    None,
    At(usize),
    Range(usize, usize),
}

impl From<usize> for Loc {
    fn from(i: usize) -> Self {
        Loc::At(i)
    }
}

impl From<(usize, usize)> for Loc {
    fn from((start, end): (usize, usize)) -> Self {
        Loc::Range(start, end)
    }
}

impl From<&serde_json::Value> for Loc {
    fn from(node: &serde_json::Value) -> Self {
        let start = node.get("start").and_then(|v| v.as_u64()).map(|v| v as usize);
        let end = node.get("end").and_then(|v| v.as_u64()).map(|v| v as usize);
        match (start, end) {
            (Some(s), Some(e)) => Loc::Range(s, e),
            (Some(s), None) => Loc::At(s),
            _ => Loc::None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompileError {
    pub code: &'static str,
    pub message: String,
    /// `[start, end]`, if the error has a position
    pub position: Option<(usize, usize)>,
}

impl CompileError {
    pub fn new(loc: Loc, code: &'static str, message: String) -> Self {
        let position = match loc {
            Loc::None => None,
            Loc::At(i) => Some((i, i)),
            Loc::Range(s, e) => Some((s, e)),
        };
        CompileError { code, message, position }
    }

    /// The message without the trailing documentation link, as tests compare it
    pub fn first_line(&self) -> &str {
        self.message.split('\n').next().unwrap_or("")
    }
}

impl fmt::Display for CompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.first_line())
    }
}

impl std::error::Error for CompileError {}

pub type Result<T> = std::result::Result<T, CompileError>;
