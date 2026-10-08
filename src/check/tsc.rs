//! Find TypeScript 7 (tsgo), run it on the overlay config, and parse its pretty output
//! (`runTypeScriptDiagnostics` / `parseDiagnostics` in svelte-check).

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error = 1,
    Warning = 2,
}

/// A diagnostic as printed by the compiler (0-based line/character)
#[derive(Debug, Clone)]
pub struct CliDiagnostic {
    pub file_path: Option<PathBuf>,
    pub line: usize,
    pub character: usize,
    /// from the `~~~` underline (UTF-16 units)
    pub length: usize,
    pub severity: Severity,
    pub code: u32,
    pub message: String,
}

/// Node-style lookup of `<name>/package.json` from `dir` upwards
fn resolve_package(name: &str, dir: &Path) -> Option<PathBuf> {
    let mut d = Some(dir);
    while let Some(cur) = d {
        let p = cur.join("node_modules").join(name).join("package.json");
        if p.is_file() {
            return Some(p);
        }
        d = cur.parent();
    }
    None
}

/// The TypeScript 7 executable for a project: `@typescript/native` (an alias of `typescript@7`)
/// or `@typescript/native-preview`, and the platform package that ships the binary
pub fn find_tsgo(tsconfig_dir: &Path) -> Result<PathBuf, String> {
    // a specific compiler binary (e.g. another TypeScript 7 build)
    if let Some(exe) = std::env::var_os("SVELTE_CHECK_TSGO") {
        return Ok(PathBuf::from(exe));
    }
    for name in ["@typescript/native", "@typescript/native-preview"] {
        let Some(pkg_json) = resolve_package(name, tsconfig_dir) else { continue };
        let Ok(text) = std::fs::read_to_string(&pkg_json) else { continue };
        let Ok(pkg) = serde_json::from_str::<Value>(&text) else { continue };
        let pkg_name = pkg["name"].as_str().unwrap_or("");
        let major: u32 = pkg["version"].as_str().unwrap_or("").split('.').next().and_then(|m| m.parse().ok()).unwrap_or(0);
        if major < 7 || !matches!(pkg_name, "typescript" | "@typescript/native-preview") {
            continue;
        }
        let base = pkg_name.rsplit('/').next().unwrap_or(pkg_name);
        let bin = if base == "typescript" { "tsc" } else { "tsgo" };
        let platform = match std::env::consts::OS {
            "macos" => "darwin",
            "windows" => "win32",
            other => other,
        };
        let arch = match std::env::consts::ARCH {
            "x86_64" => "x64",
            "aarch64" => "arm64",
            other => other,
        };
        let platform_pkg = format!("@typescript/{base}-{platform}-{arch}");
        let pkg_dir = pkg_json.parent().unwrap();
        if let Some(p) = resolve_package(&platform_pkg, pkg_dir) {
            let exe = p.parent().unwrap().join("lib").join(if platform == "win32" { format!("{bin}.exe") } else { bin.to_string() });
            if exe.is_file() {
                return Ok(exe);
            }
        }
    }
    Err("svelte-check --tsgo requires TypeScript 7 to be installed in the workspace.You can setup TypeScript 7 with an npm alias via the following command.\nnpm install --save-dev typescript@~6 @typescript/native@npm:typescript@7\n".into())
}

/// Start the compiler; `finish` collects its diagnostics
pub fn start(exe: &Path, tsconfig: &Path, cwd: &Path, build_info: Option<&Path>) -> Result<std::process::Child, String> {
    let mut cmd = Command::new(exe);
    cmd.arg("-p").arg(tsconfig).args(["--pretty", "true", "--noErrorTruncation"]);
    // extra compiler flags, e.g. `--singleThreaded` to compare compilers deterministically
    if let Some(extra) = std::env::var_os("SVELTE_CHECK_TSGO_ARGS") {
        cmd.args(extra.to_string_lossy().split_whitespace());
    }
    if let Some(b) = build_info {
        cmd.arg("--incremental").arg("--tsBuildInfoFile").arg(b);
    }
    cmd
        .current_dir(cwd)
        // Go's working directory comes from an inherited `PWD` that names the same directory,
        // which would make the paths it prints relative to a symlinked path
        .env("PWD", cwd)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to run {}: {e}", exe.display()))
}

pub fn finish(child: std::process::Child, cwd: &Path) -> Result<Vec<CliDiagnostic>, String> {
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    let text = format!("{}\n{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    let diagnostics = parse_diagnostics(&text, cwd);
    // A non-zero exit can also mean there are diagnostics, so it's only an error when nothing
    // was parsed. A signal always is: the run didn't finish.
    #[cfg(unix)]
    let signal = std::os::unix::process::ExitStatusExt::signal(&out.status);
    #[cfg(not(unix))]
    let signal: Option<i32> = None;
    if signal.is_some() || (!out.status.success() && diagnostics.is_empty()) {
        let reason = match signal {
            Some(s) => format!("was killed by signal {s}"),
            None => format!("exited with code {} without a parseable diagnostic", out.status.code().unwrap_or(-1)),
        };
        let detail = strip_ansi(&text);
        let detail = detail.trim();
        let detail: String = detail.chars().take(2000).collect();
        return Err(format!("The TypeScript compiler process {reason}.{}", if detail.is_empty() { String::new() } else { format!("\n{detail}") }));
    }
    Ok(diagnostics)
}

fn strip_ansi(s: &str) -> String {
    static RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| regex::Regex::new(r"\x1b\[[0-9;]*m").unwrap());
    RE.replace_all(s, "").into_owned()
}

/// `parseDiagnostics`: `file.ts:5:10 - error TS2322: message`, with the `~~~` underline below
pub fn parse_diagnostics(output: &str, base_dir: &Path) -> Vec<CliDiagnostic> {
    static HEADER: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"^((.+):(\d+):(\d+) - )?(error|warning) TS(\d+): (.*)$").unwrap());
    static TILDE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| regex::Regex::new(r"^(\s*)(~+)\s*$").unwrap());
    let clean = strip_ansi(output);
    let lines: Vec<&str> = clean.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l)).collect();
    let mut out = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let Some(m) = HEADER.captures(line.trim()) else { continue };
        let file = m.get(2).map(|f| {
            let p = Path::new(f.as_str());
            if p.is_absolute() {
                p.to_path_buf()
            } else {
                super::tsconfig::normalize(&base_dir.join(p))
            }
        });
        let line_num = m.get(3).map_or(0, |l| l.as_str().parse::<usize>().unwrap_or(0)).saturating_sub(1);
        let col = m.get(4).map_or(0, |c| c.as_str().parse::<usize>().unwrap_or(0)).saturating_sub(1);
        let mut length = 1;
        if file.is_some() {
            for next in lines.iter().take((i + 5).min(lines.len())).skip(i + 1) {
                if let Some(t) = TILDE.captures(next) {
                    length = t[2].len();
                    break;
                }
                if HEADER.is_match(next.trim()) {
                    break;
                }
            }
        }
        out.push(CliDiagnostic {
            file_path: file,
            line: line_num,
            character: col,
            length,
            severity: if &m[5] == "warning" { Severity::Warning } else { Severity::Error },
            code: m[6].parse().unwrap_or(0),
            message: m[7].to_string(),
        });
    }
    out
}
