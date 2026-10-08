//! Compare `svelte_rs::transform::compile_styles` with the output of
//! `oracle/gen_css_output.mjs` (`result.css` of `compile(source, { filename, generate: 'client',
//! ...compileOptions })`, and the `$$css` code of injected styles).
//!
//!   cargo run --release --bin compare_css_output -- <corpus dir> <oracle out dir>
//!
//! It also reads the records of the code generation oracle (`oracle/gen_codegen.mjs`, whose
//! manifest lists each record's `source`): then the first argument is the directory relative
//! source paths are resolved against (`oracle`), and `css` / `has_global` are compared.
//!
//! VERBOSE=1 prints the first difference of each failing file; FILTER=<substring> limits the
//! files. The compile options come from each file's oracle output. A custom `cssHash` is
//! replaced by its recorded result, after checking it got the same `filename` and `name`.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{json, Value};
use svelte_rs::transform::{compile_styles, CssHashInput, CssMode, CssOptions};

/// A JS `hash` function for evaluated `cssHash` functions (`compiler/utils.js`)
const JS_HASH: &str = "(str) => { str = str.replace(/\\r/g, ''); let hash = 5381; let i = str.length; while (i--) hash = ((hash << 5) - hash) ^ str.charCodeAt(i); return (hash >>> 0).toString(36); }";

