use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use crate::permissions::{PathGrant, PathMode, Permissions, Target, expand_home};
use crate::processes::Processes;
use crate::project_worktrees::ProjectWorktrees;
use crate::runtime::{Remote, Runner, normalize_posix, wsl_path};
use crate::writer_trees::WriterHandle;
use crate::{Sandbox, ToolError, WriterTrees};

/// The folders an agent works in: an environment's folders, the first of which is the working
/// directory (the root). Relative paths given to tools resolve against the root; absolute paths
/// may name anything inside any of the folders. Nothing may escape them, including through `..`
/// or symbolic links.
///
/// Path grants open further paths outside the folders, read-only or writable (global package
/// caches and toolchains, for example). Where a folder and a grant both cover a path, the
/// deepest one decides, so a read-only grant inside a folder makes that part read-only.
///
/// With [`Workspace::with_project_worktrees`], the git worktrees of the folders' repositories
/// count as folders too (discovered from git's records, and looked up again when a path falls
/// outside everything else, so new ones are covered).
///
/// A remote workspace's folders are paths inside a runtime ([`Remote`]); its tools run there
/// through a [`Runner`], and paths are checked as written (after resolving `.` and `..`), since
/// links inside the runtime cannot be followed from here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workspace {
    root: PathBuf,
    /// Further folders the agent may use, canonical and distinct from the root and each other.
    extra: Vec<PathBuf>,
    /// Paths outside the folders the tools may also use.
    grants: Vec<GrantedPath>,
    sandbox: Sandbox,
    remote: Option<RemoteFolders>,
    /// The permission rules the session's calls are checked against.
    permissions: Permissions,
    /// The folders' repositories' worktrees, when they are open too.
    worktrees: Option<Arc<ProjectWorktrees>>,
    /// The session's own scratch folder (canonical), which is also one of the writable grants.
    scratch: Option<PathBuf>,
    /// Commands started in the background; clones share them.
    processes: Processes,
    /// Makes writer subagents' worktrees, when the daemon offers them.
    writers: Option<WriterHandle>,
}

/// A path outside the workspace's folders that the tools may use: canonical on this machine,
/// or an absolute POSIX path inside a remote runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantedPath {
    pub path: PathBuf,
    pub writable: bool,
}

impl GrantedPath {
    /// The path as people write it, without Windows' verbatim prefix.
    pub fn display_path(&self) -> String {
        plain(&self.path)
    }

    /// The grant as people read it, such as `/home/me/.cargo (read-only)`.
    pub fn describe(&self) -> String {
        format!(
            "{}{}",
            plain(&self.path),
            if self.writable { "" } else { " (read-only)" }
        )
    }
}

/// A remote workspace's runtime and folders (POSIX paths inside it, the root first).
#[derive(Debug, Clone)]
pub(crate) struct RemoteFolders {
    pub target: Remote,
    pub folders: Vec<String>,
    pub runner: Arc<dyn Runner>,
}

impl PartialEq for RemoteFolders {
    fn eq(&self, other: &Self) -> bool {
        self.target == other.target && self.folders == other.folders
    }
}

impl Eq for RemoteFolders {}

impl Workspace {
    /// `root` must be an existing directory.
    pub fn new(root: &Path) -> std::io::Result<Self> {
        let root = existing_dir(root)?;
        Ok(Workspace {
            root,
            extra: Vec::new(),
            grants: Vec::new(),
            sandbox: Sandbox::Policy,
            remote: None,
            permissions: Permissions::default(),
            worktrees: None,
            scratch: None,
            processes: Processes::default(),
            writers: None,
        })
    }

