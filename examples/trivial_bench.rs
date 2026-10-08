//! Per-file overhead of `compile_diagnostics` on tiny components.
fn main() {
    let inputs = [
        "<div></div>",
        "<script>let a = 1;</script>\n<p>{a}</p>",
        "<script>\n\tlet { x } = $props();\n</script>\n\n<button onclick={() => x++}>{x}</button>\n<style>button { color: red }</style>",
    ];
    for src in inputs {
        let n: usize = std::env::var("N").ok().and_then(|v| v.parse().ok()).unwrap_or(100_000);
        let t = std::time::Instant::now();
        for _ in 0..n {
            std::hint::black_box(rusvelte::analyze::compile_diagnostics(src, "App.svelte")).ok();
        }
        let per = t.elapsed().as_secs_f64() / n as f64 * 1e6;
        let t = std::time::Instant::now();
        for _ in 0..n {
            let alloc = oxc_allocator::Allocator::default();
            std::hint::black_box(rusvelte::parse(&alloc, src, false)).ok();
        }
        let parse = t.elapsed().as_secs_f64() / n as f64 * 1e6;
        println!("{:6.2} µs total, {:6.2} µs parse  {:?}", per, parse, &src[..src.len().min(30)]);
    }
}
