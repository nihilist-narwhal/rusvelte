//! Time `parse_modern` over the corpus. Usage: bench <corpus> <expected dir> [iters]
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let corpus = std::path::Path::new(&args[1]);
    let manifest: Vec<serde_json::Value> =
        serde_json::from_str(&std::fs::read_to_string(std::path::Path::new(&args[2]).join("manifest.json")).unwrap()).unwrap();
    let iters: usize = args.get(3).map_or(10, |s| s.parse().unwrap());
    let files: Vec<(String, bool)> = manifest
        .iter()
        .map(|m| (std::fs::read_to_string(corpus.join(m["rel"].as_str().unwrap())).unwrap(), m["loose"].as_bool().unwrap_or(false)))
        .collect();
    let bytes: usize = files.iter().map(|f| f.0.len()).sum();
    let parse_only = || {
        for (src, loose) in &files {
            let alloc = oxc_allocator::Allocator::default();
            let src = src.strip_prefix('\u{feff}').unwrap_or(src);
            let _ = std::hint::black_box(svelte_rs::parse(&alloc, src, *loose));
        }
    };
    let with_json = || {
        for (src, loose) in &files {
            let _ = std::hint::black_box(svelte_rs::parse_modern(src, *loose));
        }
    };
    println!("{} files, {:.2} MB", files.len(), bytes as f64 / 1e6);
    for (label, run) in [("rust parse (typed AST)", &parse_only as &dyn Fn()), ("rust parse + JSON", &with_json)] {
        for _ in 0..3 {
            run();
        }
        let mut times: Vec<f64> = (0..iters)
            .map(|_| {
                let t = Instant::now();
                run();
                t.elapsed().as_secs_f64() * 1000.0
            })
            .collect();
        times.sort_by(|a, b| a.partial_cmp(b).unwrap());
        println!("{label:24} median {:6.1} ms/pass", times[times.len() / 2]);
    }
}
