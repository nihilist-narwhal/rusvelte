//! Compare the esrap port (`rusvelte::estree::print`) with the output of
//! `oracle/gen_esrap.mjs` (acorn + esrap's `print(ast, ts({ comments }))`).
//!
//!   cargo run --release --bin compare_esrap -- <oracle out.json>...
//!
//! Each file is parsed with oxc, converted to the owned ESTree and printed with the comments
//! collected the way Svelte collects them. The code must match exactly; the source map
//! mappings are compared too (they check every node's `loc`). VERBOSE=1 prints the first
//! difference of each failure, FILTER=<substring> limits the files. BENCH=<n> also prints
//! every corpus n more times, with and without source maps, and reports the time per pass.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

use oxc_allocator::Allocator;
use oxc_parser::{ParseOptions, Parser};
use oxc_span::SourceType;
use serde_json::Value;
use rusvelte::estree::{self, convert, print};
use rusvelte::locator::Locator;

fn first_difference(expected: &str, actual: &str) -> (usize, String, String) {
    let e: Vec<&str> = expected.split('\n').collect();
    let a: Vec<&str> = actual.split('\n').collect();
    for i in 0..e.len().max(a.len()) {
        let x = e.get(i).copied().unwrap_or("<eof>");
        let y = a.get(i).copied().unwrap_or("<eof>");
        if x != y {
            return (i + 1, x.to_string(), y.to_string());
        }
    }
    (0, String::new(), String::new())
}

fn main() {
    let verbose = std::env::var("VERBOSE").is_ok();
    let filter = std::env::var("FILTER").ok();
    let bench: usize = std::env::var("BENCH").ok().and_then(|n| n.parse().ok()).unwrap_or(0);
    let mut reasons: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for out in std::env::args().skip(1) {
        let oracle: Value = serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
        let base = Path::new(oracle["base"].as_str().unwrap());
        let files = oracle["files"].as_array().unwrap();

        let (mut total, mut ok, mut map_ok) = (0, 0, 0);
        let mut print_time = Duration::ZERO;
        let mut convert_time = Duration::ZERO;
        let mut parse_time = Duration::ZERO;
        let mut kept: Vec<(estree::Node, Vec<estree::Comment>)> = Vec::new();

        for entry in files {
            let rel = entry["path"].as_str().unwrap();
            if let Some(f) = &filter {
                if !rel.contains(f.as_str()) {
                    continue;
                }
            }
            total += 1;
            let expected = entry["code"].as_str().unwrap();
            let expected_map = entry["mappings"].as_str().unwrap_or("");
            let source = std::fs::read_to_string(base.join(rel)).unwrap();
            let source = source.strip_prefix('\u{feff}').unwrap_or(&source);

            let t = Instant::now();
            let alloc = Allocator::default();
            let parsed = Parser::new(&alloc, source, SourceType::mjs())
                .with_options(ParseOptions { preserve_parens: false, ..ParseOptions::default() })
                .parse();
            parse_time += t.elapsed();
            if parsed.fatal_error || !parsed.diagnostics.is_empty() {
                reasons.entry("oxc parse error".into()).or_default().push(rel.to_string());
                continue;
            }

            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let t = Instant::now();
                let locator = Locator::new(source);
                let converter = convert::Converter::new(&locator, false);
                let mut program = converter.program(&parsed.program);
                // acorn's Program spans the whole input
                let len = source.len() as u32;
                program.span = Some(estree::Span::new(0, len));
                let (line, column) = locator.acorn_line_column(source.len());
                program.loc = Some(estree::SourceLocation {
                    start: estree::Position::new(1, 0),
                    end: estree::Position::new(line as u32, column as u32),
                });
                let comments = convert::collect_comments(&parsed.program, &locator);
                let converted = t.elapsed();

                let t = Instant::now();
                let printed = print::print(
                    &program,
                    &print::PrintOptions { comments: &comments, source_map: true, ..Default::default() },
                );
                let elapsed = t.elapsed();
                (printed, converted, elapsed, (program, comments))
            }));
            let (printed, converted, printed_in, program) = match result {
                Ok(r) => r,
                Err(e) => {
                    let msg = e
                        .downcast_ref::<String>()
                        .cloned()
                        .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
                        .unwrap_or_default();
                    reasons.entry(format!("panic: {msg}")).or_default().push(rel.to_string());
                    continue;
                }
            };
            convert_time += converted;
            print_time += printed_in;
            if bench > 0 {
                kept.push(program);
            }

            if printed.code == expected {
                ok += 1;
                let mappings = printed.encode_mappings();
                if mappings == expected_map {
                    map_ok += 1;
                } else {
                    reasons.entry("mappings differ".into()).or_default().push(rel.to_string());
                    if verbose {
                        let (e, a): (Vec<&str>, Vec<&str>) = (expected_map.split(';').collect(), mappings.split(';').collect());
                        if let Some(i) = (0..e.len().max(a.len())).find(|&i| e.get(i) != a.get(i)) {
                            eprintln!("--- {rel}: mappings, generated line {}", i + 1);
                            eprintln!("  code:     {:?}", expected.split('\n').nth(i).unwrap_or(""));
                            eprintln!("  expected: {}", e.get(i).unwrap_or(&""));
                            eprintln!("  actual:   {}", a.get(i).unwrap_or(&""));
                        }
                    }
                }
            } else {
                let (line, e, a) = first_difference(expected, &printed.code);
                let key = format!("code differs: {:.60} | {:.60}", e.trim(), a.trim());
                reasons.entry(key).or_default().push(rel.to_string());
                if verbose {
                    eprintln!("--- {rel}: line {line}");
                    eprintln!("  expected: {e:?}");
                    eprintln!("  actual:   {a:?}");
                }
            }
        }

        if bench > 0 {
            for source_map in [false, true] {
                let t = Instant::now();
                let mut bytes = 0;
                for _ in 0..bench {
                    for (program, comments) in &kept {
                        let options = print::PrintOptions { comments, source_map, ..Default::default() };
                        bytes += print::print(program, &options).code.len();
                    }
                }
                println!(
                    "  bench: print {:.1} ms per pass (source maps: {source_map}, {} MB)",
                    t.elapsed().as_secs_f64() * 1000.0 / bench as f64,
                    bytes / bench / 1_000_000
                );
            }
        }

        let esrap_ms = oracle["print_ms"].as_f64().unwrap_or(0.0);
        println!(
            "{out}: {ok}/{total} code identical, {map_ok}/{ok} with identical mappings ({} skipped by the oracle); \
             oxc parse {:.0} ms, convert {:.0} ms, print {:.0} ms (esrap {esrap_ms:.0} ms)",
            oracle["skipped"].as_array().map_or(0, Vec::len),
            parse_time.as_secs_f64() * 1000.0,
            convert_time.as_secs_f64() * 1000.0,
            print_time.as_secs_f64() * 1000.0,
        );
    }

    let mut groups: Vec<_> = reasons.into_iter().collect();
    groups.sort_by_key(|(_, files)| std::cmp::Reverse(files.len()));
    for (reason, files) in groups.iter().take(40) {
        println!("{:5}  {reason}", files.len());
        for f in files.iter().take(3) {
            println!("         {f}");
        }
    }
}
