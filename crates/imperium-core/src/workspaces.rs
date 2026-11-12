//! Environments' working folders (docs/14-projects-and-automation.md#environment-working-folders-implemented).
//!
//! One folder (`workspaces.root`, `<data>/workspaces` unless set) holds a folder per
//! environment: `<root>/<environment-id>/Memory.md` for the environment's own memory and
//! `<root>/<environment-id>/sessions/<session-id>/` as each session's scratch folder. Scratch
//! folders are made when the session starts (or is unarchived, empty) and deleted when it is
//! archived or deleted. Changing the root moves the whole tree ([`move_root`]).

use std::fs;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

use imperium_memory::environment::FILE_NAME;

/// A recorded copy of where the tree was last put, so a root changed by editing the config
/// file while the daemon was off is still moved at the next start. Kept in the data folder.
pub const ROOT_RECORD: &str = "workspaces-root.txt";

/// Whether `id` is safe to use as a folder name: letters, digits, `-` and `_` only.
pub fn safe_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// The environment's folder, `<root>/<environment-id>`.
pub fn environment_dir(root: &Path, environment: &str) -> Option<PathBuf> {
    safe_id(environment).then(|| root.join(environment))
}

/// The environment's `Memory.md`.
pub fn memory_file(root: &Path, environment: &str) -> Option<PathBuf> {
    environment_dir(root, environment).map(|dir| dir.join(FILE_NAME))
}

/// The folder holding the environment's session folders.
pub fn sessions_dir(root: &Path, environment: &str) -> Option<PathBuf> {
    environment_dir(root, environment).map(|dir| dir.join("sessions"))
}

/// One session's scratch folder.
pub fn session_dir(root: &Path, environment: &str, session: &str) -> Option<PathBuf> {
    if !safe_id(session) {
        return None;
    }
    sessions_dir(root, environment).map(|dir| dir.join(session))
}

/// Makes the session's scratch folder (and the folders above it) and returns it.
pub fn ensure_session_dir(root: &Path, environment: &str, session: &str) -> io::Result<PathBuf> {
    let dir = session_dir(root, environment, session).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "environment and session ids must be letters, digits, '-' or '_'",
        )
    })?;
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Deletes the session's scratch folder; a missing one is fine. Returns whether one was
/// deleted.
pub fn remove_session_dir(root: &Path, environment: &str, session: &str) -> io::Result<bool> {
    let Some(dir) = session_dir(root, environment, session) else {
        return Ok(false);
    };
    match fs::remove_dir_all(&dir) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

/// Where the tree was last put, as recorded in the data folder.
pub fn recorded_root(data_dir: &Path) -> Option<PathBuf> {
    let text = fs::read_to_string(data_dir.join(ROOT_RECORD)).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| PathBuf::from(text))
}

/// Records where the tree is now.
pub fn record_root(data_dir: &Path, root: &Path) -> io::Result<()> {
    fs::create_dir_all(data_dir)?;
    fs::write(data_dir.join(ROOT_RECORD), root.display().to_string())
}

/// What a move did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveMethod {
    /// There was nothing to move.
    Nothing,
    /// The folder was renamed (same volume).
    Rename,
    /// The files were copied, checked against the originals, and the originals deleted.
    Copy,
}

/// The outcome of a successful [`move_root`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveReport {
    pub method: MoveMethod,
    pub files: u64,
    pub bytes: u64,
    /// Things that went wrong after the data was safely in place, such as an old folder that
    /// could not be deleted completely.
    pub warnings: Vec<String>,
}

/// A step of a move, for progress reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveProgress {
    /// `scanning`, `copying`, `verifying` or `cleaning`.
    pub phase: &'static str,
    pub files_done: u64,
    pub files_total: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum MoveError {
    #[error("the new folder must be an absolute path: {0}")]
    NotAbsolute(String),
    #[error("{0} is inside the current working folders; choose a folder outside {1}")]
    InsideOld(String, String),
    #[error("{0} is not empty; choose an empty or new folder, or empty it first")]
    TargetNotEmpty(String),
    #[error("{0} is a file, not a folder")]
    TargetIsFile(String),
    #[error("could not move the working folders: {0}. Nothing was changed.")]
    Failed(String),
}

fn failed(path: &Path, error: impl std::fmt::Display) -> MoveError {
    MoveError::Failed(format!("{}: {error}", path.display()))
}

/// Test and fault-injection knobs; production code uses the default.
#[derive(Debug, Clone, Default)]
pub(crate) struct MoveHooks {
    /// Skip renaming, as when the new folder is on another volume.
    pub force_copy: bool,
    /// Fail copying after this many files.
    pub fail_after_files: Option<u64>,
    /// Make the verification report a difference.
    pub corrupt_verify: bool,
}

