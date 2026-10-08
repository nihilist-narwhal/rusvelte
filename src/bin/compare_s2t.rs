//! Compare the svelte2tsx port against oracles from oracle/gen_htmlx2jsx.mjs (and later
//! gen_svelte2tsx.mjs). Usage: compare_s2t <corpus> <expected dir> [filter]
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde_json::Value;
use svelte_rs::svelte2tsx::{htmlx2jsx, svelte2tsx, Options, Svelte2TsxOptions};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let corpus = Path::new(&args[1]);
    let expected_dir = Path::new(&args[2]);
    let filter = args.get(3).cloned();
    let verbose = std::env::var("VERBOSE").is_ok();
    let manifest: Vec<Value> = serde_json::from_str(&fs::read_to_string(expected_dir.join("manifest.json")).unwrap()).unwrap();
    let opts = Options {
        typings_namespace: "svelteHTML".into(),
        preserve_attribute_case: false,
        svelte5_plus: true,
        emit_jsdoc: false,
        is_ts_file: false,
        mode_ts: false,
        accessors: false,
        rewrite_external_imports: None,
    };
    let (mut pass, mut fail) = (0, 0);
    let mut reasons: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for entry in &manifest {
        let id = entry["id"].as_str().unwrap();
        let rel = entry["rel"].as_str().unwrap();
        if filter.as_ref().is_some_and(|f| !rel.contains(f.as_str())) {
            continue;
        }
        let source = fs::read_to_string(corpus.join(rel)).unwrap();
        let expected: Value = serde_json::from_str(&fs::read_to_string(expected_dir.join(format!("{id}.json"))).unwrap()).unwrap();
        let o = &entry["options"];
        if o["mode"].as_str() == Some("dts") {
            continue;
        }
        let actual = std::panic::catch_unwind(|| {
            if o.is_object() {
                let s2t = Svelte2TsxOptions {
                    filename: o["filename"].as_str().map(str::to_string),
                    is_ts_file: o["isTsFile"].as_bool().unwrap_or(false),
                    mode_ts: true,
                    accessors: o["accessors"].as_bool().unwrap_or(false),
                    typings_namespace: o["typingsNamespace"].as_str().unwrap_or("svelteHTML").to_string(),
                    namespace_foreign: o["namespace"].as_str() == Some("foreign"),
                    emit_jsdoc: o["emitJsDoc"].as_bool().unwrap_or(false),
                    svelte5_plus: true,
                    rewrite_external_imports: o.get("rewriteExternalImports").map(|r| svelte_rs::svelte2tsx::rewrite_imports::RewriteExternalImports {
                        source_path: o["filename"].as_str().unwrap().into(),
                        generated_path: r["generatedPath"].as_str().unwrap().into(),
                        workspace_path: r["workspacePath"].as_str().unwrap().into(),
                    }),
                };
                svelte2tsx(&source, &s2t)
            } else {
                htmlx2jsx(&source, &opts)
            }
        });
        let reason = match (actual, expected.get("ok").and_then(Value::as_str)) {
            (Err(_), _) => Some("panic".to_string()),
            (Ok(Ok(code)), Some(exp)) => {
                if code == exp {
                    None
                } else {
                    if verbose {
                        let (a, b) = first_diff(exp, &code);
                        eprintln!("--- {rel}\n  expected: {a}\n  actual:   {b}");
                    }
                    Some("output differs".into())
                }
            }
            (Ok(Ok(_)), None) => Some(format!("expected error: {}", expected["error"].as_str().unwrap_or("").chars().take(60).collect::<String>())),
            (Ok(Err(_)), None) => None,
            (Ok(Err(e)), Some(_)) => Some(format!("unexpected error: {}", e.to_string().chars().take(70).collect::<String>())),
        };
        match reason {
            None => pass += 1,
            Some(r) => {
                fail += 1;
                reasons.entry(r).or_default().push(rel.to_string());
            }
        }
    }
    let mut sorted: Vec<_> = reasons.into_iter().collect();
    sorted.sort_by_key(|(_, v)| std::cmp::Reverse(v.len()));
    for (r, files) in sorted.iter().take(25) {
        println!("{:5}  {}   e.g. {}", files.len(), r, files[0]);
    }
    println!("\npass {pass}  fail {fail}");
}

/// The first differing region of two strings, with a little context
fn first_diff(a: &str, b: &str) -> (String, String) {
    let i = a.bytes().zip(b.bytes()).take_while(|(x, y)| x == y).count();
    let start = a[..i].char_indices().rev().nth(40).map_or(0, |(p, _)| p);
    let cut = |s: &str| {
        let end = (i + 60).min(s.len());
        let mut e = end;
        while !s.is_char_boundary(e) {
            e -= 1;
        }
        format!("{:?}", &s[start.min(s.len())..e])
    };
    (cut(a), cut(b))
}
