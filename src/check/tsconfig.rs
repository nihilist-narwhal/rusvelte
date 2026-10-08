//! Just enough of TypeScript's tsconfig handling for the overlay config: JSONC parsing,
//! `extends` resolution, and the path-valued options svelte-check reads (`rootDirs`,
//! `paths`/`baseUrl`).

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

pub struct ParsedConfig {
    /// The root config's JSON with the `include`/`exclude`/`files` it inherits (relative to
    /// its directory), like TypeScript's `parsed.raw`
    pub raw: Map<String, Value>,
    /// `options.rootDirs`, absolute
    pub root_dirs: Option<Vec<PathBuf>>,
    /// `options.paths`, with the directory they're relative to (`pathsBasePath`)
    pub paths: Option<(Map<String, Value>, PathBuf)>,
    /// `options.baseUrl`, absolute
    pub base_url: Option<PathBuf>,
    /// whether `outDir` / `rootDir` are set anywhere in the chain
    pub has_out_dir: bool,
    pub has_root_dir: bool,
    /// `options.outDir` / `options.declarationDir`, absolute (TypeScript's default `exclude`)
    pub out_dir: Option<PathBuf>,
    pub declaration_dir: Option<PathBuf>,
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
            // module resolution returns real paths
            return Some(std::fs::canonicalize(&p).unwrap_or(p));
        }
        // package.json "tsconfig" field
        let pkg = candidate.join("package.json");
        if let Ok(text) = std::fs::read_to_string(&pkg) {
            if let Ok(Value::Object(m)) = serde_json::from_str::<Value>(&text) {
                if let Some(t) = m.get("tsconfig").and_then(Value::as_str) {
                    if let Some(p) = with_json(candidate.join(t)) {
                        return Some(std::fs::canonicalize(&p).unwrap_or(p));
                    }
                }
            }
        }
        d = cur.parent();
    }
    None
}

/// The longest `extends` chain followed (TypeScript only stops at cycles; this is far beyond
/// any real project)
const MAX_EXTENDS_DEPTH: usize = 64;

/// The spec properties a config inherits from the ones it extends
const SPEC_PROPERTIES: [&str; 3] = ["include", "exclude", "files"];

/// JavaScript truthiness, which TypeScript uses to decide whether a config sets a property
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|n| n != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(_) | Value::Object(_)) => true,
    }
}

/// `combinePaths(relativeDifference, spec)` with `.`/`..` segments resolved where they can be
fn rebase_spec(rel_dir: &str, spec: &str) -> String {
    let joined = if rel_dir.is_empty() || rel_dir == "." { spec.to_string() } else { format!("{rel_dir}/{spec}") };
    let mut out: Vec<&str> = Vec::new();
    for part in joined.split('/') {
        match part {
            "." | "" => {}
            ".." if out.last().is_some_and(|p| *p != "..") => {
                out.pop();
            }
            _ => out.push(part),
        }
    }
    if out.is_empty() {
        ".".into()
    } else {
        out.join("/")
    }
}

/// One config with everything it inherits applied
struct Resolved {
    /// its JSON, with inherited `include`/`exclude`/`files` (relative to its own directory)
    raw: Map<String, Value>,
    root_dirs: Option<Vec<PathBuf>>,
    paths: Option<(Map<String, Value>, PathBuf)>,
    base_url: Option<PathBuf>,
    has_out_dir: bool,
    has_root_dir: bool,
    out_dir: Option<PathBuf>,
    declaration_dir: Option<PathBuf>,
}

/// Follows `extends` chains: each config is read once (by canonical path), cycles are errors
struct Resolver {
    /// the root config's directory (`${configDir}`)
    root_dir: PathBuf,
    done: std::collections::HashMap<PathBuf, std::rc::Rc<Resolved>>,
    /// the chain being resolved, for cycle detection
    stack: Vec<PathBuf>,
}

impl Resolver {
    fn resolve(&mut self, path: &Path) -> Result<std::rc::Rc<Resolved>, String> {
        let key = std::fs::canonicalize(path).unwrap_or_else(|_| normalize(path));
        if let Some(r) = self.done.get(&key) {
            return Ok(r.clone());
        }
        if self.stack.contains(&key) {
            let chain: Vec<String> = self.stack.iter().chain([&key]).map(|p| p.display().to_string()).collect();
            return Err(format!("Circularity detected while resolving configuration: {}", chain.join(" -> ")));
        }
        if self.stack.len() >= MAX_EXTENDS_DEPTH {
            return Err(format!("The tsconfig 'extends' chain is deeper than {MAX_EXTENDS_DEPTH} configs at '{}'", path.display()));
        }
        self.stack.push(key.clone());
        let r = self.resolve_uncached(path);
        self.stack.pop();
        let r = std::rc::Rc::new(r?);
        self.done.insert(key, r.clone());
        Ok(r)
    }

