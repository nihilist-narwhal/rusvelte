//! Byte offset → line/column (in UTF-16 units) and byte offset → UTF-16 offset.
//!
//! Svelte's own locator (`locate-character`) only breaks lines on `\n`, while acorn also
//! breaks on `\r`, ` ` and ` ` (treating `\r\n` as one break). Both are kept so
//! JS nodes get acorn's numbering and template nodes get Svelte's.

use std::cell::OnceCell;

use serde_json::{json, Value};

pub struct Locator<'s> {
    source: &'s str,
    lf_only: bool,
    ascii: bool,
    /// line start byte offsets, `\n` only (built on first use)
    lf_lines: OnceCell<Vec<usize>>,
    /// line start byte offsets, acorn's definition of a line break (built on first use)
    acorn_lines: OnceCell<Vec<usize>>,
    /// For non-ASCII sources: UTF-16 offset of every byte offset (built on first use)
    utf16: OnceCell<Vec<u32>>,
}

/// Whether the source contains `\u2028` or `\u2029` (E2 80 A8 / E2 80 A9)
fn has_line_separator(bytes: &[u8]) -> bool {
    let mut i = 0;
    while let Some(p) = bytes[i..].iter().position(|&b| b == 0xE2) {
        let j = i + p;
        if bytes.get(j + 1) == Some(&0x80) && matches!(bytes.get(j + 2), Some(0xA8 | 0xA9)) {
            return true;
        }
        i = j + 1;
    }
    false
}

impl<'s> Locator<'s> {
    pub fn new(source: &'s str) -> Self {
        let bytes = source.as_bytes();
        // ` `/` ` start with 0xE2: only search for them when that byte occurs
        let lf_only = !bytes.contains(&b'\r') && !has_line_separator(bytes);
        Locator {
            source,
            lf_only,
            ascii: source.is_ascii(),
            lf_lines: OnceCell::new(),
            acorn_lines: OnceCell::new(),
            utf16: OnceCell::new(),
        }
    }

    fn lf_lines(&self) -> &[usize] {
        self.lf_lines.get_or_init(|| {
            let mut lines = vec![0];
            lines.extend(self.source.bytes().enumerate().filter(|&(_, b)| b == b'\n').map(|(i, _)| i + 1));
            lines
        })
    }

    fn acorn_lines(&self) -> &[usize] {
        if self.lf_only {
            return self.lf_lines();
        }
        self.acorn_lines.get_or_init(|| {
            let bytes = self.source.as_bytes();
            let mut lines = vec![0];
            let mut i = 0;
            while i < bytes.len() {
                match bytes[i] {
                    b'\n' => lines.push(i + 1),
                    b'\r' => {
                        if bytes.get(i + 1) == Some(&b'\n') {
                            i += 1;
                        }
                        lines.push(i + 1);
                    }
                    // \u2028 and \u2029 are E2 80 A8 / E2 80 A9
                    0xE2 if bytes.get(i + 1) == Some(&0x80) && matches!(bytes.get(i + 2), Some(0xA8 | 0xA9)) => {
                        i += 2;
                        lines.push(i + 1);
                    }
                    _ => {}
                }
                i += 1;
            }
            lines
        })
    }

    fn utf16_table(&self) -> &[u32] {
        self.utf16.get_or_init(|| {
            let mut table = vec![0u32; self.source.len() + 1];
            let mut u = 0u32;
            for (i, c) in self.source.char_indices() {
                // continuation bytes map to the same unit as the char start
                for k in 0..c.len_utf8() {
                    table[i + k] = u;
                }
                u += c.len_utf16() as u32;
            }
            table[self.source.len()] = u;
            table
        })
    }

    pub fn lf_only(&self) -> bool {
        self.lf_only
    }

    #[inline]
    pub fn utf16(&self, byte: usize) -> usize {
        if self.ascii {
            return byte;
        }
        let table = self.utf16_table();
        if byte < table.len() {
            table[byte] as usize
        } else {
            // offsets past the end (synthetic source suffixes) keep their distance from the end
            table[table.len() - 1] as usize + (byte - (table.len() - 1))
        }
    }

    fn line_col(&self, lines: &[usize], byte: usize) -> (usize, usize) {
        let line = match lines.binary_search(&byte) {
            Ok(i) => i,
            Err(i) => i - 1,
        };
        let col = self.utf16(byte) - self.utf16(lines[line]);
        (line + 1, col)
    }

    /// Svelte's locator as numbers: (line, column, character)
    pub fn line_column(&self, byte: usize) -> (usize, usize, usize) {
        let (line, column) = self.line_col(self.lf_lines(), byte);
        (line, column, self.utf16(byte))
    }

    /// Svelte's locator: `{ line, column, character }`
    pub fn locate(&self, byte: usize) -> Value {
        let (line, column) = self.line_col(self.lf_lines(), byte);
        json!({ "line": line, "column": column, "character": self.utf16(byte) })
    }

    /// Svelte's locator, without `character`
    pub fn position(&self, byte: usize) -> Value {
        let (line, column) = self.line_col(self.lf_lines(), byte);
        json!({ "line": line, "column": column })
    }

    /// acorn's position as numbers: (line, column in UTF-16 units) with acorn's line breaks
    pub fn acorn_line_column(&self, byte: usize) -> (usize, usize) {
        self.line_col(self.acorn_lines(), byte)
    }

    /// acorn's position: `{ line, column }` with acorn's line breaks
    pub fn acorn_position(&self, byte: usize) -> Value {
        let (line, column) = self.line_col(self.acorn_lines(), byte);
        json!({ "line": line, "column": column })
    }

    pub fn source(&self) -> &'s str {
        self.source
    }
}
