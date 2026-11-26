//! The user-built `ncm-server`: approving it, and checking it at every launch.
//!
//! There are no `ncm-server` releases yet, so the only server Imperium runs is one the user built
//! and approved at a terminal (`imperium runtime ncm use <path>`). Approving shows the file's
//! path, size and SHA-256 without running it; once the user confirms, the file is copied into
//! `<data>/runtimes/ncm-server/user/<sha256>/`, the copy is asked for its `--version` (from that
//! folder, with the server's environment allowlist), and the hash is recorded in
//! `<data>/runtimes/ncm-server/user.json`. Every launch runs that copy, never the original: the
//! copy folder must hold the executable and nothing else, the copy is re-hashed through a handle
//! held open for the server's life (on Windows without write or delete sharing, so it cannot be
//! replaced meanwhile), and the server starts with that folder as its working directory. Nothing
//! the user left beside the original binary (a planted DLL, which Windows would load from the
//! executable's folder first) is ever loaded.
//!
//! The trust anchor is meant to be the data folder, but it is not one yet. The protection of
//! `<data>/runtimes/` from agent file tools and commands, and the floor that makes
//! `imperium runtime ncm use` need a person in every permission mode, arrive with #304 and #330,
//! which are not merged. Until they are, only the `local_files_only` agent profile refuses the
//! data folder, agent shell commands are not confined, and the terminal check can be faked
//! (`script -qc ... /dev/null`, `expect`). So, as a recorded residual, an agent could write
//! `user.json` and a copy that matches it, or run `runtime ncm use` under a fake terminal, and
//! Imperium would run that binary. The hash checks here defend against accidents and stale
//! copies, not against such an agent, until #304 and #330 land. NVRTC, which a CUDA build loads
//! at its first kernel compile, comes from the user's CUDA toolkit through `CUDA_PATH` and is not
//! hashed (a further residual).
//!
//! Nothing here searches `PATH` or downloads anything.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The approval record's file name, in [`runtime_dir`].
pub const RECORD: &str = "user.json";

/// What to do when no `ncm-server` is approved.
pub const HOW_TO_INSTALL: &str = "ncm-server is not installed: there are no release builds yet, \
     so build it yourself in Imperium-Models models/ncm (`cargo build --release -p ncm-server \
     --features cuda` for an NVIDIA GPU, or without `--features cuda` for a CPU-only build) and \
     approve it at a terminal with `imperium runtime ncm use <path to ncm-server>`, which also \
     records whether the build has CUDA and sets runtime.ncm.custom_path";

/// Longest `ncm-server --version` may take.
const VERSION_TIMEOUT: Duration = Duration::from_secs(10);

/// `<data>/runtimes/ncm-server`.
pub fn runtime_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("runtimes").join("ncm-server")
}

/// The executable's file name on this platform.
pub fn exe_name() -> &'static str {
    if cfg!(windows) {
        "ncm-server.exe"
    } else {
        "ncm-server"
    }
}

/// The folder an approved build with `sha256` is copied to.
pub fn copy_dir(data_dir: &Path, sha256: &str) -> PathBuf {
    runtime_dir(data_dir).join("user").join(sha256)
}

/// What the user approved, as `user.json` records it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Approval {
    /// The binary the user named.
    pub source: PathBuf,
    /// SHA-256 of the binary, lower-case hexadecimal; the copy is checked against it.
    pub sha256: String,
    pub size: u64,
    /// Seconds since the Unix epoch.
    pub approved_at: u64,
    /// What the approved copy printed for `--version`.
    #[serde(default)]
    pub version: Option<String>,
    /// Whether the build has CUDA (`--features cuda`): its `--version` says so, or the user
    /// said so when approving. Without it, `device = "auto"` runs models on the CPU.
    #[serde(default)]
    pub cuda: bool,
}

/// A binary the user named, as shown to them before they approve it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub path: PathBuf,
    pub size: u64,
    pub sha256: String,
}

/// What the approved copy says about itself.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Probe {
    pub version: Option<String>,
    /// `Some` when the version output says whether it was built with CUDA.
    pub cuda: Option<bool>,
}