    /// Base configs first, then the config's own options (`parseConfig`)
    fn resolve_uncached(&mut self, path: &Path) -> Result<Resolved, String> {
        let mut raw = read_config(path)?;
        let dir = normalize(path.parent().unwrap_or(Path::new(".")));
        let root_dir = self.root_dir.to_string_lossy().to_string();
        let mut r = Resolved { raw: Map::new(), root_dirs: None, paths: None, base_url: None, has_out_dir: false, has_root_dir: false, out_dir: None, declaration_dir: None };
        let extends: Vec<String> = match raw.get("extends") {
            Some(Value::String(s)) => vec![s.clone()],
            Some(Value::Array(a)) => a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect(),
            _ => Vec::new(),
        };
        let mut inherited: [Option<Value>; 3] = Default::default();
        for e in extends {
            let e = e.replace("${configDir}", &root_dir);
            let Some(base_path) = resolve_extends(&e, &dir) else { continue };
            let base = self.resolve(&base_path)?;
            r.root_dirs = base.root_dirs.clone().or(r.root_dirs.take());
            r.paths = base.paths.clone().or(r.paths.take());
            r.base_url = base.base_url.clone().or(r.base_url.take());
            r.out_dir = base.out_dir.clone().or(r.out_dir.take());
            r.declaration_dir = base.declaration_dir.clone().or(r.declaration_dir.take());
            r.has_out_dir |= base.has_out_dir;
            r.has_root_dir |= base.has_root_dir;
            // `include`/`exclude`/`files` the config doesn't set come from the last base that
            // does, relative to that base's directory (`applyExtendedConfig`)
            let base_dir = normalize(base_path.parent().unwrap_or(Path::new(".")));
            let rel_dir = relative_posix(&dir, &base_dir);
            for (slot, prop) in inherited.iter_mut().zip(SPEC_PROPERTIES) {
                if truthy(raw.get(prop)) || !truthy(base.raw.get(prop)) {
                    continue;
                }
                let rebase = |v: &Value| match v.as_str() {
                    Some(s) if !s.starts_with("${configDir}") && !Path::new(s).is_absolute() => Value::String(rebase_spec(&rel_dir, s)),
                    _ => v.clone(),
                };
                *slot = Some(match &base.raw[prop] {
                    Value::Array(a) => Value::Array(a.iter().map(rebase).collect()),
                    v => rebase(v),
                });
            }
        }
        for (slot, prop) in inherited.into_iter().zip(SPEC_PROPERTIES) {
            if let Some(v) = slot {
                raw.insert(prop.into(), v);
            }
        }
        let resolve = |p: &str| -> PathBuf { normalize(&dir.join(p.replace("${configDir}", &root_dir))) };
        if let Some(Value::Object(opts)) = raw.get("compilerOptions") {
            if let Some(Value::Array(dirs)) = opts.get("rootDirs") {
                r.root_dirs = Some(dirs.iter().filter_map(Value::as_str).map(resolve).collect());
            }
            r.has_out_dir |= opts.get("outDir").is_some_and(|v| !v.is_null());
            r.has_root_dir |= opts.get("rootDir").is_some_and(|v| !v.is_null());
            if let Some(Value::String(d)) = opts.get("outDir") {
                r.out_dir = Some(resolve(d));
            }
            if let Some(Value::String(d)) = opts.get("declarationDir") {
                r.declaration_dir = Some(resolve(d));
            }
            if let Some(Value::String(b)) = opts.get("baseUrl") {
                r.base_url = Some(resolve(b));
            }
            if let Some(Value::Object(paths)) = opts.get("paths") {
                let mut paths = paths.clone();
                for v in paths.values_mut() {
                    if let Value::Array(a) = v {
                        for s in a.iter_mut() {
                            if let Value::String(st) = s {
                                *st = st.replace("${configDir}", &root_dir);
                            }
                        }
                    }
                }
                r.paths = Some((paths, dir.clone()));
            }
        }
        r.raw = raw;
        Ok(r)
    }
}

