use std::path::{Component, Path, PathBuf};

use crate::ToolError;

/// The folder an agent works in. Paths given to tools resolve inside it and may not escape it,
/// including through `..` or symbolic links.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workspace {
    root: PathBuf,
}

impl Workspace {
    /// `root` must be an existing directory.
    pub fn new(root: &Path) -> std::io::Result<Self> {
        let root = root.canonicalize()?;
        if !root.is_dir() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotADirectory,
                format!("{} is not a directory", root.display()),
            ));
        }
        Ok(Workspace { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Resolves a path (relative to the root, or absolute inside it) to an absolute path inside
    /// the workspace. The path itself need not exist yet; its nearest existing ancestor must
    /// lie inside the workspace after resolving symbolic links.
    pub fn resolve(&self, path: &str) -> Result<PathBuf, ToolError> {
        let candidate = self.root.join(path);
        let mut existing = candidate.as_path();
        let mut missing = Vec::new();
        while !existing.exists() {
            let name = existing
                .file_name()
                .ok_or_else(|| ToolError::OutsideWorkspace(path.to_string()))?;
            missing.push(name.to_owned());
            existing = existing
                .parent()
                .ok_or_else(|| ToolError::OutsideWorkspace(path.to_string()))?;
        }
        let mut resolved = existing
            .canonicalize()
            .map_err(|error| ToolError::Io(format!("{path}: {error}")))?;
        if !resolved.starts_with(&self.root) {
            return Err(ToolError::OutsideWorkspace(path.to_string()));
        }
        for name in missing.into_iter().rev() {
            if Path::new(&name)
                .components()
                .any(|c| !matches!(c, Component::Normal(_)))
            {
                return Err(ToolError::OutsideWorkspace(path.to_string()));
            }
            resolved.push(name);
        }
        Ok(resolved)
    }

    /// A path for display to the model and user: relative to the root, with `/` separators.
    pub fn display(&self, path: &Path) -> String {
        let relative = path.strip_prefix(&self.root).unwrap_or(path);
        let shown = relative.to_string_lossy().replace('\\', "/");
        if shown.is_empty() { ".".into() } else { shown }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_resolve_inside_and_are_refused_outside() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        let workspace = Workspace::new(dir.path()).unwrap();

        let existing = workspace.resolve("src").unwrap();
        assert_eq!(workspace.display(&existing), "src");
        let new_file = workspace.resolve("src/new/main.rs").unwrap();
        assert_eq!(workspace.display(&new_file), "src/new/main.rs");
        assert_eq!(workspace.display(&workspace.resolve(".").unwrap()), ".");

        for outside in ["..", "../x", "src/../../x", "nothing/../../x"] {
            assert!(
                matches!(
                    workspace.resolve(outside),
                    Err(ToolError::OutsideWorkspace(_))
                ),
                "{outside} should be refused"
            );
        }
        let elsewhere = tempfile::tempdir().unwrap();
        let absolute = elsewhere.path().to_string_lossy().to_string();
        assert!(matches!(
            workspace.resolve(&absolute),
            Err(ToolError::OutsideWorkspace(_))
        ));
    }
}
