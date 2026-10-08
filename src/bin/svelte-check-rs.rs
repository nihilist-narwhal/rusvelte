//! `svelte-check --tsgo`, natively. Usage mirrors svelte-check:
//!   svelte-check-rs [--workspace <dir>] [--tsconfig <path>] [--output human|human-verbose|machine|machine-verbose]
//!                   [--threshold error|warning] [--ignore <patterns>] [--fail-on-warnings]
//!                   [--diagnostic-sources js,svelte] [--compiler-warnings code:ignore,...] [--timings]
use std::collections::HashMap;
use std::io::IsTerminal;
use std::path::PathBuf;

use svelte_rs::check::writer::{Format, Threshold};
use svelte_rs::check::{run, CheckOptions};

fn main() {
    let mut args = std::env::args().skip(1);
    let cwd = std::env::current_dir().unwrap();
    let mut workspace = cwd.clone();
    let mut tsconfig: Option<PathBuf> = None;
    let mut format = Format::HumanVerbose;
    let mut threshold = None;
    let mut ignore = Vec::new();
    let mut fail_on_warnings = false;
    let mut sources = vec!["js".to_string(), "svelte".to_string()];
    let mut compiler_warnings = HashMap::new();
    let mut timings = false;
    let mut incremental = false;
    let mut colors = std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();
    while let Some(a) = args.next() {
        let (flag, inline) = match a.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f.to_string(), Some(v.to_string())),
            _ => (a.clone(), None),
        };
        let mut value = || inline.clone().or_else(|| args.next()).unwrap_or_else(|| fail(&format!("{flag} needs a value")));
        match flag.as_str() {
            "--workspace" => workspace = cwd.join(value()),
            "--tsconfig" => tsconfig = Some(cwd.join(value())),
            "--output" => {
                format = match value().as_str() {
                    "human" => Format::Human,
                    "human-verbose" => Format::HumanVerbose,
                    "machine" => Format::Machine,
                    "machine-verbose" => Format::MachineVerbose,
                    other => fail(&format!("unknown output format {other}")),
                }
            }
            "--threshold" => {
                threshold = match value().as_str() {
                    "error" => Some(Threshold::Error),
                    "warning" => Some(Threshold::Warning),
                    other => fail(&format!("unknown threshold {other}")),
                }
            }
            "--ignore" => ignore = value().split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
            "--fail-on-warnings" => fail_on_warnings = true,
            "--diagnostic-sources" => sources = value().split(',').map(|s| s.trim().to_string()).collect(),
            "--compiler-warnings" => {
                for pair in value().split(',') {
                    if let Some((code, setting)) = pair.split_once(':') {
                        compiler_warnings.insert(code.trim().to_string(), setting.trim().to_string());
                    }
                }
            }
            "--color" => colors = true,
            "--no-color" => colors = false,
            "--timings" => timings = true,
            "--incremental" => incremental = true,
            "--tsgo" => {}
            other => fail(&format!("unknown option {other}")),
        }
    }
    let tsconfig = tsconfig.unwrap_or_else(|| {
        let t = workspace.join("tsconfig.json");
        if t.is_file() { t } else { workspace.join("jsconfig.json") }
    });
    let opts = CheckOptions {
        workspace,
        tsconfig,
        ignore,
        format,
        threshold,
        fail_on_warnings,
        sources,
        compiler_warnings,
        colors,
        threads: std::thread::available_parallelism().map_or(4, |n| n.get()),
        timings,
        incremental,
    };
    let start = std::time::Instant::now();
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    match run(&opts, &mut out) {
        Ok(summary) => {
            use std::io::Write;
            let _ = out.flush();
            if timings {
                eprintln!("[timing] total: {:.1} ms", start.elapsed().as_secs_f64() * 1000.0);
            }
            let failed = summary.error_count > 0 || (opts.fail_on_warnings && summary.warning_count > 0);
            std::process::exit(i32::from(failed));
        }
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}

fn fail(msg: &str) -> ! {
    eprintln!("{msg}");
    std::process::exit(1)
}
