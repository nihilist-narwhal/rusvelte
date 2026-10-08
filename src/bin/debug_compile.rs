//! Compile one component and print the JS (debugging aid)
//!   debug_compile <file> [client|server] [options JSON]
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let source = std::fs::read_to_string(&args[1]).unwrap();
    let mut json: serde_json::Value = args.get(3).map(|s| serde_json::from_str(s).unwrap()).unwrap_or(serde_json::json!({}));
    json["generate"] = args.get(2).cloned().unwrap_or_else(|| "server".into()).into();
    json["filename"] = args[1].clone().into();
    let options = svelte_rs::transform::options::CompileOptions::from_json(&json);
    match svelte_rs::transform::compile(&source, &options) {
        Ok(out) => print!("{}", out.js),
        Err(e) => println!("error {}: {}", e.code, e.message),
    }
}
