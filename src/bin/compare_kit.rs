//! Compare the kit-file port (svelte2tsx::kit) against oracle/gen_kit.mjs output.
//! Usage: compare_kit <oracle.json> [filter]   (VERBOSE=1 prints the differences)
//! Works for the stress runs too (the entries then carry upsertKitFile's arguments).
use std::fs;
use std::path::PathBuf;

use serde_json::Value;
use rusvelte::svelte2tsx::kit::{is_kit_file, to_original_pos, upsert_kit_file, AddedCode, KitFilesSettings};
use rusvelte::svelte2tsx::rewrite_imports::RewriteExternalImports;

fn added_code_json(a: &AddedCode) -> Value {
    serde_json::json!({
        "generatedPos": a.generated_pos,
        "originalPos": a.original_pos,
        "length": a.length,
        "inserted": a.inserted,
        "total": a.total,
    })
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let oracle: Value = serde_json::from_str(&fs::read_to_string(&args[1]).expect("oracle file")).expect("oracle json");
    let filter = args.get(2).cloned();
    let verbose = std::env::var("VERBOSE").is_ok();
    let workspace = oracle["workspacePath"].as_str().unwrap();
    let s = &oracle["settings"];
    let settings = KitFilesSettings {
        server_hooks_path: s["serverHooksPath"].as_str().unwrap().into(),
        client_hooks_path: s["clientHooksPath"].as_str().unwrap().into(),
        universal_hooks_path: s["universalHooksPath"].as_str().unwrap().into(),
        params_path: s["paramsPath"].as_str().unwrap().into(),
    };
    let emit_dir = format!("{workspace}/.svelte-kit/.svelte-check/svelte");
    let (mut files, mut is_kit_ok, mut kit, mut text_ok, mut both_ok) = (0, 0, 0, 0, 0);
    let mut failures = Vec::new();
    let (mut mapped, mut mapped_ok) = (0, 0);
    for entry in oracle["entries"].as_array().unwrap() {
        let rel = entry["rel"].as_str().unwrap();
        if filter.as_ref().is_some_and(|f| !rel.contains(f.as_str())) {
            continue;
        }
        files += 1;
        let path = format!("{workspace}/{rel}");
        // the arguments upsertKitFile got (stress runs use a made-up kit file name)
        let file_name = entry["fileName"].as_str().map_or_else(|| path.clone(), str::to_string);
        let expected_kit = entry["isKit"].as_bool().unwrap();
        if entry.get("fileName").is_some() || is_kit_file(&path, &settings) == expected_kit {
            is_kit_ok += 1;
        } else {
            failures.push(format!("{rel}: isKitFile differs (expected {expected_kit})"));
        }
        if !expected_kit {
            continue;
        }
        kit += 1;
        let text = fs::read_to_string(&path).unwrap();
        let rewrite = RewriteExternalImports {
            source_path: PathBuf::from(&file_name),
            generated_path: PathBuf::from(entry["generatedPath"].as_str().map_or_else(|| format!("{emit_dir}/{rel}"), str::to_string)),
            workspace_path: PathBuf::from(entry["workspacePath"].as_str().unwrap_or(workspace)),
        };
        let actual = std::panic::catch_unwind(|| upsert_kit_file(&file_name, &text, &settings, Some(&rewrite), None));
        let expected = &entry["result"];
        let (t_ok, a_ok) = match (&actual, expected) {
            (Err(_), _) => {
                failures.push(format!("{rel}: panic"));
                (false, false)
            }
            (Ok(None), e) if e.is_null() || e.get("error").is_some() => (true, true),
            (Ok(Some(out)), e) if e.get("text").is_some() => {
                let t_ok = e["text"].as_str() == Some(out.text.as_str());
                let actual_added: Vec<Value> = out.added_code.iter().map(added_code_json).collect();
                let a_ok = e["addedCode"].as_array().is_some_and(|exp| exp == &actual_added);
                // toOriginalPos (negative JS positions are 0 here), with the same addedCode
                for m in e["mapped"].as_array().into_iter().flatten().filter(|_| a_ok) {
                    let (pos, orig, inside) = (m[0].as_u64().unwrap() as usize, m[1].as_i64().unwrap().max(0) as usize, m[2].as_bool().unwrap());
                    mapped += 1;
                    if to_original_pos(pos, &out.added_code) == (orig, inside) {
                        mapped_ok += 1;
                    } else {
                        failures.push(format!("{rel}: toOriginalPos({pos}) differs: expected {:?}", (orig, inside)));
                    }
                }
                if !t_ok || !a_ok {
                    failures.push(format!("{rel}: {}", if t_ok { "addedCode differs" } else { "text differs" }));
                    if verbose {
                        eprintln!("--- {rel}\n expected: {}\n actual:   {}", e["addedCode"], Value::Array(actual_added));
                        if !t_ok {
                            let exp = e["text"].as_str().unwrap_or("");
                            let at = exp.bytes().zip(out.text.bytes()).take_while(|(a, b)| a == b).count();
                            let from = at.saturating_sub(60);
                            eprintln!(" expected text: {:?}\n actual text:   {:?}", exp.get(from..(at + 80).min(exp.len())), out.text.get(from..(at + 80).min(out.text.len())));
                        }
                    }
                }
                (t_ok, a_ok)
            }
            (Ok(a), e) => {
                failures.push(format!("{rel}: expected {}, got {}", if e.is_null() { "nothing" } else { "a result/error" }, if a.is_some() { "a result" } else { "nothing" }));
                (false, false)
            }
        };
        text_ok += t_ok as usize;
        both_ok += (t_ok && a_ok) as usize;
    }
    for f in &failures {
        println!("{f}");
    }
    println!("{files} files: isKitFile {is_kit_ok}/{files}; {kit} kit files: text {text_ok}/{kit}, text+addedCode {both_ok}/{kit}; toOriginalPos {mapped_ok}/{mapped}");
}