/// A `cssHash` function recorded as `{ fn: source }`: constants directly, anything else run in node
fn css_hash_fn(src: &str) -> Box<dyn Fn(&CssHashInput) -> String> {
    let constant = src
        .strip_prefix("() =>")
        .map(str::trim)
        .and_then(|s| s.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')).or_else(|| s.strip_prefix('"').and_then(|s| s.strip_suffix('"'))));
    if let Some(c) = constant {
        let c = c.to_string();
        return Box::new(move |_: &CssHashInput| c.clone());
    }
    let src = src.to_string();
    Box::new(move |input: &CssHashInput| {
        let script = format!(
            "const src = process.argv[1]; let f; try {{ f = eval('(' + src + ')'); }} catch {{ f = Object.values(eval('({{' + src + '}})'))[0]; }} \
             const input = JSON.parse(process.argv[2]); process.stdout.write(f({{ ...input, hash: {JS_HASH} }}));"
        );
        let arg = json!({ "css": input.css, "filename": input.filename, "name": input.name }).to_string();
        let out = std::process::Command::new("node").args(["-e", &script, &src, &arg]).output().expect("node");
        String::from_utf8_lossy(&out.stdout).into_owned()
    })
}

fn options(v: &Value) -> (String, CssOptions) {
    let o = &v["options"];
    let filename = o["filename"].as_str().unwrap_or("(unknown)").to_string();
    let recorded = o.pointer("/cssHash/fn").and_then(Value::as_str).map(css_hash_fn);
    let css_hash: Option<Box<dyn Fn(&CssHashInput) -> String>> = recorded.or_else(|| v.get("cssHash").map(|h| {
        let (filename, name, result) = (h["filename"].clone(), h["name"].clone(), h["result"].as_str().unwrap_or("").to_string());
        Box::new(move |input: &CssHashInput| {
            if filename.as_str() == Some(input.filename) && name.as_str() == Some(input.name) {
                result.clone()
            } else {
                format!("cssHash-input-mismatch(filename={},name={})", input.filename, input.name)
            }
        }) as Box<dyn Fn(&CssHashInput) -> String>
    }));
    let options = CssOptions {
        dev: o["dev"].as_bool().unwrap_or(false),
        css: if o["css"].as_str() == Some("injected") { CssMode::Injected } else { CssMode::External },
        css_hash,
        root_dir: o["rootDir"].as_str().map(String::from),
        custom_element: o["customElement"].as_bool().unwrap_or(false),
        runes: o["runes"].as_bool(),
        experimental_async: o.pointer("/experimental/async").and_then(Value::as_bool).unwrap_or(false),
    };
    (filename, options)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let corpus = Path::new(&args[1]);
    let out = Path::new(&args[2]);
    let verbose = std::env::var("VERBOSE").is_ok();
    let filter = std::env::var("FILTER").ok();
    let manifest: Vec<Value> = serde_json::from_str(&std::fs::read_to_string(out.join("manifest.json")).unwrap()).unwrap();

    let mut stats: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    let mut reasons: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut total_time = std::time::Duration::ZERO;

    for entry in &manifest {
        let id = entry["id"].as_str().unwrap();
        // gen_codegen.mjs records name their `source`
        let codegen = entry.get("source").is_some();
        let rel = entry.get("rel").or(entry.get("source")).and_then(Value::as_str).unwrap();
        let name = if codegen { id } else { rel };
        if filter.as_ref().is_some_and(|f| !name.contains(f.as_str())) {
            continue;
        }
        let mut expected: Value = serde_json::from_str(&std::fs::read_to_string(out.join(format!("{id}.json"))).unwrap()).unwrap();
        // the JS compiler crashed (a stack overflow on very deep nesting): not comparable
        if expected.get("crash").is_some() {
            continue;
        }
        if codegen {
            if expected["module"].as_bool() == Some(true) {
                continue;
            }
            // normalize to this binary's records
            let css = match expected.get("css").and_then(Value::as_str) {
                Some(code) => json!({ "code": code, "hasGlobal": expected["has_global"] }),
                None => Value::Null,
            };
            expected["css"] = css;
            if let Some(e) = expected.get("error").cloned() {
                expected["error"] = e.get("code").cloned().unwrap_or(e);
            }
        }
        let rel = name;
        let source = std::fs::read_to_string(corpus.join(entry.get("rel").or(entry.get("source")).and_then(Value::as_str).unwrap()))
            .unwrap()
            .replace("\r\n", "\n");
        let (filename, options) = options(&expected);

        let t = std::time::Instant::now();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| compile_styles(&source, &filename, &options)));
        total_time += t.elapsed();

        let category = if expected.get("error").is_some() {
            "errors"
        } else if expected.get("injected").is_some_and(|i| !i.is_null()) {
            "injected styles"
        } else if expected["css"].is_null() {
            "no css"
        } else {
            "css"
        };
        let stat = stats.entry(category).or_default();
        stat.1 += 1;

        let actual = match &result {
            Ok(Ok(r)) => json!({
                "css": r.css.as_ref().map(|c| json!({ "code": c.code, "hasGlobal": c.has_global })),
                "injected": r.injected.as_ref().map(|i| json!({ "hash": i.hash, "code": i.code })),
            }),
            Ok(Err(e)) => json!({ "error": e.code }),
            Err(_) => json!({ "panic": true }),
        };
        let mut actual = actual;
        if codegen {
            // gen_codegen.mjs doesn't record injected styles
            actual.as_object_mut().unwrap().remove("injected");
        }
        let expected_v = match expected.get("error") {
            Some(e) => json!({ "error": e }),
            None if codegen => json!({ "css": expected["css"] }),
            None => json!({ "css": expected["css"], "injected": expected.get("injected").cloned().unwrap_or(Value::Null) }),
        };
        // any error counts as a match when the JS throws (the error itself is compare_compile's job)
        let ok = actual == expected_v || (expected_v.get("error").is_some() && actual.get("error").is_some());
        if ok {
            stat.0 += 1;
            continue;
        }
        let reason = reason(&expected_v, &actual);
        if verbose {
            eprintln!("--- {rel}: {reason}");
            eprintln!("{}", first_difference(&expected_v, &actual));
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
    println!("total: {ok}/{all} match ({:.1} ms in compile_styles)", total_time.as_secs_f64() * 1000.0);
}

fn code(v: &Value) -> Option<&str> {
    v.pointer("/css/code").or_else(|| v.pointer("/injected/code")).and_then(Value::as_str)
}

fn reason(expected: &Value, actual: &Value) -> String {
    if actual.get("panic").is_some() {
        return "panic".into();
    }
    match (expected.get("error"), actual.get("error")) {
        (Some(e), None) => return format!("no error (expected {e})"),
        (None, Some(a)) => return format!("error {a}"),
        _ => {}
    }
    if expected["css"].is_null() != actual["css"].is_null() || expected["injected"].is_null() != actual["injected"].is_null() {
        return "css presence differs".into();
    }
    if expected.pointer("/css/hasGlobal") != actual.pointer("/css/hasGlobal") {
        return "hasGlobal differs".into();
    }
    if expected.pointer("/injected/hash") != actual.pointer("/injected/hash") {
        return "hash differs".into();
    }
    let (e, a) = (code(expected).unwrap_or(""), code(actual).unwrap_or(""));
    // group by the first differing line, with the hash abstracted
    for (le, la) in e.lines().zip(a.lines()) {
        if le != la {
            let line = le.trim();
            let line = if line.len() > 60 { format!("{}…", &line[..line.floor_char_boundary(60)]) } else { line.to_string() };
            return format!("code differs at `{line}`");
        }
    }
    "code differs in length".into()
}

fn first_difference(expected: &Value, actual: &Value) -> String {
    let (e, a) = match (code(expected), code(actual)) {
        (Some(e), Some(a)) => (e, a),
        _ => return format!("  expected: {expected}\n  actual:   {actual}"),
    };
    let el: Vec<&str> = e.lines().collect();
    let al: Vec<&str> = a.lines().collect();
    let i = el.iter().zip(&al).position(|(x, y)| x != y).unwrap_or(el.len().min(al.len()));
    let from = i.saturating_sub(2);
    let mut s = format!("  line {}\n  expected:\n", i + 1);
    for l in el.iter().skip(from).take(5) {
        s += &format!("    |{l}\n");
    }
    s += "  actual:\n";
    for l in al.iter().skip(from).take(5) {
        s += &format!("    |{l}\n");
    }
    s
}
