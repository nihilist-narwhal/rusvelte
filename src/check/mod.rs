//! A native `svelte-check --tsgo`: convert every component with the svelte2tsx port (in
//! parallel), write them next to an overlay tsconfig the way svelte-check does, run
//! TypeScript 7 on it, and map the diagnostics back.
//!
//! Two deliberate differences from svelte-check 4.7: excludes that cover the cache directory
//! are dropped (svelte-check excludes its own output when the tsconfig excludes `.svelte-kit`),
//! and `package.json` subpath imports (`#lib/*`) get `paths` that try the generated files
//! first (svelte-check only redirects relative imports and `paths`).

pub mod map;
pub mod tsc;
pub mod tsconfig;
pub mod writer;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{json, Map, Value};

use crate::svelte2tsx::{svelte2tsx_full, Svelte2TsxOptions};
use map::{Code, Diagnostic, Position, Range};
use tsc::Severity;
use tsconfig::{normalize, relative_posix};
use writer::{FileDiagnostics, Format, Threshold};

pub struct CheckOptions {
    pub workspace: PathBuf,
    pub tsconfig: PathBuf,
    pub ignore: Vec<String>,
    pub format: Format,
    pub threshold: Option<Threshold>,
    pub fail_on_warnings: bool,
    /// `js`, `svelte` (compiler warnings) — `css` isn't supported yet
    pub sources: Vec<String>,
    /// per-code `ignore` / `error`
    pub compiler_warnings: HashMap<String, String>,
    pub colors: bool,
    pub threads: usize,
    /// print timings to stderr
    pub timings: bool,
}

const SHIMS: [(&str, &str); 2] = [
    ("svelte-shims-v4.d.ts", include_str!("shims/svelte-shims-v4.d.ts")),
    ("svelte-jsx-v4.d.ts", include_str!("shims/svelte-jsx-v4.d.ts")),
];

/// A converted component
struct Entry {
    source_path: PathBuf,
    out_path: PathBuf,
    is_ts_file: bool,
    source: String,
    code: String,
}

/// `isTsSvelte`
fn is_ts_svelte(text: &str) -> bool {
    static SCRIPT: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::RegexBuilder::new(r#"<script\b((?:\s+[^=>'"/\s]+(?:=(?:"[^"]*"|'[^']*'|[^>\s]+))?)*)\s*>"#).case_insensitive(true).build().unwrap()
    });
    static LANG: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::RegexBuilder::new(r#"\blang\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s"'=<>`]+))"#).case_insensitive(true).build().unwrap()
    });
    SCRIPT.captures_iter(text).any(|m| {
        let attrs = m.get(1).map_or("", |a| a.as_str());
        LANG.captures(attrs).is_some_and(|l| {
            let v = l.get(1).or(l.get(2)).or(l.get(3)).map_or("", |v| v.as_str()).to_lowercase();
            v == "ts" || v == "typescript"
        })
    })
}

/// `findFiles`: everything under the workspace except `node_modules` and dot directories
fn find_files(dir: &Path, workspace: &Path, ignored: &[Box<dyn Fn(&str) -> bool + Sync>], out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = rd.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let name = e.file_name();
        let name = name.to_string_lossy();
        let path = e.path();
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() {
            if name == "node_modules" || name.starts_with('.') {
                continue;
            }
            find_files(&path, workspace, ignored, out);
        } else if name.ends_with(".svelte") {
            let rel = relative_posix(workspace, &path);
            if !ignored.iter().any(|i| i(&rel)) {
                out.push(path);
            }
        }
    }
}

/// `createIgnored`
fn create_ignored(patterns: &[String]) -> Result<Vec<Box<dyn Fn(&str) -> bool + Sync>>, String> {
    patterns
        .iter()
        .map(|p| {
            let mut i = p.as_str();
            if let Some(s) = i.strip_suffix("**") {
                i = s;
            }
            let err = || "Invalid svelte-check --ignore pattern: Only ** at the start or end is supported".to_string();
            if let Some(s) = i.strip_prefix("**") {
                if s.contains('*') {
                    return Err(err());
                }
                let s = s.to_string();
                Ok(Box::new(move |path: &str| path.contains(&s)) as Box<dyn Fn(&str) -> bool + Sync>)
            } else {
                if i.contains('*') {
                    return Err(err());
                }
                let s = i.to_string();
                Ok(Box::new(move |path: &str| path.starts_with(&s)) as Box<dyn Fn(&str) -> bool + Sync>)
            }
        })
        .collect()
}

