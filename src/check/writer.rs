//! svelte-check's output formats (`writers.ts`).

use std::io::Write;

use serde_json::{json, Map, Value};

use super::map::{Code, Diagnostic, Position, U16Text};
use super::tsc::Severity;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Human,
    HumanVerbose,
    Machine,
    MachineVerbose,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Threshold {
    Error,
    Warning,
}

pub struct FileDiagnostics {
    /// absolute path
    pub path: std::path::PathBuf,
    pub text: String,
    pub diagnostics: Vec<Diagnostic>,
}

pub struct Summary {
    pub file_count: usize,
    pub error_count: usize,
    pub warning_count: usize,
    pub files_with_problems: usize,
}

fn now_ms() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis())
}

struct Colors(bool);

impl Colors {
    fn wrap(&self, code: &str, s: &str) -> String {
        if self.0 {
            format!("\x1b[{code}m{s}\x1b[39m")
        } else {
            s.to_string()
        }
    }
    fn green(&self, s: &str) -> String {
        self.wrap("32", s)
    }
    fn red(&self, s: &str) -> String {
        self.wrap("31", s)
    }
    fn yellow(&self, s: &str) -> String {
        self.wrap("33", s)
    }
    fn cyan(&self, s: &str) -> String {
        self.wrap("36", s)
    }
    fn magenta(&self, s: &str) -> String {
        self.wrap("35", s)
    }
}

fn position_json(p: Position) -> Value {
    json!({ "line": p.line, "character": p.character })
}

/// `writeDiagnostics`: write everything, return the counts
pub fn write(out: &mut impl Write, format: Format, threshold: Option<Threshold>, workspace: &std::path::Path, files: &[FileDiagnostics], colors: bool) -> std::io::Result<Summary> {
    let c = Colors(colors);
    let ws = workspace.to_string_lossy();
    let keep = |d: &Diagnostic| match threshold {
        Some(Threshold::Error) => d.severity == Severity::Error,
        _ => true,
    };
    match format {
        Format::Human => {}
        Format::HumanVerbose => {
            writeln!(out, "Loading svelte-check in workspace: {ws}")?;
            writeln!(out, "Getting Svelte diagnostics...")?;
            writeln!(out)?;
        }
        Format::Machine | Format::MachineVerbose => writeln!(out, "{} START {}", now_ms(), serde_json::to_string(&ws).unwrap())?,
    }
    let mut summary = Summary { file_count: files.len(), error_count: 0, warning_count: 0, files_with_problems: 0 };
    for f in files {
        let filename = pathdiff(workspace, &f.path);
        let text = U16Text::new(&f.text);
        for d in f.diagnostics.iter().filter(|d| keep(d)) {
            match format {
                Format::Human | Format::HumanVerbose => {
                    let Position { line, character } = d.range.start;
                    writeln!(out, "{ws}{}{}:{}:{}", std::path::MAIN_SEPARATOR, c.green(&filename), line + 1, character + 1)?;
                    let source = format!("({})", d.source);
                    let msg = if format == Format::HumanVerbose {
                        let code = related_code(d, &text, &c);
                        format!("{} {source}\n{}", d.message, c.cyan(code.trim_end()))
                    } else {
                        format!("{} {source}", d.message)
                    };
                    match d.severity {
                        Severity::Error => writeln!(out, "{}: {msg}", c.red("Error"))?,
                        Severity::Warning => writeln!(out, "{}: {msg}", c.yellow("Warn"))?,
                    }
                    writeln!(out)?;
                }
                Format::MachineVerbose => {
                    let mut m = Map::new();
                    m.insert("type".into(), json!(if d.severity == Severity::Error { "ERROR" } else { "WARNING" }));
                    m.insert("filename".into(), json!(filename));
                    m.insert("start".into(), position_json(d.range.start));
                    m.insert("end".into(), position_json(d.range.end));
                    m.insert("message".into(), json!(d.message));
                    match &d.code {
                        Some(Code::Num(n)) => {
                            m.insert("code".into(), json!(n));
                        }
                        Some(Code::Str(s)) => {
                            m.insert("code".into(), json!(s));
                        }
                        None => {}
                    }
                    if let Some(href) = &d.code_description {
                        m.insert("codeDescription".into(), json!({ "href": href }));
                    }
                    m.insert("source".into(), json!(d.source));
                    writeln!(out, "{} {}", now_ms(), Value::Object(m))?;
                }
                Format::Machine => {
                    let ty = if d.severity == Severity::Error { "ERROR" } else { "WARNING" };
                    writeln!(
                        out,
                        "{} {ty} {} {}:{} {}",
                        now_ms(),
                        serde_json::to_string(&filename).unwrap(),
                        d.range.start.line + 1,
                        d.range.start.character + 1,
                        serde_json::to_string(&d.message).unwrap()
                    )?;
                }
            }
        }
        let mut has_problems = false;
        for d in &f.diagnostics {
            match d.severity {
                Severity::Error => summary.error_count += 1,
                Severity::Warning => summary.warning_count += 1,
            }
            has_problems = true;
        }
        if has_problems {
            summary.files_with_problems += 1;
        }
    }
    match format {
        Format::Human | Format::HumanVerbose => {
            if summary.files_with_problems > 0 {
                writeln!(out, "====================================")?;
            }
            let (e, w, fwp) = (summary.error_count, summary.warning_count, summary.files_with_problems);
            let message = format!(
                "svelte-check found {e} {} and {w} {}{}\n",
                if e == 1 { "error" } else { "errors" },
                if w == 1 { "warning" } else { "warnings" },
                if fwp > 0 { format!(" in {fwp} {}", if fwp == 1 { "file" } else { "files" }) } else { String::new() }
            );
            let colored = if e != 0 {
                c.red(&message)
            } else if w != 0 {
                c.yellow(&message)
            } else {
                c.green(&message)
            };
            write!(out, "{colored}")?;
        }
        Format::Machine | Format::MachineVerbose => writeln!(
            out,
            "{} COMPLETED {} FILES {} ERRORS {} WARNINGS {} FILES_WITH_PROBLEMS",
            now_ms(),
            summary.file_count,
            summary.error_count,
            summary.warning_count,
            summary.files_with_problems
        )?,
    }
    Ok(summary)
}

/// `formatRelatedCode`: the previous line, the line with the range highlighted, the next line
fn related_code(d: &Diagnostic, text: &U16Text, c: &Colors) -> String {
    if text.is_empty() || d.position_unknown {
        return String::new();
    }
    let line = |l: i64| text.slice(text.offset_at(Position { line: l, character: 0 }), text.offset_at(Position { line: l, character: i64::MAX / 4 }));
    let start = text.offset_at(d.range.start);
    let end = text.offset_at(d.range.end);
    let prev = text.slice(text.offset_at(Position { line: d.range.start.line, character: 0 }), start);
    let post = text.slice(end, text.offset_at(Position { line: d.range.end.line, character: i64::MAX / 4 }));
    format!("{}{prev}{}{post}{}", line(d.range.start.line - 1), c.magenta(&text.slice(start, end)), line(d.range.end.line + 1))
}

/// `path.relative(workspace, file)` with the platform separator
fn pathdiff(base: &std::path::Path, path: &std::path::Path) -> String {
    let rel = super::tsconfig::relative_posix(base, path);
    if std::path::MAIN_SEPARATOR == '/' {
        rel
    } else {
        rel.replace('/', std::path::MAIN_SEPARATOR_STR)
    }
}
