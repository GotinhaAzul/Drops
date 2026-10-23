//! Project-root path confinement.
//!
//! Every filesystem tool resolves a model-supplied path through [`confine`].
//! The root is canonicalized once, then paths are resolved by canonicalizing
//! their nearest existing ancestor. This rejects:
//!
//! * `..` traversal that leaves the root,
//! * absolute paths outside the root,
//! * symlinks (files or directories) that point outside the root.
//!
//! Lexical components are normalized before the filesystem walk, so a path is
//! never accepted merely because `Path::starts_with` sees a matching prefix
//! (for example `/root/../root2`).

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

use anyhow::{bail, Context, Result};

/// Canonicalize the project root, creating it if needed.
pub(crate) fn canonical_root(dir: &Path) -> Result<PathBuf> {
    if !dir.exists() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("creating project directory {}", dir.display()))?;
    }
    let canonical = std::fs::canonicalize(dir)
        .with_context(|| format!("resolving project directory {}", dir.display()))?;
    if !canonical.is_dir() {
        bail!("project root is not a directory: {}", dir.display());
    }
    Ok(canonical)
}

/// Resolve a model-supplied path against `root` and require it to stay inside.
///
/// `root` must already be canonical (see [`canonical_root`]). The returned path
/// is lexically inside the root even when the leaf does not exist yet, which is
/// what `write` needs before creating parent directories.
pub(crate) fn confine(root: &Path, requested: &str) -> Result<PathBuf> {
    if requested.trim().is_empty() {
        bail!("path must not be empty");
    }
    if requested.contains('\0') {
        bail!("path contains a NUL byte");
    }

    let raw = Path::new(requested);
    let candidate = if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        root.join(raw)
    };

    // Lexically normalize. `PathBuf::pop` on `/` returns false, so attempts to
    // climb above the filesystem root fail here rather than later.
    let mut normalized = PathBuf::new();
    for component in candidate.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    bail!("path escapes project root: {requested}");
                }
            }
            Component::Normal(part) => normalized.push(part),
        }
    }

    // Walk up to the nearest existing path. Missing trailing components are
    // appended after canonicalizing the existing ancestor, so creating nested
    // files cannot be used to escape through a symlinked parent.
    let mut existing = normalized.clone();
    let mut missing: Vec<OsString> = Vec::new();
    loop {
        match std::fs::symlink_metadata(&existing) {
            Ok(_) => break,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                let name = existing.file_name().ok_or_else(|| {
                    anyhow::anyhow!("cannot resolve path inside project root: {requested}")
                })?;
                missing.push(name.to_os_string());
                if !existing.pop() || existing.as_os_str().is_empty() {
                    bail!("cannot resolve path inside project root: {requested}");
                }
            }
            Err(err) => {
                return Err(err).with_context(|| format!("inspecting {}", existing.display()));
            }
        }
    }

    let canonical_existing = std::fs::canonicalize(&existing)
        .with_context(|| format!("resolving {}", existing.display()))?;
    if !canonical_existing.starts_with(root) {
        bail!(
            "path escapes project root: {} (resolved to {})",
            requested,
            canonical_existing.display()
        );
    }

    let mut resolved = canonical_existing;
    for part in missing.iter().rev() {
        resolved.push(part);
    }

    // Belt and braces: after re-attaching the missing components the result must
    // still be component-wise inside the root. Symlinked descendants cannot
    // appear here because they did not exist when we walked the path.
    if !resolved.starts_with(root) {
        bail!("path escapes project root: {requested}");
    }
    Ok(resolved)
}