fn cache_dir(workspace: &Path) -> PathBuf {
    let kit = workspace.join(".svelte-kit");
    if kit.is_dir() {
        kit.join(".svelte-check")
    } else {
        workspace.join(".svelte-check")
    }
}

/// `getOutputPaths`: `++Foo.svelte.ts` and `Foo.d.svelte.ts` mirroring the workspace
fn output_paths(workspace: &Path, emit_dir: &Path, source: &Path, is_ts: bool) -> (PathBuf, PathBuf) {
    let rel = source.strip_prefix(workspace).unwrap_or(source);
    let rel = rel.to_string_lossy();
    let base = rel.strip_suffix(".svelte").unwrap_or(&rel);
    let base_out = emit_dir.join(format!("{base}.svelte.{}", if is_ts { "ts" } else { "js" }));
    let dir = base_out.parent().unwrap().to_path_buf();
    let out = dir.join(format!("++{}", base_out.file_name().unwrap().to_string_lossy()));
    let dts = emit_dir.join(format!("{base}.d.svelte.ts"));
    (out, dts)
}

fn timed<T>(enabled: bool, label: &str, f: impl FnOnce() -> T) -> T {
    let start = std::time::Instant::now();
    let r = f();
    if enabled {
        eprintln!("[timing] {label}: {:.1} ms", start.elapsed().as_secs_f64() * 1000.0);
    }
    r
}

/// Run the check; returns the summary
pub fn run(opts: &CheckOptions, out: &mut impl std::io::Write) -> Result<writer::Summary, String> {
    let workspace = normalize(&opts.workspace);
    let tsconfig_path = normalize(&opts.tsconfig);
    let tsconfig_dir = tsconfig_path.parent().unwrap().to_path_buf();
    let cache = cache_dir(&workspace);
    let emit_dir = cache.join("svelte");
    let overlay_path = cache.join("tsconfig.json");
    let ignored = create_ignored(&opts.ignore)?;
    let use_ts = opts.sources.iter().any(|s| s == "js");
    let use_svelte = opts.sources.iter().any(|s| s == "svelte");

    // find and convert the components
    let files = timed(opts.timings, "find files", || {
        let mut files = Vec::new();
        find_files(&workspace, &workspace, &ignored, &mut files);
        files
    });
    // start on the compiler warnings right away (they don't depend on the emit)
    let svelte_warnings = if use_svelte { Some(svelte_warnings_start(&workspace, &cache, &files)?) } else { None };

    let _ = std::fs::remove_dir_all(&emit_dir);
    std::fs::create_dir_all(&emit_dir).map_err(|e| e.to_string())?;
    let entries: Vec<Option<Entry>> = timed(opts.timings, "svelte2tsx + write", || {
        let next = AtomicUsize::new(0);
        let results: Vec<std::sync::Mutex<Option<Entry>>> = files.iter().map(|_| std::sync::Mutex::new(None)).collect();
        std::thread::scope(|s| {
            for _ in 0..opts.threads.max(1) {
                s.spawn(|| loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(path) = files.get(i) else { break };
                    let Ok(source) = std::fs::read_to_string(path) else { continue };
                    let is_ts = is_ts_svelte(&source);
                    let (out_path, dts_path) = output_paths(&workspace, &emit_dir, path, is_ts);
                    let o = Svelte2TsxOptions {
                        filename: Some(path.to_string_lossy().to_string()),
                        is_ts_file: is_ts,
                        emit_jsdoc: true,
                        ..Default::default()
                    };
                    // svelte2tsx errors are left to the compiler diagnostics
                    let Ok(r) = svelte2tsx_full(&source, &o, false) else { continue };
                    let _ = std::fs::create_dir_all(out_path.parent().unwrap());
                    let import = format!("./{}", out_path.file_name().unwrap().to_string_lossy());
                    if std::fs::write(&out_path, &r.code).is_err()
                        || std::fs::write(&dts_path, format!("export {{ default }} from \"{import}\";\nexport * from \"{import}\";\n")).is_err()
                    {
                        continue;
                    }
                    *results[i].lock().unwrap() = Some(Entry { source_path: path.clone(), out_path, is_ts_file: is_ts, source, code: r.code });
                });
            }
        });
        results.into_iter().map(|m| m.into_inner().unwrap()).collect()
    });
    let entries: Vec<Entry> = entries.into_iter().flatten().collect();

    let mut by_file: Vec<FileDiagnostics> = Vec::new();
    let mut index: HashMap<PathBuf, usize> = HashMap::new();
    for e in &entries {
        index.insert(e.source_path.clone(), by_file.len());
        by_file.push(FileDiagnostics { path: e.source_path.clone(), text: e.source.clone(), diagnostics: Vec::new() });
    }

    // type-check while the compiler warnings finish
    let tsgo = if use_ts {
        timed(opts.timings, "overlay tsconfig", || write_overlay(&tsconfig_path, &tsconfig_dir, &workspace, &cache, &overlay_path, &entries))?;
        let exe = tsc::find_tsgo(&tsconfig_dir)?;
        Some((std::time::Instant::now(), tsc::start(&exe, &overlay_path, &workspace)?))
    } else {
        None
    };

    // compiler warnings come first in each file
    if let Some(handle) = svelte_warnings {
        let results = timed(opts.timings, "svelte compiler warnings (wait)", || svelte_warnings_finish(handle))?;
        for (path, diags) in results {
            let mapped = map_compiler_diagnostics(&path, diags, &opts.compiler_warnings);
            match index.get(&path) {
                Some(&i) => by_file[i].diagnostics.extend(mapped),
                None => {
                    if !mapped.is_empty() {
                        let text = std::fs::read_to_string(&path).unwrap_or_default();
                        index.insert(path.clone(), by_file.len());
                        by_file.push(FileDiagnostics { path, text, diagnostics: mapped });
                    }
                }
            }
        }
    }

    if let Some((started, child)) = tsgo {
        let diags = tsc::finish(child, &workspace)?;
        if opts.timings {
            eprintln!("[timing] tsgo (from start): {:.1} ms", started.elapsed().as_secs_f64() * 1000.0);
        }
        let mapped = timed(opts.timings, "map diagnostics", || map_ts_diagnostics(&diags, &entries, &tsconfig_path, opts.threads));
        for (path, text, diags) in mapped {
            match index.get(&path) {
                Some(&i) => by_file[i].diagnostics.extend(diags),
                None => {
                    index.insert(path.clone(), by_file.len());
                    by_file.push(FileDiagnostics { path, text, diagnostics: diags });
                }
            }
        }
    }

    let summary = writer::write(out, opts.format, opts.threshold, &workspace, &by_file, opts.colors).map_err(|e| e.to_string())?;
    Ok(summary)
}

