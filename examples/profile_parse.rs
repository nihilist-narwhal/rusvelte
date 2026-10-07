//! Parse the corpus in a loop, for profiling
fn main() {
    let corpus = std::path::Path::new("svelte-upstream/packages/svelte/tests");
    let manifest: Vec<serde_json::Value> = serde_json::from_str(&std::fs::read_to_string("oracle/expected/manifest.json").unwrap()).unwrap();
    let files: Vec<(String, bool)> = manifest
        .iter()
        .map(|m| (std::fs::read_to_string(corpus.join(m["rel"].as_str().unwrap())).unwrap(), m["loose"].as_bool().unwrap_or(false)))
        .collect();
    let secs: f64 = std::env::args().nth(1).map_or(8.0, |s| s.parse().unwrap());
    let t = std::time::Instant::now();
    while t.elapsed().as_secs_f64() < secs {
        for (src, loose) in &files {
            let alloc = oxc_allocator::Allocator::default();
            let _ = std::hint::black_box(svelte_rs::parse(&alloc, src.strip_prefix('\u{feff}').unwrap_or(src), *loose));
        }
    }
}
