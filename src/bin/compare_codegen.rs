//! Compare `rusvelte::transform::compile` with the output of `oracle/gen_codegen.mjs`.
//!
//!   cargo run --release --bin compare_codegen -- <oracle out dir> [client|server]
//!
//! Each record holds the options it was compiled with; a missing `rootDir` is the oracle's
//! working directory (`oracle/`), as `compile` defaults it to `process.cwd()`. `VERBOSE=1`
//! prints the first differences, `FILTER=<substring>` limits the records, `GROUPS=n` the
//! number of failure groups shown, `MODULES=1` / `MODULES=0` only modules / components.
//! Modules (`module: true`) are compiled with `compile_module`.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;
use rusvelte::transform::options::{CompileOptions, Generate};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let dir = Path::new(&args[1]);
    let only = args.get(2).map(String::as_str);
    let verbose = std::env::var("VERBOSE").is_ok();
    let filter = std::env::var("FILTER").ok();
    // `MODULES=1`: only modules, `MODULES=0`: only components
    let modules: Option<bool> = std::env::var("MODULES").ok().map(|m| m == "1");
    let groups_shown: usize = std::env::var("GROUPS").ok().and_then(|g| g.parse().ok()).unwrap_or(25);
    // `MAPS=1` also compares the source maps' mappings, `WARNINGS=1` the warnings and `metadata.runes`
    let check_maps = std::env::var("MAPS").is_ok();
    let check_warnings = std::env::var("WARNINGS").is_ok();
    let oracle_cwd = Path::new(env!("CARGO_MANIFEST_DIR")).join("oracle");

    let manifest: Vec<Value> = serde_json::from_str(&std::fs::read_to_string(dir.join("manifest.json")).unwrap()).unwrap();
    let (mut total, mut matched, mut skipped) = (0, 0, 0);
    let (mut error_total, mut error_matched) = (0, 0);
    let mut groups: BTreeMap<String, (usize, String)> = BTreeMap::new();
    let mut shown = 0;
    let started = std::time::Instant::now();
    let mut compile_time = std::time::Duration::ZERO;

    for entry in &manifest {
        let id = entry["id"].as_str().unwrap();
        if filter.as_ref().is_some_and(|f| !id.contains(f.as_str())) {
            continue;
        }
        let record: Value = serde_json::from_str(&std::fs::read_to_string(dir.join(format!("{id}.json"))).unwrap()).unwrap();
        if let Some(g) = only {
            if record["generate"].as_str() != Some(g) {
                continue;
            }
        }
        let is_module = record["module"] == Value::Bool(true);
        if modules == Some(!is_module) {
            continue;
        }
        // `compileModule` errors are compared by code; components' errors are left to the
        // diagnostics tools
        let expected_error = record.get("error").map(|e| e["code"].as_str());
        if expected_error.is_some() && !(is_module && expected_error.flatten().is_some()) {
            skipped += 1;
            continue;
        }
        let source_path = entry["source"].as_str().unwrap();
        // stripped `.svelte.ts` modules live in `<dir>/_src` (the manifest may name another
        // checkout); relative paths are relative to the oracle's directory, which is the main
        // checkout's when this runs in a worktree without `experiments/` or `svelte-upstream/`
        let source_path = match source_path.rfind("/_src/") {
            Some(i) => dir.join(&source_path[i + 1..]),
            None => {
                let local = oracle_cwd.join(source_path);
                let main = dir.join("..").join("..").join("..").join("oracle").join(source_path);
                if !local.exists() && main.exists() { main } else { local }
            }
        };
        let source = std::fs::read_to_string(&source_path).unwrap().replace("\r\n", "\n");
        let mut options = CompileOptions::from_json(&record["options"]);
        if options.root_dir.is_none() {
            options.root_dir = Some(oracle_cwd.to_string_lossy().into_owned());
        }
        if let Some(Some(code)) = expected_error {
            error_total += 1;
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| rusvelte::transform::compile_module(&source, &options)));
            let got = match &result {
                Err(_) => "panic".to_string(),
                Ok(Err(e)) => e.code.to_string(),
                Ok(Ok(_)) => "no error".to_string(),
            };
            if got == code {
                error_matched += 1;
            } else {
                let g = groups.entry(format!("error: expected {code} got {got}")).or_insert((0, id.to_string()));
                g.0 += 1;
            }
            continue;
        }
        if options.generate == Generate::None {
            skipped += 1;
            continue;
        }
        total += 1;
        let t = std::time::Instant::now();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if is_module {
                rusvelte::transform::compile_module(&source, &options)
            } else {
                rusvelte::transform::compile(&source, &options)
            }
        }));
        compile_time += t.elapsed();
        let expected_js = normalize(record["js"].as_str().unwrap_or(""));
        let (key, detail) = match result {
            Err(_) => ("panic".to_string(), String::new()),
            Ok(Err(e)) => (format!("error {}", e.code), e.message.clone()),
            Ok(Ok(out)) => {
                let js = normalize(&out.js);
                if js != expected_js {
                    first_diff(&expected_js, &js)
                } else if out.css.as_ref().map(|c| c.code.as_str()) != record["css"].as_str() {
                    ("css differs".to_string(), String::new())
                } else if check_maps && record.get("js_map").is_some() && Some(out.js_mappings.as_str()) != record["js_map"]["mappings"].as_str() {
                    first_diff_mappings("js map", record["js_map"]["mappings"].as_str().unwrap_or(""), &out.js_mappings)
                } else if check_maps
                    && record.get("css_map").is_some()
                    && out.css.as_ref().map(|c| c.mappings.as_str()) != record["css_map"]["mappings"].as_str()
                {
                    first_diff_mappings("css map", record["css_map"]["mappings"].as_str().unwrap_or(""), out.css.as_ref().map_or("", |c| c.mappings.as_str()))
                } else if check_warnings && record.get("warnings").is_some() && warnings_json(&out.warnings) != record["warnings"] {
                    let expected: Vec<String> = record["warnings"].as_array().unwrap().iter().map(|w| w["code"].as_str().unwrap_or("").to_string()).collect();
                    let actual: Vec<&str> = out.warnings.iter().map(|w| w.code).collect();
                    (format!("warnings differ: expected {expected:?} got {actual:?}"), String::new())
                } else if check_warnings && record.get("runes").is_some() && Some(out.runes) != record["runes"].as_bool() {
                    ("metadata.runes differs".to_string(), String::new())
                } else {
                    matched += 1;
                    continue;
                }
            }
        };
        let g = groups.entry(key.clone()).or_insert((0, id.to_string()));
        g.0 += 1;
        if verbose && shown < 5 {
            shown += 1;
            println!("--- {id}\n{key}\n{detail}");
        }
    }

    let mut sorted: Vec<_> = groups.into_iter().collect();
    sorted.sort_by(|a, b| b.1 .0.cmp(&a.1 .0));
    for (key, (count, example)) in sorted.iter().take(groups_shown) {
        println!("{count:5}  {}  (e.g. {example})", key.lines().next().unwrap_or(""));
    }
    if error_total > 0 {
        println!("module errors: {error_matched}/{error_total} match");
    }
    println!(
        "total: {matched}/{total} match ({skipped} skipped), {:.1} ms compiling, {:.1} s overall",
        compile_time.as_secs_f64() * 1000.0,
        started.elapsed().as_secs_f64()
    );
}