/// `writeOverlayTsconfig`
fn write_overlay(tsconfig_path: &Path, tsconfig_dir: &Path, workspace: &Path, cache: &Path, overlay_path: &Path, entries: &[Entry]) -> Result<(), String> {
    let overlay_dir = cache;
    let parsed = tsconfig::parse_config(tsconfig_path)?;
    let raw = &parsed.raw;
    let base_root_dirs: Vec<PathBuf> = parsed.root_dirs.clone().unwrap_or_else(|| vec![tsconfig_dir.to_path_buf()]);
    let mut root_dirs: Vec<String> = Vec::new();
    for d in base_root_dirs.iter().cloned().chain([cache.join("svelte")]) {
        let r = relative_posix(overlay_dir, &d);
        if !root_dirs.contains(&r) {
            root_dirs.push(r);
        }
    }
    let specs = |key: &str| -> Option<Vec<String>> {
        match raw.get(key) {
            None | Some(Value::Null) => None,
            Some(Value::Array(a)) => Some(a.iter().map(|v| v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string())).collect()),
            Some(v) => Some(vec![v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string())]),
        }
    };
    let rebase = |spec: &str| -> String {
        static CONFIG_DIR: std::sync::LazyLock<regex::Regex> =
            std::sync::LazyLock::new(|| regex::RegexBuilder::new(r"^\$\{configDir\}").case_insensitive(true).build().unwrap());
        let resolved = if CONFIG_DIR.is_match(spec) {
            tsconfig_dir.join(CONFIG_DIR.replace(spec, ".").as_ref())
        } else {
            tsconfig_dir.join(spec)
        };
        relative_posix(overlay_dir, &resolved)
    };
    let virtual_spec = |spec: &str| -> String {
        static CONFIG_DIR: std::sync::LazyLock<regex::Regex> =
            std::sync::LazyLock::new(|| regex::RegexBuilder::new(r"^\$\{configDir\}").case_insensitive(true).build().unwrap());
        let n = CONFIG_DIR.replace(spec, ".");
        let n = n.strip_suffix(".svelte").map(|b| format!("{b}.d.svelte.ts")).unwrap_or_else(|| n.to_string());
        format!("svelte/{n}")
    };
    let raw_include = specs("include");
    let raw_exclude = specs("exclude");
    let raw_files = specs("files").unwrap_or_default();

    let mut files: Vec<String> = raw_files.iter().filter(|f| !f.ends_with(".svelte")).map(|f| rebase(f)).collect();
    // (svelte-check maps every `files` entry to a virtual one; only `.svelte` ones exist)
    files.extend(raw_files.iter().filter(|f| f.ends_with(".svelte")).map(|f| virtual_spec(f)));
    for (name, content) in SHIMS {
        let p = cache.join(name);
        std::fs::write(&p, content).map_err(|e| e.to_string())?;
        files.push(relative_posix(overlay_dir, &p));
    }
    let mut seen = std::collections::HashSet::new();
    files.retain(|f| seen.insert(f.clone()));

    let mut include: Vec<String> = raw_include.iter().flatten().map(|s| rebase(s)).collect();
    include.extend(raw_include.iter().flatten().map(|s| virtual_spec(s)));
    let mut exclude: Vec<String> = raw_exclude.iter().flatten().map(|s| rebase(s)).collect();
    exclude.extend(raw_exclude.iter().flatten().map(|s| virtual_spec(s)));
    exclude.extend(entries.iter().map(|e| relative_posix(overlay_dir, &e.source_path)));
    let mut seen = std::collections::HashSet::new();
    exclude.retain(|e| seen.insert(e.clone()));
    // don't exclude our own output (a tsconfig excluding `.svelte-kit` would)
    exclude.retain(|e| {
        let abs = normalize(&overlay_dir.join(e));
        !(abs == cache || cache.starts_with(&abs))
    });

    let mut compiler_options = Map::new();
    compiler_options.insert("rootDirs".into(), json!(root_dirs));
    compiler_options.insert("allowArbitraryExtensions".into(), json!(true));
    compiler_options.insert("noEmit".into(), json!(true));
    compiler_options.insert("incremental".into(), json!(false));
    compiler_options.insert("tsBuildInfoFile".into(), json!(relative_posix(overlay_dir, &cache.join("tsbuildinfo.json"))));
    let mut paths = rebase_paths(&parsed, tsconfig_dir, overlay_dir);
    add_subpath_import_paths(&mut paths, workspace, overlay_dir, &cache.join("svelte"));
    if !paths.is_empty() {
        compiler_options.insert("paths".into(), Value::Object(paths));
    }

    let mut overlay = Map::new();
    overlay.insert("extends".into(), json!(relative_posix(overlay_dir, tsconfig_path)));
    overlay.insert("compilerOptions".into(), Value::Object(compiler_options));
    overlay.insert("files".into(), json!(files));
    if !include.is_empty() {
        overlay.insert("include".into(), json!(include));
    }
    if !exclude.is_empty() {
        overlay.insert("exclude".into(), json!(exclude));
    }
    if let Some(Value::Array(refs)) = raw.get("references") {
        let refs: Vec<Value> = refs
            .iter()
            .map(|r| {
                let mut r = r.clone();
                if let Some(p) = r.get("path").and_then(Value::as_str).map(str::to_string) {
                    r["path"] = json!(relative_posix(overlay_dir, &tsconfig_dir.join(p)));
                }
                r
            })
            .collect();
        overlay.insert("references".into(), Value::Array(refs));
    }
    let text = serde_json::to_string_pretty(&Value::Object(overlay)).unwrap();
    std::fs::write(overlay_path, text).map_err(|e| e.to_string())
}