/// Render a path relative to the root for tool output, never leaking a host
/// path that the model was not allowed to see.
pub(crate) fn display_path(root: &Path, path: &Path) -> String {
    match path.strip_prefix(root) {
        Ok(relative) if !relative.as_os_str().is_empty() => relative.display().to_string(),
        Ok(_) => ".".to_string(),
        Err(_) => path.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    #[test]
    fn resolves_relative_paths_inside_root() {
        let temp = TempDir::new();
        let root = canonical_root(&temp.path().join("project")).unwrap();
        std::fs::create_dir_all(root.join("nested")).unwrap();
        std::fs::write(root.join("nested/file.txt"), "hello").unwrap();

        let resolved = confine(&root, "nested/file.txt").unwrap();
        assert_eq!(resolved, root.join("nested/file.txt"));
        assert_eq!(display_path(&root, &resolved), "nested/file.txt");
    }

    #[test]
    fn accepts_absolute_paths_inside_root() {
        let temp = TempDir::new();
        let root = canonical_root(&temp.path().join("project")).unwrap();
        std::fs::write(root.join("a.txt"), "a").unwrap();

        let absolute = root.join("a.txt").to_string_lossy().into_owned();
        assert_eq!(confine(&root, &absolute).unwrap(), root.join("a.txt"));
    }

    #[test]
    fn rejects_parent_traversal() {
        let temp = TempDir::new();
        let root = canonical_root(&temp.path().join("project")).unwrap();
        let err = confine(&root, "../outside.txt").unwrap_err().to_string();
        assert!(err.contains("escapes project root"), "{err}");
    }

    #[test]
    fn rejects_sneaky_traversal_that_reenters_a_sibling_prefix() {
        let temp = TempDir::new();
        let root = canonical_root(&temp.path().join("project")).unwrap();
        let sibling = format!("{}-sibling", root.display());
        std::fs::create_dir_all(&sibling).unwrap();
        let attempted = format!(
            "{sibling}/../{}",
            root.file_name().unwrap().to_string_lossy()
        );

        // This actually resolves back to the root, which is allowed. The important
        // case is a sibling with a shared string prefix:
        let sibling_escape = format!(
            "../{}-sibling/secret.txt",
            root.file_name().unwrap().to_string_lossy()
        );
        let err = confine(&root, &sibling_escape).unwrap_err().to_string();
        assert!(err.contains("escapes project root"), "{err}");

        // And the normalized path must not be treated as inside via string prefix.
        let resolved = confine(&root, &attempted).unwrap();
        assert_eq!(resolved, root);
    }

    #[test]
    fn rejects_absolute_paths_outside_root() {
        let temp = TempDir::new();
        let root = canonical_root(&temp.path().join("project")).unwrap();
        let outside = temp.path().join("outside.txt");
        std::fs::write(&outside, "nope").unwrap();

        let err = confine(&root, &outside.to_string_lossy())
            .unwrap_err()
            .to_string();
        assert!(err.contains("escapes project root"), "{err}");
    }

    #[test]
    fn allows_missing_leaf_and_nested_parents() {
        let temp = TempDir::new();
        let root = canonical_root(&temp.path().join("project")).unwrap();
        let resolved = confine(&root, "new/nested/file.txt").unwrap();
        assert_eq!(resolved, root.join("new/nested/file.txt"));
        assert!(!resolved.exists());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinked_file_outside_root() {
        let temp = TempDir::new();
        let root = canonical_root(&temp.path().join("project")).unwrap();
        let outside = temp.path().join("outside.txt");
        std::fs::write(&outside, "secret").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("link.txt")).unwrap();

        let err = confine(&root, "link.txt").unwrap_err().to_string();
        assert!(err.contains("escapes project root"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinked_directory_outside_root() {
        let temp = TempDir::new();
        let root = canonical_root(&temp.path().join("project")).unwrap();
        let outside = temp.path().join("outside-dir");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), "secret").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("linkdir")).unwrap();

        let err = confine(&root, "linkdir/secret.txt")
            .unwrap_err()
            .to_string();
        assert!(err.contains("escapes project root"), "{err}");
        let err = confine(&root, "linkdir/new-file.txt")
            .unwrap_err()
            .to_string();
        assert!(err.contains("escapes project root"), "{err}");
    }

    #[test]
    fn rejects_empty_paths() {
        let temp = TempDir::new();
        let root = canonical_root(&temp.path().join("project")).unwrap();
        assert!(confine(&root, "").is_err());
        assert!(confine(&root, "   ").is_err());
        assert!(confine(&root, "a\0b").is_err());
    }
}
