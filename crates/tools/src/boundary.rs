use std::path::{Path, PathBuf};

/// Path boundary for file operations.
///
/// When `sandbox_enabled` is true, `check()` enforces that the canonicalized
/// path lies within the allowlist (project root, common cache/tmp dirs, and
/// any explicitly-configured extra paths). When false, the raw path is
/// returned unchanged (no check).
#[derive(Clone)]
pub struct ProjectBoundary {
    root: PathBuf,
    sandbox_enabled: bool,
    allowed_paths: Vec<PathBuf>,
}

impl ProjectBoundary {
    /// Create a new boundary with sandboxing enabled by default.
    ///
    /// The default allowlist includes the project root, `~/.claude`,
    /// `/tmp`, `/var/folders` (macOS), `std::env::temp_dir()`,
    /// and `$XDG_CACHE_HOME` (or `~/.cache`).
    pub fn new(root: PathBuf) -> Self {
        let root = root.canonicalize().unwrap_or(root);
        // Honor BRIDGE_SANDBOX_ENABLED for callers that don't know about the
        // config struct (builtin tool registration paths, tests). Default on.
        let sandbox_enabled = match std::env::var("BRIDGE_SANDBOX_ENABLED") {
            Ok(v) => !matches!(v.trim().to_lowercase().as_str(), "0" | "false" | "no" | "off"),
            Err(_) => true,
        };
        Self {
            root,
            sandbox_enabled,
            allowed_paths: Vec::new(),
        }
    }

    /// Disable sandboxing. When disabled, `check()` returns the raw path.
    pub fn with_sandbox_enabled(mut self, enabled: bool) -> Self {
        self.sandbox_enabled = enabled;
        self
    }

    /// Extend the allowlist with additional paths.
    pub fn with_allowed_paths(mut self, paths: Vec<PathBuf>) -> Self {
        self.allowed_paths = paths;
        self
    }