/// `rebasePathsConfig` (tsgo mode: no `baseUrl`)
fn rebase_paths(parsed: &tsconfig::ParsedConfig, tsconfig_dir: &Path, overlay_dir: &Path) -> Map<String, Value> {
    let mut out = Map::new();
    let Some((paths, base)) = &parsed.paths else { return out };
    for (key, specs) in paths {
        let mut result = Vec::new();
        for spec in specs.as_array().into_iter().flatten().filter_map(Value::as_str) {
            let absolute = normalize(&base.join(spec));
            result.push(json!(relative_posix(overlay_dir, &absolute)));
            let rel_to_tsconfig = relative_posix(tsconfig_dir, &absolute);
            if !rel_to_tsconfig.starts_with("../") {
                let virtual_pattern = overlay_dir.join("svelte").join(&rel_to_tsconfig);
                result.push(json!(format!("./{}", relative_posix(overlay_dir, &virtual_pattern))));
            }
        }
        out.insert(key.clone(), Value::Array(result));
    }
    out
}

/// `package.json` `imports` (`#lib/*`) as `paths` that try the generated files first, since
/// rootDirs don't apply to them
fn add_subpath_import_paths(paths: &mut Map<String, Value>, workspace: &Path, overlay_dir: &Path, emit_dir: &Path) {
    let Ok(text) = std::fs::read_to_string(workspace.join("package.json")) else { return };
    let Ok(Value::Object(pkg)) = serde_json::from_str::<Value>(&text) else { return };
    let Some(Value::Object(imports)) = pkg.get("imports") else { return };
    for (key, target) in imports {
        if paths.contains_key(key) {
            continue;
        }
        let target = match target {
            Value::String(s) => Some(s.clone()),
            Value::Object(conditions) => ["types", "import", "default"].iter().find_map(|c| conditions.get(*c).and_then(Value::as_str).map(str::to_string)),
            _ => None,
        };
        let Some(target) = target.filter(|t| t.starts_with("./")) else { continue };
        paths.insert(
            key.clone(),
            json!([
                format!("./{}", relative_posix(overlay_dir, &emit_dir.join(&target[2..]))),
                relative_posix(overlay_dir, &workspace.join(&target[2..])),
            ]),
        );
    }
}

