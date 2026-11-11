//! Git for the daemon's own work in a session's repositories (checkpoints, rewinding, review):
//! nothing a repository's config names can run in the daemon. Hooks and fsmonitor are turned
//! off on every call, contents are hashed and read without filters (`hash-object
//! --no-filters`, `cat-file blob`), no diff driver, pager, or editor is used, nothing prompts,
//! and git's own environment variables from the daemon's surroundings are dropped.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// A hooks folder that holds no hooks.
#[cfg(windows)]
const NO_HOOKS: &str = "core.hooksPath=NUL";
#[cfg(not(windows))]
const NO_HOOKS: &str = "core.hooksPath=/dev/null";

/// Variables that would point git somewhere else or change what it runs.
pub(crate) const DROPPED_ENV: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_COMMON_DIR",
    "GIT_NAMESPACE",
    "GIT_EXTERNAL_DIFF",
    "GIT_PAGER",
    "GIT_EDITOR",
    "GIT_SEQUENCE_EDITOR",
    "GIT_SSH",
    "GIT_SSH_COMMAND",
    "GIT_ASKPASS",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
    "GIT_GLOB_PATHSPECS",
    "GIT_NOGLOB_PATHSPECS",
    "GIT_ICASE_PATHSPECS",
];

/// A git command in `dir` with hooks and fsmonitor off, no pager, no prompts, and no console
/// window.
pub(crate) fn command(dir: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .arg("--no-pager")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            NO_HOOKS,
            "-c",
            "core.fsmonitor=false",
            "-c",
            "gc.auto=0",
            "-c",
            "maintenance.auto=false",
            "-c",
            "core.quotePath=false",
        ])
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        // Paths given to git are paths, never pathspec patterns: `[ab].txt` is one file.
        .env("GIT_LITERAL_PATHSPECS", "1")
        // No network: a missing object in a partial clone is not fetched (which would run the
        // transport, ssh command, or credential helper the repository's config names), and no
        // transport is allowed whatever the repository's `protocol.*.allow` says. A push sets
        // its own list.
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_ALLOW_PROTOCOL", "none")
        .env("GCM_INTERACTIVE", "never")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for name in DROPPED_ENV {
        command.env_remove(name);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: the daemon has no console, so git would otherwise open one.
        command.creation_flags(0x0800_0000);
    }
    command
}

/// Runs `command`, returning its standard output or its error message.
pub(crate) fn run(command: &mut Command) -> Result<Vec<u8>, String> {
    let output = command
        .output()
        .map_err(|error| format!("could not run git: {error}"))?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(failure(command, &output.stderr))
    }
}

/// Runs `command` with `input` on its standard input.
pub(crate) fn run_with_input(command: &mut Command, input: Vec<u8>) -> Result<Vec<u8>, String> {
    let mut child = command
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|error| format!("could not run git: {error}"))?;
    let mut stdin = child.stdin.take().expect("stdin is piped");
    // Written on its own thread, so a full output pipe cannot block the write.
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&input);
    });
    let output = child
        .wait_with_output()
        .map_err(|error| format!("could not run git: {error}"))?;
    let _ = writer.join();
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(failure(command, &output.stderr))
    }
}

fn failure(command: &Command, stderr: &[u8]) -> String {
    // The subcommand: the first argument after the `-c` pairs.
    let args: Vec<String> = command
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let sub = args
        .iter()
        .skip(3)
        .scan(false, |skip, arg| {
            let this = !*skip && arg != "-c";
            *skip = arg == "-c";
            Some(this.then(|| arg.clone()))
        })
        .flatten()
        .next()
        .unwrap_or_default();
    format!(
        "git {sub} failed: {}",
        String::from_utf8_lossy(stderr).trim()
    )
}

/// Output as trimmed text.
pub(crate) fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).trim().to_string()
}

/// The top folder of the working tree `folder` is in (a linked worktree's own folder), if it
/// is in one.
pub(crate) fn toplevel(folder: &Path) -> Option<PathBuf> {
    let out = run(command(folder).args(["rev-parse", "--show-toplevel"])).ok()?;
    let top = PathBuf::from(text(&out)).canonicalize().ok()?;
    Some(plain(&top))
}

/// The commit HEAD is at; `None` in a repository without commits.
pub(crate) fn head(top: &Path) -> Option<String> {
    run(command(top).args(["rev-parse", "--verify", "--quiet", "HEAD^{commit}"]))
        .ok()
        .map(|out| text(&out))
        .filter(|sha| !sha.is_empty())
}

/// A path without Windows' `\\?\` prefix, which git does not take.
pub(crate) fn plain(path: &Path) -> PathBuf {
    let shown = path.display().to_string();
    match shown.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC") => PathBuf::from(rest),
        _ => path.to_path_buf(),
    }
}
