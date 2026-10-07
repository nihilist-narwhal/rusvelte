//! Compare our parser against the oracle produced by `oracle/gen.mjs`.
//! Usage: compare <corpus dir> <expected dir> [filter]

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde_json::Value;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let corpus = Path::new(&args[1]);
    let expected_dir = Path::new(&args[2]);
    let filter = args.get(3).cloned();
    let verbose = std::env::var("VERBOSE").is_ok();
    let legacy = std::env::var("LEGACY").is_ok();

    let manifest: Vec<Value> = serde_json::from_str(&fs::read_to_string(expected_dir.join("manifest.json")).unwrap()).unwrap();
    let (mut pass, mut fail, mut soft) = (0, 0, 0);
    let mut reasons: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for entry in &manifest {
        let id = entry["id"].as_str().unwrap();
        let rel = entry["rel"].as_str().unwrap();
        if let Some(f) = &filter {
            if !rel.contains(f.as_str()) {
                continue;
            }
        }
        let loose = entry["loose"].as_bool().unwrap_or(false);
        let source = fs::read_to_string(corpus.join(rel)).unwrap();
        let expected: Value = serde_json::from_str(&fs::read_to_string(expected_dir.join(format!("{id}.json"))).unwrap()).unwrap();

        let actual = std::panic::catch_unwind(|| {
            if legacy {
                svelte_rs::parse_legacy(&source, loose)
            } else {
                svelte_rs::parse_modern(&source, loose)
            }
        });
        let reason = match (actual, &expected) {
            (Err(_), _) => Some("panic".to_string()),
            (Ok(Ok(ast)), Value::Object(m)) if m.contains_key("ok") => {
                let ast = normalize(ast);
                if ast == m["ok"] {
                    None
                } else {
                    let path = first_diff(&m["ok"], &ast, String::new());
                    if verbose {
                        eprintln!("--- {rel}\n{path}");
                    }
                    Some(format!("ast: {}", path.split(':').next().unwrap_or("").split('.').filter(|s| !s.chars().all(|c| c.is_ascii_digit() || c == '[' || c == ']')).last().unwrap_or("")))
                }
            }
            (Ok(Ok(_)), _) => Some(format!("expected error {}", expected["error"]["code"])),
            (Ok(Err(err)), Value::Object(m)) if m.contains_key("error") => {
                let exp = &m["error"];
                let pos = exp["position"].as_array().map(|p| (p[0].as_u64().unwrap() as usize, p[1].as_u64().unwrap() as usize));
                if exp["code"] != err.code {
                    Some(format!("error code: expected {} got {}", exp["code"], err.code))
                } else if pos != err.position {
                    if err.code == "js_parse_error" {
                        soft += 1;
                        None
                    } else {
                        Some(format!("error position ({})", err.code))
                    }
                } else if err.code != "js_parse_error" && exp["message"].as_str() != Some(err.first_line()) {
                    Some(format!("error message ({})", err.code))
                } else {
                    None
                }
            }
            (Ok(Err(err)), _) => Some(format!("unexpected error {} {}", err.code, err.first_line())),
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
    for (reason, files) in &sorted {
        println!("{:5}  {}   e.g. {}", files.len(), reason, files[0]);
    }
    println!("\npass {pass}  fail {fail}  (js_parse_error position differs but code matches: {soft})");
}

/// Round-trip through a string so numbers compare like the oracle's
fn normalize(v: Value) -> Value {
    serde_json::from_str(&serde_json::to_string(&v).unwrap()).unwrap()
}

fn first_diff(expected: &Value, actual: &Value, path: String) -> String {
    match (expected, actual) {
        (Value::Object(a), Value::Object(b)) => {
            for (k, va) in a {
                match b.get(k) {
                    None => return format!("{path}.{k}: missing (expected {})", short(va)),
                    Some(vb) if va != vb => return first_diff(va, vb, format!("{path}.{k}")),
                    _ => {}
                }
            }
            for (k, vb) in b {
                if !a.contains_key(k) {
                    return format!("{path}.{k}: unexpected {}", short(vb));
                }
            }
            format!("{path}: ?")
        }
        (Value::Array(a), Value::Array(b)) => {
            if a.len() != b.len() {
                return format!("{path}: length {} vs {} (expected {} got {})", a.len(), b.len(), short(expected), short(actual));
            }
            for (i, (va, vb)) in a.iter().zip(b).enumerate() {
                if va != vb {
                    return first_diff(va, vb, format!("{path}[{i}]"));
                }
            }
            format!("{path}: ?")
        }
        _ => format!("{path}: expected {} got {}", short(expected), short(actual)),
    }
}

fn short(v: &Value) -> String {
    let s = v.to_string();
    if s.len() > 300 { format!("{}…", &s[..300]) } else { s }
}
