//! Compile one component (or `.svelte.js` / `.svelte.ts` module, with `compileModule`) and
//! print the JS (debugging aid)
//!   debug_compile <file> [client|server] [options JSON]
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let source = std::fs::read_to_string(&args[1]).unwrap();
    let mut json: serde_json::Value = args.get(3).map(|s| serde_json::from_str(s).unwrap()).unwrap_or(serde_json::json!({}));
    json["generate"] = args.get(2).cloned().unwrap_or_else(|| "server".into()).into();
    json["filename"] = args[1].clone().into();
    let options = rusvelte::transform::options::CompileOptions::from_json(&json);
    let result = if args[1].ends_with(".js") || args[1].ends_with(".ts") {
        rusvelte::transform::compile_module(&source, &options)
    } else {
        rusvelte::transform::compile(&source, &options)
    };
    match result {
        Ok(out) => print!("{}", out.js),
        Err(e) => println!("error {}: {} @{:?}", e.code, e.first_line(), e.position.map(|p| p.0)),
    }
}
