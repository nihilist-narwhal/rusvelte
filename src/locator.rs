//! Byte offset → line/column (in UTF-16 units) and byte offset → UTF-16 offset.
//!
//! Svelte's own locator (`locate-character`) only breaks lines on `\n`, while acorn also
//! breaks on `\r`, ` ` and ` ` (treating `\r\n` as one break). Both are kept so
//! JS nodes get acorn's numbering and template nodes get Svelte's.

use serde_json::{json, Value};

pub struct Locator<'s> {
    source: &'s str,
    /// line start byte offsets, `\n` only
    lf_lines: Vec<usize>,
    /// line start byte offsets, acorn's definition of a line break; `None` if same as `lf_lines`
    acorn_lines: Option<Vec<usize>>,
    ascii: bool,
    /// For non-ASCII sources: UTF-16 offset of every byte offset that starts a char (and of `len`)
    utf16: Vec<u32>,
}

impl<'s> Locator<'s> {
    pub fn new(source: &'s str) -> Self {
        let bytes = source.as_bytes();
        let mut lf_lines = vec![0];
        let mut lf_only = true;
        for (i, &b) in bytes.iter().enumerate() {
            match b {
                b'\n' => lf_lines.push(i + 1),
                b'\r' => lf_only = false,
                //   and   are E2 80 A8 / E2 80 A9
                0xE2 if bytes.get(i + 1) == Some(&0x80)
                    && matches!(bytes.get(i + 2), Some(0xA8 | 0xA9)) =>
                {
                    lf_only = false
                }
                _ => {}
            }
        }

        let acorn_lines = if lf_only {
            None
        } else {
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
                    0xE2 if bytes.get(i + 1) == Some(&0x80)
                        && matches!(bytes.get(i + 2), Some(0xA8 | 0xA9)) =>
                    {
                        i += 2;
                        lines.push(i + 1);
                    }
                    _ => {}
                }
                i += 1;
            }
            Some(lines)
        };

        let ascii = source.is_ascii();
        let utf16 = if ascii {
            Vec::new()
        } else {
            let mut table = vec![0u32; bytes.len() + 1];
            let mut u = 0u32;
            for (i, c) in source.char_indices() {
                table[i] = u;
                // continuation bytes map to the same unit as the char start
                for k in 1..c.len_utf8() {
                    table[i + k] = u;
                }
                u += c.len_utf16() as u32;
            }
            table[bytes.len()] = u;
            table
        };

        Locator { source, lf_lines, acorn_lines, ascii, utf16 }
    }

    pub fn lf_only(&self) -> bool {
        self.acorn_lines.is_none()
    }

    #[inline]
    pub fn utf16(&self, byte: usize) -> usize {
        if self.ascii {
            byte
        } else if byte < self.utf16.len() {
            self.utf16[byte] as usize
        } else {
            // offsets past the end (synthetic source suffixes) keep their distance from the end
            self.utf16[self.utf16.len() - 1] as usize + (byte - (self.utf16.len() - 1))
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

    /// Svelte's locator: `{ line, column, character }`
    pub fn locate(&self, byte: usize) -> Value {
        let (line, column) = self.line_col(&self.lf_lines, byte);
        json!({ "line": line, "column": column, "character": self.utf16(byte) })
    }

    /// Svelte's locator, without `character`
    pub fn position(&self, byte: usize) -> Value {
        let (line, column) = self.line_col(&self.lf_lines, byte);
        json!({ "line": line, "column": column })
    }

    /// acorn's position: `{ line, column }` with acorn's line breaks
    pub fn acorn_position(&self, byte: usize) -> Value {
        let lines = self.acorn_lines.as_deref().unwrap_or(&self.lf_lines);
        let (line, column) = self.line_col(lines, byte);
        json!({ "line": line, "column": column })
    }

    pub fn source(&self) -> &'s str {
        self.source
    }
}
