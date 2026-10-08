//! A port of [magic-string](https://github.com/rich-harris/magic-string) 0.30.21: the subset
//! `svelte2tsx` uses, with the same behavior (including which edits throw).
//!
//! Positions are byte offsets into the original string. Source maps are generated in UTF-16
//! columns, like the JS version.

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MagicStringError(pub String);

impl fmt::Display for MagicStringError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for MagicStringError {}

type Result<T> = std::result::Result<T, MagicStringError>;

const NONE: u32 = u32::MAX;

#[derive(Debug, Clone)]
struct Chunk {
    start: usize,
    end: usize,
    intro: String,
    outro: String,
    /// `None` while the content is still the original text (`original[start..end]`)
    content: Option<String>,
    edited: bool,
    previous: u32,
    next: u32,
}

impl Chunk {
    fn new(start: usize, end: usize) -> Self {
        Chunk { start, end, intro: String::new(), outro: String::new(), content: None, edited: false, previous: NONE, next: NONE }
    }

    fn content<'s>(&'s self, original: &'s str) -> &'s str {
        match &self.content {
            Some(c) => c,
            None => &original[self.start..self.end],
        }
    }

    fn contains(&self, index: usize) -> bool {
        self.start < index && index < self.end
    }

    fn edit(&mut self, content: String, content_only: bool) {
        self.content = Some(content);
        if !content_only {
            self.intro.clear();
            self.outro.clear();
        }
        self.edited = true;
    }
}

pub struct MagicString<'s> {
    pub original: &'s str,
    intro: String,
    outro: String,
    chunks: Vec<Chunk>,
    first_chunk: u32,
    last_chunk: u32,
    last_searched_chunk: u32,
    /// chunk starting / ending at each offset (dense, `NONE` if none)
    by_start: Vec<u32>,
    by_end: Vec<u32>,
    /// original start offset → chunk, to find the chunk containing an offset quickly
    starts: std::collections::BTreeMap<usize, u32>,
}

impl<'s> MagicString<'s> {
    pub fn new(original: &'s str) -> Self {
        let mut by_start = vec![NONE; original.len() + 1];
        let mut by_end = vec![NONE; original.len() + 1];
        by_start[0] = 0;
        by_end[original.len()] = 0;
        MagicString {
            original,
            intro: String::new(),
            outro: String::new(),
            chunks: vec![Chunk::new(0, original.len())],
            first_chunk: 0,
            last_chunk: 0,
            last_searched_chunk: 0,
            by_start,
            by_end,
            starts: std::collections::BTreeMap::from([(0, 0)]),
        }
    }

    fn by_start(&self, i: usize) -> Option<u32> {
        self.by_start.get(i).copied().filter(|&c| c != NONE)
    }

    fn by_end(&self, i: usize) -> Option<u32> {
        self.by_end.get(i).copied().filter(|&c| c != NONE)
    }

    fn chunk(&self, id: u32) -> &Chunk {
        &self.chunks[id as usize]
    }

    fn chunk_mut(&mut self, id: u32) -> &mut Chunk {
        &mut self.chunks[id as usize]
    }

    pub fn append(&mut self, content: &str) -> &mut Self {
        self.outro.push_str(content);
        self
    }

    pub fn prepend(&mut self, content: &str) -> &mut Self {
        self.intro.insert_str(0, content);
        self
    }

    pub fn append_left(&mut self, index: usize, content: &str) -> Result<&mut Self> {
        self.split(index)?;
        match self.by_end(index) {
            Some(c) => self.chunk_mut(c).outro.push_str(content),
            None => self.intro.push_str(content),
        }
        Ok(self)
    }

    pub fn append_right(&mut self, index: usize, content: &str) -> Result<&mut Self> {
        self.split(index)?;
        match self.by_start(index) {
            Some(c) => self.chunk_mut(c).intro.push_str(content),
            None => self.outro.push_str(content),
        }
        Ok(self)
    }