pub fn parse_config(path: &Path) -> Result<ParsedConfig, String> {
    let root_dir = normalize(path.parent().unwrap_or(Path::new(".")));
    let mut resolver = Resolver { root_dir, done: Default::default(), stack: Vec::new() };
    let r = resolver.resolve(path)?;
    drop(resolver);
    let r = std::rc::Rc::try_unwrap(r).unwrap_or_else(|_| unreachable!("the resolver is gone"));
    Ok(ParsedConfig {
        raw: r.raw,
        root_dirs: r.root_dirs,
        paths: r.paths,
        base_url: r.base_url,
        has_out_dir: r.has_out_dir,
        has_root_dir: r.has_root_dir,
        out_dir: r.out_dir,
        declaration_dir: r.declaration_dir,
    })
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

/// `path.relative(from, to)` with `/` separators, `.` for the same path; across Windows drives
/// (no relative path) the absolute target
pub fn relative_posix(from: &Path, to: &Path) -> String {
    relative_posix_str(&normalize(from).to_string_lossy(), &normalize(to).to_string_lossy(), cfg!(windows))
}

fn relative_posix_str(from: &str, to: &str, windows: bool) -> String {
    match crate::svelte2tsx::rewrite_imports::relative_parts(from, to, windows) {
        Ok(parts) if parts.is_empty() => ".".into(),
        Ok(parts) => parts.join("/"),
        Err(absolute) => absolute,
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

    #[test]
    fn relative_across_windows_drives() {
        assert_eq!(relative_posix_str(r"C:\p\.svelte-check", r"C:\p\src\x.ts", true), "../src/x.ts");
        assert_eq!(relative_posix_str(r"C:\p\.svelte-check", r"D:\lib\tsconfig.json", true), "D:/lib/tsconfig.json");
        assert_eq!(relative_posix_str(r"C:\p", r"c:\P", true), ".");
    }

    fn temp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("rusvelte-tsconfig-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        normalize(&std::fs::canonicalize(&d).unwrap())
    }

    #[test]
    fn extends_cycles_are_errors() {
        let d = temp_dir("cycle");
        std::fs::write(d.join("tsconfig.json"), r#"{ "extends": "./a.json" }"#).unwrap();
        std::fs::write(d.join("a.json"), r#"{ "extends": "./b.json" }"#).unwrap();
        std::fs::write(d.join("b.json"), r#"{ "extends": "./a.json" }"#).unwrap();
        let e = parse_config(&d.join("tsconfig.json")).err().unwrap();
        assert!(e.starts_with("Circularity detected"), "{e}");
        assert!(e.contains("a.json -> ") && e.ends_with("a.json"), "{e}");
        // a config extending itself, twice: an error, not 2^n reads
        std::fs::write(d.join("self.json"), r#"{ "extends": ["./self.json", "./self.json"] }"#).unwrap();
        assert!(parse_config(&d.join("self.json")).err().unwrap().starts_with("Circularity detected"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn extends_diamond_and_depth() {
        let d = temp_dir("diamond");
        // every level extends the next one twice: read once each thanks to the cache
        for i in 0..40 {
            let next = format!("./c{}.json", i + 1);
            std::fs::write(d.join(format!("c{i}.json")), format!(r#"{{ "extends": ["{next}", "{next}"] }}"#)).unwrap();
        }
        std::fs::write(d.join("c40.json"), r#"{ "compilerOptions": { "outDir": "out" }, "include": ["src"] }"#).unwrap();
        let p = parse_config(&d.join("c0.json")).unwrap();
        assert_eq!(p.out_dir, Some(d.join("out")));
        assert_eq!(p.raw["include"], serde_json::json!(["src"]));
        // too long a chain is reported
        for i in 0..70 {
            std::fs::write(d.join(format!("l{i}.json")), format!(r#"{{ "extends": "./l{}.json" }}"#, i + 1)).unwrap();
        }
        std::fs::write(d.join("l70.json"), "{}").unwrap();
        assert!(parse_config(&d.join("l0.json")).err().unwrap().contains("deeper than"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn inherited_specs() {
        let d = temp_dir("inherit");
        std::fs::create_dir_all(d.join(".svelte-kit")).unwrap();
        std::fs::create_dir_all(d.join("base")).unwrap();
        std::fs::write(
            d.join(".svelte-kit/tsconfig.json"),
            r#"{ "include": ["ambient.d.ts", "./types/**/$types.d.ts", "../src/**/*.svelte", "${configDir}/x.ts"], "exclude": ["../node_modules/**"] }"#,
        )
        .unwrap();
        std::fs::write(d.join("base/tsconfig.json"), r#"{ "files": ["a.ts"], "exclude": ["dist"] }"#).unwrap();
        // the last base that sets a property wins; the config's own properties win over both
        std::fs::write(d.join("tsconfig.json"), r#"{ "extends": ["./.svelte-kit/tsconfig.json", "./base/tsconfig.json"], "files": [] }"#).unwrap();
        let p = parse_config(&d.join("tsconfig.json")).unwrap();
        assert_eq!(
            p.raw["include"],
            serde_json::json!([".svelte-kit/ambient.d.ts", ".svelte-kit/types/**/$types.d.ts", "src/**/*.svelte", "${configDir}/x.ts"])
        );
        assert_eq!(p.raw["exclude"], serde_json::json!(["base/dist"]));
        assert_eq!(p.raw["files"], serde_json::json!([]));
        let _ = std::fs::remove_dir_all(&d);
    }
}