    /// Build the full allowlist including the dynamic defaults (home caches,
    /// tmp dirs). Canonicalized where possible so the prefix check works.
    fn build_allowlist(&self) -> Vec<PathBuf> {
        let mut list: Vec<PathBuf> = Vec::new();
        list.push(self.root.clone());

        if let Ok(home) = std::env::var("HOME") {
            let claude_dir = PathBuf::from(&home).join(".claude");
            if claude_dir.exists() {
                list.push(claude_dir.canonicalize().unwrap_or(claude_dir));
            }
            let xdg_cache = std::env::var("XDG_CACHE_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from(&home).join(".cache"));
            if xdg_cache.exists() {
                list.push(xdg_cache.canonicalize().unwrap_or(xdg_cache));
            }
        }

        // /var/folders is macOS's per-user tmp. /private/tmp and /private/var/folders
        // are the canonical (firmlink-resolved) forms of /tmp and /var/folders on macOS.
        // We deliberately avoid /private/var root — it holds /private/var/root and
        // /private/var/log etc. which should not be writable by agents.
        for base in [
            "/tmp",
            "/var/folders",
            "/private/tmp",
            "/private/var/folders",
        ] {
            let p = PathBuf::from(base);
            if p.exists() {
                list.push(p.canonicalize().unwrap_or(p));
            }
        }

        let tmp = std::env::temp_dir();
        list.push(tmp.canonicalize().unwrap_or(tmp));

        for extra in &self.allowed_paths {
            list.push(extra.canonicalize().unwrap_or_else(|_| extra.clone()));
        }

        list
    }

    /// Check if a path is within the allowlist.
    ///
    /// Returns the canonicalized path on success. When sandboxing is disabled,
    /// returns the raw path unchanged (legacy behavior).
    pub fn check(&self, path: &str) -> Result<PathBuf, String> {
        if !self.sandbox_enabled {
            return Ok(PathBuf::from(path));
        }

        let p = Path::new(path);

        // Canonicalize: for existing paths, direct canonicalize. For new
        // files, canonicalize the deepest existing ancestor and re-append
        // the remainder so writes to new files still resolve.
        let canonical = if p.exists() {
            p.canonicalize()
                .map_err(|e| format!("Failed to resolve path '{}': {}", path, e))?
        } else {
            let (anchor, remainder) = deepest_existing_ancestor(p);
            let anchor_canonical = anchor
                .canonicalize()
                .map_err(|e| format!("Failed to resolve parent of '{}': {}", path, e))?;
            let mut joined = anchor_canonical;
            for component in remainder {
                joined.push(component);
            }
            joined
        };

        let allowlist = self.build_allowlist();
        if allowlist.iter().any(|a| canonical.starts_with(a)) {
            Ok(canonical)
        } else {
            let allowlist_str: Vec<String> =
                allowlist.iter().map(|p| p.display().to_string()).collect();
            Err(format!(
                "Access denied: '{}' is outside the sandbox allowlist ({})",
                path,
                allowlist_str.join(", ")
            ))
        }
    }

    /// Like `check` but returns a structured `BridgeError::SandboxViolation`
    /// rather than a `String`.
    pub fn check_typed(&self, path: &str) -> Result<PathBuf, bridge_core::BridgeError> {
        if !self.sandbox_enabled {
            return Ok(PathBuf::from(path));
        }

        let p = Path::new(path);
        let canonical = if p.exists() {
            p.canonicalize().map_err(|e| {
                bridge_core::BridgeError::InvalidRequest(format!(
                    "Failed to resolve path '{}': {}",
                    path, e
                ))
            })?
        } else {
            let (anchor, remainder) = deepest_existing_ancestor(p);
            let anchor_canonical = anchor.canonicalize().map_err(|e| {
                bridge_core::BridgeError::InvalidRequest(format!(
                    "Failed to resolve parent of '{}': {}",
                    path, e
                ))
            })?;
            let mut joined = anchor_canonical;
            for component in remainder {
                joined.push(component);
            }
            joined
        };

        let allowlist = self.build_allowlist();
        if allowlist.iter().any(|a| canonical.starts_with(a)) {
            Ok(canonical)
        } else {
            Err(bridge_core::BridgeError::SandboxViolation {
                path: path.to_string(),
                allowlist: allowlist.iter().map(|p| p.display().to_string()).collect(),
            })
        }
    }

    /// Return the project root path.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Whether sandboxing is currently enabled.
    pub fn sandbox_enabled(&self) -> bool {
        self.sandbox_enabled
    }
}

/// Walk ancestors of `p` until we find one that exists; return that anchor
/// plus the remaining components (in original order) to re-append.
fn deepest_existing_ancestor(p: &Path) -> (PathBuf, Vec<std::ffi::OsString>) {
    let mut remainder: Vec<std::ffi::OsString> = Vec::new();
    let mut cur = p.to_path_buf();
    loop {
        if cur.exists() {
            return (cur, remainder.into_iter().rev().collect());
        }
        match cur.file_name() {
            Some(name) => remainder.push(name.to_os_string()),
            None => {
                // Reached root or relative base with no parent — fall back
                // to "." (current dir) as the anchor.
                return (PathBuf::from("."), remainder.into_iter().rev().collect());
            }
        }
        if !cur.pop() {
            return (PathBuf::from("."), remainder.into_iter().rev().collect());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_path_within_root_allowed() {
        let dir = tempdir().expect("create temp dir");
        let file_path = dir.path().join("test.txt");
        fs::write(&file_path, "hello").expect("write");

        let boundary = ProjectBoundary::new(dir.path().to_path_buf());
        let result = boundary.check(file_path.to_str().unwrap());
        assert!(result.is_ok());
    }

    #[test]
    fn test_sandbox_disabled_allows_any_path() {
        let dir = tempdir().expect("create temp dir");
        let boundary =
            ProjectBoundary::new(dir.path().to_path_buf()).with_sandbox_enabled(false);

        // With sandbox disabled, arbitrary paths are returned as-is
        let result = boundary.check("/etc/passwd");
        assert!(result.is_ok());
    }

    #[test]
    fn test_sandbox_enabled_blocks_outside_paths() {
        let dir = tempdir().expect("create temp dir");
        let boundary = ProjectBoundary::new(dir.path().to_path_buf());

        // /etc is not in the default allowlist
        let result = boundary.check("/etc/passwd");
        assert!(result.is_err(), "expected /etc/passwd to be denied");
    }

    #[test]
    fn test_new_file_within_root_allowed() {
        let dir = tempdir().expect("create temp dir");
        let boundary = ProjectBoundary::new(dir.path().to_path_buf());

        let new_file = dir.path().join("new_file.txt");
        let result = boundary.check(new_file.to_str().unwrap());
        assert!(result.is_ok());
    }

    #[test]
    fn test_tmp_path_is_allowed() {
        let boundary = ProjectBoundary::new(std::env::temp_dir());
        let tmp_file = std::env::temp_dir().join("bridge_test_file.txt");
        let result = boundary.check(tmp_file.to_str().unwrap());
        assert!(result.is_ok(), "expected tmp path to be allowed: {:?}", result);
    }

    #[test]
    fn test_traversal_is_defeated_by_canonicalize() {
        let dir = tempdir().expect("create temp dir");
        let boundary = ProjectBoundary::new(dir.path().to_path_buf());

        // Attempt `..` traversal from inside the project to /etc
        let traversal = format!("{}/../../../../../etc/passwd", dir.path().display());
        let result = boundary.check(&traversal);
        assert!(result.is_err(), "traversal should be denied: {:?}", result);
    }

    #[test]
    fn test_extra_allowed_paths() {
        let dir = tempdir().expect("create temp dir");
        let extra_dir = tempdir().expect("create extra dir");
        let boundary = ProjectBoundary::new(dir.path().to_path_buf())
            .with_allowed_paths(vec![extra_dir.path().to_path_buf()]);

        let file_in_extra = extra_dir.path().join("file.txt");
        fs::write(&file_in_extra, "hi").expect("write");
        let result = boundary.check(file_in_extra.to_str().unwrap());
        assert!(result.is_ok());
    }

    #[test]
    fn test_check_typed_returns_sandbox_violation() {
        let dir = tempdir().expect("create temp dir");
        let boundary = ProjectBoundary::new(dir.path().to_path_buf());

        let result = boundary.check_typed("/etc/passwd");
        match result {
            Err(bridge_core::BridgeError::SandboxViolation { path, allowlist }) => {
                assert_eq!(path, "/etc/passwd");
                assert!(!allowlist.is_empty());
            }
            other => panic!("expected SandboxViolation, got {:?}", other),
        }
    }
}
