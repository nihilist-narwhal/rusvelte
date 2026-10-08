//! Stack use of deep nesting: parses (or fully compiles) a pathological input on a thread with
//! the given stack and prints `ok`; an overflow kills the process.
//!   cargo run --release --example depth_probe -- <shape> <depth> <stack MiB> [parse|compile]
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (shape, depth, stack_mib) = (args[1].clone(), args[2].parse::<usize>().unwrap(), args[3].parse::<usize>().unwrap());
    let parse_only = args.get(4).map(String::as_str) == Some("parse");
    let source = match shape.as_str() {
        "bang" => format!("<script>let x = {}0;</script>", "!".repeat(depth)),
        "array" => format!("<script>let x = {}{};</script>", "[".repeat(depth), "]".repeat(depth)),
        "ternary" => format!("<script>let x = {}0;</script>", "a?b:".repeat(depth)),
        "object" => format!("<script>let x = {}0{};</script>", "{a:".repeat(depth), "}".repeat(depth)),
        "func" => format!("<script>{}{}</script>", "function f(){".repeat(depth), "}".repeat(depth)),
        "tpl" => format!("<script>let x = {}{};</script>", "`${".repeat(depth), "}`".repeat(depth)),
        _ => panic!("unknown shape"),
    };
    let ok = std::thread::Builder::new()
        .stack_size(stack_mib << 20)
        .spawn(move || {
            if parse_only {
                let alloc = oxc_allocator::Allocator::default();
                rusvelte::parse(&alloc, &source, false).is_ok()
            } else {
                rusvelte::transform::compile(&source, &rusvelte::transform::options::CompileOptions::default()).is_ok()
            }
        })
        .unwrap()
        .join()
        .unwrap();
    println!("{}", if ok { "ok" } else { "error" });
}
