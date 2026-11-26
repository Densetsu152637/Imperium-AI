//! Which program a terminal runs and with what environment (decision D6 of
//! `docs/plans/terminal-file-panes.md`).
//!
//! The program is always an absolute path chosen without the working folder or a `PATH` entry
//! inside the session's folders: the working folder is a repository that may hold a planted
//! `pwsh.exe` or `bash`. Everything the choice reads is gathered in [`ShellSources`], so the tests
//! can give each input explicitly.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

/// The program and arguments a terminal runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shell {
    /// An absolute path.
    pub program: PathBuf,
    pub args: Vec<OsString>,
}

/// Why no shell was found.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ShellError {
    #[error("terminal.shell names {0:?}, which is neither an absolute path nor a bare name")]
    Relative(String),
    #[error("terminal.shell names {0:?}, which was not found")]
    NotFound(String),
    #[error("no shell was found on this computer")]
    NoDefault,
}

/// Everything the shell choice reads. [`ShellSources::from_system`] fills it from this computer;
/// the tests fill it by hand.
#[derive(Debug, Clone, Default)]
pub struct ShellSources {
    /// `terminal.shell` from the user's own settings: the program, then its arguments.
    pub configured: Vec<String>,
    /// The folders of the session: a `PATH` entry inside one is skipped.
    pub session_folders: Vec<PathBuf>,
    /// `PATH`, for a bare `terminal.shell` program.
    pub path: Option<OsString>,
    /// Unix: the account's shell (`pw_shell`).
    pub account_shell: Option<PathBuf>,
    /// Unix: `$SHELL`.
    pub env_shell: Option<PathBuf>,
    /// Unix: the text of `/etc/shells`.
    pub etc_shells: Option<String>,
    /// Windows: `FOLDERID_ProgramFiles`.
    pub program_files: Option<PathBuf>,
    /// Windows: the `pwsh.exe` named by its App Paths key.
    pub app_paths_pwsh: Option<PathBuf>,
    /// Windows: `FOLDERID_Windows`.
    pub windows_dir: Option<PathBuf>,
}

impl ShellSources {
    /// The sources on this computer. `ComSpec` is never read: the environment controls it.
    pub fn from_system(configured: Vec<String>, session_folders: Vec<PathBuf>) -> Self {
        let mut sources = Self {
            configured,
            session_folders,
            path: std::env::var_os("PATH"),
            ..Self::default()
        };
        #[cfg(unix)]
        {
            sources.account_shell = nix::unistd::User::from_uid(nix::unistd::getuid())
                .ok()
                .flatten()
                .map(|user| user.shell);
            sources.env_shell = std::env::var_os("SHELL").map(PathBuf::from);
            sources.etc_shells = std::fs::read_to_string("/etc/shells").ok();
        }
        #[cfg(windows)]
        {
            use windows_sys::Win32::UI::Shell::{FOLDERID_ProgramFiles, FOLDERID_Windows};
            sources.program_files = dirs_sys::known_folder(FOLDERID_ProgramFiles);
            sources.windows_dir = dirs_sys::known_folder(FOLDERID_Windows);
            sources.app_paths_pwsh = app_paths_pwsh();
        }
        sources
    }
}

/// The `pwsh.exe` its installer registered under App Paths, when it is an absolute file.
#[cfg(windows)]
fn app_paths_pwsh() -> Option<PathBuf> {
    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};

    const KEY: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths\pwsh.exe";
    [HKEY_LOCAL_MACHINE, HKEY_CURRENT_USER]
        .into_iter()
        .find_map(|hive| {
            let value: String = RegKey::predef(hive)
                .open_subkey(KEY)
                .ok()?
                .get_value("")
                .ok()?;
            let path = PathBuf::from(value.trim().trim_matches('"'));
            (path.is_absolute() && path.is_file()).then_some(path)
        })
}

/// The program a terminal runs: `terminal.shell` when set, else the default of this OS.
pub fn resolve_shell(sources: &ShellSources) -> Result<Shell, ShellError> {
    if let Some((program, args)) = sources.configured.split_first() {
        let path = configured_program(program, sources)?;
        return Ok(Shell {
            program: path,
            args: args.iter().map(OsString::from).collect(),
        });
    }
    default_shell(sources)
}