/// Moves the working folders from `old` to `new`.
///
/// Refuses a relative `new`, a `new` inside `old`, and a `new` that exists and is not empty.
/// A folder on the same volume is renamed; otherwise the tree is copied, every file compared
/// with its original, and only then are the originals deleted. If anything fails before that,
/// what was written at `new` is removed and `old` is exactly as it was. Once the copy is
/// verified the data is safe at `new`; originals that cannot be deleted are reported as
/// warnings, not as failure.
pub fn move_root(
    old: &Path,
    new: &Path,
    progress: &mut dyn FnMut(MoveProgress),
) -> Result<MoveReport, MoveError> {
    move_root_with(old, new, progress, &MoveHooks::default())
}

pub(crate) fn move_root_with(
    old: &Path,
    new: &Path,
    progress: &mut dyn FnMut(MoveProgress),
    hooks: &MoveHooks,
) -> Result<MoveReport, MoveError> {
    if !new.is_absolute() {
        return Err(MoveError::NotAbsolute(new.display().to_string()));
    }
    let resolved_old = resolve(old);
    let resolved_new = resolve(new);
    if resolved_old == resolved_new {
        return Ok(MoveReport {
            method: MoveMethod::Nothing,
            files: 0,
            bytes: 0,
            warnings: Vec::new(),
        });
    }
    if resolved_new.starts_with(&resolved_old) {
        return Err(MoveError::InsideOld(
            new.display().to_string(),
            old.display().to_string(),
        ));
    }
    // `new` is outside `old`; it must not be an ancestor holding `old` either (a non-empty
    // folder, refused below, but with a clearer message).
    let new_existed = match fs::metadata(new) {
        Ok(meta) if meta.is_dir() => {
            let mut entries = fs::read_dir(new).map_err(|error| failed(new, error))?;
            if entries.next().is_some() {
                return Err(MoveError::TargetNotEmpty(new.display().to_string()));
            }
            true
        }
        Ok(_) => return Err(MoveError::TargetIsFile(new.display().to_string())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(error) => return Err(failed(new, error)),
    };
    if !old.is_dir() {
        // Nothing has been kept yet: the new folder is just made.
        fs::create_dir_all(new).map_err(|error| failed(new, error))?;
        return Ok(MoveReport {
            method: MoveMethod::Nothing,
            files: 0,
            bytes: 0,
            warnings: Vec::new(),
        });
    }
    progress(MoveProgress {
        phase: "scanning",
        files_done: 0,
        files_total: 0,
    });
    let (files, bytes) = count(old).map_err(|error| failed(old, error))?;

    if !hooks.force_copy && try_rename(old, new, new_existed) {
        return Ok(MoveReport {
            method: MoveMethod::Rename,
            files,
            bytes,
            warnings: Vec::new(),
        });
    }

    // Copy, verify, then delete the originals.
    if let Some(parent) = new.parent() {
        fs::create_dir_all(parent).map_err(|error| failed(parent, error))?;
    }
    let copied = copy_tree(old, new, files, progress, hooks)
        .and_then(|()| verify_tree(old, new, files, progress, hooks));
    if let Err(error) = copied {
        // Take back what was written; the originals were never touched.
        let _ = fs::remove_dir_all(new);
        if new_existed {
            let _ = fs::create_dir_all(new);
        }
        return Err(failed(new, error));
    }
    progress(MoveProgress {
        phase: "cleaning",
        files_done: files,
        files_total: files,
    });
    let mut warnings = Vec::new();
    if let Err(error) = fs::remove_dir_all(old) {
        warnings.push(format!(
            "the files are at {} but the old folder {} could not be deleted completely: {error}",
            new.display(),
            old.display()
        ));
    }
    Ok(MoveReport {
        method: MoveMethod::Copy,
        files,
        bytes,
        warnings,
    })
}

/// Renames `old` to `new`, first removing an empty `new` (Windows will not rename onto an
/// existing folder). False, with both exactly as they were, when it could not.
fn try_rename(old: &Path, new: &Path, new_existed: bool) -> bool {
    if new_existed && fs::remove_dir(new).is_err() {
        return false;
    }
    if let Some(parent) = new.parent()
        && fs::create_dir_all(parent).is_err()
    {
        if new_existed {
            let _ = fs::create_dir_all(new);
        }
        return false;
    }
    if fs::rename(old, new).is_ok() {
        return true;
    }
    if new_existed {
        let _ = fs::create_dir_all(new);
    }
    false
}

/// `path` made absolute with the part that exists resolved through links, and `.`/`..`
/// removed from the rest, for comparing folders that may not exist yet.
fn resolve(path: &Path) -> PathBuf {
    let mut existing = path;
    let mut rest = Vec::new();
    while !existing.exists() {
        let (Some(name), Some(parent)) = (existing.file_name(), existing.parent()) else {
            break;
        };
        rest.push(name.to_owned());
        existing = parent;
    }
    let mut resolved = existing
        .canonicalize()
        .unwrap_or_else(|_| existing.to_path_buf());
    for name in rest.into_iter().rev() {
        resolved.push(name);
    }
    // Remove any `..` left in the part that does not exist.
    let mut clean = PathBuf::new();
    for component in resolved.components() {
        match component {
            Component::ParentDir => {
                clean.pop();
            }
            Component::CurDir => {}
            other => clean.push(other.as_os_str()),
        }
    }
    clean
}

/// Files and bytes below `dir`.
fn count(dir: &Path) -> io::Result<(u64, u64)> {
    let mut files = 0;
    let mut bytes = 0;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            let (inner_files, inner_bytes) = count(&entry.path())?;
            files += inner_files;
            bytes += inner_bytes;
        } else {
            files += 1;
            bytes += entry.metadata().map_or(0, |meta| meta.len());
        }
    }
    Ok((files, bytes))
}

