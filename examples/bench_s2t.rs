//! Time svelte2tsx over a corpus (sources preloaded), with svelte-check's options
fn main() {
    let dir = std::env::args().nth(1).unwrap();
    let files: Vec<(String, String)> = glob(&std::path::PathBuf::from(&dir));
    let opts: Vec<svelte_rs::svelte2tsx::Svelte2TsxOptions> = files
        .iter()
        .map(|(name, src)| svelte_rs::svelte2tsx::Svelte2TsxOptions {
            filename: Some(name.clone()),
            is_ts_file: src.contains("lang=\"ts\"") || src.contains("lang='ts'"),
            emit_jsdoc: true,
            ..Default::default()
        })
        .collect();
    let run = || {
        for ((_, src), o) in files.iter().zip(&opts) {
            let _ = std::hint::black_box(svelte_rs::svelte2tsx::svelte2tsx(src, o));
        }
    };
    for _ in 0..3 {
        run();
    }
    let mut t: Vec<f64> = (0..15)
        .map(|_| {
            let s = std::time::Instant::now();
            run();
            s.elapsed().as_secs_f64() * 1000.0
        })
        .collect();
    t.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!("rust svelte2tsx: {} files, median {:.1} ms", files.len(), t[t.len() / 2]);

    // the same, across threads
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
    let run_par = || {
        let next = std::sync::atomic::AtomicUsize::new(0);
        std::thread::scope(|s| {
            for _ in 0..threads {
                s.spawn(|| loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some((_, src)) = files.get(i) else { break };
                    let _ = std::hint::black_box(svelte_rs::svelte2tsx::svelte2tsx(src, &opts[i]));
                });
            }
        });
    };
    for _ in 0..3 {
        run_par();
    }
    let mut t: Vec<f64> = (0..15)
        .map(|_| {
            let s = std::time::Instant::now();
            run_par();
            s.elapsed().as_secs_f64() * 1000.0
        })
        .collect();
    t.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!("rust svelte2tsx, {threads} threads: median {:.1} ms", t[t.len() / 2]);
}

fn glob(dir: &std::path::Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(glob(&p));
        } else if p.extension().is_some_and(|x| x == "svelte") {
            out.push((p.file_name().unwrap().to_string_lossy().to_string(), std::fs::read_to_string(&p).unwrap()));
        }
    }
    out
}
