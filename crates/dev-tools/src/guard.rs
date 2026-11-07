//! Argument validation and the workspace allow-list.

use std::path::{Component, Path, PathBuf};

use tool_registry::ToolError;

fn invalid(msg: impl Into<String>) -> ToolError {
    ToolError::InvalidInput(msg.into())
}

/// Validate a repository-relative pathspec. Rejects option-like values
/// (leading `-`), pathspec magic (leading `:`), absolute paths, `..`, NUL and
/// line breaks.
pub(crate) fn check_path(p: &str) -> Result<(), ToolError> {
    if p.is_empty() {
        return Err(invalid("path is empty"));
    }
    if p.starts_with('-') {
        return Err(invalid(format!("path must not start with '-': {p:?}")));
    }
    if p.starts_with(':') {
        return Err(invalid(format!("path must not start with ':': {p:?}")));
    }
    if p.contains(['\0', '\n', '\r']) {
        return Err(invalid("path contains a control character"));
    }
    if p.starts_with(['/', '\\']) {
        return Err(invalid(format!("absolute paths are not allowed: {p:?}")));
    }
    for c in Path::new(p).components() {
        match c {
            Component::Prefix(_) | Component::RootDir => {
                return Err(invalid(format!("absolute paths are not allowed: {p:?}")));
            }
            Component::ParentDir => {
                return Err(invalid(format!("'..' is not allowed in paths: {p:?}")));
            }
            Component::CurDir | Component::Normal(_) => {}
        }
    }
    Ok(())
}

/// Validate a list of paths.
pub(crate) fn check_paths(paths: &[String]) -> Result<(), ToolError> {
    paths.iter().try_for_each(|p| check_path(p))
}

/// Validate a revision expression (`HEAD~2`, `main..topic`, `v1.0`, a hash).
pub(crate) fn check_rev(r: &str) -> Result<(), ToolError> {
    if r.is_empty() || r.len() > 200 {
        return Err(invalid("revision must be 1..=200 characters"));
    }
    if r.starts_with('-') {
        return Err(invalid(format!("revision must not start with '-': {r:?}")));
    }
    let ok = r
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "._/-@~^{}".contains(c));
    if !ok {
        return Err(invalid(format!(
            "revision has unsupported characters: {r:?}"
        )));
    }
    Ok(())
}

/// Cheap pre-check for a branch name; `git check-ref-format --branch` is the
/// authoritative check afterwards.
pub(crate) fn check_branch_chars(name: &str) -> Result<(), ToolError> {
    if name.is_empty() || name.len() > 200 {
        return Err(invalid("branch name must be 1..=200 characters"));
    }
    if name.starts_with('-') {
        return Err(invalid(format!(
            "branch name must not start with '-': {name:?}"
        )));
    }
    if name
        .chars()
        .any(|c| c.is_control() || c.is_whitespace() || "~^:?*[\\@{".contains(c))
    {
        return Err(invalid(format!("invalid branch name: {name:?}")));
    }
    Ok(())
}

/// The set of directories git tools may touch.
#[derive(Debug, Clone)]
pub(crate) struct Workspace {
    roots: Vec<PathBuf>,
}

impl Workspace {
    pub(crate) fn new(roots: Vec<PathBuf>) -> Self {
        Self { roots }
    }

    /// `true` when `path` (already canonical) is inside an allowed root.
    pub(crate) fn contains_canonical(&self, path: &Path) -> bool {
        self.roots
            .iter()
            .filter_map(|r| r.canonicalize().ok())
            .any(|r| path.starts_with(r))
    }

    /// Resolve the `repo` argument to the directory git is run in. `None` means
    /// the first allowed root; a relative path is relative to the first root.
    pub(crate) fn resolve_dir(&self, repo: Option<&str>) -> Result<PathBuf, ToolError> {
        let first = self.roots.first().ok_or_else(|| {
            ToolError::PermissionDenied("no workspace roots are configured".into())
        })?;
        let dir = match repo {
            None => first.clone(),
            Some(r) => {
                if r.is_empty() || r.contains('\0') {
                    return Err(invalid("repo path is empty or contains NUL"));
                }
                let p = PathBuf::from(r);
                if p.is_absolute() { p } else { first.join(p) }
            }
        };
        let canon = dir
            .canonicalize()
            .map_err(|e| invalid(format!("repo {} is not accessible: {e}", dir.display())))?;
        if !canon.is_dir() {
            return Err(invalid(format!("{} is not a directory", dir.display())));
        }
        if !self.contains_canonical(&canon) {
            return Err(ToolError::PermissionDenied(format!(
                "{} is outside the allowed workspace roots",
                dir.display()
            )));
        }
        Ok(canon)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_rules() {
        for bad in [
            "",
            "-x",
            "--output=a",
            ":(top)a",
            "../a",
            "a/../b",
            "/etc/passwd",
            "\\a",
            "a\nb",
        ] {
            assert!(check_path(bad).is_err(), "{bad:?} should be rejected");
        }
        for good in ["a.txt", "src/lib.rs", "./a", "dir/with space/f"] {
            assert!(check_path(good).is_ok(), "{good:?} should pass");
        }
        #[cfg(windows)]
        assert!(check_path("C:\\x").is_err());
    }

    #[test]
    fn rev_rules() {
        for bad in ["", "-1", "--output=x", "a b", "a;b", "a:b", "$(x)"] {
            assert!(check_rev(bad).is_err(), "{bad:?}");
        }
        for good in [
            "HEAD",
            "HEAD~2",
            "main..topic",
            "v1.0.0",
            "abc123",
            "HEAD@{1}",
            "origin/main^",
        ] {
            assert!(check_rev(good).is_ok(), "{good:?}");
        }
    }

    #[test]
    fn branch_chars() {
        for bad in ["", "-b", "a b", "a~1", "a:b", "x@{u}", "a\\b"] {
            assert!(check_branch_chars(bad).is_err(), "{bad:?}");
        }
        assert!(check_branch_chars("feat/ok-1.2").is_ok());
    }
}
