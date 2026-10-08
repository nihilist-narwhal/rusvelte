fn main() {
    let src = std::env::args().nth(1).unwrap();
    let src = if std::path::Path::new(&src).exists() { std::fs::read_to_string(&src).unwrap() } else { src };
    let opts = svelte_rs::svelte2tsx::Options { typings_namespace: "svelteHTML".into(), preserve_attribute_case: false, svelte5_plus: true, emit_jsdoc: false, is_ts_file: false, mode_ts: false, accessors: false };
    match svelte_rs::svelte2tsx::htmlx2jsx(&src, &opts) { Ok(c) => println!("{c}"), Err(e) => println!("ERR {e}") }
}