    pub fn prepend_left(&mut self, index: usize, content: &str) -> Result<&mut Self> {
        self.split(index)?;
        match self.by_end(index) {
            Some(c) => self.chunk_mut(c).outro.insert_str(0, content),
            None => self.intro.insert_str(0, content),
        }
        Ok(self)
    }

    pub fn prepend_right(&mut self, index: usize, content: &str) -> Result<&mut Self> {
        self.split(index)?;
        match self.by_start(index) {
            Some(c) => self.chunk_mut(c).intro.insert_str(0, content),
            None => self.outro.insert_str(0, content),
        }
        Ok(self)
    }

    /// `overwrite(start, end, content, { contentOnly })`
    pub fn overwrite(&mut self, start: usize, end: usize, content: &str, content_only: bool) -> Result<&mut Self> {
        self.update_impl(start, end, content, !content_only)
    }

    /// `update(start, end, content)`: keeps the intro/outro of the replaced range
    pub fn update(&mut self, start: usize, end: usize, content: &str) -> Result<&mut Self> {
        self.update_impl(start, end, content, false)
    }

    fn update_impl(&mut self, start: usize, end: usize, content: &str, overwrite: bool) -> Result<&mut Self> {
        if end > self.original.len() {
            return Err(MagicStringError("end is out of bounds".into()));
        }
        if start == end {
            return Err(MagicStringError(
                "Cannot overwrite a zero-length range – use appendLeft or prependRight instead".into(),
            ));
        }
        self.split(start)?;
        self.split(end)?;

        let first = self.by_start(start);
        let last = self.by_end(end);

        if let Some(first) = first {
            let mut chunk = first;
            while Some(chunk) != last {
                let c = self.chunk(chunk);
                let next = c.next;
                // `chunk.next !== this.byStart[chunk.end]` (where `null !== undefined`)
                if next == NONE || self.by_start(c.end) != Some(next) {
                    return Err(MagicStringError("Cannot overwrite across a split point".into()));
                }
                chunk = next;
                self.chunk_mut(chunk).edit(String::new(), false);
            }
            self.chunk_mut(first).edit(content.to_string(), !overwrite);
        } else {
            // must be inserting at the end
            let mut new_chunk = Chunk::new(start, end);
            new_chunk.edit(content.to_string(), false);
            let Some(last) = last else {
                return Err(MagicStringError("Cannot set properties of undefined (setting 'next')".into()));
            };
            let id = self.chunks.len() as u32;
            new_chunk.previous = last;
            self.chunks.push(new_chunk);
            self.chunk_mut(last).next = id;
        }
        Ok(self)
    }

    pub fn remove(&mut self, start: usize, end: usize) -> Result<&mut Self> {
        if start == end {
            return Ok(self);
        }
        if end > self.original.len() {
            return Err(MagicStringError("Character is out of bounds".into()));
        }
        if start > end {
            return Err(MagicStringError("end must be greater than start".into()));
        }
        self.split(start)?;
        self.split(end)?;

        let mut chunk = self.by_start(start);
        while let Some(c) = chunk {
            let ch = self.chunk_mut(c);
            ch.intro.clear();
            ch.outro.clear();
            ch.edit(String::new(), false);
            let chunk_end = ch.end;
            chunk = if end > chunk_end { self.by_start(chunk_end) } else { None };
        }
        Ok(self)
    }