fn configured_program(program: &str, sources: &ShellSources) -> Result<PathBuf, ShellError> {
    let path = Path::new(program);
    if path.is_absolute() {
        return if path.is_file() {
            Ok(path.to_path_buf())
        } else {
            Err(ShellError::NotFound(program.to_owned()))
        };
    }
    // `./bash`, `bin/bash` and a drive-relative `C:bash` all depend on the working folder.
    if path.components().count() != 1 || program.contains(['/', '\\', ':']) {
        return Err(ShellError::Relative(program.to_owned()));
    }
    search_path(program, sources).ok_or_else(|| ShellError::NotFound(program.to_owned()))
}

/// A bare name on `PATH`, skipping empty and relative entries and any entry inside the
/// session's folders.
fn search_path(name: &str, sources: &ShellSources) -> Option<PathBuf> {
    let path = sources.path.as_ref()?;
    let names: Vec<OsString> = if cfg!(windows) && Path::new(name).extension().is_none() {
        vec![format!("{name}.exe").into(), format!("{name}.com").into()]
    } else {
        vec![name.into()]
    };
    let names = &names;
    std::env::split_paths(path)
        .filter(|dir| !dir.as_os_str().is_empty() && dir.is_absolute())
        .filter(|dir| !inside_any(dir, &sources.session_folders))
        .flat_map(|dir| names.iter().map(move |name| dir.join(name)))
        .find(|candidate| candidate.is_file())
}

/// Whether `dir` is one of `folders` or inside one, by canonical path where it exists.
fn inside_any(dir: &Path, folders: &[PathBuf]) -> bool {
    let canonical = |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
    let dir = canonical(dir);
    folders
        .iter()
        .any(|folder| dir.starts_with(canonical(folder)))
}

#[cfg(unix)]
fn default_shell(sources: &ShellSources) -> Result<Shell, ShellError> {
    let listed: Vec<&str> = sources
        .etc_shells
        .as_deref()
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect();
    let usable = |shell: &Option<PathBuf>| {
        shell
            .as_ref()
            .filter(|path| {
                path.is_absolute()
                    && path.to_str().is_some_and(|text| listed.contains(&text))
                    && path.is_file()
            })
            .cloned()
    };
    let program = usable(&sources.account_shell)
        .or_else(|| usable(&sources.env_shell))
        .unwrap_or_else(|| PathBuf::from("/bin/sh"));
    if !program.is_file() {
        return Err(ShellError::NoDefault);
    }
    // A login shell, so an app started from the Dock or a launcher gets the user's `PATH`.
    Ok(Shell {
        program,
        args: vec!["-l".into()],
    })
}

#[cfg(windows)]
fn default_shell(sources: &ShellSources) -> Result<Shell, ShellError> {
    let powershell = |program: PathBuf| Shell {
        program,
        args: vec!["-NoLogo".into()],
    };
    if let Some(pwsh) = installed_pwsh(sources) {
        return Ok(powershell(pwsh));
    }
    let windows = sources.windows_dir.as_ref().ok_or(ShellError::NoDefault)?;
    let system = windows.join("System32");
    let legacy = system.join(r"WindowsPowerShell\v1.0\powershell.exe");
    if legacy.is_file() {
        return Ok(powershell(legacy));
    }
    let cmd = system.join("cmd.exe");
    if cmd.is_file() {
        return Ok(Shell {
            program: cmd,
            args: Vec::new(),
        });
    }
    Err(ShellError::NoDefault)
}

/// PowerShell 7 from its install folder (`<Program Files>\PowerShell\<n>\pwsh.exe`, the highest
/// `n`), else from its App Paths key.
#[cfg(windows)]
fn installed_pwsh(sources: &ShellSources) -> Option<PathBuf> {
    let from_folder = sources.program_files.as_ref().and_then(|program_files| {
        let mut versions: Vec<(u32, PathBuf)> = std::fs::read_dir(program_files.join("PowerShell"))
            .ok()?
            .flatten()
            .filter_map(|entry| {
                let version: u32 = entry.file_name().to_str()?.parse().ok()?;
                let pwsh = entry.path().join("pwsh.exe");
                pwsh.is_file().then_some((version, pwsh))
            })
            .collect();
        versions.sort();
        versions.pop().map(|(_, pwsh)| pwsh)
    });
    from_folder.or_else(|| sources.app_paths_pwsh.clone())
}

