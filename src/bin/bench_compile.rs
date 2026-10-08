//! Time `compile_diagnostics` (parse + analysis) over a corpus, against parsing alone.
//!   bench_compile <corpus> <oracle out dir (for manifest.json)> [iters]
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let corpus = std::path::Path::new(&args[1]);
    let manifest: Vec<serde_json::Value> =
        serde_json::from_str(&std::fs::read_to_string(std::path::Path::new(&args[2]).join("manifest.json")).unwrap()).unwrap();
    let iters: usize = args.get(3).map_or(10, |s| s.parse().unwrap());
    let files: Vec<(String, String)> = manifest
        .iter()
        .map(|m| {
            let rel = m["rel"].as_str().unwrap();
            let name = std::path::Path::new(rel).file_name().unwrap().to_str().unwrap().to_string();
            (std::fs::read_to_string(corpus.join(rel)).unwrap(), name)
        })
        .collect();
    let bytes: usize = files.iter().map(|f| f.0.len()).sum();
    let parse_only = || {
        for (src, _) in &files {
            let alloc = oxc_allocator::Allocator::default();
            let src = src.strip_prefix('\u{feff}').unwrap_or(src);
            let _ = std::hint::black_box(rusvelte::parse(&alloc, src, false));
        }
    };
    let diagnostics = || {
        for (src, name) in &files {
            let _ = std::hint::black_box(rusvelte::analyze::compile_diagnostics(src, name));
        }
    };
    println!("{} files, {:.2} MB", files.len(), bytes as f64 / 1e6);
    let only = std::env::var("BENCH_ONLY").ok();
    for (label, run) in [("parse", &parse_only as &dyn Fn()), ("parse + analysis", &diagnostics)] {
        if only.as_deref().is_some_and(|o| o != label) {
            continue;
        }
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
        println!("{label:18} median {:6.1} ms/pass", times[times.len() / 2]);
    }
}
