//! Port of `htmlxtojsx_v2/utils/node-utils.ts` and `utils/ignore.ts`.
//!
//! Positions are byte offsets. Where the JS code steps one character with `+ 1`/`- 1`
//! (a UTF-16 unit), this steps one char.

use crate::magic_string::{MagicString, MagicStringError};

pub type Result<T> = std::result::Result<T, MagicStringError>;

/// One step of a transformation (`TransformationArray` in the JS)
#[derive(Debug, Clone, PartialEq)]
pub enum T {
    /// generated code that is appended
    Str(String),
    /// original code that is included as-is
    Range(usize, usize),
    /// a position after which things that should be deleted are moved to the end first
    Delete(usize),
}

impl From<&str> for T {
    fn from(s: &str) -> Self {
        T::Str(s.to_string())
    }
}

impl From<String> for T {
    fn from(s: String) -> Self {
        T::Str(s)
    }
}

impl From<(usize, usize)> for T {
    fn from((a, b): (usize, usize)) -> Self {
        T::Range(a, b)
    }
}

pub type Ts = Vec<T>;

pub const IGNORE_START_COMMENT: &str = "/*Ωignore_startΩ*/";
pub const IGNORE_END_COMMENT: &str = "/*Ωignore_endΩ*/";
pub const IGNORE_POSITION_COMMENT: &str = "/*Ωignore_positionΩ*/";

pub fn surround_with_ignore_comments(s: &str) -> String {
    format!("{IGNORE_START_COMMENT}{s}{IGNORE_END_COMMENT}")
}

// --- string helpers ----------------------------------------------------------------------

/// The byte offset one char after `i`
pub fn next_char(s: &str, i: usize) -> usize {
    match s.get(i..).and_then(|r| r.chars().next()) {
        Some(c) => i + c.len_utf8(),
        None => i + 1,
    }
}

/// The byte offset of the char before `i`
pub fn prev_char(s: &str, i: usize) -> usize {
    match s.get(..i).and_then(|r| r.chars().next_back()) {
        Some(c) => i - c.len_utf8(),
        None => i.saturating_sub(1),
    }
}

/// `str.charAt(i)` (as a string slice; empty past the end)
pub fn char_at(s: &str, i: usize) -> &str {
    match s.get(i..).and_then(|r| r.chars().next()) {
        Some(c) => &s[i..i + c.len_utf8()],
        None => "",
    }
}

pub fn byte_at(s: &str, i: usize) -> Option<u8> {
    s.as_bytes().get(i).copied()
}

/// `str.indexOf(needle, from)`, or `None` for -1
pub fn index_of(s: &str, needle: &str, from: usize) -> Option<usize> {
    let mut from = from.min(s.len());
    while !s.is_char_boundary(from) {
        from += 1;
    }
    s[from..].find(needle).map(|p| p + from)
}

/// `str.lastIndexOf(needle, from)`: the last occurrence starting at or before `from`
pub fn last_index_of(s: &str, needle: &str, from: usize) -> Option<usize> {
    let mut limit = from.saturating_add(needle.len()).min(s.len());
    // round up to a char boundary (a match can't straddle one anyway)
    while !s.is_char_boundary(limit) {
        limit += 1;
    }
    s[..limit].rfind(needle).filter(|&i| i <= from)
}

/// `/\s/.test(c)` for the char at `i`
pub fn is_space_at(s: &str, i: usize) -> bool {
    s.get(i..).and_then(|r| r.chars().next()).is_some_and(crate::parser::utils::is_whitespace_char)
}

/// `/^\s*$/.test(s)`
pub fn is_blank(s: &str) -> bool {
    s.chars().all(crate::parser::utils::is_whitespace_char)
}

// --- node-utils ----------------------------------------------------------------------------

