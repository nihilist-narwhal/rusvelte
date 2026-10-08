fn main() {
    let dir = std::env::args().nth(1).unwrap();
    let mut files = Vec::new();
    let mut stack = vec![std::path::PathBuf::from(dir)];
    while let Some(d) = stack.pop() { for e in std::fs::read_dir(d).unwrap().flatten() { let p = e.path(); if p.is_dir() { stack.push(p) } else if p.extension().is_some_and(|x| x == "svelte") { files.push(std::fs::read_to_string(&p).unwrap()) } } }
    let opts = rusvelte::svelte2tsx::Options { typings_namespace: "svelteHTML".into(), preserve_attribute_case: false, svelte5_plus: true, emit_jsdoc: false, is_ts_file: false, mode_ts: false, accessors: false, rewrite_external_imports: None };
    let t = std::time::Instant::now();
    while t.elapsed().as_secs() < 8 { for f in &files { let _ = std::hint::black_box(rusvelte::svelte2tsx::htmlx2jsx(f, &opts)); } }
}
