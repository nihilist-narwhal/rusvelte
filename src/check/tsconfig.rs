//! Just enough of TypeScript's tsconfig handling for the overlay config: JSONC parsing,
//! `extends` resolution, and the path-valued options svelte-check reads (`rootDirs`,
//! `paths`/`baseUrl`).

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

pub struct ParsedConfig {
    /// The root config's own JSON (`parsed.raw`)
    pub raw: Map<String, Value>,
    /// `options.rootDirs`, absolute
    pub root_dirs: Option<Vec<PathBuf>>,
    /// `options.paths`, with the directory they're relative to (`pathsBasePath`)
    pub paths: Option<(Map<String, Value>, PathBuf)>,
    /// `options.baseUrl`, absolute
    pub base_url: Option<PathBuf>,
}

/// Strip comments and trailing commas so serde_json accepts the text
pub fn parse_jsonc(text: &str) -> Result<Value, String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    let mut last = 0;
    while i < b.len() {
        match b[i] {
            b'"' => {
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    if b[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
                i += 1;
            }
            b'/' if b.get(i + 1) == Some(&b'/') => {
                out.push_str(&text[last..i]);
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
                last = i;
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                out.push_str(&text[last..i]);
                i += 2;
                while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                    i += 1;
                }
                i = (i + 2).min(b.len());
                last = i;
            }
            b',' => {
                // a trailing comma: the next significant char closes the object/array
                let mut j = i + 1;
                loop {
                    while j < b.len() && b[j].is_ascii_whitespace() {
                        j += 1;
                    }
                    if b.get(j) == Some(&b'/') && b.get(j + 1) == Some(&b'/') {
                        while j < b.len() && b[j] != b'\n' {
                            j += 1;
                        }
                    } else if b.get(j) == Some(&b'/') && b.get(j + 1) == Some(&b'*') {
                        j += 2;
                        while j + 1 < b.len() && !(b[j] == b'*' && b[j + 1] == b'/') {
                            j += 1;
                        }
                        j += 2;
                    } else {
                        break;
                    }
                }
                if matches!(b.get(j), Some(b'}' | b']')) {
                    out.push_str(&text[last..i]);
                    last = i + 1;
                }
                i += 1;
            }
            _ => i += 1,
        }
    }
    out.push_str(&text[last.min(text.len())..]);
    serde_json::from_str(&out).map_err(|e| e.to_string())
}

fn read_config(path: &Path) -> Result<Map<String, Value>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("Cannot read file '{}': {e}", path.display()))?;
    match parse_jsonc(&text).map_err(|e| format!("Failed to parse '{}': {e}", path.display()))? {
        Value::Object(m) => Ok(m),
        _ => Err(format!("'{}' is not a JSON object", path.display())),
    }
}

/// Resolve an `extends` value from the config in `dir`
fn resolve_extends(spec: &str, dir: &Path) -> Option<PathBuf> {
    let with_json = |p: PathBuf| -> Option<PathBuf> {
        if p.is_file() {
            return Some(p);
        }
        let mut s = p.clone().into_os_string();
        s.push(".json");
        let j = PathBuf::from(s);
        if j.is_file() {
            return Some(j);
        }
        let t = p.join("tsconfig.json");
        t.is_file().then_some(t)
    };
    if spec.starts_with("./") || spec.starts_with("../") || Path::new(spec).is_absolute() {
        return with_json(dir.join(spec));
    }
    // a package: node_modules lookup upwards
    let mut d = Some(dir);
    while let Some(cur) = d {
        let candidate = cur.join("node_modules").join(spec);
        if let Some(p) = with_json(candidate.clone()) {
            return Some(p);
        }
        // package.json "tsconfig" field
        let pkg = candidate.join("package.json");
        if let Ok(text) = std::fs::read_to_string(&pkg) {
            if let Ok(Value::Object(m)) = serde_json::from_str::<Value>(&text) {
                if let Some(t) = m.get("tsconfig").and_then(Value::as_str) {
                    if let Some(p) = with_json(candidate.join(t)) {
                        return Some(p);
                    }
                }
            }
        }
        d = cur.parent();
    }
    None
}

/// Merge the compiler options we care about, base configs first
fn collect(path: &Path, root_dir: &Path, depth: usize, out: &mut ParsedConfig) -> Result<Map<String, Value>, String> {
    let config = read_config(path)?;
    let dir = path.parent().unwrap_or(Path::new("."));
    if depth < 32 {
        let extends: Vec<String> = match config.get("extends") {
            Some(Value::String(s)) => vec![s.clone()],
            Some(Value::Array(a)) => a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect(),
            _ => Vec::new(),
        };
        for e in extends {
            let e = e.replace("${configDir}", &root_dir.to_string_lossy());
            if let Some(base) = resolve_extends(&e, dir) {
                collect(&base, root_dir, depth + 1, out)?;
            }
        }
    }
    let resolve = |p: &str| -> PathBuf {
        let p = p.replace("${configDir}", &root_dir.to_string_lossy());
        normalize(&dir.join(p))
    };
    if let Some(Value::Object(opts)) = config.get("compilerOptions") {
        if let Some(Value::Array(dirs)) = opts.get("rootDirs") {
            out.root_dirs = Some(dirs.iter().filter_map(Value::as_str).map(resolve).collect());
        }
        if let Some(Value::String(b)) = opts.get("baseUrl") {
            out.base_url = Some(resolve(b));
        }
        if let Some(Value::Object(paths)) = opts.get("paths") {
            let mut paths = paths.clone();
            for v in paths.values_mut() {
                if let Value::Array(a) = v {
                    for s in a.iter_mut() {
                        if let Value::String(st) = s {
                            *st = st.replace("${configDir}", &root_dir.to_string_lossy());
                        }
                    }
                }
            }
            out.paths = Some((paths, dir.to_path_buf()));
        }
    }
    Ok(config)
}

pub fn parse_config(path: &Path) -> Result<ParsedConfig, String> {
    let root_dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
    let mut out = ParsedConfig { raw: Map::new(), root_dirs: None, paths: None, base_url: None };
    out.raw = collect(path, &root_dir, 0, &mut out)?;
    Ok(out)
}

/// `path.resolve`-style normalization (no symlink resolution)
pub fn normalize(p: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// `path.relative(from, to)` with `/` separators, `.` for the same path
pub fn relative_posix(from: &Path, to: &Path) -> String {
    let from = normalize(from);
    let to = normalize(to);
    let f: Vec<_> = from.components().collect();
    let t: Vec<_> = to.components().collect();
    let common = f.iter().zip(&t).take_while(|(a, b)| a == b).count();
    let mut parts: Vec<String> = vec!["..".to_string(); f.len() - common];
    parts.extend(t[common..].iter().map(|c| c.as_os_str().to_string_lossy().to_string()));
    if parts.is_empty() {
        ".".into()
    } else {
        parts.join("/")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jsonc() {
        let v = parse_jsonc("{\n // c\n \"a\": [1, 2,], /* x */ \"b\": \"//not\",\n}").unwrap();
        assert_eq!(v["a"][1], 2);
        assert_eq!(v["b"], "//not");
    }

    #[test]
    fn relative() {
        assert_eq!(relative_posix(Path::new("/a/b/c"), Path::new("/a/d")), "../../d");
        assert_eq!(relative_posix(Path::new("/a"), Path::new("/a")), ".");
    }
}
