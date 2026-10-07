//! Replay tools/fuzz_magic_string.mjs cases against src/magic_string.rs
use svelte_rs::magic_string::MagicString;

/// UTF-16 offset → byte offset
fn byte_at(s: &str, utf16: usize) -> usize {
    let mut units = 0;
    for (i, c) in s.char_indices() {
        if units >= utf16 {
            return i;
        }
        units += c.len_utf16();
    }
    s.len()
}

fn main() {
    let path = std::env::args().nth(1).expect("cases.json");
    let cases: Vec<serde_json::Value> = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let (mut pass, mut fail) = (0, 0);
    for (n, case) in cases.iter().enumerate() {
        let original = case["original"].as_str().unwrap();
        let mut ms = MagicString::new(original);
        let mut problem = None;
        for (i, op) in case["ops"].as_array().unwrap().iter().enumerate() {
            let args = op["args"].as_array().unwrap();
            let num = |k: usize| byte_at(original, args[k].as_u64().unwrap() as usize);
            let text = |k: usize| args[k].as_str().unwrap().to_string();
            let name = op["op"].as_str().unwrap();
            let r: Result<Option<String>, String> = match name {
                "overwrite" => ms.overwrite(num(0), num(1), &text(2), false).map(|_| None).map_err(|e| e.0),
                "overwriteContentOnly" => ms.overwrite(num(0), num(1), &text(2), true).map(|_| None).map_err(|e| e.0),
                "update" => ms.update(num(0), num(1), &text(2)).map(|_| None).map_err(|e| e.0),
                "remove" => ms.remove(num(0), num(1)).map(|_| None).map_err(|e| e.0),
                "move" => ms.move_(num(0), num(1), num(2)).map(|_| None).map_err(|e| e.0),
                "appendLeft" => ms.append_left(num(0), &text(1)).map(|_| None).map_err(|e| e.0),
                "appendRight" => ms.append_right(num(0), &text(1)).map(|_| None).map_err(|e| e.0),
                "prependLeft" => ms.prepend_left(num(0), &text(1)).map(|_| None).map_err(|e| e.0),
                "prependRight" => ms.prepend_right(num(0), &text(1)).map(|_| None).map_err(|e| e.0),
                "append" => { ms.append(&text(0)); Ok(None) }
                "prepend" => { ms.prepend(&text(0)); Ok(None) }
                "slice" => ms.slice(num(0), num(1)).map(Some).map_err(|e| e.0),
                _ => unreachable!(),
            };
            let expected_ok = op["ok"].as_bool().unwrap();
            match (&r, expected_ok) {
                (Ok(res), true) => {
                    if let Some(res) = res {
                        if op["result"].as_str() != Some(res.as_str()) {
                            problem = Some(format!("op {i} {name}{args:?}: slice {res:?} vs {}", op["result"]));
                        }
                    }
                }
                (Err(e), false) => {
                    // compare the message up to the location (positions are UTF-16 in JS)
                    let exp = op["error"].as_str().unwrap();
                    let strip = |m: &str| m.split('(').next().unwrap_or("").chars().filter(|c| !c.is_ascii_digit()).collect::<String>();
                    if strip(exp) != strip(e) {
                        problem = Some(format!("op {i} {name}{args:?}: error {e:?} vs {exp:?}"));
                    }
                }
                (Ok(_), false) => problem = Some(format!("op {i} {name}{args:?}: expected error {}", op["error"])),
                (Err(e), true) => problem = Some(format!("op {i} {name}{args:?}: unexpected error {e}")),
            }
            if problem.is_some() {
                break;
            }
        }
        if problem.is_none() {
            if let Some(expected) = case["final"].as_str() {
                let actual = ms.to_string();
                if actual != expected {
                    problem = Some(format!("final {actual:?} vs {expected:?}"));
                } else if case["mappings"].as_str() != Some(ms.mappings_hires().as_str()) {
                    problem = Some(format!("mappings {:?} vs {}", ms.mappings_hires(), case["mappings"]));
                }
            }
        }
        match problem {
            None => pass += 1,
            Some(p) => {
                fail += 1;
                if fail <= 5 {
                    println!("case {n} {original:?}: {p}\n  ops: {}", case["ops"]);
                }
            }
        }
    }
    println!("pass {pass}  fail {fail}");
}