/// Reports progress about every 64 files.
struct Ticker {
    done: u64,
}

impl Ticker {
    fn file(&mut self, phase: &'static str, total: u64, progress: &mut dyn FnMut(MoveProgress)) {
        self.done += 1;
        if self.done % 64 == 0 || self.done == total {
            progress(MoveProgress {
                phase,
                files_done: self.done,
                files_total: total,
            });
        }
    }
}

fn copy_tree(
    old: &Path,
    new: &Path,
    total: u64,
    progress: &mut dyn FnMut(MoveProgress),
    hooks: &MoveHooks,
) -> io::Result<()> {
    fs::create_dir_all(new)?;
    let mut ticker = Ticker { done: 0 };
    copy_dir(old, new, total, progress, hooks, &mut ticker)
}

fn copy_dir(
    from: &Path,
    to: &Path,
    total: u64,
    progress: &mut dyn FnMut(MoveProgress),
    hooks: &MoveHooks,
    ticker: &mut Ticker,
) -> io::Result<()> {
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let source = entry.path();
        let target = to.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_dir() {
            fs::create_dir_all(&target)?;
            copy_dir(&source, &target, total, progress, hooks, ticker)?;
        } else if kind.is_symlink() {
            copy_link(&source, &target)?;
            ticker.file("copying", total, progress);
        } else {
            if hooks
                .fail_after_files
                .is_some_and(|limit| ticker.done >= limit)
            {
                return Err(io::Error::other("injected failure while copying"));
            }
            fs::copy(&source, &target)?;
            ticker.file("copying", total, progress);
        }
    }
    Ok(())
}

#[cfg(unix)]
fn copy_link(source: &Path, target: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(fs::read_link(source)?, target)
}

#[cfg(windows)]
fn copy_link(source: &Path, target: &Path) -> io::Result<()> {
    let destination = fs::read_link(source)?;
    if fs::metadata(source).is_ok_and(|meta| meta.is_dir()) {
        std::os::windows::fs::symlink_dir(destination, target)
    } else {
        std::os::windows::fs::symlink_file(destination, target)
    }
}

/// Compares the copy with the original: the same names, kinds, sizes and bytes.
fn verify_tree(
    old: &Path,
    new: &Path,
    total: u64,
    progress: &mut dyn FnMut(MoveProgress),
    hooks: &MoveHooks,
) -> io::Result<()> {
    let mut ticker = Ticker { done: 0 };
    verify_dir(old, new, total, progress, hooks, &mut ticker)
}

