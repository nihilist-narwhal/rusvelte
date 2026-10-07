//! How much of the script cost is oxc itself vs. JSON materialization
use std::time::Instant;
use oxc_allocator::Allocator;
use oxc_estree::{CompactSerializer, ESTree};
use oxc_parser::Parser;
use oxc_span::SourceType;

fn main() {
    let corpus = std::path::Path::new("svelte-upstream/packages/svelte/tests");
    let manifest: Vec<serde_json::Value> = serde_json::from_str(&std::fs::read_to_string("oracle/expected/manifest.json").unwrap()).unwrap();
    let re = regex::Regex::new(r"(?s)<script[^>]*>(.*?)</script>").unwrap();
    let scripts: Vec<String> = manifest.iter().flat_map(|m| {
        let src = std::fs::read_to_string(corpus.join(m["rel"].as_str().unwrap())).unwrap();
        re.captures_iter(&src).map(|c| c[1].to_string()).collect::<Vec<_>>()
    }).collect();
    let mut alloc = Allocator::default();
    for (label, mode) in [("oxc parse only", 0), ("+ ESTree JSON string", 1), ("+ serde_json::Value", 2)] {
        let mut best = f64::MAX;
        for _ in 0..7 {
            let t = Instant::now();
            for s in &scripts {
                alloc.reset();
                let ret = Parser::new(&alloc, s, SourceType::mjs()).parse();
                if mode >= 1 {
                    let mut ser = CompactSerializer::new(false, false);
                    ret.program.serialize(&mut ser);
                    let json = ser.into_string();
                    if mode == 2 {
                        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
                        std::hint::black_box(v);
                    } else {
                        std::hint::black_box(json);
                    }
                }
            }
            best = best.min(t.elapsed().as_secs_f64() * 1000.0);
        }
        println!("{label:24} {best:7.1} ms  ({} scripts)", scripts.len());
    }
}