    /// `move(start, end, index)`
    pub fn move_(&mut self, start: usize, end: usize, index: usize) -> Result<&mut Self> {
        if index >= start && index <= end {
            return Err(MagicStringError("Cannot move a selection inside itself".into()));
        }
        self.split(start)?;
        self.split(end)?;
        self.split(index)?;

        // (JS throws a TypeError reading `undefined.previous`/`.next` here)
        let (Some(first), Some(last)) = (self.by_start(start), self.by_end(end)) else {
            return Err(MagicStringError("Cannot read properties of undefined (reading 'previous')".into()));
        };

        let old_left = self.chunk(first).previous;
        let old_right = self.chunk(last).next;

        let new_right = self.by_start(index);
        if new_right.is_none() && last == self.last_chunk {
            return Ok(self);
        }
        let new_left = match new_right {
            Some(r) => self.chunk(r).previous,
            None => self.last_chunk,
        };

        if old_left != NONE {
            self.chunk_mut(old_left).next = old_right;
        }
        if old_right != NONE {
            self.chunk_mut(old_right).previous = old_left;
        }

        if new_left != NONE {
            self.chunk_mut(new_left).next = first;
        }
        if let Some(r) = new_right {
            self.chunk_mut(r).previous = last;
        }

        if self.chunk(first).previous == NONE {
            self.first_chunk = self.chunk(last).next;
        }
        if self.chunk(last).next == NONE {
            self.last_chunk = self.chunk(first).previous;
            let lc = self.last_chunk;
            self.chunk_mut(lc).next = NONE;
        }

        self.chunk_mut(first).previous = new_left;
        self.chunk_mut(last).next = new_right.unwrap_or(NONE);

        if new_left == NONE {
            self.first_chunk = first;
        }
        if new_right.is_none() {
            self.last_chunk = last;
        }
        Ok(self)
    }

    /// `slice(start, end)`: the generated content between two original positions
    pub fn slice(&self, start: usize, end: usize) -> Result<String> {
        let mut result = String::new();

        // find start chunk
        let mut chunk = self.first_chunk;
        while chunk != NONE && (self.chunk(chunk).start > start || self.chunk(chunk).end <= start) {
            let c = self.chunk(chunk);
            // found end chunk before start
            if c.start < end && c.end >= end {
                return Ok(result);
            }
            chunk = c.next;
        }

        if chunk != NONE && self.chunk(chunk).edited && self.chunk(chunk).start != start {
            return Err(MagicStringError(format!("Cannot use replaced character {start} as slice start anchor.")));
        }

        let start_chunk = chunk;
        while chunk != NONE {
            let c = self.chunk(chunk);
            if !c.intro.is_empty() && (start_chunk != chunk || c.start == start) {
                result.push_str(&c.intro);
            }

            let contains_end = c.start < end && c.end >= end;
            if contains_end && c.edited && c.end != end {
                return Err(MagicStringError(format!("Cannot use replaced character {end} as slice end anchor.")));
            }

            let content = c.content(self.original);
            let slice_start = if start_chunk == chunk { start - c.start } else { 0 };
            let slice_end = if contains_end { content.len() + end - c.end } else { content.len() };
            // edited chunks have arbitrary content: slice it like JS would (by position)
            result.push_str(content.get(slice_start.min(slice_end)..slice_end.min(content.len())).unwrap_or(""));

            if !c.outro.is_empty() && (!contains_end || c.end == end) {
                result.push_str(&c.outro);
            }

            if contains_end {
                break;
            }
            chunk = c.next;
        }

        Ok(result)
    }

    fn split(&mut self, index: usize) -> Result<()> {
        if self.by_start(index).is_some() || self.by_end(index).is_some() {
            return Ok(());
        }

        // Fast path: the chunk whose original range contains `index`. magic-string finds it by
        // walking neighbors from the last searched chunk; when that walk would succeed it ends
        // at this same chunk.
        if let Some((_, &c)) = self.starts.range(..=index).next_back() {
            if self.chunk(c).contains(index) {
                return self.split_chunk(c, index);
            }
        }

        let mut chunk = self.last_searched_chunk;
        let mut previous_chunk = chunk;
        let search_forward = index > self.chunk(chunk).end;

        while chunk != NONE {
            if self.chunk(chunk).contains(index) {
                return self.split_chunk(chunk, index);
            }
            let c = self.chunk(chunk);
            chunk = if search_forward {
                self.by_start(c.end).unwrap_or(NONE)
            } else {
                self.by_end(c.start).unwrap_or(NONE)
            };
            // Prevent infinite loop (e.g. via empty chunks, where start === end)
            if chunk == previous_chunk {
                return Ok(());
            }
            previous_chunk = chunk;
        }
        Ok(())
    }

