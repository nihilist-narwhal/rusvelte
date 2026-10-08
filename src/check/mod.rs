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

use crate::svelte2tsx::kit::{is_kit_file, to_original_pos, upsert_kit_file, AddedCode, KitFilesSettings};
use crate::svelte2tsx::rewrite_imports::RewriteExternalImports;
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
    /// keep generated files, tsgo's build info and compiler warnings between runs
    pub incremental: bool,
    pub watch: writer::WatchOutput,
}

const SHIMS: [(&str, &str); 2] = [
    ("svelte-shims-v4.d.ts", include_str!("shims/svelte-shims-v4.d.ts")),
    ("svelte-jsx-v4.d.ts", include_str!("shims/svelte-jsx-v4.d.ts")),
];

/// A converted component
struct Entry {
    source_path: PathBuf,
    out_path: PathBuf,
    dts_path: PathBuf,
    is_ts_file: bool,
    source: String,
    code: String,
    /// a SvelteKit file: the code svelte-check inserted
    kit: Option<Vec<AddedCode>>,
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
fn find_files(dir: &Path, workspace: &Path, ignored: &[Box<dyn Fn(&str) -> bool + Sync>], out: &mut Vec<PathBuf>, kit: &mut Vec<PathBuf>) {
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
            find_files(&path, workspace, ignored, out, kit);
        } else if name.ends_with(".svelte") {
            let rel = relative_posix(workspace, &path);
            if !ignored.iter().any(|i| i(&rel)) {
                out.push(path);
            }
        } else if name.ends_with(".ts") || name.ends_with(".js") {
            let rel = relative_posix(workspace, &path);
            if !ignored.iter().any(|i| i(&rel)) {
                kit.push(path);
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
    let (files, scripts) = timed(opts.timings, "find files", || {
        let mut files = Vec::new();
        let mut scripts = Vec::new();
        find_files(&workspace, &workspace, &ignored, &mut files, &mut scripts);
        (files, scripts)
    });
    let _ = std::fs::create_dir_all(&cache);
    // the Svelte config, the way the language server resolves it (or known without Node)
    let static_config = static_config_guess(&workspace);
    let probe = if static_config.is_some() { None } else { config_probe_start(&workspace, &cache) };
    // Compiler warnings: natively unless preprocessors may change the code. When the config
    // mentions preprocessors, start the Node path right away; otherwise wait for the probe.
    let mut engine = if !use_svelte {
        Engine::None
    } else if probe.is_some() && config_mentions_preprocess(&workspace) {
        Engine::Node
    } else {
        Engine::Undecided
    };
    let mut warnings_cache = WarningsCache::default();
    let mut svelte_warnings = None;
    if engine == Engine::Node {
        if opts.incremental {
            warnings_cache = WarningsCache::load(&workspace, &cache, "node");
        }
        let todo: Vec<PathBuf> = files.iter().filter(|f| !warnings_cache.is_fresh(f)).cloned().collect();
        if !todo.is_empty() {
            svelte_warnings = Some(svelte_warnings_start(&workspace, &cache, &todo, (opts.threads / 2).max(1))?);
        }
    }

    let previous_outputs: Vec<PathBuf> = if opts.incremental {
        std::fs::read_to_string(cache.join("emitted.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    } else {
        let _ = std::fs::remove_dir_all(&emit_dir);
        Vec::new()
    };
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
                        rewrite_external_imports: Some(RewriteExternalImports {
                            source_path: path.clone(),
                            generated_path: out_path.clone(),
                            workspace_path: workspace.clone(),
                        }),
                        ..Default::default()
                    };
                    // svelte2tsx errors are left to the compiler diagnostics
                    let Ok(r) = svelte2tsx_full(&source, &o, false) else { continue };
                    let _ = std::fs::create_dir_all(out_path.parent().unwrap());
                    let import = format!("./{}", out_path.file_name().unwrap().to_string_lossy());
                    let dts = format!("export {{ default }} from \"{import}\";\nexport * from \"{import}\";\n");
                    if write_if_changed(&out_path, &r.code).is_err() || write_if_changed(&dts_path, &dts).is_err() {
                        continue;
                    }
                    *results[i].lock().unwrap() = Some(Entry { source_path: path.clone(), out_path, dts_path, is_ts_file: is_ts, source, code: r.code, kit: None });
                });
            }
        });
        results.into_iter().map(|m| m.into_inner().unwrap()).collect()
    });
    let mut entries: Vec<Entry> = entries.into_iter().flatten().collect();
    let svelte_entry_count = entries.len();
    let mut kit_settings = KitFilesSettings::default();
    entries.extend(emit_kit_files(&scripts, &kit_settings, &workspace, &emit_dir));
    if opts.incremental {
        // remove what earlier runs generated for files that are gone or failed now
        let current: std::collections::HashSet<&Path> = entries.iter().flat_map(|e| [e.out_path.as_path(), e.dts_path.as_path()]).collect();
        for p in &previous_outputs {
            if !current.contains(p.as_path()) {
                let _ = std::fs::remove_file(p);
            }
        }
        let list: Vec<&Path> = current.into_iter().collect();
        let _ = std::fs::write(cache.join("emitted.json"), serde_json::to_string(&list).unwrap());
    }

    let mut by_file: Vec<FileDiagnostics> = Vec::new();
    let mut index: HashMap<PathBuf, usize> = HashMap::new();
    // every converted file gets a record when svelte-check runs its Svelte diagnostics
    for e in entries.iter().filter(|_| use_svelte || opts.sources.iter().any(|s| s == "css")) {
        index.insert(e.source_path.clone(), by_file.len());
        by_file.push(FileDiagnostics { path: e.source_path.clone(), text: e.source.clone(), diagnostics: Vec::new() });
    }

    // type-check while the compiler warnings finish
    let build_info = opts.incremental.then(|| cache.join("tsbuildinfo.json"));
    let start_tsgo = |entries: &[Entry]| -> Result<(std::time::Instant, std::process::Child), String> {
        timed(opts.timings, "overlay tsconfig", || write_overlay(&tsconfig_path, &tsconfig_dir, &workspace, &cache, &overlay_path, entries, opts.incremental))?;
        let exe = tsc::find_tsgo(&tsconfig_dir)?;
        Ok((std::time::Instant::now(), tsc::start(&exe, &overlay_path, &workspace, build_info.as_deref())?))
    };
    let mut tsgo = if use_ts { Some(start_tsgo(&entries)?) } else { None };

    // the probe's results: SvelteKit's `files` settings and what the compiler warnings need
    let config = match static_config {
        Some(c) => Some(c),
        None => probe.and_then(|p| timed(opts.timings, "config probe (wait)", || config_probe_finish(p))),
    };
    if let Some(settings) = config.as_ref().and_then(|c| c.kit_files.clone()) {
        if settings != kit_settings {
            // other SvelteKit file settings (often just the defaults as absolute paths): only
            // when that changes the kit files, redo them and the type-check
            let new_kit = emit_kit_files(&scripts, &settings, &workspace, &emit_dir);
            let same = new_kit.len() == entries.len() - svelte_entry_count
                && new_kit.iter().zip(&entries[svelte_entry_count..]).all(|(a, b)| a.source_path == b.source_path && a.code == b.code);
            kit_settings = settings;
            if !same {
                let new_paths: std::collections::HashSet<&Path> = new_kit.iter().map(|e| e.out_path.as_path()).collect();
                for e in &entries[svelte_entry_count..] {
                    if !new_paths.contains(e.out_path.as_path()) {
                        let _ = std::fs::remove_file(&e.out_path);
                    }
                }
                entries.truncate(svelte_entry_count);
                entries.extend(new_kit);
                if let Some((_, mut child)) = tsgo.take() {
                    let _ = child.kill();
                    let _ = child.wait();
                    tsgo = Some(start_tsgo(&entries)?);
                }
            }
        }
    }
    let _ = &kit_settings;
    if engine == Engine::Undecided {
        engine = match &config {
            Some(c) if c.native_ok => Engine::Native,
            Some(_) => Engine::Node,
            // no config: the language server's fallback preprocessor only touches `lang` scripts/styles
            None if !files.iter().any(|f| std::fs::read_to_string(f).is_ok_and(|s| has_lang_attribute(&s))) => Engine::Native,
            None => Engine::Node,
        };
        if opts.incremental {
            warnings_cache = WarningsCache::load(&workspace, &cache, if engine == Engine::Native { "native" } else { "node" });
        }
        if engine == Engine::Node {
            let todo: Vec<PathBuf> = files.iter().filter(|f| !warnings_cache.is_fresh(f)).cloned().collect();
            if !todo.is_empty() {
                svelte_warnings = Some(svelte_warnings_start(&workspace, &cache, &todo, (opts.threads / 2).max(1))?);
            }
        }
    }
    if opts.timings && use_svelte {
        eprintln!("[timing] compiler warnings engine: {engine:?}");
    }

    // compiler warnings come first in each file
    if use_svelte {
        let fresh = match svelte_warnings {
            Some(handle) => timed(opts.timings, "svelte compiler warnings (wait)", || svelte_warnings_finish(handle))?,
            None if engine == Engine::Native => {
                let todo: Vec<PathBuf> = files.iter().filter(|f| !warnings_cache.is_fresh(f)).cloned().collect();
                let options = config.as_ref().map(|c| c.compile_options.clone()).unwrap_or_default();
                let results = timed(opts.timings, "svelte compiler warnings (native)", || native_compiler_results(&todo, &options, opts.threads));
                (results, false)
            }
            None => (Vec::new(), warnings_cache.has_preprocess),
        };
        let has_preprocess = if engine == Engine::Native {
            config.as_ref().is_some_and(|c| c.has_preprocess)
        } else if fresh.0.is_empty() {
            warnings_cache.has_preprocess
        } else {
            fresh.1
        };
        warnings_cache.has_preprocess = has_preprocess;
        for (path, raw) in fresh.0 {
            warnings_cache.insert(&path, raw);
        }
        if opts.incremental {
            warnings_cache.save(&cache);
        }
        let results: Vec<(PathBuf, CompilerResult)> = files.iter().filter_map(|f| warnings_cache.get(f).map(|raw| (f.clone(), to_compiler_result(raw)))).collect();
        for (path, diags) in results {
            let mapped = map_compiler_diagnostics(&path, diags, &opts.compiler_warnings, has_preprocess);
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
        let mapped = timed(opts.timings, "map diagnostics", || map_ts_diagnostics(&diags, &entries, &workspace, &tsconfig_path, opts.threads));
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

    let summary = writer::write(out, opts.format, opts.threshold, &workspace, &by_file, opts.colors, opts.watch).map_err(|e| e.to_string())?;
    Ok(summary)
}

/// `writeOverlayTsconfig`
fn write_overlay(tsconfig_path: &Path, tsconfig_dir: &Path, workspace: &Path, cache: &Path, overlay_path: &Path, entries: &[Entry], incremental: bool) -> Result<(), String> {
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
    compiler_options.insert("incremental".into(), json!(incremental));
    compiler_options.insert("tsBuildInfoFile".into(), json!(relative_posix(overlay_dir, &cache.join("tsbuildinfo.json"))));
    // With `outDir` and no `rootDir`, TypeScript 6+ defaults `rootDir` to the config's directory,
    // which would be ours: keep the project's own default
    if parsed.has_out_dir && !parsed.has_root_dir {
        compiler_options.insert("rootDir".into(), json!(relative_posix(overlay_dir, tsconfig_dir)));
    }
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
    write_if_changed(overlay_path, &text).map_err(|e| e.to_string())
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
fn map_ts_diagnostics(diags: &[tsc::CliDiagnostic], entries: &[Entry], workspace: &Path, tsconfig: &Path, threads: usize) -> Vec<(PathBuf, String, Vec<Diagnostic>)> {
    let by_out: HashMap<&Path, &Entry> = entries.iter().map(|e| (e.out_path.as_path(), e)).collect();
    // kit files that got code inserted may still be reached through .svelte-kit/types; skip those
    let excluded: std::collections::HashSet<&Path> =
        entries.iter().filter(|e| e.kit.as_ref().is_some_and(|a| !a.is_empty())).map(|e| e.source_path.as_path()).collect();
    let diags: Vec<&tsc::CliDiagnostic> = diags.iter().filter(|d| !d.file_path.as_deref().is_some_and(|p| excluded.contains(p))).collect();
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
                    Some(entry) if entry.kit.is_some() => (entry.source_path.clone(), entry.source.clone(), map_kit_entry(entry, &owned)),
                    Some(entry) => (entry.source_path.clone(), entry.source.clone(), map_entry(entry, workspace, &owned)),
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

/// A SvelteKit file's diagnostics, mapped back through the inserted code
fn map_kit_entry(entry: &Entry, diags: &[tsc::CliDiagnostic]) -> Vec<Diagnostic> {
    let added = entry.kit.as_deref().unwrap_or(&[]);
    let source = map::U16Text::new(&entry.source);
    let generated = map::U16Text::new(&entry.code);
    let kind = if is_typescript_file(&entry.source_path) { "ts" } else { "js" };
    diags
        .iter()
        .map(|d| {
            let offset = generated.offset_at(Position { line: d.line as i64, character: d.character as i64 });
            let (start, _) = to_original_pos(offset, added);
            let (end, _) = to_original_pos(offset + d.length, added);
            Diagnostic {
                range: Range { start: source.position_at(start as i64), end: source.position_at(end as i64) },
                severity: d.severity,
                source: kind,
                message: d.message.clone(),
                code: Some(Code::Num(d.code)),
                code_description: None,
                position_unknown: false,
            }
        })
        .collect()
}

fn is_typescript_file(p: &Path) -> bool {
    matches!(p.extension().and_then(|e| e.to_str()), Some("ts" | "tsx" | "mts" | "cts"))
}

/// Map one component's diagnostics (re-running svelte2tsx for the source map, as the
/// language server does)
fn map_entry(entry: &Entry, workspace: &Path, diags: &[tsc::CliDiagnostic]) -> Vec<Diagnostic> {
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
    let o = Svelte2TsxOptions {
        filename: Some(entry.source_path.to_string_lossy().to_string()),
        is_ts_file: is_ts,
        emit_jsdoc: true,
        rewrite_external_imports: Some(RewriteExternalImports {
            source_path: entry.source_path.clone(),
            generated_path: entry.out_path.clone(),
            workspace_path: workspace.to_path_buf(),
        }),
        ..Default::default()
    };
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
const { pathToFileURL } = require('node:url');
const fs = require('node:fs');
const path = require('node:path');
const [workspace, listFile] = process.argv.slice(2);
const req = createRequire(path.join(workspace, 'noop.js'));
const { compile, preprocess } = req('svelte/compiler');

// sourcemap decoding + trace-mapping's originalPositionFor (greatest lower bound)
const B64 = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';
function decode(mappings) {
    const lines = [];
    let line = [], i = 0, col = 0, src = 0, sl = 0, sc = 0, nm = 0;
    const vlq = () => {
        let value = 0, shift = 0, c;
        do { c = B64.indexOf(mappings[i++]); value |= (c & 31) << shift; shift += 5; } while (c & 32);
        return value & 1 ? -(value >>> 1) : value >>> 1;
    };
    while (i <= mappings.length) {
        const ch = mappings[i];
        if (i === mappings.length || ch === ';') { line.sort((a, b) => a[0] - b[0]); lines.push(line); line = []; col = 0; i++; continue; }
        if (ch === ',') { i++; continue; }
        col += vlq(); const seg = [col];
        if (i < mappings.length && mappings[i] !== ',' && mappings[i] !== ';') {
            src += vlq(); sl += vlq(); sc += vlq(); seg.push(src, sl, sc);
            if (i < mappings.length && mappings[i] !== ',' && mappings[i] !== ';') { nm += vlq(); }
        }
        line.push(seg);
    }
    return lines;
}
function originalPositionFor(lines, line, column) {
    const segs = lines[line];
    if (!segs || !segs.length) return null;
    let lo = 0, hi = segs.length - 1, found = -1;
    while (lo <= hi) { const mid = (lo + hi) >> 1; if (segs[mid][0] <= column) { found = mid; lo = mid + 1; } else hi = mid - 1; }
    if (found === -1) return null;
    while (found > 0 && segs[found - 1][0] === segs[found][0]) found--;
    const s = segs[found];
    return s.length === 1 ? null : { line: s[2], character: s[3] };
}

async function loadConfig() {
    // the way the language server finds it (vite.config or svelte.config)
    try {
        const r = await req('@sveltejs/load-config').loadConfig(workspace, { traverse: false });
        if (r && 'config' in r) {
            let config = r.config;
            if ('kit' in config && !('prerender' in config)) config = { ...config, ...config.kit };
            return config;
        }
        if (r && 'error' in r) return { loadError: String(r.error) };
        return null;
    } catch {}
    for (const name of ['svelte.config.js', 'svelte.config.mjs', 'svelte.config.cjs']) {
        const p = path.join(workspace, name);
        if (fs.existsSync(p)) {
            try { return (await import(pathToFileURL(p).href)).default ?? {}; } catch (e) { return { loadError: String(e) }; }
        }
    }
    return null;
}

function wrap(preprocessors) {
    return (Array.isArray(preprocessors) ? preprocessors : [preprocessors]).map((p) => ({ markup: p.markup, script: p.script, style: p.style }));
}

(async () => {
    const config = await loadConfig();
    const files = JSON.parse(fs.readFileSync(listFile, 'utf8'));
    const out = {};
    for (const f of files) {
        const text = fs.readFileSync(f, 'utf8');
        let code = text, lines = null;
        try {
            if (config && config.preprocess) {
                const pre = await preprocess(text, wrap(config.preprocess), { filename: f });
                const result = pre.code || (pre.toString && pre.toString()) || '';
                if (result !== text) {
                    code = result;
                    const map = pre.map && (typeof pre.map === 'string' ? JSON.parse(pre.map) : pre.map);
                    lines = map && map.mappings !== undefined ? decode(typeof map.mappings === 'string' ? map.mappings : '') : null;
                }
            }
        } catch (e) {
            out[f] = { preprocessError: String(e && e.message || e) };
            continue;
        }
        // positions as the language server gets them, mapped through the preprocessor's map
        const range = (w) => {
            const start = w.start || { line: 1, column: 0 };
            const end = w.end || start;
            const r = { start: { line: start.line - 1, character: start.column }, end: { line: end.line - 1, character: end.column } };
            if (!lines) return r;
            const map = (p) => (p.line < 0 ? { line: -1, character: -1 } : originalPositionFor(lines, p.line, p.character) || { line: -1, character: -1 });
            const o = { start: map(r.start), end: map(r.end) };
            if (o.start.line === o.end.line && r.start.line === r.end.line && o.end.character - o.start.character === r.end.character - r.start.character - 1) o.end.character += 1;
            return o;
        };
        try {
            const options = { dev: true, ...((config && config.compilerOptions) || {}), generate: false, filename: f };
            const r = compile(code, options);
            out[f] = { warnings: r.warnings.map((w) => ({ code: w.code, message: w.message, range: range(w) })) };
        } catch (e) {
            out[f] = { error: { code: e.code, message: e.message, range: range(e) } };
        }
    }
    out['\0config'] = { preprocess: !!(config && config.preprocess) };
    process.stdout.write(JSON.stringify(out));
})();
"#;

struct WarningsHandle(Vec<std::process::Child>);

/// Spread the files over a few Node processes (preprocessors like PostCSS are slow)
fn svelte_warnings_start(workspace: &Path, cache: &Path, files: &[PathBuf], processes: usize) -> Result<WarningsHandle, String> {
    std::fs::create_dir_all(cache).map_err(|e| e.to_string())?;
    let script = cache.join("compiler-warnings.cjs");
    std::fs::write(&script, WARNINGS_SCRIPT).map_err(|e| e.to_string())?;
    // about 50 files per process at least, so small projects don't pay for many Node startups
    let processes = processes.clamp(1, files.len().div_ceil(50).max(1));
    let mut children = Vec::new();
    for (i, chunk) in files.chunks(files.len().div_ceil(processes).max(1)).enumerate() {
        let list = cache.join(format!("compiler-warnings-files-{i}.json"));
        std::fs::write(&list, serde_json::to_string(&chunk.iter().map(|f| f.to_string_lossy()).collect::<Vec<_>>()).unwrap()).map_err(|e| e.to_string())?;
        let child = std::process::Command::new("node")
            .arg(&script)
            .arg(workspace)
            .arg(&list)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .spawn()
            .map_err(|e| format!("Failed to run node for the compiler warnings: {e}"))?;
        children.push(child);
    }
    Ok(WarningsHandle(children))
}

/// Per file: `Ok(warnings)` or `Err(error)`, as JSON
type CompilerResult = Result<Vec<Value>, Value>;

fn svelte_warnings_finish(h: WarningsHandle) -> Result<(Vec<(PathBuf, Value)>, bool), String> {
    let mut results = Vec::new();
    let mut has_preprocess = false;
    for child in h.0 {
        let out = child.wait_with_output().map_err(|e| e.to_string())?;
        let v: Value = serde_json::from_slice(&out.stdout).map_err(|e| format!("compiler warnings: {e}"))?;
        let Value::Object(mut m) = v else { continue };
        has_preprocess |= m.remove("\0config").is_some_and(|c| c["preprocess"] == Value::Bool(true));
        results.extend(m.into_iter().map(|(k, v)| (PathBuf::from(k), v)));
    }
    Ok((results, has_preprocess))
}

fn to_compiler_result(v: &Value) -> CompilerResult {
    if let Some(e) = v.get("error") {
        Err(e.clone())
    } else if let Some(e) = v.get("preprocessError") {
        Err(json!({ "message": e, "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 0 } } }))
    } else {
        Ok(v.get("warnings").and_then(Value::as_array).cloned().unwrap_or_default())
    }
}

fn write_if_changed(path: &Path, content: &str) -> std::io::Result<()> {
    if std::fs::read(path).is_ok_and(|old| old == content.as_bytes()) {
        return Ok(());
    }
    std::fs::write(path, content)
}

fn hash_bytes(b: &[u8]) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    b.hash(&mut h);
    format!("{:016x}", h.finish())
}

/// Compiler results by file content, valid as long as the Svelte config and compiler are the same
#[derive(Default)]
struct WarningsCache {
    key: String,
    entries: Map<String, Value>,
    has_preprocess: bool,
    hashes: HashMap<PathBuf, String>,
}

impl WarningsCache {
    fn config_key(workspace: &Path) -> String {
        let mut parts = Vec::new();
        for name in [
            "svelte.config.js",
            "svelte.config.mjs",
            "svelte.config.cjs",
            "svelte.config.ts",
            "vite.config.js",
            "vite.config.mjs",
            "vite.config.ts",
            "vite.config.mts",
            "package.json",
            "node_modules/svelte/package.json",
        ] {
            parts.extend(std::fs::read(workspace.join(name)).unwrap_or_default());
            parts.push(0);
        }
        hash_bytes(&parts)
    }

    fn load(workspace: &Path, cache: &Path, engine: &str) -> Self {
        let key = format!("{engine}:{}", Self::config_key(workspace));
        let mut c = WarningsCache { key: key.clone(), ..Default::default() };
        if let Some(Value::Object(m)) = std::fs::read_to_string(cache.join("compiler-warnings.json")).ok().and_then(|t| serde_json::from_str(&t).ok()) {
            if m.get("key").and_then(Value::as_str) == Some(&key) {
                c.has_preprocess = m.get("preprocess") == Some(&Value::Bool(true));
                if let Some(Value::Object(e)) = m.get("entries") {
                    c.entries = e.clone();
                }
            }
        }
        c
    }

    fn hash(&mut self, path: &Path) -> String {
        if let Some(h) = self.hashes.get(path) {
            return h.clone();
        }
        let h = hash_bytes(&std::fs::read(path).unwrap_or_default());
        self.hashes.insert(path.to_path_buf(), h.clone());
        h
    }

    fn is_fresh(&mut self, path: &Path) -> bool {
        let h = self.hash(path);
        self.entries.get(&*path.to_string_lossy()).is_some_and(|e| e["hash"].as_str() == Some(&h))
    }

    fn insert(&mut self, path: &Path, raw: Value) {
        let h = self.hash(path);
        self.entries.insert(path.to_string_lossy().to_string(), json!({ "hash": h, "result": raw }));
    }

    fn get(&self, path: &Path) -> Option<&Value> {
        self.entries.get(&*path.to_string_lossy()).map(|e| &e["result"])
    }

    fn save(&self, cache: &Path) {
        let v = json!({ "key": self.key, "preprocess": self.has_preprocess, "entries": self.entries });
        let _ = std::fs::write(cache.join("compiler-warnings.json"), v.to_string());
    }
}

/// The Svelte plugin's `getDiagnostics` for compiler output (no preprocessors)
fn map_compiler_diagnostics(path: &Path, result: CompilerResult, settings: &HashMap<String, String>, has_preprocess: bool) -> Vec<Diagnostic> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let verbatim = crate::svelte2tsx::htmlx::find_verbatim_elements(&text);
    let lang_of = |style: bool| -> String {
        let tag = verbatim.iter().find(|v| v.is_style == style);
        let attr = tag.and_then(|t| t.attributes.iter().find(|a| a.name == "lang").or_else(|| t.attributes.iter().find(|a| a.name == "type")));
        attr.and_then(|a| a.value).map_or(String::new(), |v| v.2.trim_start_matches("text/").to_string())
    };
    let script_lang = lang_of(false);
    let style_lang = lang_of(true);
    // without preprocessors, warnings in scripts/styles in other languages are noise
    let ignore_script = !has_preprocess && !script_lang.is_empty() && script_lang != "ts";
    let ignore_style = !has_preprocess && !style_lang.is_empty();
    let range_of = |v: &Value| -> Range {
        let p = |p: &Value| Position { line: p["line"].as_i64().unwrap_or(0), character: p["character"].as_i64().unwrap_or(0) };
        Range { start: p(&v["range"]["start"]), end: p(&v["range"]["end"]) }
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
            if message.contains("expected") && has_lang && !has_preprocess {
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

// --- watch mode -----------------------------------------------------------------------------

/// The files a change to which triggers a new run (svelte-check's watcher filter)
fn watched_files(dir: &Path, cache: &Path, out: &mut Vec<(PathBuf, std::time::SystemTime, u64)>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let path = e.path();
        let name = e.file_name();
        let name = name.to_string_lossy();
        let Ok(meta) = e.metadata() else { continue };
        if meta.is_dir() {
            if name == "node_modules" || name == ".git" || path == cache {
                continue;
            }
            watched_files(&path, cache, out);
        } else {
            static ENDING: std::sync::LazyLock<regex::Regex> =
                std::sync::LazyLock::new(|| regex::Regex::new(r"\.(svelte|d\.ts|ts|js|jsx|tsx|mjs|cjs|mts|cts)$").unwrap());
            static VITE_TIMESTAMP: std::sync::LazyLock<regex::Regex> =
                std::sync::LazyLock::new(|| regex::Regex::new(r"vite\.config\.(js|ts)\.timestamp-").unwrap());
            if ENDING.is_match(&name) && !VITE_TIMESTAMP.is_match(&name) {
                out.push((path, meta.modified().unwrap_or(std::time::UNIX_EPOCH), meta.len()));
            }
        }
    }
}

/// `--watch`: run, then run again a second after files stop changing
pub fn watch(opts: &CheckOptions, out: &mut impl std::io::Write) -> ! {
    let workspace = normalize(&opts.workspace);
    let cache = cache_dir(&workspace);
    let snapshot = || {
        let mut files = Vec::new();
        watched_files(&workspace, &cache, &mut files);
        files.sort();
        files
    };
    let mut last = snapshot();
    loop {
        if let Err(e) = run(opts, out) {
            let _ = writeln!(out, "{e}");
        }
        let _ = out.flush();
        // wait for a change, then for a quiet second
        let mut changed_at: Option<std::time::Instant> = None;
        loop {
            std::thread::sleep(std::time::Duration::from_millis(250));
            let now = snapshot();
            if now != last {
                last = now;
                changed_at = Some(std::time::Instant::now());
            } else if changed_at.is_some_and(|t| t.elapsed() >= std::time::Duration::from_secs(1)) {
                break;
            }
        }
    }
}

// --- the Svelte config and native compiler warnings ----------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Engine {
    None,
    Undecided,
    Native,
    Node,
}

/// Emit the SvelteKit files among `scripts` (`emitSvelteFiles`' kit part)
fn emit_kit_files(scripts: &[PathBuf], settings: &KitFilesSettings, workspace: &Path, emit_dir: &Path) -> Vec<Entry> {
    let mut entries = Vec::new();
    for path in scripts.iter().filter(|p| is_kit_file(&p.to_string_lossy(), settings)) {
        let Ok(source) = std::fs::read_to_string(path) else { continue };
        let out_path = emit_dir.join(path.strip_prefix(workspace).unwrap_or(path));
        let rewrite = RewriteExternalImports { source_path: path.clone(), generated_path: out_path.clone(), workspace_path: workspace.to_path_buf() };
        let Some(r) = upsert_kit_file(&path.to_string_lossy(), &source, settings, Some(&rewrite), None) else { continue };
        let _ = std::fs::create_dir_all(out_path.parent().unwrap());
        if write_if_changed(&out_path, &r.text).is_err() {
            continue;
        }
        entries.push(Entry {
            source_path: path.clone(),
            dts_path: out_path.clone(),
            out_path,
            is_ts_file: path.extension().is_some_and(|e| e == "ts"),
            source,
            code: r.text,
            kit: Some(r.added_code),
        });
    }
    entries
}

const CONFIG_FILES: [&str; 11] = [
    "vite.config.js",
    "vite.config.mjs",
    "vite.config.cjs",
    "vite.config.ts",
    "vite.config.mts",
    "vite.config.cts",
    "svelte.config.js",
    "svelte.config.mjs",
    "svelte.config.cjs",
    "svelte.config.ts",
    "svelte.config.mts",
];

/// A cheap guess: does any config file mention preprocessing?
fn config_mentions_preprocess(workspace: &Path) -> bool {
    CONFIG_FILES.iter().any(|n| std::fs::read_to_string(workspace.join(n)).is_ok_and(|t| t.contains("reprocess")))
}

/// `lang=` (or `type=`) on a script, style or template tag
fn has_lang_attribute(source: &str) -> bool {
    static RE: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r#"<(script|style|template)\b[^>]*\b(lang|type)\s*="#).unwrap());
    RE.is_match(source)
}

const CONFIG_PROBE_SCRIPT: &str = r#"
import { createRequire } from 'node:module';
import { pathToFileURL } from 'node:url';
import fs from 'node:fs';
import path from 'node:path';
const ws = process.argv[2];
const req = createRequire(path.join(ws, 'noop.js'));
let result;
try {
    result = await req('@sveltejs/load-config').loadConfig(ws, { traverse: false });
} catch {
    for (const n of ['svelte.config.js', 'svelte.config.mjs', 'svelte.config.cjs']) {
        const p = path.join(ws, n);
        if (fs.existsSync(p)) {
            try { result = { config: (await import(pathToFileURL(p).href)).default ?? {} }; } catch (e) { result = { error: e }; }
            break;
        }
    }
}
const out = { found: false };
if (result && 'config' in result) {
    let config = result.config;
    const files = 'files' in config ? config.files : config.kit && config.kit.files;
    if ('kit' in config && !('prerender' in config)) config = { ...config, ...config.kit };
    const pre = config.preprocess;
    const list = pre == null ? [] : Array.isArray(pre) ? pre : [pre];
    const co = config.compilerOptions || {};
    Object.assign(out, {
        found: true,
        preprocess: list.map((p) => (p && typeof p.name === 'string' ? p.name : null)),
        compilerOptionKeys: Object.keys(co),
        runes: typeof co.runes === 'boolean' ? co.runes : null,
        customElement: co.customElement === true,
        customElementOther: co.customElement !== undefined && typeof co.customElement !== 'boolean',
        experimentalKeys: Object.keys(co.experimental || {}),
        experimentalAsync: !!(co.experimental && co.experimental.async),
        files: files ? { params: files.params, hooks: files.hooks } : null
    });
} else if (result && 'error' in result) {
    out.error = String(result.error);
}
process.stdout.write(JSON.stringify(out));
"#;

struct ConfigProbe(std::process::Child);

/// Start the probe, if the workspace has a config file (and Node)
fn config_probe_start(workspace: &Path, cache: &Path) -> Option<ConfigProbe> {
    if !CONFIG_FILES.iter().any(|n| workspace.join(n).is_file()) {
        return None;
    }
    let script = cache.join("config-probe.mjs");
    std::fs::write(&script, CONFIG_PROBE_SCRIPT).ok()?;
    let child = std::process::Command::new("node")
        .arg(&script)
        .arg(workspace)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    Some(ConfigProbe(child))
}

pub struct ProbedConfig {
    /// the compiler warnings can be computed natively: no preprocessor that changes code, and
    /// only compiler options the native analysis knows
    native_ok: bool,
    has_preprocess: bool,
    compile_options: crate::analyze::CompileOptions,
    kit_files: Option<KitFilesSettings>,
}

/// Preprocessors that never change the code
const NOOP_PREPROCESSORS: [&str; 1] = ["sveltekit:warnings"];
/// Compiler options that don't affect warnings, or that the native analysis takes
const KNOWN_COMPILER_OPTIONS: [&str; 12] =
    ["runes", "customElement", "experimental", "dev", "generate", "css", "cssHash", "hmr", "discloseVersion", "preserveComments", "preserveWhitespace", "modernAst"];

fn config_probe_finish(p: ConfigProbe) -> Option<ProbedConfig> {
    let fallback = ProbedConfig { native_ok: false, has_preprocess: false, compile_options: Default::default(), kit_files: None };
    let Ok(out) = p.0.wait_with_output() else { return Some(fallback) };
    let Ok(v) = serde_json::from_slice::<Value>(&out.stdout) else { return Some(fallback) };
    if v.get("error").is_some() {
        return Some(fallback);
    }
    if v["found"] != Value::Bool(true) {
        // no Svelte config: the language server's fallback preprocessor applies
        return None;
    }
    let preprocess: Vec<Option<&str>> = v["preprocess"].as_array().map(|a| a.iter().map(Value::as_str).collect()).unwrap_or_default();
    let noop = preprocess.iter().all(|p| p.is_some_and(|n| NOOP_PREPROCESSORS.contains(&n)));
    let keys_ok = v["compilerOptionKeys"].as_array().is_some_and(|a| a.iter().all(|k| k.as_str().is_some_and(|k| KNOWN_COMPILER_OPTIONS.contains(&k))));
    let experimental_ok = v["experimentalKeys"].as_array().is_some_and(|a| a.iter().all(|k| k == "async"));
    let custom_element_ok = v["customElementOther"] != Value::Bool(true);
    let kit_files = v["files"].as_object().map(|f| {
        let d = KitFilesSettings::default();
        let hooks = f.get("hooks");
        let s = |v: Option<&Value>, d: &str| v.and_then(Value::as_str).map_or_else(|| d.to_string(), str::to_string);
        KitFilesSettings {
            params_path: s(f.get("params"), &d.params_path),
            server_hooks_path: s(hooks.and_then(|h| h.get("server")), &d.server_hooks_path),
            client_hooks_path: s(hooks.and_then(|h| h.get("client")), &d.client_hooks_path),
            universal_hooks_path: s(hooks.and_then(|h| h.get("universal")), &d.universal_hooks_path),
        }
    });
    Some(ProbedConfig {
        native_ok: noop && keys_ok && experimental_ok && custom_element_ok,
        has_preprocess: !preprocess.is_empty(),
        compile_options: crate::analyze::CompileOptions {
            runes: v["runes"].as_bool(),
            custom_element: v["customElement"] == Value::Bool(true),
            experimental_async: v["experimentalAsync"] == Value::Bool(true),
        },
        kit_files,
    })
}

/// The compiler results the Node helper would produce, computed natively
fn native_compiler_results(files: &[PathBuf], options: &crate::analyze::CompileOptions, threads: usize) -> Vec<(PathBuf, Value)> {
    let position = |p: &Option<crate::analyze::Position>| p.as_ref().map(|p| json!({ "line": p.line as i64 - 1, "character": p.column }));
    let range = |start: &Option<crate::analyze::Position>, end: &Option<crate::analyze::Position>| {
        let s = position(start).unwrap_or_else(|| json!({ "line": 0, "character": 0 }));
        let e = position(end).unwrap_or_else(|| s.clone());
        json!({ "start": s, "end": e })
    };
    let next = AtomicUsize::new(0);
    let results: Vec<std::sync::Mutex<Option<Value>>> = files.iter().map(|_| Default::default()).collect();
    std::thread::scope(|s| {
        for _ in 0..threads.max(1) {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some(path) = files.get(i) else { break };
                let Ok(source) = std::fs::read_to_string(path) else { continue };
                let v = match crate::analyze::compile_diagnostics_with(&source, &path.to_string_lossy(), options) {
                    Ok(ws) => json!({ "warnings": ws.iter().map(|w| json!({ "code": w.code, "message": w.message, "range": range(&w.start, &w.end) })).collect::<Vec<_>>() }),
                    Err(e) => json!({ "error": { "code": e.code, "message": e.message, "range": range(&e.start, &e.end) } }),
                };
                *results[i].lock().unwrap() = Some(v);
            });
        }
    });
    files.iter().cloned().zip(results).filter_map(|(f, m)| m.into_inner().unwrap().map(|v| (f, v))).collect()
}

/// The resolved config for the SvelteKit 3 default shape, without running Node: a vite config
/// with `sveltekit(...)` and no `svelte.config`, that doesn't mention preprocessing, compiler
/// options or file locations and imports nothing local. SvelteKit then adds only its
/// `sveltekit:warnings` preprocessor, which never changes code.
fn static_config_guess(workspace: &Path) -> Option<ProbedConfig> {
    let present: Vec<&str> = CONFIG_FILES.iter().copied().filter(|n| workspace.join(n).is_file()).collect();
    if present.len() != 1 || !present[0].starts_with("vite.config.") {
        return None;
    }
    let text = std::fs::read_to_string(workspace.join(present[0])).ok()?;
    // (comments removed; a `//` inside a string only makes this more conservative)
    static COMMENTS: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| regex::Regex::new(r"(?s)/\*.*?\*/|//[^\n]*").unwrap());
    let code = COMMENTS.replace_all(&text, "");
    static LOCAL_IMPORT: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r#"(from|import|require)\s*\(?\s*['"]\.{1,2}/"#).unwrap());
    static MENTIONS: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"\b(preprocess|sveltePreprocess|vitePreprocess|compilerOptions|files|vitePlugin)\b|svelte\.config|vite-plugin-svelte").unwrap()
    });
    if !code.contains("sveltekit(") || MENTIONS.is_match(&code) || LOCAL_IMPORT.is_match(&code) {
        return None;
    }
    Some(ProbedConfig { native_ok: true, has_preprocess: true, compile_options: Default::default(), kit_files: None })
}