/// The temporary folders of this machine, where anyone (agents included) may write: refused as a
/// place to approve a binary from.
pub fn temp_folders() -> Vec<PathBuf> {
    let mut folders = vec![std::env::temp_dir()];
    for name in ["TEMP", "TMP", "TMPDIR"] {
        if let Some(dir) = std::env::var_os(name).filter(|dir| !dir.is_empty()) {
            folders.push(PathBuf::from(dir));
        }
    }
    if cfg!(unix) {
        folders.extend(["/tmp", "/var/tmp", "/dev/shm"].map(PathBuf::from));
    }
    if cfg!(windows)
        && let Some(local) = std::env::var_os("LOCALAPPDATA").filter(|dir| !dir.is_empty())
    {
        folders.push(PathBuf::from(local).join("Temp"));
    }
    folders.sort();
    folders.dedup();
    folders
}

/// Checks `path` and hashes it for the user to approve, without running it. It must be absolute,
/// a regular file and not a symbolic link, and not inside any of `forbidden` (folders agents
/// write to: session workspaces, worktrees, temporary folders).
pub fn inspect(path: &Path, forbidden: &[PathBuf]) -> Result<Candidate, String> {
    if !path.is_absolute() {
        return Err(format!(
            "{} is not an absolute path; name the ncm-server you built by its full path",
            path.display()
        ));
    }
    let meta = fs::symlink_metadata(path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    if meta.file_type().is_symlink() {
        return Err(format!(
            "{} is a symbolic link; name the file it points to",
            path.display()
        ));
    }
    if !meta.is_file() {
        return Err(format!("{} is not a regular file", path.display()));
    }
    let canonical = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    for root in forbidden {
        let root = fs::canonicalize(root).unwrap_or_else(|_| root.clone());
        if canonical.starts_with(&root) || path.starts_with(&root) {
            return Err(format!(
                "{} is inside {}, a folder agents can write to; build ncm-server somewhere \
                 else (or copy it out) and approve that file",
                path.display(),
                root.display()
            ));
        }
    }
    let mut file =
        File::open(path).map_err(|error| format!("cannot open {}: {error}", path.display()))?;
    let (size, sha256) = hash(&mut file).map_err(|error| format!("{}: {error}", path.display()))?;
    Ok(Candidate {
        path: path.to_path_buf(),
        size,
        sha256,
    })
}

/// Once the user approved `candidate`: copies it into [`copy_dir`] (unless a copy with its hash
/// is already there, as when the user approves the build a running server uses again), asks the
/// copy what it is with `probe` (never the original), and records it in [`RECORD`]. `ask_cuda`
/// decides whether the build has CUDA when its version output does not say; when it fails (the
/// user's answer could not be read), nothing is recorded. The copy must hash to what the user was
/// shown: a file changed since [`inspect`] is refused.
pub fn approve(
    data_dir: &Path,
    candidate: &Candidate,
    approved_at: u64,
    probe: impl FnOnce(&Path, &Path) -> Probe,
    ask_cuda: impl FnOnce(Option<&str>) -> Result<bool, String>,
) -> Result<Approval, String> {
    let dir = install_copy(data_dir, candidate)?;
    let program = dir.join(exe_name());
    let found = probe(&program, &dir);
    let cuda = match found.cuda {
        Some(cuda) => cuda,
        None => ask_cuda(found.version.as_deref())?,
    };
    let approval = Approval {
        source: candidate.path.clone(),
        sha256: candidate.sha256.clone(),
        size: candidate.size,
        approved_at,
        version: found.version,
        cuda,
    };
    let record = runtime_dir(data_dir).join(RECORD);
    let text = serde_json::to_string_pretty(&approval).map_err(|error| error.to_string())?;
    let partial = record.with_extension("json.partial");
    fs::write(&partial, text).map_err(|error| format!("{}: {error}", partial.display()))?;
    fs::rename(&partial, &record).map_err(|error| format!("{}: {error}", record.display()))?;
    Ok(approval)
}

/// Puts a copy of `candidate` in its [`copy_dir`], which then holds nothing else, and returns
/// the folder.
fn install_copy(data_dir: &Path, candidate: &Candidate) -> Result<PathBuf, String> {
    let dir = copy_dir(data_dir, &candidate.sha256);
    fs::create_dir_all(&dir).map_err(|error| format!("{}: {error}", dir.display()))?;
    let target = dir.join(exe_name());
    // Anything else in the folder (a leftover partial copy, a dropped file) goes.
    for entry in fs::read_dir(&dir).map_err(|error| format!("{}: {error}", dir.display()))? {
        let entry = entry.map_err(|error| error.to_string())?;
        if entry.file_name() != exe_name() {
            let path = entry.path();
            let removed = if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                fs::remove_dir_all(&path)
            } else {
                fs::remove_file(&path)
            };
            removed.map_err(|error| format!("{}: {error}", path.display()))?;
        }
    }
    let intact = File::open(&target)
        .and_then(|mut file| hash(&mut file))
        .is_ok_and(|(size, sha256)| size == candidate.size && sha256 == candidate.sha256);
    if intact {
        return Ok(dir);
    }
    let staging = dir.join(format!("{}.partial", exe_name()));
    let copied = (|| -> Result<(u64, String), String> {
        let mut source = File::open(&candidate.path)
            .map_err(|error| format!("cannot open {}: {error}", candidate.path.display()))?;
        let mut out =
            File::create(&staging).map_err(|error| format!("{}: {error}", staging.display()))?;
        std::io::copy(&mut source, &mut out).map_err(|error| error.to_string())?;
        out.flush().map_err(|error| error.to_string())?;
        drop(out);
        let mut written = File::open(&staging).map_err(|error| error.to_string())?;
        hash(&mut written).map_err(|error| error.to_string())
    })();
    let (size, sha256) = match copied {
        Ok(found) => found,
        Err(error) => {
            let _ = fs::remove_file(&staging);
            return Err(error);
        }
    };
    if sha256 != candidate.sha256 || size != candidate.size {
        let _ = fs::remove_file(&staging);
        return Err(format!(
            "{} changed after it was shown to you (SHA-256 now {sha256}); approve it again",
            candidate.path.display()
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&staging, fs::Permissions::from_mode(0o755))
            .map_err(|error| error.to_string())?;
    }
    fs::rename(&staging, &target).map_err(|error| format!("{}: {error}", target.display()))?;
    Ok(dir)
}

/// Runs the approved copy `program` with `--version`, from its folder `dir` and with the
/// server's environment allowlist, for at most [`VERSION_TIMEOUT`].
pub fn probe(program: &Path, dir: &Path) -> Probe {
    let mut command = Command::new(program);
    command
        .arg("--version")
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .env_clear()
        .envs(crate::backend::allowed_env(
            crate::backend::NCM_ENV,
            std::env::vars_os().filter_map(|(name, value)| Some((name.into_string().ok()?, value))),
            cfg!(windows),
        ));
    let Ok(mut child) = command.spawn() else {
        return Probe::default();
    };
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut out = String::new();
                if let Some(stdout) = child.stdout.take() {
                    let _ = stdout.take(4096).read_to_string(&mut out);
                }
                if !status.success() {
                    return Probe::default();
                }
                let version = out.trim().to_string();
                return Probe {
                    cuda: cuda_in_version(&version),
                    version: Some(version),
                };
            }
            Ok(None) if started.elapsed() < VERSION_TIMEOUT => {
                std::thread::sleep(Duration::from_millis(50));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Probe::default();
            }
        }
    }
}