/// `mapCliDiagnosticsToLsp`: group by file, map the ones in generated files
fn map_ts_diagnostics(diags: &[tsc::CliDiagnostic], entries: &[Entry], tsconfig: &Path, threads: usize) -> Vec<(PathBuf, String, Vec<Diagnostic>)> {
    let by_out: HashMap<&Path, &Entry> = entries.iter().map(|e| (e.out_path.as_path(), e)).collect();
    let mut groups: Vec<(PathBuf, Vec<&tsc::CliDiagnostic>)> = Vec::new();
    let mut pos: HashMap<PathBuf, usize> = HashMap::new();
    for d in diags {
        let key = d.file_path.clone().unwrap_or_else(|| tsconfig.to_path_buf());
        match pos.get(&key) {
            Some(&i) => groups[i].1.push(d),
            None => {
                pos.insert(key.clone(), groups.len());
                groups.push((key, vec![d]));
            }
        }
    }
    let results: Vec<std::sync::Mutex<Option<(PathBuf, String, Vec<Diagnostic>)>>> = groups.iter().map(|_| Default::default()).collect();
    let next = AtomicUsize::new(0);
    std::thread::scope(|s| {
        for _ in 0..threads.max(1) {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some((path, file_diags)) = groups.get(i) else { break };
                let owned: Vec<tsc::CliDiagnostic> = file_diags.iter().map(|d| (*d).clone()).collect();
                let r = match by_out.get(path.as_path()) {
                    Some(entry) => (entry.source_path.clone(), entry.source.clone(), map_entry(entry, &owned)),
                    None => {
                        let text = std::fs::read_to_string(path).unwrap_or_default();
                        let source = if is_typescript_file(path) { "ts" } else { "js" };
                        (path.clone(), text, owned.iter().map(|d| map::map_plain_diagnostic(d, source)).collect())
                    }
                };
                *results[i].lock().unwrap() = Some(r);
            });
        }
    });
    results.into_iter().filter_map(|m| m.into_inner().unwrap()).collect()
}

fn is_typescript_file(p: &Path) -> bool {
    matches!(p.extension().and_then(|e| e.to_str()), Some("ts" | "tsx" | "mts" | "cts"))
}