/// Variables dropped from the daemon's environment before a shell gets it: they make a shell or
/// a tool run code it was not asked to. The list is **not exhaustive** (no list of every
/// program's start-up variable can be): it covers the shells' own, the dynamic loaders' and the
/// common interpreters' and git's. The shell is the user's own and runs outside every isolation
/// tier, so this guards against what the daemon itself inherited, not against the user.
const DROPPED: &[&str] = &[
    "ENV",
    "BASH_ENV",
    "ZDOTDIR",
    "PROMPT_COMMAND",
    "NODE_OPTIONS",
    "PYTHONSTARTUP",
    "PERL5OPT",
    "RUBYOPT",
    "JAVA_TOOL_OPTIONS",
];
/// Prefixes of dropped variables: the dynamic loaders', git's injected configuration
/// (`GIT_CONFIG`, `GIT_CONFIG_COUNT`, `GIT_CONFIG_KEY_<n>`, `GIT_CONFIG_PARAMETERS` and the
/// like) and the daemon's own.
const DROPPED_PREFIXES: &[&str] = &["LD_", "DYLD_", "GIT_CONFIG", "IMPERIUM_"];
/// Set for the shell; any inherited value is replaced.
const REPLACED: &[&str] = &[
    "TERM",
    "COLORTERM",
    "TERM_PROGRAM",
    "TERM_PROGRAM_VERSION",
    "PWD",
    "OLDPWD",
    "NODEFAULTCURRENTDIRECTORYINEXEPATH",
];
/// An `IMPERIUM_` variable a shell keeps: where the user's Imperium home is, so an `imperium`
/// command in the terminal reaches this daemon. It is a path, not a secret.
const KEPT_IMPERIUM: &[&str] = &["IMPERIUM_HOME"];

fn dropped(name: &OsStr) -> bool {
    let Some(name) = name.to_str() else {
        // A name that is not Unicode is no variable this list knows; keep it.
        return false;
    };
    // Windows' variable names ignore case.
    let upper = name.to_ascii_uppercase();
    let name = if cfg!(windows) { upper.as_str() } else { name };
    if KEPT_IMPERIUM.contains(&name) {
        return false;
    }
    DROPPED.contains(&name)
        || REPLACED.contains(&name)
        || DROPPED_PREFIXES
            .iter()
            .any(|prefix| name.starts_with(prefix))
}

/// Whether `name` is `PATH` (any case on Windows).
fn is_path(name: &OsStr) -> bool {
    if cfg!(windows) {
        name.to_str()
            .is_some_and(|name| name.eq_ignore_ascii_case("PATH"))
    } else {
        name == "PATH"
    }
}

/// `PATH` without its empty and relative entries: a shell searches those from its working
/// folder, the repository, which may hold a planted `git` or `ls`.
fn absolute_path_entries(path: &OsStr) -> OsString {
    let kept: Vec<PathBuf> = std::env::split_paths(path)
        .filter(|entry| !entry.as_os_str().is_empty() && entry.is_absolute())
        .collect();
    // An entry that cannot be joined back (one holding the separator) was not split from it.
    std::env::join_paths(kept).unwrap_or_default()
}

/// The shell's whole environment: the daemon's own (`inherited`) minus [`DROPPED`] and the
/// daemon's internal `IMPERIUM_*` variables, with `PATH` cut to its absolute entries, plus the
/// terminal's own and the spawn mark (and, on Windows, `NoDefaultCurrentDirectoryInExePath`, so
/// `cmd` and `CreateProcess` do not look for a bare program in the working folder first).
/// `GH_TOKEN` and the like stay: this shell is the user's, outside every isolation tier and
/// permission rule.
pub fn shell_env(
    inherited: impl IntoIterator<Item = (OsString, OsString)>,
    session_id: &str,
    cwd: &Path,
) -> Vec<(OsString, OsString)> {
    let mut env: Vec<(OsString, OsString)> = inherited
        .into_iter()
        .filter(|(name, _)| !dropped(name))
        .map(|(name, value)| {
            let value = if is_path(&name) {
                absolute_path_entries(&value)
            } else {
                value
            };
            (name, value)
        })
        .collect();
    if cfg!(windows) {
        env.push(("NoDefaultCurrentDirectoryInExePath".into(), "1".into()));
    }
    let own: [(&str, OsString); 7] = [
        ("TERM", "xterm-256color".into()),
        ("COLORTERM", "truecolor".into()),
        ("TERM_PROGRAM", "Imperium".into()),
        ("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION").into()),
        ("PWD", cwd.as_os_str().to_owned()),
        ("IMPERIUM_SESSION_ID", session_id.into()),
        (imperium_tools::SPAWNED_MARK, "1".into()),
    ];
    env.extend(own.into_iter().map(|(name, value)| (name.into(), value)));
    env
}
