//! Port of `helpers/rewriteExternalImports.ts`: when the generated file lives somewhere else
//! than its source, relative imports that leave the workspace are rewritten to still point at
//! the same file.

use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone)]
pub struct RewriteExternalImports {
    pub source_path: PathBuf,
    pub generated_path: PathBuf,
    pub workspace_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExternalImportRewrite {
    pub rewritten: String,
    pub inserted_prefix: String,
}

fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// The root of an absolute path (`/`, a Windows drive or UNC share) and its other components.
/// With `windows`, `\` is a separator too and drive/UNC prefixes are roots (a verbatim `\\?\`
/// prefix is dropped: it names the same location). Roots come back with `/` separators.
fn split_root(p: &str, windows: bool) -> (String, Vec<&str>) {
    fn parts(s: &str) -> Vec<&str> {
        s.split(['/', '\\']).filter(|c| !c.is_empty() && *c != ".").collect()
    }
    if !windows {
        let root = if p.starts_with('/') { "/" } else { "" };
        return (root.to_string(), p.split('/').filter(|c| !c.is_empty() && *c != ".").collect());
    }
    let is_sep = |b: u8| b == b'/' || b == b'\\';
    let mut rest = p;
    let mut unc = false;
    let b = rest.as_bytes();
    if b.len() >= 4 && is_sep(b[0]) && is_sep(b[1]) && b[2] == b'?' && is_sep(b[3]) {
        rest = &rest[4..];
        let b = rest.as_bytes();
        if b.len() >= 4 && b[..3].eq_ignore_ascii_case(b"UNC") && is_sep(b[3]) {
            rest = &rest[4..];
            unc = true;
        }
    }
    let b = rest.as_bytes();
    if unc || (b.len() >= 2 && is_sep(b[0]) && is_sep(b[1])) {
        // `\\server\share`: both belong to the root
        let all = parts(rest);
        let n = all.len().min(2);
        return (format!("//{}/", all[..n].join("/")), all[n..].to_vec());
    }
    if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
        let rooted = b.get(2).is_some_and(|&c| is_sep(c));
        return (format!("{}:{}", (b[0] as char).to_ascii_uppercase(), if rooted { "/" } else { "" }), parts(&rest[2..]));
    }
    let root = if b.first().is_some_and(|&c| is_sep(c)) { "/" } else { "" };
    (root.to_string(), parts(rest))
}

/// `path.relative(from, to)` for normalized absolute paths, as parts (none for the same path).
/// Paths on different roots (Windows drives or shares) have no relative path: `Err` holds the
/// absolute target with `/` separators, which is what Node's `path.relative` returns there.
pub fn relative_parts(from: &str, to: &str, windows: bool) -> Result<Vec<String>, String> {
    let (from_root, f) = split_root(from, windows);
    let (to_root, t) = split_root(to, windows);
    let same_root = if windows { from_root.eq_ignore_ascii_case(&to_root) } else { from_root == to_root };
    if !same_root {
        return Err(format!("{to_root}{}", t.join("/")));
    }
    let eq = |a: &&str, b: &&str| if windows { a.eq_ignore_ascii_case(b) } else { a == b };
    let common = f.iter().zip(&t).take_while(|(a, b)| eq(a, b)).count();
    let mut parts: Vec<String> = vec!["..".to_string(); f.len() - common];
    parts.extend(t[common..].iter().map(|c| c.to_string()));
    Ok(parts)
}

/// `path.relative`, with `/` separators (`Err`: the absolute target, on another drive)
fn relative(from: &Path, to: &Path) -> Result<String, String> {
    relative_parts(&from.to_string_lossy(), &to.to_string_lossy(), cfg!(windows)).map(|p| p.join("/"))
}

fn is_within_directory(file: &Path, dir: &Path) -> bool {
    // (a file on another drive never is)
    relative(&normalize(dir), &normalize(file)).is_ok_and(|rel| rel.is_empty() || !rel.starts_with(".."))
}

/// `getExternalImportRewrite`
pub fn external_import_rewrite(specifier: &str, o: &RewriteExternalImports) -> Option<ExternalImportRewrite> {
    let source_dir = o.source_path.parent().unwrap_or(Path::new(""));
    let generated_dir = o.generated_path.parent().unwrap_or(Path::new(""));
    let cut = match (specifier.find('?'), specifier.find('#')) {
        (Some(q), Some(h)) => Some(q.min(h)),
        (q, h) => q.or(h),
    };
    let (path_part, suffix) = match cut {
        Some(i) => (&specifier[..i], &specifier[i..]),
        None => (specifier, ""),
    };
    if !path_part.starts_with("../") {
        return None;
    }
    let target = normalize(&source_dir.join(path_part));
    if is_within_directory(&target, &o.workspace_path) {
        return None;
    }
    let rewritten_relative = relative(&normalize(generated_dir), &target).unwrap_or_else(|absolute| absolute);
    let rewritten = format!("{rewritten_relative}{suffix}");
    if rewritten == specifier {
        return None;
    }
    let mut prefix_len = rewritten_relative.len().saturating_sub(path_part.len());
    while !rewritten_relative.is_char_boundary(prefix_len) {
        prefix_len -= 1;
    }
    Some(ExternalImportRewrite { inserted_prefix: rewritten_relative[..prefix_len].to_string(), rewritten })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_outside_workspace_only() {
        let o = RewriteExternalImports {
            source_path: "/w/app/src/lib/A.svelte".into(),
            generated_path: "/w/app/.svelte-kit/.svelte-check/svelte/src/lib/++A.svelte.ts".into(),
            workspace_path: "/w/app".into(),
        };
        assert_eq!(external_import_rewrite("../x.js", &o), None);
        let r = external_import_rewrite("../../../shared/x.js?raw", &o).unwrap();
        assert_eq!(r.rewritten, "../../../../../../shared/x.js?raw");
        assert_eq!(r.inserted_prefix, "../../../");
    }

    #[test]
    fn relative_parts_across_windows_roots() {
        let rel = |f: &str, t: &str| relative_parts(f, t, true).map(|p| p.join("/"));
        assert_eq!(rel(r"C:\w\app\src", r"C:\w\shared\x.js"), Ok("../../shared/x.js".into()));
        // drive letters and names compare case-insensitively
        assert_eq!(rel(r"c:\W\App", r"C:\w\app\x"), Ok("x".into()));
        assert_eq!(rel(r"C:\w", r"C:\w"), Ok(String::new()));
        // another drive or share: the absolute target, not `../../D:/...`
        assert_eq!(rel(r"C:\w\app", r"D:\shared\x.js"), Err("D:/shared/x.js".into()));
        assert_eq!(rel(r"C:\w\app", r"\\srv\share\x.js"), Err("//srv/share/x.js".into()));
        assert_eq!(rel(r"\\srv\share\a", r"\\srv\share\b"), Ok("../b".into()));
        assert_eq!(rel(r"\\srv\share\a", r"\\srv\other\b"), Err("//srv/other/b".into()));
        // a verbatim path (from canonicalize) is on the same drive or share
        assert_eq!(rel(r"C:\w\app", r"\\?\C:\w\lib\x.js"), Ok("../lib/x.js".into()));
        assert_eq!(rel(r"\\srv\share\a", r"\\?\UNC\srv\share\b"), Ok("../b".into()));
        // POSIX: `\` is an ordinary file name character
        assert_eq!(relative_parts("/a/b", r"/a/c\d", false).map(|p| p.join("/")), Ok(r"../c\d".into()));
        assert_eq!(relative_parts("/a/b", "/a/b/c", false).map(|p| p.join("/")), Ok("c".into()));
    }
}