    fn split_chunk(&mut self, chunk: u32, index: usize) -> Result<()> {
        let c = self.chunk(chunk);
        if c.edited && !c.content(self.original).is_empty() {
            // zero-length edited chunks are a special case (overlapping replacements)
            let (line, column) = locate(self.original, index);
            return Err(MagicStringError(format!(
                "Cannot split a chunk that has already been edited ({line}:{column} – \"{}\")",
                &self.original[c.start..c.end]
            )));
        }

        // Chunk::split
        let new_id = self.chunks.len() as u32;
        let c = self.chunk_mut(chunk);
        let mut new_chunk = Chunk::new(index, c.end);
        new_chunk.outro = std::mem::take(&mut c.outro);
        c.end = index;
        if c.edited {
            // keep the edit on the first half, like magic-string does for source maps
            new_chunk.edit(String::new(), false);
            c.content = Some(String::new());
        } else {
            c.content = None;
        }
        new_chunk.next = c.next;
        new_chunk.previous = chunk;
        c.next = new_id;
        let next = new_chunk.next;
        let new_end = new_chunk.end;
        self.chunks.push(new_chunk);
        if next != NONE {
            self.chunk_mut(next).previous = new_id;
        }

        self.by_end[index] = chunk;
        self.by_start[index] = new_id;
        self.by_end[new_end] = new_id;
        self.starts.insert(index, new_id);

        if chunk == self.last_chunk {
            self.last_chunk = new_id;
        }
        self.last_searched_chunk = chunk;
        Ok(())
    }

    /// Whether anything was edited
    pub fn has_changed(&self) -> bool {
        self.to_string() != self.original
    }

    /// `generateMap({ hires: true })` / `generateDecodedMap`: the decoded mappings, as
    /// `[generatedColumn, sourceIndex, originalLine, originalColumn]` per segment, per line.
    /// Columns count UTF-16 units.
    pub fn decoded_map_hires(&self) -> Vec<Vec<[u32; 4]>> {
        self.decoded_map(true, &Default::default())
    }

    /// `generateDecodedMap({ hires })`: with `hires: false`, unedited chunks get a segment at
    /// their start, at the start of each line and at the `sourcemapLocations` (byte offsets)
    pub fn decoded_map(&self, hires: bool, locations: &rustc_hash::FxHashSet<usize>) -> Vec<Vec<[u32; 4]>> {
        let mut m = Mappings { raw: vec![Vec::new()], line: 0, column: 0 };
        let line_starts: Vec<usize> =
            std::iter::once(0).chain(self.original.match_indices('\n').map(|(i, _)| i + 1)).collect();
        let locate = |index: usize| {
            let line = match line_starts.binary_search(&index) {
                Ok(i) => i,
                Err(i) => i - 1,
            };
            (line as u32, utf16_len(&self.original[line_starts[line]..index]) as u32)
        };

        m.advance(&self.intro);
        let mut chunk = self.first_chunk;
        while chunk != NONE {
            let c = self.chunk(chunk);
            let (mut line, mut column) = locate(c.start);
            m.advance(&c.intro);
            if c.edited {
                let content = c.content(self.original);
                if !content.is_empty() {
                    // addEdit
                    let len_minus_one = content.len() - 1;
                    let mut previous_line_end: Option<usize> = None;
                    let mut line_end = content.find('\n');
                    while let Some(le) = line_end {
                        if len_minus_one <= le {
                            break;
                        }
                        m.push([m.column, 0, line, column]);
                        m.line += 1;
                        m.raw.push(Vec::new());
                        m.column = 0;
                        previous_line_end = Some(le);
                        line_end = content[le + 1..].find('\n').map(|p| p + le + 1);
                    }
                    m.push([m.column, 0, line, column]);
                    m.advance(&content[previous_line_end.map_or(0, |p| p + 1)..]);
                }
            } else {
                // addUneditedChunk (hires: a segment for every UTF-16 unit)
                let mut first = true;
                for (offset, ch) in self.original[c.start..c.end].char_indices() {
                    if ch == '\n' {
                        line += 1;
                        column = 0;
                        m.line += 1;
                        m.raw.push(Vec::new());
                        m.column = 0;
                        first = true;
                    } else {
                        for _ in 0..ch.len_utf16() {
                            if hires || first || locations.contains(&(c.start + offset)) {
                                m.push([m.column, 0, line, column]);
                            }
                            column += 1;
                            m.column += 1;
                            first = false;
                        }
                    }
                }
            }
            m.advance(&c.outro);
            chunk = c.next;
        }
        m.advance(&self.outro);
        m.raw
    }