    /// A workspace inside a remote runtime. `folders` are absolute POSIX paths there, the
    /// working directory first (for WSL, Windows paths are translated to `/mnt/<drive>/…`).
    /// Nothing is checked on the runtime itself: a missing folder fails the tool that uses it.
    pub fn remote(
        target: Remote,
        folders: &[String],
        runner: Arc<dyn Runner>,
    ) -> Result<Self, ToolError> {
        let mut checked: Vec<String> = Vec::new();
        for folder in folders {
            let written = match target {
                Remote::Wsl { .. } => wsl_path(folder),
                _ => folder.trim().to_string(),
            };
            let folder = normalize_posix(&written).ok_or_else(|| {
                ToolError::Invalid(format!(
                    "{written} is not an absolute path inside the runtime"
                ))
            })?;
            if !checked.contains(&folder) {
                checked.push(folder);
            }
        }
        let Some(first) = checked.first() else {
            return Err(ToolError::Invalid("the workspace has no folder".into()));
        };
        Ok(Workspace {
            root: PathBuf::from(first),
            extra: Vec::new(),
            grants: Vec::new(),
            sandbox: Sandbox::Policy,
            remote: Some(RemoteFolders {
                target,
                folders: checked,
                runner,
            }),
            permissions: Permissions::default(),
            worktrees: None,
            scratch: None,
            writers: None,
            processes: Processes::default(),
        })
    }

    /// The runtime a remote workspace's tools run in.
    pub fn runtime(&self) -> Option<&Remote> {
        self.remote.as_ref().map(|remote| &remote.target)
    }

    pub(crate) fn remote_folders(&self) -> Option<&RemoteFolders> {
        self.remote.as_ref()
    }

    /// Where the session's commands run, such as `tier 0 · folder policy` or `runtime · WSL
    /// Ubuntu`.
    pub fn isolation_label(&self) -> String {
        match &self.remote {
            Some(remote) => remote.target.label(),
            None => self.sandbox.label(),
        }
    }

    /// A local folder to run local helpers (hooks) in: the root, or for a remote workspace the
    /// system's temporary folder, since its root is not on this machine.
    pub fn local_dir(&self) -> PathBuf {
        match &self.remote {
            Some(_) => std::env::temp_dir(),
            None => self.root.clone(),
        }
    }

    /// Resolves a path for a remote workspace (relative to the root, or absolute inside any
    /// folder) to an absolute POSIX path inside the runtime.
    /// `write` refuses a path covered by a read-only grant.
    pub(crate) fn resolve_remote(&self, path: &str, write: bool) -> Result<String, ToolError> {
        let Some(remote) = &self.remote else {
            return Err(ToolError::Io("not a remote workspace".into()));
        };
        let resolved = self
            .locate_remote(path)
            .ok_or_else(|| ToolError::OutsideWorkspace(path.into()))?;
        let inside = |folder: &str| {
            folder == "/"
                || resolved == folder
                || resolved
                    .strip_prefix(folder)
                    .is_some_and(|rest| rest.starts_with('/'))
        };
        // The deepest folder or grant containing the path decides whether it is writable.
        let covering =
            remote
                .folders
                .iter()
                .map(|folder| (folder.as_str(), true, false))
                .chain(self.grants.iter().filter_map(|grant| {
                    grant.path.to_str().map(|path| (path, grant.writable, true))
                }))
                .filter(|(folder, _, _)| inside(folder))
                .max_by_key(|(folder, _, grant)| (folder.len(), *grant));
        match covering {
            None => Err(ToolError::OutsideWorkspace(path.into())),
            Some((_, false, _)) if write => Err(ToolError::ReadOnly(path.into())),
            Some(_) => Ok(resolved),
        }
    }

    /// A remote path as an absolute POSIX path inside the runtime, unchecked.
    fn locate_remote(&self, path: &str) -> Option<String> {
        let remote = self.remote.as_ref()?;
        let written = match remote.target {
            Remote::Wsl { .. } => wsl_path(path),
            _ => path.trim().to_string(),
        };
        let joined = if written.starts_with('/') {
            written
        } else {
            format!("{}/{written}", remote.folders[0])
        };
        normalize_posix(&joined)
    }

