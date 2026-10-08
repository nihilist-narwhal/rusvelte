//! Compare `svelte_rs::analyze::compile_diagnostics` with the output of
//! `oracle/gen_compile.mjs` (`compile(source, { dev: true, generate: false, filename })`).
//!
//!   cargo run --release --bin compare_compile -- <corpus dir> <oracle out dir>
//!
//! VERBOSE=1 prints the first differences; FILTER=<substring> limits the files. The compile
//! options the oracle used (`runes`, `customElement`, `experimental.async`) are read from
//! `options.json` in the oracle's output.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{json, Value};
use svelte_rs::analyze::{compile_diagnostics_with, CompileOptions, Diagnostic, Position};

fn pos(p: &Option<Position>) -> Value {
    match p {
        Some(p) => json!({ "line": p.line, "column": p.column, "character": p.character }),
        None => Value::Null,
    }
}

fn diag(d: &Diagnostic) -> Value {
    json!({ "code": d.code, "message": d.message, "start": pos(&d.start), "end": pos(&d.end) })
}

fn norm(v: &Value) -> Value {
    // the oracle omits start/end when undefined
    let mut v = v.clone();
    if let Value::Object(m) = &mut v {
        for k in ["start", "end"] {
            if !m.contains_key(k) {
                m.insert(k.into(), Value::Null);
            }
        }
    }
    v
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let corpus = Path::new(&args[1]);
    let out = Path::new(&args[2]);
    let verbose = std::env::var("VERBOSE").is_ok();
    let filter = std::env::var("FILTER").ok();
    let options = match std::fs::read_to_string(out.join("options.json")) {
        Ok(json) => {
            let v: Value = serde_json::from_str(&json).unwrap();
            CompileOptions {
                runes: v.get("runes").and_then(Value::as_bool),
                custom_element: v.get("customElement").and_then(Value::as_bool).unwrap_or(false),
                experimental_async: v.pointer("/experimental/async").and_then(Value::as_bool).unwrap_or(false),
            }
        }
        Err(_) => CompileOptions::default(),
    };

    let manifest: Vec<Value> = serde_json::from_str(&std::fs::read_to_string(out.join("manifest.json")).unwrap()).unwrap();

    let mut stats: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    let mut reasons: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut total_time = std::time::Duration::ZERO;

    for entry in &manifest {
        let id = entry["id"].as_str().unwrap();
        let rel = entry["rel"].as_str().unwrap();
        if let Some(f) = &filter {
            if !rel.contains(f.as_str()) {
                continue;
            }
        }
        let expected: Value = serde_json::from_str(&std::fs::read_to_string(out.join(format!("{id}.json"))).unwrap()).unwrap();
        if expected.get("crash").is_some() {
            continue;
        }
        let source = std::fs::read_to_string(corpus.join(rel)).unwrap();
        let filename = Path::new(rel).file_name().unwrap().to_str().unwrap();

        let t = std::time::Instant::now();
        let result = std::panic::catch_unwind(|| compile_diagnostics_with(&source, filename, &options));
        total_time += t.elapsed();

        let category = if expected.get("error").is_some() {
            "error files"
        } else if expected["warnings"].as_array().is_some_and(|w| !w.is_empty()) {
            "files with warnings"
        } else {
            "files without warnings"
        };
        let stat = stats.entry(category).or_default();
        stat.1 += 1;

        let actual = match &result {
            Ok(Ok(warnings)) => json!({ "warnings": warnings.iter().map(diag).collect::<Vec<_>>() }),
            Ok(Err(error)) => json!({ "error": diag(error) }),
            Err(_) => json!({ "panic": true }),
        };
        let expected = match expected.get("error") {
            Some(e) => json!({ "error": norm(e) }),
            None => json!({ "warnings": expected["warnings"].as_array().unwrap().iter().map(norm).collect::<Vec<_>>() }),
        };
        if actual == expected {
            stat.0 += 1;
            continue;
        }

        let reason = reason(&expected, &actual);
        if verbose {
            eprintln!("--- {rel}: {reason}");
            eprintln!("  expected: {}", short(&expected));
            eprintln!("  actual:   {}", short(&actual));
        }
        reasons.entry(reason).or_default().push(rel.to_string());
    }

    let mut sorted: Vec<_> = reasons.into_iter().collect();
    sorted.sort_by_key(|(_, files)| std::cmp::Reverse(files.len()));
    for (reason, files) in &sorted {
        println!("{:5}  {}  (e.g. {})", files.len(), reason, files[0]);
    }
    let (mut ok, mut all) = (0, 0);
    for (k, (o, a)) in &stats {
        println!("{k}: {o}/{a}");
        ok += o;
        all += a;
    }
    println!("total: {ok}/{all} match ({:.1} ms in compile_diagnostics)", total_time.as_secs_f64() * 1000.0);
}

fn short(v: &Value) -> String {
    let s = v.to_string();
    if s.len() > 1500 { format!("{}…", &s[..1500]) } else { s }
}

fn codes(v: &Value) -> Vec<String> {
    v["warnings"].as_array().map_or(Vec::new(), |w| w.iter().map(|w| w["code"].as_str().unwrap_or("").to_string()).collect())
}

fn reason(expected: &Value, actual: &Value) -> String {
    if actual.get("panic").is_some() {
        return "panic".into();
    }
    match (expected.get("error"), actual.get("error")) {
        (Some(e), Some(a)) => {
            if e["code"] != a["code"] {
                return format!("error {} instead of {}", a["code"].as_str().unwrap_or(""), e["code"].as_str().unwrap_or(""));
            }
            if e["message"] != a["message"] {
                return format!("error message differs: {}", e["code"].as_str().unwrap_or(""));
            }
            format!("error position differs: {}", e["code"].as_str().unwrap_or(""))
        }
        (Some(e), None) => format!("missing error {}", e["code"].as_str().unwrap_or("")),
        (None, Some(a)) => format!("unexpected error {}", a["code"].as_str().unwrap_or("")),
        (None, None) => {
            let (ec, ac) = (codes(expected), codes(actual));
            if ec != ac {
                for c in &ec {
                    if ec.iter().filter(|x| *x == c).count() > ac.iter().filter(|x| *x == c).count() {
                        return format!("missing warning {c}");
                    }
                }
                for c in &ac {
                    if ac.iter().filter(|x| *x == c).count() > ec.iter().filter(|x| *x == c).count() {
                        return format!("extra warning {c}");
                    }
                }
                return "warning order differs".into();
            }
            let (ew, aw) = (expected["warnings"].as_array().unwrap(), actual["warnings"].as_array().unwrap());
            for (e, a) in ew.iter().zip(aw) {
                if e["message"] != a["message"] {
                    return format!("message differs: {}", e["code"].as_str().unwrap_or(""));
                }
                if e != a {
                    return format!("position differs: {}", e["code"].as_str().unwrap_or(""));
                }
            }
            "differs".into()
        }
    }
}