/// Whether a `--version` line names the build's backends: `cuda` among its words says CUDA, a
/// `cpu`-only list says none; `ncm-server 0.0.0` (today's output) says nothing.
pub fn cuda_in_version(version: &str) -> Option<bool> {
    let lower = version.to_ascii_lowercase();
    let words: Vec<&str> = lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect();
    if words.contains(&"cuda") {
        Some(true)
    } else if words.contains(&"cpu") {
        Some(false)
    } else {
        None
    }
}

/// The approval on record, if any.
pub fn approved(data_dir: &Path) -> Result<Option<Approval>, String> {
    let record = runtime_dir(data_dir).join(RECORD);
    let text = match fs::read_to_string(&record) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("{}: {error}", record.display())),
    };
    let approval: Approval = serde_json::from_str(&text).map_err(|error| {
        format!(
            "{} is damaged ({error}); approve ncm-server again",
            record.display()
        )
    })?;
    if !is_sha256(&approval.sha256) {
        return Err(format!(
            "{} names no valid SHA-256; approve ncm-server again",
            record.display()
        ));
    }
    Ok(Some(approval))
}

/// Whether the approved build has CUDA; false with no (readable) approval.
pub fn approved_cuda(data_dir: &Path) -> bool {
    approved(data_dir)
        .ok()
        .flatten()
        .is_some_and(|approval| approval.cuda)
}

/// The approved copy, checked for this launch. `held` stays open for the server's life.
#[derive(Debug)]
pub struct Verified {
    /// The copy to run.
    pub program: PathBuf,
    /// Its folder: the server's working directory.
    pub dir: PathBuf,
    pub approval: Approval,
    /// The handle the copy was hashed through; on Windows it denies writes and deletes while it
    /// is open.
    pub held: File,
}

