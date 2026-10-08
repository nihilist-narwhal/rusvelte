//! Time htmlx2jsx over a corpus (sources preloaded)
fn main() {
    let dir = std::env::args().nth(1).unwrap();
    let files: Vec<String> = glob(&std::path::PathBuf::from(&dir));
    let opts = rusvelte::svelte2tsx::Options { typings_namespace: "svelteHTML".into(), preserve_attribute_case: false, svelte5_plus: true, emit_jsdoc: false, is_ts_file: false, mode_ts: false, accessors: false, rewrite_external_imports: None };
    let run = || for f in &files { let _ = std::hint::black_box(rusvelte::svelte2tsx::htmlx2jsx(f, &opts)); };
    for _ in 0..3 { run(); }
    let mut t: Vec<f64> = (0..15).map(|_| { let s = std::time::Instant::now(); run(); s.elapsed().as_secs_f64() * 1000.0 }).collect();
    t.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!("rust htmlx2jsx: {} files, median {:.1} ms", files.len(), t[t.len() / 2]);
}
fn glob(dir: &std::path::Path) -> Vec<String> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() { out.extend(glob(&p)); } else if p.extension().is_some_and(|x| x == "svelte") { out.push(std::fs::read_to_string(&p).unwrap()); }
    }
    out
}