    /// The `mappings` string of `generateMap({ hires: true })`
    pub fn mappings_hires(&self) -> String {
        encode_mappings(&self.decoded_map_hires())
    }
}

impl fmt::Display for MagicString<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.intro)?;
        let mut chunk = self.first_chunk;
        while chunk != NONE {
            let c = self.chunk(chunk);
            f.write_str(&c.intro)?;
            f.write_str(c.content(self.original))?;
            f.write_str(&c.outro)?;
            chunk = c.next;
        }
        f.write_str(&self.outro)
    }
}

struct Mappings {
    raw: Vec<Vec<[u32; 4]>>,
    line: usize,
    column: u32,
}

impl Mappings {
    fn push(&mut self, segment: [u32; 4]) {
        self.raw[self.line].push(segment);
    }

    fn advance(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        let mut lines = s.split('\n');
        let mut last = lines.next().unwrap();
        let mut had_newline = false;
        for l in lines {
            self.line += 1;
            self.raw.push(Vec::new());
            last = l;
            had_newline = true;
        }
        if had_newline {
            self.column = 0;
        }
        self.column += utf16_len(last) as u32;
    }
}

fn utf16_len(s: &str) -> usize {
    if s.is_ascii() {
        s.len()
    } else {
        s.chars().map(char::len_utf16).sum()
    }
}

/// `getLocator(original)(index)`: zero-based line and UTF-16 column
fn locate(original: &str, index: usize) -> (usize, usize) {
    let before = &original[..index.min(original.len())];
    let line = before.matches('\n').count();
    let line_start = before.rfind('\n').map_or(0, |p| p + 1);
    (line, utf16_len(&before[line_start..]))
}

/// `@jridgewell/sourcemap-codec`'s `encode`
pub fn encode_mappings(lines: &[Vec<[u32; 4]>]) -> String {
    let mut out = String::new();
    let (mut source_index, mut source_line, mut source_column) = (0i64, 0i64, 0i64);
    for (i, line) in lines.iter().enumerate() {
        if i > 0 {
            out.push(';');
        }
        let mut generated_column = 0i64;
        for (j, seg) in line.iter().enumerate() {
            if j > 0 {
                out.push(',');
            }
            vlq(&mut out, seg[0] as i64 - generated_column);
            generated_column = seg[0] as i64;
            vlq(&mut out, seg[1] as i64 - source_index);
            source_index = seg[1] as i64;
            vlq(&mut out, seg[2] as i64 - source_line);
            source_line = seg[2] as i64;
            vlq(&mut out, seg[3] as i64 - source_column);
            source_column = seg[3] as i64;
        }
    }
    out
}

fn vlq(out: &mut String, value: i64) {
    const CHARS: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut v = if value < 0 { ((-value) << 1) | 1 } else { value << 1 };
    loop {
        let mut digit = (v & 0b11111) as usize;
        v >>= 5;
        if v > 0 {
            digit |= 0b100000;
        }
        out.push(CHARS[digit] as char);
        if v == 0 {
            break;
        }
    }
}