/// Checks the approved copy before a launch: there is an approval, its folder holds the
/// executable and nothing else (a DLL dropped there would be loaded before any other), and the
/// copy hashes to the approval through a handle that is kept ([`Verified::held`]).
pub fn verify(data_dir: &Path) -> Result<Verified, String> {
    let approval = approved(data_dir)?.ok_or_else(|| HOW_TO_INSTALL.to_string())?;
    let dir = copy_dir(data_dir, &approval.sha256);
    let program = dir.join(exe_name());
    let again = || {
        format!(
            "approve it again with `imperium runtime ncm use {}`",
            approval.source.display()
        )
    };
    let entries = fs::read_dir(&dir).map_err(|_| {
        format!(
            "the approved ncm-server copy {} is missing; {}",
            program.display(),
            again()
        )
    })?;
    for entry in entries {
        let entry = entry.map_err(|error| format!("{}: {error}", dir.display()))?;
        if entry.file_name() != exe_name() {
            return Err(format!(
                "{} holds {}, which Imperium did not put there; ncm-server is not run from it \
                 until you {}",
                dir.display(),
                entry.file_name().to_string_lossy(),
                again()
            ));
        }
    }
    let meta = fs::symlink_metadata(&program).map_err(|_| {
        format!(
            "the approved ncm-server copy {} is missing; {}",
            program.display(),
            again()
        )
    })?;
    if !meta.is_file() {
        return Err(format!("{} is not a regular file", program.display()));
    }
    let mut held = open_held(&program)
        .map_err(|error| format!("cannot open {}: {error}", program.display()))?;
    let (_, sha256) = hash(&mut held).map_err(|error| format!("{}: {error}", program.display()))?;
    if sha256 != approval.sha256 {
        return Err(format!(
            "the approved ncm-server copy {} has changed since you approved it (SHA-256 {sha256}, \
             approved {}); it is not run. Approve a build again with `imperium runtime ncm use \
             <path>`",
            program.display(),
            approval.sha256
        ));
    }
    Ok(Verified {
        program,
        dir,
        approval,
        held,
    })
}

/// Opens `path` for reading; on Windows other handles may read it (and execute it) but neither
/// write nor delete it while this one is open.
fn open_held(path: &Path) -> std::io::Result<File> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ: u32 = 0x1;
        options.share_mode(FILE_SHARE_READ);
    }
    options.open(path)
}

/// Bytes and lower-case hexadecimal SHA-256 of everything `reader` yields.
fn hash(reader: &mut impl Read) -> std::io::Result<(u64, String)> {
    let mut digest = Sha256::new();
    let mut buffer = vec![0; 1 << 16];
    let mut size = 0_u64;
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        size += read as u64;
        digest.update(&buffer[..read]);
    }
    let hex = digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Ok((size, hex))
}