/// Map one component's diagnostics (re-running svelte2tsx for the source map, as the
/// language server does)
fn map_entry(entry: &Entry, diags: &[tsc::CliDiagnostic]) -> Vec<Diagnostic> {
    let verbatim = crate::svelte2tsx::htmlx::find_verbatim_elements(&entry.source);
    let script_lang = |module: bool| {
        verbatim.iter().filter(|v| !v.is_style).find_map(|v| {
            let is_module = v.attributes.iter().any(|a| (a.name == "context" && a.value.is_some_and(|x| x.2 == "module")) || a.name == "module");
            if is_module != module {
                return None;
            }
            let lang = v.attributes.iter().find(|a| a.name == "lang").or_else(|| v.attributes.iter().find(|a| a.name == "type"));
            Some(lang.and_then(|a| a.value).map_or(String::new(), |x| x.2.to_string()))
        })
    };
    // getScriptKindFromAttributes for the instance and module scripts
    let is_ts = [script_lang(false), script_lang(true)]
        .iter()
        .flatten()
        .any(|l| matches!(l.as_str(), "ts" | "typescript" | "text/ts" | "text/typescript"));
    let o = Svelte2TsxOptions { filename: Some(entry.source_path.to_string_lossy().to_string()), is_ts_file: is_ts, emit_jsdoc: true, ..Default::default() };
    let Ok(r) = svelte2tsx_full(&entry.source, &o, true) else { return Vec::new() };
    let script_content = verbatim
        .iter()
        .filter(|v| !v.is_style)
        .map(|v| &entry.source[v.content_start..v.content_end])
        .next()
        .unwrap_or("");
    let ts_check = map::ts_check_comment(script_content);

    // the template, for the checks that look at nodes
    let blanked = crate::svelte2tsx::htmlx::blank_verbatim_content(&entry.source, &verbatim);
    let alloc = oxc_allocator::Allocator::default();
    let component = crate::parse(&alloc, &blanked, false).ok();
    let legacy = component.as_ref().map(|c| crate::legacy::convert(&c.ast, &c.root, &blanked));
    let file = map::SvelteFile {
        source: &entry.source,
        generated: &entry.code,
        mappings: r.mappings.as_deref().unwrap_or(&[]),
        exported_names: &r.exported_names,
        source_kind: if is_ts { "ts" } else { "js" },
        ts_check,
        nodes: legacy.as_ref().map(|l| &l.children[..]),
    };
    let _ = entry.is_ts_file;
    map::map_svelte_diagnostics(&file, diags)
}

// --- compiler warnings (for now through the project's own Svelte compiler) -----------------

const WARNINGS_SCRIPT: &str = r#"
const { createRequire } = require('node:module');
const fs = require('node:fs');
const path = require('node:path');
const [workspace, listFile] = process.argv.slice(2);
const req = createRequire(path.join(workspace, 'noop.js'));
const { compile } = req('svelte/compiler');
const files = JSON.parse(fs.readFileSync(listFile, 'utf8'));
const out = {};
const pos = (p) => p && { line: p.line, column: p.column };
for (const f of files) {
    const text = fs.readFileSync(f, 'utf8');
    try {
        const r = compile(text, { dev: true, generate: false, filename: f });
        out[f] = { warnings: r.warnings.map((w) => ({ code: w.code, message: w.message, start: pos(w.start), end: pos(w.end) })) };
    } catch (e) {
        out[f] = { error: { code: e.code, message: e.message, start: pos(e.start), end: pos(e.end) } };
    }
}
process.stdout.write(JSON.stringify(out));
"#;

struct WarningsHandle(std::process::Child);

fn svelte_warnings_start(workspace: &Path, cache: &Path, files: &[PathBuf]) -> Result<WarningsHandle, String> {
    std::fs::create_dir_all(cache).map_err(|e| e.to_string())?;
    let script = cache.join("compiler-warnings.cjs");
    let list = cache.join("compiler-warnings-files.json");
    std::fs::write(&script, WARNINGS_SCRIPT).map_err(|e| e.to_string())?;
    std::fs::write(&list, serde_json::to_string(&files.iter().map(|f| f.to_string_lossy()).collect::<Vec<_>>()).unwrap()).map_err(|e| e.to_string())?;
    let child = std::process::Command::new("node")
        .arg(&script)
        .arg(workspace)
        .arg(&list)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .map_err(|e| format!("Failed to run node for the compiler warnings: {e}"))?;
    Ok(WarningsHandle(child))
}

/// Per file: `Ok(warnings)` or `Err(error)`, as JSON
type CompilerResult = Result<Vec<Value>, Value>;

