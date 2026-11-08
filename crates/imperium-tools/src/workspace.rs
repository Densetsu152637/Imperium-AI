use std::path::{Component, Path, PathBuf};

use crate::{Sandbox, ToolError};

/// The folders an agent works in: an environment's folders, the first of which is the working
/// directory (the root). Relative paths given to tools resolve against the root; absolute paths
/// may name anything inside any of the folders. Nothing may escape them, including through `..`
/// or symbolic links.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workspace {
    root: PathBuf,
    /// Further folders the agent may use, canonical and distinct from the root and each other.
    extra: Vec<PathBuf>,
    sandbox: Sandbox,
}

impl Workspace {
    /// `root` must be an existing directory.
    pub fn new(root: &Path) -> std::io::Result<Self> {
        let root = existing_dir(root)?;
        Ok(Workspace {
            root,
            extra: Vec::new(),
            sandbox: Sandbox::Policy,
        })
    }

    /// The same workspace with further folders the agent may use. Each must be an existing
    /// directory; one already covered (the root, or a folder given twice) is skipped.
    pub fn with_folders<P: AsRef<Path>>(
        mut self,
        folders: impl IntoIterator<Item = P>,
    ) -> std::io::Result<Self> {
        for folder in folders {
            let folder = existing_dir(folder.as_ref())?;
            if folder != self.root && !self.extra.contains(&folder) {
                self.extra.push(folder);
            }
        }
        Ok(self)
    }

    /// The same workspace with commands run under `sandbox`.
    pub fn with_sandbox(self, sandbox: Sandbox) -> Self {
        Workspace { sandbox, ..self }
    }

    /// The working directory: the first folder.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Every folder the agent may use, the root first.
    pub fn roots(&self) -> Vec<&Path> {
        std::iter::once(self.root.as_path())
            .chain(self.extra.iter().map(PathBuf::as_path))
            .collect()
    }

    /// Every folder as people write them, the root first.
    pub fn folders_display(&self) -> Vec<String> {
        self.roots().into_iter().map(plain).collect()
    }

    pub fn sandbox(&self) -> &Sandbox {
        &self.sandbox
    }

    /// Resolves a path (relative to the root, or absolute inside any folder) to an absolute path
    /// inside the workspace. The path itself need not exist yet; its nearest existing ancestor
    /// must lie inside one of the folders after resolving symbolic links.
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
        if !self.roots().iter().any(|root| resolved.starts_with(root)) {
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

    /// The root as people write it: without Windows' `\\?\` verbatim prefix on drive paths.
    pub fn root_display(&self) -> String {
        plain(&self.root)
    }

    /// A path for display to the model and user: relative to the root, with `/` separators.
    /// Paths in the other folders are shown in full, since that is how the agent names them.
    pub fn display(&self, path: &Path) -> String {
        match path.strip_prefix(&self.root) {
            Ok(relative) => {
                let shown = relative.to_string_lossy().replace('\\', "/");
                if shown.is_empty() { ".".into() } else { shown }
            }
            Err(_) => plain(path).replace('\\', "/"),
        }
    }
}

fn existing_dir(path: &Path) -> std::io::Result<PathBuf> {
    let path = path.canonicalize()?;
    if !path.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotADirectory,
            format!("{} is not a directory", path.display()),
        ));
    }
    Ok(path)
}

/// A path as people write it: without Windows' `\\?\` verbatim prefix on drive paths.
fn plain(path: &Path) -> String {
    let shown = path.display().to_string();
    match shown.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC") => rest.to_string(),
        _ => shown,
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
        assert!(!workspace.root_display().starts_with(r"\\?\"));

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

    #[test]
    fn every_folder_of_an_environment_is_inside_and_nothing_else() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(second.path().join("notes.txt"), "two").unwrap();
        let workspace = Workspace::new(first.path())
            .unwrap()
            .with_folders([second.path(), first.path(), second.path()])
            .unwrap();
        assert_eq!(workspace.roots().len(), 2, "duplicates are skipped");
        assert_eq!(workspace.folders_display().len(), 2);
        assert!(!workspace.folders_display()[1].starts_with(r"\\?\"));

        let in_second = second.path().join("notes.txt");
        let resolved = workspace.resolve(&in_second.to_string_lossy()).unwrap();
        assert!(workspace.display(&resolved).ends_with("/notes.txt"));
        let new_in_second = second.path().join("new").join("file.txt");
        assert!(workspace.resolve(&new_in_second.to_string_lossy()).is_ok());
        // Relative paths start at the first folder and cannot climb out of it.
        assert!(workspace.resolve("../x").is_err());
        let elsewhere = outside.path().join("x");
        assert!(matches!(
            workspace.resolve(&elsewhere.to_string_lossy()),
            Err(ToolError::OutsideWorkspace(_))
        ));
        assert!(
            Workspace::new(first.path())
                .unwrap()
                .with_folders([outside.path().join("missing")])
                .is_err()
        );
    }
}