/// The `generated by Svelte vX` comment of modules
fn normalize(code: &str) -> String {
    code.to_string()
}

/// The first differing line, as a grouping key with both versions
fn first_diff(expected: &str, actual: &str) -> (String, String) {
    let e: Vec<&str> = expected.lines().collect();
    let a: Vec<&str> = actual.lines().collect();
    for i in 0..e.len().max(a.len()) {
        let el = e.get(i).copied().unwrap_or("<eof>");
        let al = a.get(i).copied().unwrap_or("<eof>");
        if el != al {
            let key = format!("expected `{}` got `{}`", shorten(el.trim()), shorten(al.trim()));
            let context = format!(
                "line {}:\n  expected: {el}\n  actual:   {al}\n--- expected ---\n{}\n--- actual ---\n{}",
                i + 1,
                e[i.saturating_sub(3)..(i + 4).min(e.len())].join("\n"),
                a[i.saturating_sub(3)..(i + 4).min(a.len())].join("\n")
            );
            return (key, context);
        }
    }
    ("identical lines?".into(), String::new())
}

fn shorten(s: &str) -> String {
    let s: String = s.chars().take(70).collect();
    s
}

/// The warnings as the oracle records them: `[{ code, position }]`
fn warnings_json(warnings: &[rusvelte::analyze::Warning]) -> Value {
    Value::Array(
        warnings
            .iter()
            .map(|w| serde_json::json!({ "code": w.code, "position": w.position.map(|(s, e)| vec![s, e]) }))
            .collect(),
    )
}

/// The first generated line whose mappings differ
fn first_diff_mappings(what: &str, expected: &str, actual: &str) -> (String, String) {
    let e: Vec<&str> = expected.split(';').collect();
    let a: Vec<&str> = actual.split(';').collect();
    for i in 0..e.len().max(a.len()) {
        let el = e.get(i).copied().unwrap_or("<eof>");
        let al = a.get(i).copied().unwrap_or("<eof>");
        if el != al {
            return (format!("{what} differs (line {})", i + 1), format!("expected: {el}\nactual:   {al}"));
        }
    }
    (format!("{what} differs"), String::new())
}