fn verify_dir(
    from: &Path,
    to: &Path,
    total: u64,
    progress: &mut dyn FnMut(MoveProgress),
    hooks: &MoveHooks,
    ticker: &mut Ticker,
) -> io::Result<()> {
    let mismatch = |path: &Path| {
        io::Error::other(format!(
            "the copy of {} does not match the original",
            path.display()
        ))
    };
    let mut names = 0usize;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        names += 1;
        let source = entry.path();
        let target = to.join(entry.file_name());
        let kind = entry.file_type()?;
        let copy = fs::symlink_metadata(&target).map_err(|_| mismatch(&target))?;
        if kind.is_dir() {
            if !copy.is_dir() {
                return Err(mismatch(&target));
            }
            verify_dir(&source, &target, total, progress, hooks, ticker)?;
        } else if kind.is_symlink() {
            if !copy.file_type().is_symlink() || fs::read_link(&source)? != fs::read_link(&target)?
            {
                return Err(mismatch(&target));
            }
            ticker.file("verifying", total, progress);
        } else {
            if !copy.is_file()
                || copy.len() != entry.metadata()?.len()
                || hooks.corrupt_verify
                || !same_bytes(&source, &target)?
            {
                return Err(mismatch(&target));
            }
            ticker.file("verifying", total, progress);
        }
    }
    if fs::read_dir(to)?.count() != names {
        return Err(mismatch(to));
    }
    Ok(())
}