fn svelte_warnings_finish(h: WarningsHandle) -> Result<Vec<(PathBuf, CompilerResult)>, String> {
    let out = h.0.wait_with_output().map_err(|e| e.to_string())?;
    let v: Value = serde_json::from_slice(&out.stdout).map_err(|e| format!("compiler warnings: {e}"))?;
    let Value::Object(m) = v else { return Ok(Vec::new()) };
    Ok(m
        .into_iter()
        .map(|(k, v)| {
            let r = match v.get("error") {
                Some(e) => Err(e.clone()),
                None => Ok(v.get("warnings").and_then(Value::as_array).cloned().unwrap_or_default()),
            };
            (PathBuf::from(k), r)
        })
        .collect())
}

/// The Svelte plugin's `getDiagnostics` for compiler output (no preprocessors)
fn map_compiler_diagnostics(path: &Path, result: CompilerResult, settings: &HashMap<String, String>) -> Vec<Diagnostic> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let verbatim = crate::svelte2tsx::htmlx::find_verbatim_elements(&text);
    let lang_of = |style: bool| -> String {
        let tag = verbatim.iter().find(|v| v.is_style == style);
        let attr = tag.and_then(|t| t.attributes.iter().find(|a| a.name == "lang").or_else(|| t.attributes.iter().find(|a| a.name == "type")));
        attr.and_then(|a| a.value).map_or(String::new(), |v| v.2.trim_start_matches("text/").to_string())
    };
    let script_lang = lang_of(false);
    let style_lang = lang_of(true);
    let ignore_script = !script_lang.is_empty() && script_lang != "ts";
    let ignore_style = !style_lang.is_empty();
    let range_of = |v: &Value| -> Range {
        let p = |p: Option<&Value>| -> Option<Position> {
            let p = p?;
            Some(Position { line: p["line"].as_i64()? - 1, character: p["column"].as_i64()? })
        };
        let start = p(v.get("start")).unwrap_or(Position { line: 0, character: 0 });
        let end = p(v.get("end")).unwrap_or(start);
        Range { start, end }
    };
    let description = |code: &str| -> Option<String> {
        let first = code.chars().next()?;
        (first.is_ascii_lowercase() && (code.contains('-') || code.contains('_')))
            .then(|| format!("https://svelte.dev/docs/svelte/compiler-warnings#{}", code.replace('-', "_")))
    };
    let adjust = |mut r: Range| -> Range {
        r.start.character = r.start.character.max(0);
        r.end.character = r.end.character.max(0);
        if r.start.line < 0 {
            r.start = Position { line: 0, character: 0 };
        }
        if r.end.line < 0 {
            r.end = Position { line: 0, character: 0 };
        }
        if r.end.line < r.start.line || (r.end.line == r.start.line && r.end.character < r.start.character) {
            r.start = r.end;
        }
        r
    };
    match result {
        Ok(warnings) => warnings
            .iter()
            .filter_map(|w| {
                let code = w["code"].as_str().unwrap_or("").to_string();
                let setting = settings.get(&code).map(String::as_str);
                if setting == Some("ignore") {
                    return None;
                }
                if ignore_script && !code.starts_with("a11y") {
                    return None;
                }
                if ignore_style && code == "css_unused_selector" {
                    return None;
                }
                Some(Diagnostic {
                    range: adjust(range_of(w)),
                    severity: if setting == Some("error") { Severity::Error } else { Severity::Warning },
                    source: "svelte",
                    message: w["message"].as_str().unwrap_or("").to_string(),
                    code_description: description(&code),
                    code: Some(Code::Str(code)),
                    position_unknown: false,
                })
            })
            .collect(),
        Err(e) => {
            let message = e["message"].as_str().unwrap_or("").to_string();
            let has_lang = !script_lang.is_empty() || !style_lang.is_empty();
            if message.contains("expected") && has_lang {
                return Vec::new();
            }
            let code = e["code"].as_str().map(str::to_string);
            vec![Diagnostic {
                range: adjust(range_of(&e)),
                severity: Severity::Error,
                source: "svelte",
                code_description: code.as_deref().and_then(|c| {
                    let first = c.chars().next()?;
                    (first.is_ascii_lowercase() && (c.contains('-') || c.contains('_')))
                        .then(|| format!("https://svelte.dev/docs/svelte/compiler-errors#{}", c.replace('-', "_")))
                }),
                code: code.map(Code::Str),
                message,
                position_unknown: false,
            }]
        }
    }
}