    /// A remote path for display: relative to the root, else in full.
    pub(crate) fn display_remote(&self, path: &str) -> String {
        let root = self.root_display();
        if path == root {
            return ".".into();
        }
        match path
            .strip_prefix(root.as_str())
            .and_then(|rest| rest.strip_prefix('/'))
        {
            Some(relative) => relative.to_string(),
            None => path.to_string(),
        }
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

    /// The same workspace with paths outside its folders opened by `grants`. `~` means the home
    /// folder on this machine. A grant whose path does not exist, or one already given, is
    /// skipped (so list the environment's grants before the global ones to let them win). For a
    /// remote workspace, grants are absolute paths inside the runtime (Windows paths are
    /// translated for WSL); `~` cannot be expanded there, so such grants are skipped.
    pub fn with_grants(mut self, grants: &[PathGrant]) -> Self {
        for grant in grants {
            let writable = grant.mode == PathMode::ReadWrite;
            let path = match &self.remote {
                Some(remote) => {
                    if grant.path.trim().starts_with('~') {
                        continue;
                    }
                    let written = match remote.target {
                        Remote::Wsl { .. } => wsl_path(&grant.path),
                        _ => grant.path.trim().to_string(),
                    };
                    match normalize_posix(&written) {
                        Some(path) => PathBuf::from(path),
                        None => continue,
                    }
                }
                None => match Path::new(&expand_home(&grant.path)).canonicalize() {
                    Ok(path) => path,
                    Err(_) => continue,
                },
            };
            if !self.grants.iter().any(|known| known.path == path) {
                self.grants.push(GrantedPath { path, writable });
            }
        }
        self
    }

    /// The same workspace with `dir`, the session's scratch folder, open for reading and
    /// writing as a grant (so the permission rules, deny rules and the safety floor apply to it
    /// like to any other path). A folder that does not exist, or a remote workspace, gets none.
    pub fn with_scratch(mut self, dir: &Path) -> Self {
        if self.remote.is_some() {
            return self;
        }
        self = self.with_grants(&[PathGrant {
            path: dir.display().to_string(),
            mode: PathMode::ReadWrite,
        }]);
        self.scratch = dir.canonicalize().ok();
        self
    }

    /// The session's scratch folder, when it has one.
    pub fn scratch(&self) -> Option<&Path> {
        self.scratch.as_deref()
    }

    /// The same workspace with the git worktrees of its folders' repositories open as folders
    /// too: the main checkout and every linked worktree, at the same place inside each as the
    /// folder is in its own (a folder below the repository's top opens that subfolder of each).
    /// They are as writable as the folders. Local workspaces only; a remote one is unchanged.
    pub fn with_project_worktrees(mut self) -> Self {
        if self.remote.is_none() {
            self.worktrees = ProjectWorktrees::discover(&self.roots()).map(Arc::new);
        }
        self
    }

    /// The project worktrees open beyond the folders (those not already inside one), looked up
    /// afresh. Empty unless [`Workspace::with_project_worktrees`] found a repository.
    pub fn project_worktrees(&self) -> Vec<PathBuf> {
        if let Some(worktrees) = &self.worktrees {
            worktrees.refresh();
        }
        self.extra_worktrees()
    }

    /// The project worktrees as last found, without those inside a folder.
    fn extra_worktrees(&self) -> Vec<PathBuf> {
        let Some(worktrees) = &self.worktrees else {
            return Vec::new();
        };
        let roots = self.roots();
        worktrees
            .known()
            .into_iter()
            .filter(|tree| !roots.iter().any(|root| tree.starts_with(root)))
            .collect()
    }

    /// The folders commands may use: the folders, then the project worktrees (looked up
    /// afresh).
    pub fn command_roots(&self) -> Vec<PathBuf> {
        self.roots()
            .into_iter()
            .map(Path::to_path_buf)
            .chain(self.project_worktrees())
            .collect()
    }

    /// The same workspace with `writers` making worktrees for parallel writer subagents. Local
    /// workspaces only; a remote one is unchanged.
    pub fn with_writer_trees(mut self, writers: Arc<dyn WriterTrees>) -> Self {
        if self.remote.is_none() {
            self.writers = Some(WriterHandle(writers));
        }
        self
    }

    /// Makes and finishes writer subagents' worktrees, when this workspace has them.
    pub fn writer_trees(&self) -> Option<Arc<dyn WriterTrees>> {
        self.writers.as_ref().map(|handle| Arc::clone(&handle.0))
    }

    /// The same workspace with other folders (the first is the working directory; each must be
    /// an existing directory) and nothing else of its own: the grants, scratch folder, sandbox,
    /// permissions and background commands stay, but the project worktrees do not open and the
    /// copy cannot make worktrees. A writer subagent works in this, so its edits stay in its
    /// own worktrees.
    pub fn with_roots<P: AsRef<Path>>(&self, folders: &[P]) -> std::io::Result<Self> {
        let Some((first, rest)) = folders.split_first() else {
            return Err(std::io::Error::other("a workspace needs a folder"));
        };
        let mut moved = Workspace {
            root: existing_dir(first.as_ref())?,
            extra: Vec::new(),
            worktrees: None,
            writers: None,
            ..self.clone()
        };
        moved = moved.with_folders(rest)?;
        Ok(moved)
    }

    /// The same workspace with commands run under `sandbox`.
    pub fn with_sandbox(self, sandbox: Sandbox) -> Self {
        Workspace { sandbox, ..self }
    }

    /// The same workspace with calls checked against `permissions`.
    pub fn with_permissions(self, permissions: Permissions) -> Self {
        Workspace {
            permissions,
            ..self
        }
    }

    /// The permission rules calls in this workspace are checked against.
    pub fn permissions(&self) -> &Permissions {
        &self.permissions
    }

    /// The paths outside the folders the tools may also use.
    pub fn grants(&self) -> &[GrantedPath] {
        &self.grants
    }

    /// What a file tool's `path` names, for matching rule patterns: the absolute path (with
    /// `/` separators) and, inside the working directory, the relative one. The path need not
    /// be inside the workspace.
    pub fn rule_target(&self, path: &str) -> Target {
        if self.remote.is_some() {
            let absolute = self
                .locate_remote(path)
                .unwrap_or_else(|| path.trim().to_string());
            let root = self.root_display();
            let relative = if absolute == root {
                Some(".".to_string())
            } else {
                absolute
                    .strip_prefix(root.as_str())
                    .and_then(|rest| rest.strip_prefix('/'))
                    .map(str::to_string)
            };
            return Target::Path { absolute, relative };
        }
        let full = self
            .locate(path)
            .unwrap_or_else(|| self.root.join(path.trim()));
        let relative = full.strip_prefix(&self.root).ok().map(|rest| {
            let shown = rest.to_string_lossy().replace('\\', "/");
            if shown.is_empty() { ".".into() } else { shown }
        });
        Target::Path {
            absolute: plain(&full).replace('\\', "/"),
            relative,
        }
    }

    /// [`Workspace::rule_target`] for a path already resolved on this machine (as the search
    /// tools find them), without resolving it again.
    pub(crate) fn rule_target_resolved(&self, full: &Path) -> Target {
        let relative = full.strip_prefix(&self.root).ok().map(|rest| {
            let shown = rest.to_string_lossy().replace('\\', "/");
            if shown.is_empty() { ".".into() } else { shown }
        });
        Target::Path {
            absolute: plain(full).replace('\\', "/"),
            relative,
        }
    }

    /// The session's background processes (`run_command` with `background`).
    pub fn processes(&self) -> &Processes {
        &self.processes
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
        if let Some(remote) = &self.remote {
            return remote.folders.clone();
        }
        self.roots().into_iter().map(plain).collect()
    }

    pub fn sandbox(&self) -> &Sandbox {
        &self.sandbox
    }

    /// Resolves a path to read (relative to the root, or absolute inside any folder or grant) to
    /// an absolute path inside the workspace. The path itself need not exist yet; its nearest
    /// existing ancestor must lie inside one of the folders or grants after resolving symbolic
    /// links.
    pub fn resolve(&self, path: &str) -> Result<PathBuf, ToolError> {
        self.resolve_for(path, false)
    }

    /// Like [`Workspace::resolve`], for a path to write, edit, or delete: a path a read-only
    /// grant covers is refused.
    pub fn resolve_writable(&self, path: &str) -> Result<PathBuf, ToolError> {
        self.resolve_for(path, true)
    }

    fn resolve_for(&self, path: &str, write: bool) -> Result<PathBuf, ToolError> {
        if self.remote.is_some() {
            return self.resolve_remote(path, write).map(PathBuf::from);
        }
        let mut resolved = self.locate_checked(path)?;
        // Outside everything known: a worktree may have been added since the last look.
        if resolved
            .as_ref()
            .is_none_or(|found| self.writable(found).is_none())
            && self
                .worktrees
                .as_ref()
                .is_some_and(|worktrees| worktrees.refresh())
        {
            resolved = self.locate_checked(path)?;
        }
        let resolved = resolved.ok_or_else(|| ToolError::OutsideWorkspace(path.to_string()))?;
        match self.writable(&resolved) {
            None => Err(ToolError::OutsideWorkspace(path.to_string())),
            Some(false) if write => Err(ToolError::ReadOnly(path.to_string())),
            Some(_) => Ok(resolved),
        }
    }

    /// Whether a canonical path may be written: `None` outside every folder and grant. The
    /// deepest folder or grant containing it decides (a grant wins over a folder at the same
    /// depth). Project worktrees count as folders.
    fn writable(&self, resolved: &Path) -> Option<bool> {
        let worktrees = self.extra_worktrees();
        self.roots()
            .into_iter()
            .chain(worktrees.iter().map(PathBuf::as_path))
            .map(|root| (root, true, false))
            .chain(
                self.grants
                    .iter()
                    .map(|grant| (grant.path.as_path(), grant.writable, true)),
            )
            .filter(|(base, _, _)| resolved.starts_with(base))
            .max_by_key(|(base, _, grant)| (base.components().count(), *grant))
            .map(|(_, writable, _)| writable)
    }

    /// The absolute path `path` names on this machine (symbolic links resolved for the part
    /// that exists), wherever it is; `None` when it cannot be worked out.
    fn locate(&self, path: &str) -> Option<PathBuf> {
        self.locate_checked(path).ok().flatten()
    }

    /// Tries each way of reading what the model wrote ([`spellings`]) and keeps the first that
    /// lands inside a folder or grant, else the first reading. Every reading is checked the same
    /// way, so the looser spellings open nothing the exact one would not.
    fn locate_checked(&self, path: &str) -> Result<Option<PathBuf>, ToolError> {
        let mut first = None;
        for spelling in spellings(path) {
            let located = self.locate_one(&spelling);
            if let Ok(Some(resolved)) = &located
                && self.writable(resolved).is_some()
            {
                return located;
            }
            first.get_or_insert(located);
        }
        first.unwrap_or(Ok(None))
    }

    fn locate_one(&self, path: &str) -> Result<Option<PathBuf>, ToolError> {
        let candidate = self.root.join(path);
        let mut existing = candidate.as_path();
        let mut missing = Vec::new();
        while !existing.exists() {
            let (Some(name), Some(parent)) = (existing.file_name(), existing.parent()) else {
                return Ok(None);
            };
            missing.push(name.to_owned());
            existing = parent;
        }
        let mut resolved = existing
            .canonicalize()
            .map_err(|error| ToolError::Io(format!("{path}: {error}")))?;
        for name in missing.into_iter().rev() {
            if Path::new(&name)
                .components()
                .any(|c| !matches!(c, Component::Normal(_)))
            {
                return Ok(None);
            }
            let Some(name) = creatable_name(name) else {
                return Ok(None);
            };
            resolved.push(name);
        }
        Ok(Some(resolved))
    }

    /// The root as people write it: without Windows' `\\?\` verbatim prefix on drive paths.
    pub fn root_display(&self) -> String {
        if let Some(remote) = &self.remote {
            return remote.folders[0].clone();
        }
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

/// The name a not-yet-existing path component gets when it is created. Win32 drops trailing
/// dots and spaces from a name, so `key.pem.` creates `key.pem`; resolving it that way lets
/// rule patterns, containment, and the write all see the same file. `None` when nothing is
/// left (`...`, a lone space) or when the name has a `:`, which on NTFS names an alternate
/// data stream (`key.pem::$DATA` and `key.pem:x` write to `key.pem`) that no rule pattern for
/// the file would match.
fn creatable_name(name: OsString) -> Option<OsString> {
    if !cfg!(windows) {
        return Some(name);
    }
    let Some(text) = name.to_str() else {
        return Some(name);
    };
    if text.contains(':') {
        return None;
    }
    match without_trailing_dots_and_spaces(text) {
        "" => None,
        trimmed if trimmed.len() == text.len() => Some(name),
        trimmed => Some(trimmed.into()),
    }
}

fn without_trailing_dots_and_spaces(name: &str) -> &str {
    name.trim_end_matches(['.', ' '])
}

/// The ways to read a path a model wrote for this machine, the likeliest first: as written
/// without surrounding whitespace, quotes, or a `file://` scheme, then (on Windows) a Git Bash
/// (`/d/…`), WSL (`/mnt/d/…`), or URL-style (`/D:/…`) path as the drive path it names.
fn spellings(path: &str) -> Vec<String> {
    let mut written = path.trim();
    for quote in ['"', '\'', '`'] {
        if written.len() >= 2 && written.starts_with(quote) && written.ends_with(quote) {
            written = written[1..written.len() - 1].trim();
            break;
        }
    }
    if let Some(rest) = written.strip_prefix("file://") {
        written = rest;
    }
    let mut found = vec![written.to_string()];
    if cfg!(windows)
        && let Some(drive) = drive_path(written)
    {
        found.push(drive);
    }
    found
}

/// A POSIX-style spelling of a Windows drive path (`/d/x`, `/mnt/d/x`, `/D:/x`) as `D:/x`.
fn drive_path(written: &str) -> Option<String> {
    let rest = written.strip_prefix('/')?;
    let rest = rest.strip_prefix("mnt/").unwrap_or(rest);
    let mut chars = rest.chars();
    let letter = chars.next().filter(char::is_ascii_alphabetic)?;
    let tail = chars.as_str();
    let tail = tail.strip_prefix(':').unwrap_or(tail);
    if !(tail.is_empty() || tail.starts_with('/') || tail.starts_with('\\')) {
        return None;
    }
    Some(format!(
        "{}:/{}",
        letter.to_ascii_uppercase(),
        tail.get(1..).unwrap_or("")
    ))
}

/// A path as people write it: without Windows' `\\?\` verbatim prefix on drive paths.
pub fn plain(path: &Path) -> String {
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
    fn trailing_dots_and_spaces_are_dropped_from_names_win32_would_drop_them_from() {
        assert_eq!(without_trailing_dots_and_spaces("key.pem."), "key.pem");
        assert_eq!(without_trailing_dots_and_spaces("key.pem . "), "key.pem");
        assert_eq!(without_trailing_dots_and_spaces(".env"), ".env");
        assert_eq!(without_trailing_dots_and_spaces("..."), "");
        assert_eq!(
            creatable_name("key.pem.".into()),
            if cfg!(windows) {
                Some("key.pem".into())
            } else {
                Some("key.pem.".into())
            }
        );
        assert_eq!(creatable_name(" ".into()).is_none(), cfg!(windows));
        for stream in ["key.pem::$DATA", "key.pem:foo", ":x"] {
            assert_eq!(creatable_name(stream.into()).is_none(), cfg!(windows));
        }
    }

    #[cfg(windows)]
    #[test]
    fn a_new_name_with_a_trailing_dot_or_space_matches_the_rule_for_the_real_name() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = Workspace::new(dir.path()).unwrap();
        let plain = workspace.rule_target("key.pem");
        for evasion in ["key.pem.", "key.pem ", "key.pem. .", "sub./key.pem"] {
            let target = workspace.rule_target(evasion);
            if evasion.starts_with("sub") {
                let Target::Path { relative, .. } = target else {
                    panic!("a path target");
                };
                assert_eq!(relative.as_deref(), Some("sub/key.pem"));
                continue;
            }
            assert_eq!(target, plain, "{evasion} is the same file as key.pem");
            assert_eq!(
                workspace.resolve_writable(evasion).unwrap(),
                workspace.resolve_writable("key.pem").unwrap()
            );
        }
        assert!(workspace.resolve_writable("...").is_err());
        for stream in ["key.pem::$DATA", "key.pem:foo", "sub/key.pem:foo"] {
            assert!(
                workspace.resolve_writable(stream).is_err(),
                "{stream} is a stream of key.pem"
            );
        }
    }

    #[test]
    fn a_folder_with_spaces_resolves_however_the_model_spells_it() {
        let base = tempfile::tempdir().unwrap();
        let project = base.path().join("Git Repositories").join("Imperium AI");
        std::fs::create_dir_all(project.join("docs")).unwrap();
        let workspace = Workspace::new(&project).unwrap();
        let root = workspace.root().to_path_buf();
        let written = project.display().to_string();
        let forward = written.replace('\\', "/");
        let mut spellings = vec![
            ".".to_string(),
            written.clone(),
            forward.clone(),
            workspace.root_display(),
            format!("\"{written}\""),
            format!("'{forward}'"),
            format!("  {written}\n"),
            format!("file://{forward}"),
        ];
        if cfg!(windows) {
            let drive = written[..1].to_lowercase();
            let rest = &forward[2..];
            spellings.push(format!("/{drive}{rest}"));
            spellings.push(format!("/mnt/{drive}{rest}"));
            spellings.push(format!("/{}:{rest}", &written[..1]));
            spellings.push(format!("file:///{forward}"));
        }
        for spelling in &spellings {
            assert_eq!(workspace.resolve(spelling).unwrap(), root, "{spelling:?}");
        }
        assert_eq!(
            workspace.resolve(&format!("\"{forward}/docs\"")).unwrap(),
            root.join("docs")
        );
        // A looser spelling of a path outside is still refused.
        let elsewhere = tempfile::tempdir().unwrap();
        let outside = elsewhere.path().display().to_string();
        assert!(matches!(
            workspace.resolve(&format!("\"{outside}\"")),
            Err(ToolError::OutsideWorkspace(_))
        ));
    }

    #[test]
    fn posix_spellings_of_drive_paths() {
        assert_eq!(
            drive_path("/d/Git Repositories/x").as_deref(),
            Some("D:/Git Repositories/x")
        );
        assert_eq!(drive_path("/mnt/c/Users").as_deref(), Some("C:/Users"));
        assert_eq!(drive_path("/D:/x").as_deref(), Some("D:/x"));
        assert_eq!(drive_path("/e").as_deref(), Some("E:/"));
        assert_eq!(drive_path("/docs/x"), None);
        assert_eq!(drive_path("src/x"), None);
    }

    #[test]
    fn remote_grants_are_paths_inside_the_runtime() {
        use crate::permissions::{PathGrant, PathMode};
        let workspace = Workspace::remote(
            Remote::Ssh { host: "box".into() },
            &["/home/me/project".to_string()],
            Arc::new(crate::ProcessRunner),
        )
        .unwrap()
        .with_grants(&[
            PathGrant {
                path: "/home/me/.cargo".into(),
                mode: PathMode::ReadWrite,
            },
            PathGrant {
                path: "/opt/toolchains".into(),
                mode: PathMode::Read,
            },
            PathGrant {
                path: "~/.npm".into(),
                mode: PathMode::Read,
            },
        ]);
        assert_eq!(workspace.grants().len(), 2, "~ cannot be expanded remotely");
        assert!(
            workspace
                .resolve_remote("/home/me/.cargo/registry", true)
                .is_ok()
        );
        assert!(
            workspace
                .resolve_remote("/opt/toolchains/bin/go", false)
                .is_ok()
        );
        assert!(matches!(
            workspace.resolve_remote("/opt/toolchains/bin/go", true),
            Err(ToolError::ReadOnly(_))
        ));
        assert!(matches!(
            workspace.resolve_remote("/etc/passwd", false),
            Err(ToolError::OutsideWorkspace(_))
        ));
        assert_eq!(
            workspace.rule_target("src/main.rs"),
            Target::Path {
                absolute: "/home/me/project/src/main.rs".into(),
                relative: Some("src/main.rs".into()),
            }
        );
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