fn is_sha256(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn build(dir: &Path, body: &[u8]) -> PathBuf {
        fs::create_dir_all(dir).unwrap();
        let path = dir.join(exe_name());
        fs::write(&path, body).unwrap();
        path
    }

    /// Approves `candidate` with a probe that records where it ran and says `version`.
    fn approve_with(
        data: &Path,
        candidate: &Candidate,
        version: &str,
        ran: &Mutex<Vec<(PathBuf, PathBuf)>>,
    ) -> Result<Approval, String> {
        approve(
            data,
            candidate,
            1,
            |program, dir| {
                ran.lock()
                    .unwrap()
                    .push((program.to_path_buf(), dir.to_path_buf()));
                Probe {
                    version: Some(version.into()),
                    cuda: cuda_in_version(version),
                }
            },
            |_| Ok(false),
        )
    }

    #[test]
    fn nothing_approved_says_how_to_build_and_approve() {
        let data = tempfile::tempdir().unwrap();
        let error = verify(data.path()).unwrap_err();
        assert!(
            error.contains("cargo build --release -p ncm-server"),
            "{error}"
        );
        assert!(error.contains("--features cuda"), "{error}");
        assert!(error.contains("imperium runtime ncm use"), "{error}");
    }

    #[test]
    fn paths_that_are_relative_links_folders_or_in_agent_folders_are_refused() {
        let root = tempfile::tempdir().unwrap();
        let binary = build(&root.path().join("build"), b"server");
        assert!(inspect(Path::new("ncm-server"), &[]).is_err());
        assert!(inspect(root.path(), &[]).is_err());
        let error = inspect(&binary, &[root.path().to_path_buf()]).unwrap_err();
        assert!(error.contains("agents can write to"), "{error}");
        // The temporary folder (where this test's files are) is one of them.
        let error = inspect(&binary, &temp_folders()).unwrap_err();
        assert!(error.contains("agents can write to"), "{error}");
        #[cfg(unix)]
        {
            let link = root.path().join("link");
            std::os::unix::fs::symlink(&binary, &link).unwrap();
            assert!(inspect(&link, &[]).unwrap_err().contains("symbolic link"));
        }
        let candidate = inspect(&binary, &[]).unwrap();
        assert_eq!(candidate.size, 6);
        assert!(is_sha256(&candidate.sha256));
    }

    #[test]
    fn temp_folders_include_the_system_one() {
        assert!(temp_folders().contains(&std::env::temp_dir()));
    }

    #[test]
    fn an_approved_build_is_copied_and_only_the_copy_is_ever_run() {
        let data = tempfile::tempdir().unwrap();
        let source_dir = tempfile::tempdir().unwrap();
        let binary = build(source_dir.path(), b"the server");
        // A DLL planted beside the original binary: Windows would load it from the
        // executable's folder before any other.
        fs::write(source_dir.path().join("nvrtc64_130_0.dll"), b"planted").unwrap();
        fs::write(source_dir.path().join("cudart64_13.dll"), b"planted").unwrap();
        let candidate = inspect(&binary, &[]).unwrap();
        let ran = Mutex::new(Vec::new());
        let approval = approve_with(data.path(), &candidate, "ncm-server 0.0.0", &ran).unwrap();
        assert_eq!(approval.source, binary);
        assert_eq!(approval.version.as_deref(), Some("ncm-server 0.0.0"));

        let verified = verify(data.path()).unwrap();
        assert_eq!(verified.dir, copy_dir(data.path(), &candidate.sha256));
        assert_eq!(verified.program, verified.dir.join(exe_name()));
        assert_ne!(verified.dir, source_dir.path());
        assert!(verified.program.starts_with(data.path()));
        // The version was asked of the copy, from its own folder, never of the original.
        assert_eq!(
            *ran.lock().unwrap(),
            [(verified.program.clone(), verified.dir.clone())]
        );
        // Only the executable is in the folder it runs from (and is its working directory), so
        // the planted DLLs are never found there.
        let in_copy: Vec<_> = fs::read_dir(&verified.dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(in_copy, [exe_name()]);
        drop(verified);

        // The original changing afterwards does not change what runs.
        fs::write(&binary, b"swapped").unwrap();
        assert_eq!(
            fs::read(verify(data.path()).unwrap().program).unwrap(),
            b"the server"
        );
    }

    #[test]
    fn a_file_dropped_into_the_copy_folder_stops_the_launch() {
        let data = tempfile::tempdir().unwrap();
        let source_dir = tempfile::tempdir().unwrap();
        let binary = build(source_dir.path(), b"the server");
        let candidate = inspect(&binary, &[]).unwrap();
        approve_with(data.path(), &candidate, "x", &Mutex::new(Vec::new())).unwrap();
        let dir = copy_dir(data.path(), &candidate.sha256);
        fs::write(dir.join("nvrtc64_130_0.dll"), b"planted").unwrap();
        let error = verify(data.path()).unwrap_err();
        assert!(error.contains("nvrtc64_130_0.dll"), "{error}");
        // Approving again clears the folder.
        approve_with(data.path(), &candidate, "x", &Mutex::new(Vec::new())).unwrap();
        assert!(verify(data.path()).is_ok());
    }

    #[test]
    fn approving_the_build_already_copied_keeps_the_copy() {
        let data = tempfile::tempdir().unwrap();
        let source_dir = tempfile::tempdir().unwrap();
        let binary = build(source_dir.path(), b"the server");
        let candidate = inspect(&binary, &[]).unwrap();
        approve_with(data.path(), &candidate, "x", &Mutex::new(Vec::new())).unwrap();
        // A running server holds the copy open (on Windows, without write or delete sharing):
        // approving the same build again must not need to replace it.
        let held = verify(data.path()).unwrap();
        approve_with(data.path(), &candidate, "y", &Mutex::new(Vec::new())).unwrap();
        drop(held);
        assert_eq!(
            approved(data.path()).unwrap().unwrap().version.as_deref(),
            Some("y")
        );
    }

    #[test]
    fn whether_the_build_has_cuda_comes_from_its_version_or_the_user() {
        assert_eq!(cuda_in_version("ncm-server 0.0.0"), None);
        assert_eq!(cuda_in_version("ncm-server 0.1.0 (cuda)"), Some(true));
        assert_eq!(
            cuda_in_version("ncm-server 0.1.0 backends: cpu"),
            Some(false)
        );
        let data = tempfile::tempdir().unwrap();
        let source_dir = tempfile::tempdir().unwrap();
        let binary = build(source_dir.path(), b"the server");
        let candidate = inspect(&binary, &[]).unwrap();
        let asked = Mutex::new(None);
        let approval = approve(
            data.path(),
            &candidate,
            1,
            |_, _| Probe {
                version: Some("ncm-server 0.0.0".into()),
                cuda: None,
            },
            |version| {
                *asked.lock().unwrap() = version.map(str::to_string);
                Ok(true)
            },
        )
        .unwrap();
        assert!(approval.cuda);
        assert_eq!(asked.lock().unwrap().as_deref(), Some("ncm-server 0.0.0"));
        assert!(approved_cuda(data.path()));
        let said = approve_with(
            data.path(),
            &candidate,
            "ncm-server 0.1.0 cpu",
            &Mutex::new(Vec::new()),
        )
        .unwrap();
        assert!(!said.cuda);
        assert!(!approved_cuda(data.path()));
    }

    #[test]
    fn an_unread_cuda_answer_records_nothing() {
        let data = tempfile::tempdir().unwrap();
        let source_dir = tempfile::tempdir().unwrap();
        let binary = build(source_dir.path(), b"the server");
        let candidate = inspect(&binary, &[]).unwrap();
        let error = approve(
            data.path(),
            &candidate,
            1,
            |_, _| Probe::default(),
            |_| Err("stdin closed".into()),
        )
        .unwrap_err();
        assert!(error.contains("stdin closed"), "{error}");
        assert!(approved(data.path()).unwrap().is_none());
    }

    #[test]
    fn a_copy_changed_after_approval_is_refused() {
        let data = tempfile::tempdir().unwrap();
        let source_dir = tempfile::tempdir().unwrap();
        let binary = build(source_dir.path(), b"the server");
        let candidate = inspect(&binary, &[]).unwrap();
        approve_with(data.path(), &candidate, "x", &Mutex::new(Vec::new())).unwrap();
        let copy = copy_dir(data.path(), &candidate.sha256).join(exe_name());
        fs::write(&copy, b"tampered").unwrap();
        let error = verify(data.path()).unwrap_err();
        assert!(
            error.contains("has changed since you approved it"),
            "{error}"
        );
    }

    #[test]
    fn a_source_changed_between_showing_and_approving_is_refused() {
        let data = tempfile::tempdir().unwrap();
        let source_dir = tempfile::tempdir().unwrap();
        let binary = build(source_dir.path(), b"the server");
        let candidate = inspect(&binary, &[]).unwrap();
        fs::write(&binary, b"another server").unwrap();
        let ran = Mutex::new(Vec::new());
        assert!(approve_with(data.path(), &candidate, "x", &ran).is_err());
        assert!(approved(data.path()).unwrap().is_none());
        // Nothing was run.
        assert!(ran.lock().unwrap().is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn the_held_copy_cannot_be_replaced_or_deleted_while_it_is_open() {
        let data = tempfile::tempdir().unwrap();
        let source_dir = tempfile::tempdir().unwrap();
        let binary = build(source_dir.path(), b"the server");
        let candidate = inspect(&binary, &[]).unwrap();
        approve_with(data.path(), &candidate, "x", &Mutex::new(Vec::new())).unwrap();
        let verified = verify(data.path()).unwrap();
        assert!(
            fs::OpenOptions::new()
                .write(true)
                .open(&verified.program)
                .is_err()
        );
        assert!(fs::remove_file(&verified.program).is_err());
        assert!(fs::read(&verified.program).is_ok());
        let program = verified.program.clone();
        drop(verified);
        assert!(fs::remove_file(&program).is_ok());
    }

    #[test]
    fn a_damaged_record_is_not_trusted() {
        let data = tempfile::tempdir().unwrap();
        let dir = runtime_dir(data.path());
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join(RECORD),
            r#"{"source": "/x", "sha256": "../../evil", "size": 1, "approved_at": 1}"#,
        )
        .unwrap();
        assert!(
            verify(data.path())
                .unwrap_err()
                .contains("no valid SHA-256")
        );
    }
}
