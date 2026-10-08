//! Port of `utils/htmlxparser.ts`: find `<script>`/`<style>` tags, blank out their contents
//! and parse the rest as a Svelte template.

use std::sync::LazyLock;

/// A `<script>` or `<style>` tag as svelte2tsx sees it ("verbatim element")
#[derive(Debug, Clone)]
pub struct Verbatim<'a> {
    pub is_style: bool,
    pub start: usize,
    pub end: usize,
    pub attributes: Vec<VerbatimAttr<'a>>,
    pub content_start: usize,
    pub content_end: usize,
}

#[derive(Debug, Clone)]
pub struct VerbatimAttr<'a> {
    pub name: &'a str,
    pub start: usize,
    pub end: usize,
    /// `(start, end, raw)` of the value, `None` for `true`
    pub value: Option<(usize, usize, &'a str)>,
}

const WS: &str = r"\t\n\x0B\x0C\r \u{a0}\u{1680}\u{2000}-\u{200a}\u{2028}\u{2029}\u{202f}\u{205f}\u{3000}\u{feff}";

fn tag_regex(tag: &str) -> regex::Regex {
    // (<!--[^]*?-->)|(<tag((?:\s+[^=>'"\/\s]+=(?:"[^"]*"|'[^']*'|[^>\s]+)|\s+[^=>'"\/\s]+)*\s*)>)([\S\s]*?)<\/tag>
    regex::Regex::new(&format!(
        r#"(<!--[\s\S]*?-->)|(<{tag}((?:[{WS}]+[^=>'"/{WS}]+=(?:"[^"]*"|'[^']*'|[^>{WS}]+)|[{WS}]+[^=>'"/{WS}]+)*[{WS}]*)>)([\s\S]*?)</{tag}>"#
    ))
    .unwrap()
}

static SCRIPT: LazyLock<regex::Regex> = LazyLock::new(|| tag_regex("script"));
static STYLE: LazyLock<regex::Regex> = LazyLock::new(|| tag_regex("style"));

/// The opening tag alone, anchored, for its groups (running the capture engine over a whole
/// element is slow when the contents are large)
fn open_tag_regex(tag: &str) -> regex::Regex {
    regex::Regex::new(&format!(
        r#"^<{tag}((?:[{WS}]+[^=>'"/{WS}]+=(?:"[^"]*"|'[^']*'|[^>{WS}]+)|[{WS}]+[^=>'"/{WS}]+)*[{WS}]*)>"#
    ))
    .unwrap()
}

static SCRIPT_OPEN: LazyLock<regex::Regex> = LazyLock::new(|| open_tag_regex("script"));
static STYLE_OPEN: LazyLock<regex::Regex> = LazyLock::new(|| open_tag_regex("style"));
static ATTR: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r#"([\w\-$]+\b)(?:=(?:"([^"]*)"|'([^']*)'|(\S+)))?"#).unwrap());

fn parse_attributes(s: &str, start: usize) -> Vec<VerbatimAttr<'_>> {
    let mut attrs = Vec::new();
    for m in ATTR.captures_iter(s) {
        let whole = m.get(0).unwrap();
        let name = m.get(1).unwrap().as_str();
        let value = m.get(2).or(m.get(3)).or(m.get(4)).filter(|v| !v.as_str().is_empty());
        // `start + str.indexOf(attr)`: the first occurrence of the match text
        let attr_start = start + s.find(whole.as_str()).unwrap_or(whole.start());
        let attr = whole.as_str();
        attrs.push(VerbatimAttr {
            name,
            start: attr_start,
            end: attr_start + attr.len(),
            value: value.map(|v| {
                let eq = attr.find('=').map_or(0, |p| p + 1);
                (attr_start + eq, attr_start + attr.len(), v.as_str())
            }),
        });
    }
    attrs
}

fn find_next(htmlx: &str, from: usize) -> Option<(Verbatim<'_>, usize)> {
    let mut best: Option<(Verbatim, usize)> = None;
    for (is_style, re, open_re) in [(false, &*SCRIPT, &*SCRIPT_OPEN), (true, &*STYLE, &*STYLE_OPEN)] {
        let mut pos = from;
        while let Some(whole) = re.find_at(htmlx, pos) {
            if whole.as_str().starts_with("<!--") {
                pos = whole.end().max(pos + 1);
                continue;
            }
            if best.as_ref().is_none_or(|(b, _)| whole.start() < b.start) {
                let tag = if is_style { "style" } else { "script" };
                // the regex matched `<tag attrs>content</tag>`: re-read the opening tag for its groups
                let Some(open) = open_re.captures(&htmlx[whole.start()..]) else { break };
                let open_len = open.get(0).unwrap().len();
                let attrs = open.get(1).map_or("", |a| a.as_str());
                let content_start = whole.start() + open_len;
                let content_end = whole.end() - format!("</{tag}>").len();
                best = Some((
                    Verbatim {
                        is_style,
                        start: whole.start(),
                        end: whole.end(),
                        attributes: parse_attributes(attrs, whole.start() + tag.len() + 1),
                        content_start,
                        content_end: content_end.max(content_start),
                    },
                    whole.end(),
                ));
            }
            break;
        }
    }
    best
}

pub fn find_verbatim_elements(htmlx: &str) -> Vec<Verbatim<'_>> {
    let mut tags = Vec::new();
    let mut from = 0;
    while let Some((node, next)) = find_next(htmlx, from) {
        tags.push(node);
        from = next;
    }
    tags
}

/// Replace every non-newline byte of the contents with a space (keeping byte offsets), and
/// break up long blank runs with `/**/` comments, as svelte2tsx does
pub fn blank_verbatim_content(htmlx: &str, elements: &[Verbatim]) -> String {
    let mut out = htmlx.as_bytes().to_vec();
    for el in elements {
        let content = &mut out[el.content_start..el.content_end];
        for b in content.iter_mut() {
            if *b != b'\n' {
                *b = b' ';
            }
        }
        // .replace(/[^\n][^\n][^\n][^\n]\n/g, '/**/\n')
        let mut i = 0;
        while i + 4 < content.len() {
            if content[i..i + 4].iter().all(|&b| b != b'\n') && content[i + 4] == b'\n' {
                content[i..i + 4].copy_from_slice(b"/**/");
                i += 5;
            } else {
                i += 1;
            }
        }
    }
    String::from_utf8(out).expect("blanking keeps UTF-8 valid")
}