fn same_bytes(a: &Path, b: &Path) -> io::Result<bool> {
    let mut left = io::BufReader::new(fs::File::open(a)?);
    let mut right = io::BufReader::new(fs::File::open(b)?);
    let mut first = vec![0u8; 64 * 1024];
    let mut second = vec![0u8; 64 * 1024];
    loop {
        let read = left.read(&mut first)?;
        if read == 0 {
            return Ok(right.read(&mut second)? == 0);
        }
        right.read_exact(&mut second[..read]).or_else(|error| {
            if error.kind() == io::ErrorKind::UnexpectedEof {
                Ok(())
            } else {
                Err(error)
            }
        })?;
        if first[..read] != second[..read] {
            return Ok(false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed(root: &Path) {
        let session = session_dir(root, "env1", "s1").unwrap();
        fs::create_dir_all(&session).unwrap();
        fs::write(session.join("notes.txt"), "scratch").unwrap();
        fs::create_dir_all(session.join("deep/er")).unwrap();
        fs::write(session.join("deep/er/data.bin"), vec![7u8; 200_000]).unwrap();
        fs::write(memory_file(root, "env1").unwrap(), "# Memory\n").unwrap();
    }

    fn quiet() -> impl FnMut(MoveProgress) {
        |_| {}
    }

    fn assert_seeded(root: &Path) {
        let session = session_dir(root, "env1", "s1").unwrap();
        assert_eq!(
            fs::read_to_string(session.join("notes.txt")).unwrap(),
            "scratch"
        );
        assert_eq!(
            fs::read(session.join("deep/er/data.bin")).unwrap().len(),
            200_000
        );
        assert_eq!(
            fs::read_to_string(memory_file(root, "env1").unwrap()).unwrap(),
            "# Memory\n"
        );
    }

    #[test]
    fn the_layout_is_the_root_the_environment_and_its_sessions() {
        let root = Path::new("/w");
        assert_eq!(
            memory_file(root, "abc").unwrap(),
            Path::new("/w/abc/Memory.md")
        );
        assert_eq!(
            session_dir(root, "abc", "s9").unwrap(),
            Path::new("/w/abc/sessions/s9")
        );
        assert!(session_dir(root, "../x", "s9").is_none());
        assert!(session_dir(root, "abc", "a/b").is_none());
        assert!(memory_file(root, "").is_none());
    }

    #[test]
    fn a_session_folder_is_made_on_demand_and_removed_alone() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("w");
        assert!(!root.exists(), "nothing exists until a session needs it");
        let first = ensure_session_dir(&root, "e", "s1").unwrap();
        let second = ensure_session_dir(&root, "e", "s2").unwrap();
        assert!(first.is_dir() && second.is_dir());
        fs::write(first.join("x"), "1").unwrap();
        assert!(remove_session_dir(&root, "e", "s1").unwrap());
        assert!(!first.exists());
        assert!(second.is_dir(), "other sessions keep theirs");
        assert!(!remove_session_dir(&root, "e", "s1").unwrap());
    }

    #[test]
    fn moving_on_the_same_volume_renames_the_tree() {
        let temp = tempfile::tempdir().unwrap();
        let (old, new) = (temp.path().join("old"), temp.path().join("new"));
        seed(&old);
        let report = move_root(&old, &new, &mut quiet()).unwrap();
        assert_eq!(report.method, MoveMethod::Rename);
        assert_eq!(report.files, 3);
        assert!(!old.exists());
        assert_seeded(&new);
    }

    #[test]
    fn moving_onto_an_existing_empty_folder_works() {
        let temp = tempfile::tempdir().unwrap();
        let (old, new) = (temp.path().join("old"), temp.path().join("new"));
        seed(&old);
        fs::create_dir_all(&new).unwrap();
        move_root(&old, &new, &mut quiet()).unwrap();
        assert!(!old.exists());
        assert_seeded(&new);
    }

    #[test]
    fn moving_across_volumes_copies_verifies_and_deletes_the_original() {
        let temp = tempfile::tempdir().unwrap();
        let (old, new) = (temp.path().join("old"), temp.path().join("deeper/new"));
        seed(&old);
        let mut phases = Vec::new();
        let report = move_root_with(
            &old,
            &new,
            &mut |step| phases.push(step.phase),
            &MoveHooks {
                force_copy: true,
                ..MoveHooks::default()
            },
        )
        .unwrap();
        assert_eq!(report.method, MoveMethod::Copy);
        assert_eq!((report.files, report.bytes), (3, 200_000 + 7 + 9));
        assert!(report.warnings.is_empty());
        assert!(!old.exists(), "the original is deleted after verifying");
        assert_seeded(&new);
        assert!(phases.contains(&"copying") && phases.contains(&"verifying"));
    }

    #[test]
    fn a_target_that_is_not_empty_is_refused_and_nothing_changes() {
        let temp = tempfile::tempdir().unwrap();
        let (old, new) = (temp.path().join("old"), temp.path().join("new"));
        seed(&old);
        fs::create_dir_all(&new).unwrap();
        fs::write(new.join("mine.txt"), "keep").unwrap();
        let error = move_root(&old, &new, &mut quiet()).unwrap_err();
        assert!(matches!(error, MoveError::TargetNotEmpty(_)), "{error}");
        assert_seeded(&old);
        assert_eq!(fs::read_to_string(new.join("mine.txt")).unwrap(), "keep");
    }

    #[test]
    fn a_target_inside_the_old_root_or_a_relative_one_is_refused() {
        let temp = tempfile::tempdir().unwrap();
        let old = temp.path().join("old");
        seed(&old);
        let inside = old.join("env1/nested");
        assert!(matches!(
            move_root(&old, &inside, &mut quiet()),
            Err(MoveError::InsideOld(..))
        ));
        assert!(matches!(
            move_root(&old, Path::new("relative/path"), &mut quiet()),
            Err(MoveError::NotAbsolute(_))
        ));
        let file = temp.path().join("a-file");
        fs::write(&file, "x").unwrap();
        assert!(matches!(
            move_root(&old, &file, &mut quiet()),
            Err(MoveError::TargetIsFile(_))
        ));
        assert_seeded(&old);
    }

    #[test]
    fn a_failed_copy_rolls_back_and_loses_nothing() {
        let temp = tempfile::tempdir().unwrap();
        let (old, new) = (temp.path().join("old"), temp.path().join("new"));
        seed(&old);
        let error = move_root_with(
            &old,
            &new,
            &mut quiet(),
            &MoveHooks {
                force_copy: true,
                fail_after_files: Some(1),
                ..MoveHooks::default()
            },
        )
        .unwrap_err();
        assert!(matches!(error, MoveError::Failed(_)), "{error}");
        assert_seeded(&old);
        assert!(!new.exists(), "what was copied is taken back");
    }

    #[test]
    fn a_failed_verification_keeps_the_original_and_restores_an_empty_target() {
        let temp = tempfile::tempdir().unwrap();
        let (old, new) = (temp.path().join("old"), temp.path().join("new"));
        seed(&old);
        fs::create_dir_all(&new).unwrap();
        let error = move_root_with(
            &old,
            &new,
            &mut quiet(),
            &MoveHooks {
                force_copy: true,
                corrupt_verify: true,
                ..MoveHooks::default()
            },
        )
        .unwrap_err();
        assert!(matches!(error, MoveError::Failed(_)), "{error}");
        assert_seeded(&old);
        assert!(new.is_dir() && fs::read_dir(&new).unwrap().next().is_none());
    }

    #[test]
    fn moving_nothing_just_makes_the_new_folder() {
        let temp = tempfile::tempdir().unwrap();
        let new = temp.path().join("new");
        let report = move_root(&temp.path().join("missing"), &new, &mut quiet()).unwrap();
        assert_eq!(report.method, MoveMethod::Nothing);
        assert!(new.is_dir());
    }

    #[test]
    fn the_recorded_root_round_trips() {
        let temp = tempfile::tempdir().unwrap();
        assert_eq!(recorded_root(temp.path()), None);
        record_root(temp.path(), Path::new("/x/y")).unwrap();
        assert_eq!(recorded_root(temp.path()), Some(PathBuf::from("/x/y")));
    }
}
