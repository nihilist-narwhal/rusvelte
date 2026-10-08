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

/// `path.relative`, with `/` separators
fn relative(from: &Path, to: &Path) -> String {
    let f: Vec<_> = from.components().collect();
    let t: Vec<_> = to.components().collect();
    let common = f.iter().zip(&t).take_while(|(a, b)| a == b).count();
    let mut parts: Vec<String> = vec!["..".to_string(); f.len() - common];
    parts.extend(t[common..].iter().map(|c| c.as_os_str().to_string_lossy().to_string()));
    parts.join("/")
}

fn is_within_directory(file: &Path, dir: &Path) -> bool {
    let rel = relative(&normalize(dir), &normalize(file));
    rel.is_empty() || (!rel.starts_with("..") && !Path::new(&rel).is_absolute())
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
    let rewritten_relative = relative(&normalize(generated_dir), &target);
    let rewritten = format!("{rewritten_relative}{suffix}");
    if rewritten == specifier {
        return None;
    }
    let prefix_len = rewritten_relative.len().saturating_sub(path_part.len());
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
}
