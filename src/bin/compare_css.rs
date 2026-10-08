//! Compare `css_lint::style_diagnostics` against the oracle output of `oracle/css_oracle.cjs`.
//! Usage: compare_css <corpus dir> <oracle.json> [filter]
//! `VERBOSE=1` prints expected/actual for each mismatching file.
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use svelte_rs::css_lint::{CssDiagnostic, style_diagnostics};

fn to_json(d: &CssDiagnostic) -> Value {
    json!({
        "range": {
            "start": { "line": d.range.start.line, "character": d.range.start.character },
            "end": { "line": d.range.end.line, "character": d.range.end.character },
        },
        "severity": d.severity as u8,
        "message": d.message,
        "code": d.code,
        "source": d.source,
    })
}

fn run() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: compare_css <corpus dir> <oracle.json> [filter]");
        std::process::exit(2);
    }
    let corpus = Path::new(&args[1]);
    let expected: serde_json::Map<String, Value> =
        serde_json::from_str(&fs::read_to_string(&args[2]).unwrap()).unwrap();
    let filter = args.get(3).cloned();
    let verbose = std::env::var("VERBOSE").is_ok();
    // `REPEAT=n` runs style_diagnostics n times per file (for profiling; the reported time is the total)
    let repeat: u32 = std::env::var("REPEAT").ok().and_then(|r| r.parse().ok()).unwrap_or(1);

    let (mut total, mut pass, mut with_diags, mut with_diags_pass, mut n_diags) = (0, 0, 0, 0, 0);
    let mut time = Duration::ZERO;
    let mut failures = Vec::new();
    for (rel, exp) in &expected {
        if filter.as_ref().is_some_and(|f| !rel.contains(f.as_str())) {
            continue;
        }
        let path = if rel.is_empty() { corpus.to_path_buf() } else { corpus.join(rel) };
        let source = fs::read_to_string(&path).unwrap();
        let t = Instant::now();
        let actual = std::panic::catch_unwind(|| style_diagnostics(&source));
        for _ in 1..repeat {
            std::hint::black_box(style_diagnostics(&source));
        }
        time += t.elapsed();
        let actual = match actual {
            Ok(a) => Value::Array(a.iter().map(to_json).collect()),
            Err(_) => json!("panic"),
        };
        total += 1;
        let exp_len = exp.as_array().map_or(0, |a| a.len());
        n_diags += exp_len;
        if exp_len > 0 {
            with_diags += 1;
        }
        if &actual == exp {
            pass += 1;
            if exp_len > 0 {
                with_diags_pass += 1;
            }
        } else {
            failures.push(rel.clone());
            if verbose {
                println!("FAIL {rel}\n  expected: {exp}\n  actual:   {actual}");
            }
        }
    }
    for f in failures.iter().take(if verbose { 0 } else { 30 }) {
        println!("FAIL {f}");
    }
    println!(
        "{pass}/{total} files match exactly ({with_diags_pass}/{with_diags} files with diagnostics, {n_diags} expected diagnostics); style_diagnostics total {:.1} ms",
        time.as_secs_f64() * 1000.0
    );
}

fn main() {
    // deeply nested stylesheets recurse deeply, like the JS parser
    let child = std::thread::Builder::new().stack_size(256 << 20).spawn(run).unwrap();
    child.join().unwrap();
}