/// Moves or inserts text to `end` in order, then removes what's left of `start..end`.
pub fn transform(str: &mut MagicString, start: usize, end: usize, transformations: &[T]) -> Result<()> {
    let original = str.original;
    let mut moves: Vec<(usize, usize)> = Vec::new();
    let mut append_position = end;
    let mut ignore_next_string = false;
    let mut delete_pos: Option<usize> = None;
    let mut delete_dest: Option<usize> = None;

    for (i, t) in transformations.iter().enumerate() {
        match t {
            T::Delete(dest) => {
                delete_pos = Some(moves.len());
                delete_dest = Some(*dest);
            }
            T::Str(s) => {
                if !ignore_next_string {
                    str.append_left(append_position, s)?;
                }
                ignore_next_string = false;
            }
            T::Range(t_start, t_end) => {
                let t_start = *t_start;
                let mut t_end = *t_end;
                if t_start == t_end {
                    // zero-range selection, don't move, it would cause bugs and isn't necessary anyway
                    continue;
                }
                if t_end < prev_char(original, end)
                    && !transformations.iter().any(|t| matches!(t, T::Range(s, _) if *s == t_end))
                {
                    t_end = next_char(original, t_end);
                    let next = transformations.get(i + 1);
                    ignore_next_string = matches!(next, Some(T::Str(_)));
                    // Do not append the next string, rather overwrite the next character
                    let overwrite = match next {
                        Some(T::Str(s)) => s.as_str(),
                        _ => "",
                    };
                    str.overwrite(prev_char(original, t_end), t_end, overwrite, true)?;
                }
                append_position = t_end;
                moves.push((t_start, t_end));
            }
        }
    }

    let delete_pos = delete_pos.unwrap_or(moves.len());
    for m in &moves[..delete_pos] {
        str.move_(m.0, m.1, end)?;
    }

    let mut remove_start = start;
    let mut sorted_moves = moves.clone();
    sorted_moves.sort_by_key(|m| m.0);
    // Remove everything between the transformations up until the end position
    for m in &sorted_moves {
        if remove_start < m.0 {
            if delete_pos != moves.len()
                && delete_dest.is_some_and(|d| remove_start > d)
                && remove_start < end
                && m.0 < end
            {
                str.move_(remove_start, m.0, end)?;
            }
            if m.0 < end {
                // Use one space because of hover etc: maps deleted characters to the whitespace
                str.overwrite(remove_start, m.0, " ", true)?;
            }
        }
        remove_start = m.1;
    }

    if remove_start > end {
        // Reset the end to the last transformation before the end if there were
        // transformations after the end so we still delete the correct range afterwards
        let idx = sorted_moves.iter().position(|m| m.0 > end).map_or(-1, |p| p as i64) - 1;
        remove_start = if idx >= 0 { sorted_moves[idx as usize].1 } else { end };
        if idx < 0 {
            // `sortedMoves[-1]` / `sortedMoves[-2]` are undefined in JS
            remove_start = end;
        }
    }

    if remove_start < end {
        // Completely delete the first character afterwards
        let next = next_char(original, remove_start);
        str.overwrite(remove_start, next, "", true)?;
        remove_start = next;
    }
    if remove_start < end {
        let last = prev_char(original, end);
        if delete_pos != moves.len() && delete_dest.is_some_and(|d| remove_start > d) && next_char(original, remove_start) < end {
            // Can only move stuff up to the end, not including
            str.move_(remove_start, last, end)?;
            str.overwrite(remove_start, last, " ", true)?;
            str.overwrite(last, end, "", true)?;
        } else {
            str.overwrite(remove_start, end, " ", true)?;
        }
    }

    for m in &moves[delete_pos..] {
        // Can happen when there's not enough space left at the end of an unfinished tag
        if m.1 >= end && m.0 <= end {
            break;
        }
        str.move_(m.0, m.1, end)?;
    }
    Ok(())
}

/// Surrounds the range with a prefix and suffix, overwriting the first/last char so the
/// mappings stay correct. Returns the range for convenience.
pub fn surround_with(str: &mut MagicString, (start, end): (usize, usize), prefix: &str, suffix: &str) -> Result<(usize, usize)> {
    let original = str.original;
    let first_end = next_char(original, start);
    if first_end == end {
        let c = char_at(original, start);
        str.overwrite(start, end, &format!("{prefix}{c}{suffix}"), true)?;
    } else {
        let c = char_at(original, start);
        str.overwrite(start, first_end, &format!("{prefix}{c}"), true)?;
        let last = prev_char(original, end);
        let c = &original[last..end];
        str.overwrite(last, end, &format!("{c}{suffix}"), true)?;
    }
    Ok((start, end))
}

/// `use:foo` → the range of `foo`
pub fn directive_name_range(original: &str, start: usize, name: &str) -> (usize, usize) {
    let colon = index_of(original, ":", start).map_or(0, |c| c + 1);
    (colon, colon + name.len())
}

/// Replaces chars that are invalid in TS identifiers with `_`
pub fn sanitize_prop_name(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '$' || c == '_' { c.to_string() } else { "_".repeat(c.len_utf16()) })
        .collect()
}

/// Bumps `position` past a trailing `.`/`?.` member access (left over from loose parsing)
pub fn with_trailing_property_access(original: &str, position: usize) -> usize {
    let bytes = original.as_bytes();
    let mut index = position;
    while index < bytes.len() {
        let c = char_at(original, index);
        if c.trim().is_empty() || is_space_at(original, index) {
            index += c.len().max(1);
            continue;
        }
        if c == "." {
            return index + 1;
        }
        if c == "?" && bytes.get(index + 1) == Some(&b'.') {
            return index + 2;
        }
        break;
    }
    position
}

pub fn range_with_trailing_property_access(original: &str, (start, end): (usize, usize)) -> (usize, usize) {
    (start, with_trailing_property_access(original, end))
}
