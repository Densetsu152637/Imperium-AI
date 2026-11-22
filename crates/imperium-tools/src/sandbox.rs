//! Isolation tiers for commands agents run. Tier 0 (policy) confines only the file tools; the
//! OS tier runs commands in an OS sandbox that can write only inside the workspace (bubblewrap
//! on Linux, Seatbelt on macOS); the container tier runs them in a container with only the
//! workspace mounted. A tier that is not available fails the command rather than silently
//! running it with less isolation.
//!
//! Path grants carry over where the tier can express them: the OS sandbox makes writable grants
//! writable (everything is readable there already), and the container tier bind-mounts every
//! grant, read-only ones with `:ro`. Tier 0 runs commands with the user's own rights, so grants
//! (and their read-only mode) bind only the file tools there.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use tokio::process::Command;

use crate::GrantedPath;

use crate::ToolError;
use crate::exec::{SPAWNED_MARK, shell};

/// What the daemon keeps out of the OS tier's reach: the files and folders that hold its
/// credentials or its local socket, and the loopback ports it listens on. A command in the OS
/// sandbox that could read the access token (or open the socket) could drive the unconfined
/// daemon, so these are hidden from it; see docs/05-tools-and-isolation.md.
#[derive(Debug)]
struct Hidden {
    paths: Vec<PathBuf>,
    /// Folders no command may write, whatever folder or grant contains them.
    guarded: Vec<PathBuf>,
    /// Folders inside a guarded one that stay writable (the workspaces root).
    open: Vec<PathBuf>,
    loopback_ports: Vec<u16>,
}

static HIDDEN: Mutex<Hidden> = Mutex::new(Hidden {
    paths: Vec::new(),
    guarded: Vec::new(),
    open: Vec::new(),
    loopback_ports: Vec::new(),
});

/// Registers folders (the daemon's configuration and data folders) that commands in the OS and
/// container tiers must not write, even when a session folder or a writable grant contains them
/// (a workspace of `~`): the OS tier mounts them read-only, the container tier mounts an empty
/// folder over them. Only the folders registered with [`leave_writable_in_guards`] stay writable
/// inside them.
pub fn guard_from_commands(paths: impl IntoIterator<Item = PathBuf>) {
    let mut hidden = HIDDEN.lock().unwrap_or_else(PoisonError::into_inner);
    for path in paths {
        if !hidden.guarded.contains(&path) {
            hidden.guarded.push(path);
        }
    }
}

/// Registers folders that stay writable inside a guarded one: the workspaces root (the default
/// sessions workspace and every session's scratch folder are under it) and the folders agents
/// work in. A writable folder or grant anywhere else in a guarded folder, such as `<config>/sub`
/// or `<data>/sessions`, stays read-only.
pub fn leave_writable_in_guards(paths: impl IntoIterator<Item = PathBuf>) {
    let mut hidden = HIDDEN.lock().unwrap_or_else(PoisonError::into_inner);
    for path in paths {
        if !hidden.open.contains(&path) {
            hidden.open.push(path);
        }
    }
}

/// A path under its own name and, on Unix, its resolved one (`/home` may be a link to
/// `/var/home`): the mount namespace and Seatbelt see the resolved one, a setting holds the other.
fn names_of(path: &Path) -> Vec<PathBuf> {
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut names = vec![path.to_path_buf()];
    #[cfg(unix)]
    {
        if let Ok(resolved) = std::fs::canonicalize(path)
            && resolved.as_path() != path
        {
            names.push(resolved);
        }
    }
    names
}

/// The folders commands must not write, and the folders inside them that stay writable.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Guards {
    folders: Vec<PathBuf>,
    open: Vec<PathBuf>,
}

impl Guards {
    /// Whether a writable `path` inside the guarded `guard` stays writable: only under an open
    /// folder, never anywhere else in it (`<config>`, `<data>/sessions`, `<data>/packs`, …).
    fn leaves_writable(&self, path: &Path, guard: &Path) -> bool {
        path.starts_with(guard)
            && path != guard
            && self.open.iter().any(|open| path.starts_with(open))
    }

    /// Whether `source`, mounted into a container, is inside (or is) a guarded folder and not
    /// under an open one: it is mounted read-only.
    fn guards(&self, source: &Path) -> bool {
        self.folders.iter().any(|guard| source.starts_with(guard))
            && !self.open.iter().any(|open| source.starts_with(open))
    }
}

/// The registered guarded and open folders that exist now, each under every name it has.
fn current_guards() -> Guards {
    let (guarded, open) = {
        let hidden = HIDDEN.lock().unwrap_or_else(PoisonError::into_inner);
        (hidden.guarded.clone(), hidden.open.clone())
    };
    let existing = |paths: Vec<PathBuf>| {
        let mut out: Vec<PathBuf> = Vec::new();
        for path in paths.iter().filter(|path| path.is_dir()) {
            for name in names_of(path) {
                if !out.contains(&name) {
                    out.push(name);
                }
            }
        }
        out
    };
    Guards {
        folders: existing(guarded),
        open: existing(open),
    }
}

/// Registers files and folders the OS sandbox hides from commands (the endpoint file holding the
/// access token, the local socket's folder, the secret store files). Only paths that exist when
/// a command starts are hidden; registering a path twice is harmless.
pub fn hide_from_os_sandbox(paths: impl IntoIterator<Item = PathBuf>) {
    let mut hidden = HIDDEN.lock().unwrap_or_else(PoisonError::into_inner);
    for path in paths {
        if !hidden.paths.contains(&path) {
            hidden.paths.push(path);
        }
    }
}

/// Registers a loopback TCP port the daemon listens on. Seatbelt (macOS) denies commands
/// connections to it; bubblewrap on Linux cannot filter one port while the network is shared, so
/// there the port stays reachable and the access token's unreadability is the protection.
pub fn hide_loopback_port(port: u16) {
    let mut hidden = HIDDEN.lock().unwrap_or_else(PoisonError::into_inner);
    if port != 0 && !hidden.loopback_ports.contains(&port) {
        hidden.loopback_ports.push(port);
    }
}

/// A path hidden inside the OS sandbox: a file is replaced by an empty one, a folder by an empty
/// one (so a socket inside it is gone).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Mask {
    path: String,
    dir: bool,
}

impl Mask {
    #[cfg(test)]
    fn file(path: &str) -> Self {
        Mask {
            path: path.into(),
            dir: false,
        }
    }

    #[cfg(test)]
    fn dir(path: &str) -> Self {
        Mask {
            path: path.into(),
            dir: true,
        }
    }
}

/// The masks for the registered paths that exist now (a missing path cannot be a mount point in
/// a read-only tree, and has nothing to hide), each also under its resolved name, plus the
/// desktop session's D-Bus socket and keyring sockets, through which the Secret Service would
/// hand a command the secrets the daemon stores.
#[cfg_attr(not(any(target_os = "linux", target_os = "macos")), allow(dead_code))]
fn hidden_masks() -> Vec<Mask> {
    let mut paths: Vec<PathBuf> = HIDDEN
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .paths
        .clone();
    paths.extend(runtime_dirs());
    masks_for(&paths)
}

/// The user's runtime folder, hidden whole: it holds the session bus, the keyring, the systemd
/// user manager's private socket (`systemd-run --user` would start a process outside the
/// sandbox), the ssh and gpg agent sockets and a rootless container runtime's socket, and any one
/// of them hands a command more than the sandbox gives it. The cost is that a display server's
/// socket (X11 or Wayland) is hidden too, so graphical programs do not start in the OS tier.
/// `XDG_RUNTIME_DIR`, and `/run/user/<uid>` when it is unset.
#[cfg_attr(not(any(target_os = "linux", target_os = "macos")), allow(dead_code))]
fn runtime_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR").filter(|dir| !dir.is_empty()) {
        dirs.push(PathBuf::from(runtime));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let Ok(me) = std::fs::metadata("/proc/self") {
            let fallback = PathBuf::from(format!("/run/user/{}", me.uid()));
            if !dirs.contains(&fallback) {
                dirs.push(fallback);
            }
        }
    }
    dirs
}

#[cfg_attr(not(any(target_os = "linux", target_os = "macos")), allow(dead_code))]
fn masks_for(paths: &[PathBuf]) -> Vec<Mask> {
    let mut masks: Vec<Mask> = Vec::new();
    for path in paths {
        let Ok(metadata) = std::fs::metadata(path) else {
            continue;
        };
        let mut names = vec![path.clone()];
        if let Ok(resolved) = std::fs::canonicalize(path)
            && resolved != *path
        {
            names.push(resolved);
        }
        for name in names {
            let mask = Mask {
                path: name.display().to_string(),
                dir: metadata.is_dir(),
            };
            if !masks.contains(&mask) {
                masks.push(mask);
            }
        }
    }
    masks
}

/// The inherited environment variables an OS-tier command must not see: everything named
/// `IMPERIUM_*` (the daemon's home folder, its secret-store passphrase file, port overrides: each
/// points a nested `imperium` at the daemon or its files) and the desktop session bus address.
fn daemon_env_names(names: impl IntoIterator<Item = OsString>) -> Vec<OsString> {
    names
        .into_iter()
        .filter(|name| {
            let name = name.to_string_lossy();
            name.starts_with("IMPERIUM_") || name == "DBUS_SESSION_BUS_ADDRESS"
        })
        .collect()
}

/// Where commands run.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Sandbox {
    /// The user's own shell, in the workspace (tier 0).
    #[default]
    Policy,
    /// An OS sandbox: writes only inside the workspace (and temporary files).
    Os { network: bool },
    /// A container with the workspace mounted at `/workspace`.
    Container {
        runtime: String,
        image: String,
        network: bool,
    },
}

impl Sandbox {
    /// A short label for the interface, such as `tier 2 · container ubuntu:24.04`.
    pub fn label(&self) -> String {
        match self {
            Sandbox::Policy => "tier 0 · folder policy".into(),
            Sandbox::Os { network } => format!(
                "tier 1 · OS sandbox{}",
                if *network { "" } else { ", no network" }
            ),
            Sandbox::Container { image, network, .. } => format!(
                "tier 2 · container {image}{}",
                if *network { "" } else { ", no network" }
            ),
        }
    }

    /// The process that runs `command` under this tier, in the first of `roots` (the workspace's
    /// folders). The OS sandbox lets it write in every folder and writable grant; the container
    /// mounts them all and the grants.
    pub fn command(
        &self,
        roots: &[&Path],
        grants: &[GrantedPath],
        command: &str,
    ) -> Result<Command, ToolError> {
        self.named_command(roots, grants, command, None, false)
    }

    /// Like [`Sandbox::command`]; a container is given `name`, so a background command's
    /// container can be removed by name when it is stopped. With `read_only` the folders are a
    /// read-only mount: the OS sandbox and the container mount `roots` read-only (grants keep
    /// their own mode), and tier 0, which cannot keep a command from writing, refuses it.
    pub(crate) fn named_command(
        &self,
        roots: &[&Path],
        grants: &[GrantedPath],
        command: &str,
        name: Option<&str>,
        read_only: bool,
    ) -> Result<Command, ToolError> {
        let Some(root) = roots.first() else {
            return Err(ToolError::Io("the workspace has no folder".into()));
        };
        match self {
            Sandbox::Policy => {
                if read_only {
                    return Err(ToolError::Io(READ_ONLY_TIER_0.into()));
                }
                let mut process = shell(command);
                process.current_dir(root);
                Ok(process)
            }
            Sandbox::Os { network } => {
                let mut writable: Vec<&Path> = if read_only {
                    Vec::new()
                } else {
                    roots.to_vec()
                };
                writable.extend(
                    grants
                        .iter()
                        .filter(|grant| grant.writable)
                        .map(|grant| grant.path.as_path()),
                );
                // A writable grant inside (or around) a read-only folder must not reopen it.
                let protected: Vec<&Path> = if read_only {
                    roots.to_vec()
                } else {
                    Vec::new()
                };
                os_command(root, &writable, &protected, command, *network).map(|mut process| {
                    // Nothing that names the daemon or its files is inherited; the mark below
                    // is the one `IMPERIUM_*` variable a command is meant to carry.
                    for name in daemon_env_names(std::env::vars_os().map(|(name, _)| name)) {
                        process.env_remove(name);
                    }
                    process.env(SPAWNED_MARK, "1");
                    process
                })
            }
            Sandbox::Container {
                runtime,
                image,
                network,
            } => {
                let mut process = Command::new(runtime);
                let mut args = container_args(roots, grants, image, *network, command, read_only);
                if let Some(name) = name {
                    // After `run --rm -i`.
                    args.splice(3..3, ["--name".to_string(), name.to_string()]);
                }
                process.args(args).env(SPAWNED_MARK, "1");
                Ok(process)
            }
        }
    }
}

/// Why a read-only session cannot run commands under tier 0.
const READ_ONLY_TIER_0: &str = "this session's folders are a read-only mount, and tier 0 \
(folder policy) cannot keep a command from writing there, so commands are refused; set \
isolation.tier to \"container\" (or \"os\" on Linux and macOS) to run commands in a read-only \
session";

/// The first folder is mounted at `/workspace` (the working directory) and the others at
/// `/folders/2`, `/folders/3`, … in order, read-only (`:ro`) when `read_only`. Grants are
/// mounted at their own path when it is a POSIX path, else (a Windows path) at `/grants/1`,
/// `/grants/2`, …; read-only ones with `:ro`.
fn container_args(
    roots: &[&Path],
    grants: &[GrantedPath],
    image: &str,
    network: bool,
    command: &str,
    read_only: bool,
) -> Vec<String> {
    container_args_guarding(
        roots,
        grants,
        image,
        network,
        command,
        read_only,
        &current_guards(),
        &hidden_masks(),
    )
}

/// Like [`container_args`], keeping the daemon out of the container. A mounted folder or grant
/// that is, or is inside, a hidden path is replaced by an empty tmpfs; one that is a guarded
/// folder, or inside one but not under an open folder (the workspaces root), is mounted
/// read-only. Every guarded folder and hidden path that a mount contains is covered at its path
/// in the container, by an empty tmpfs or (a file) `/dev/null` mounted read-only, unless
/// another mount or cover already has that path (a container refuses a duplicate mount point).
/// A mounted folder inside a guarded one (the default workspace is inside `<data>`) mounts only
/// itself, so nothing around it is there.
#[allow(clippy::too_many_arguments)]
fn container_args_guarding(
    roots: &[&Path],
    grants: &[GrantedPath],
    image: &str,
    network: bool,
    command: &str,
    read_only: bool,
    guards: &Guards,
    hidden: &[Mask],
) -> Vec<String> {
    // (source, source as written, target in the container, read-only)
    let mut planned: Vec<(PathBuf, String, String, bool)> = Vec::new();
    for (index, root) in roots.iter().enumerate() {
        let target = if index == 0 {
            "/workspace".to_string()
        } else {
            format!("/folders/{}", index + 1)
        };
        planned.push((
            root.to_path_buf(),
            root.display().to_string(),
            target,
            read_only,
        ));
    }
    for (index, grant) in grants.iter().enumerate() {
        let source = grant.display_path();
        let target = if source.starts_with('/') {
            source.clone()
        } else {
            format!("/grants/{}", index + 1)
        };
        // A writable grant that is inside a read-only folder, or around it, would reopen it at
        // the grant's own path: it is mounted read-only too.
        let reopens = read_only
            && roots
                .iter()
                .any(|root| grant.path.starts_with(root) || root.starts_with(&grant.path));
        planned.push((
            grant.path.clone(),
            source,
            target,
            !grant.writable || reopens,
        ));
    }
    let mut args = vec!["run".into(), "--rm".into(), "-i".into()];
    if !network {
        args.extend(["--network".into(), "none".into()]);
    }
    let mut mounted: Vec<String> = Vec::new();
    let mut covers: Vec<(String, bool)> = Vec::new();
    for (source, shown, target, ro) in &planned {
        mounted.push(target.clone());
        let hides_all = hidden.iter().any(|mask| {
            let path = Path::new(&mask.path);
            source.as_path() == path || (mask.dir && source.starts_with(path))
        });
        if hides_all {
            args.extend(["--tmpfs".into(), target.clone()]);
            continue;
        }
        let mode = if *ro || guards.guards(source) {
            ":ro"
        } else {
            ""
        };
        args.extend(["-v".into(), format!("{shown}:{target}{mode}")]);
        let inside = |inner: &Path| -> Option<String> {
            let relative = inner.strip_prefix(source).ok()?;
            if relative.as_os_str().is_empty() {
                return None;
            }
            let parts: Vec<String> = relative
                .components()
                .map(|part| part.as_os_str().to_string_lossy().into_owned())
                .collect();
            Some(format!(
                "{}/{}",
                target.trim_end_matches('/'),
                parts.join("/")
            ))
        };
        for guard in &guards.folders {
            if let Some(at) = inside(guard) {
                covers.push((at, true));
            }
        }
        for mask in hidden {
            if let Some(at) = inside(Path::new(&mask.path)) {
                covers.push((at, mask.dir));
            }
        }
    }
    let folders: Vec<String> = covers
        .iter()
        .filter(|(_, dir)| *dir)
        .map(|(at, _)| at.clone())
        .collect();
    let mut done: Vec<String> = Vec::new();
    for (at, dir) in covers {
        let under_another = folders
            .iter()
            .any(|folder| *folder != at && at.starts_with(&format!("{folder}/")));
        if mounted.contains(&at) || done.contains(&at) || under_another {
            continue;
        }
        done.push(at.clone());
        if dir {
            args.extend(["--tmpfs".into(), at]);
        } else {
            args.extend(["-v".into(), format!("/dev/null:{at}:ro")]);
        }
    }
    args.extend([
        "-w".into(),
        "/workspace".into(),
        image.into(),
        "sh".into(),
        "-c".into(),
        command.into(),
    ]);
    args
}

/// The guarded folders that a writable path is, contains or is inside of: the others are
/// read-only already.
#[cfg_attr(not(any(target_os = "linux", target_os = "macos")), allow(dead_code))]
fn relevant_guards(guards: Guards, writable: &[PathBuf]) -> Guards {
    Guards {
        folders: guards
            .folders
            .into_iter()
            .filter(|guard| {
                writable
                    .iter()
                    .any(|path| path.starts_with(guard) || guard.starts_with(path))
            })
            .collect(),
        open: guards.open,
    }
}

/// Every writable path under every name it has, so that a link in the way (`/home` to
/// `/var/home`) cannot make a guard and a workspace disagree about where one is.
#[cfg_attr(not(any(target_os = "linux", target_os = "macos")), allow(dead_code))]
fn writable_names(writable: &[&Path]) -> Vec<PathBuf> {
    let mut names: Vec<PathBuf> = Vec::new();
    for path in writable {
        for name in names_of(path) {
            if !names.contains(&name) {
                names.push(name);
            }
        }
    }
    names
}

#[cfg_attr(not(any(target_os = "linux", target_os = "macos")), allow(dead_code))]
fn shown(paths: &[PathBuf]) -> Vec<String> {
    paths
        .iter()
        .map(|path| path.display().to_string())
        .collect()
}

#[cfg_attr(not(any(target_os = "linux", target_os = "macos")), allow(dead_code))]
fn displayed(roots: &[&Path]) -> Vec<String> {
    roots
        .iter()
        .map(|root| root.display().to_string())
        .collect()
}

/// Runs `command` in `cwd` with writes allowed only in `writable` (and temporary files), and
/// never in `protected`, even where a writable path covers it.
#[cfg(target_os = "linux")]
fn os_command(
    cwd: &Path,
    writable: &[&Path],
    protected: &[&Path],
    command: &str,
    network: bool,
) -> Result<Command, ToolError> {
    let mut process = Command::new("bwrap");
    let writable = writable_names(writable);
    let guards = relevant_guards(current_guards(), &writable);
    process
        .args(bwrap_args(
            &shown(&writable),
            &displayed(protected),
            &guards,
            &hidden_masks(),
            &cwd.display().to_string(),
            network,
        ))
        .args(["sh", "-c", command]);
    process.current_dir(cwd);
    Ok(process)
}

/// bubblewrap: the whole filesystem read-only, the `writable` paths and a private /tmp
/// writable, its own process namespace, and optionally no network. Runs in `cwd`. The
/// `protected` paths are bound read-only last, so they stay read-only inside a writable one.
/// The `guarded` folders are bound read-only after the writable paths, then the writable paths
/// inside them again. The `hidden` paths are masked after every other mount, so no folder or
/// grant (even one around them) brings them back: a file is replaced by `/dev/null`, a folder by an empty tmpfs.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn bwrap_args(
    writable: &[String],
    protected: &[String],
    guards: &Guards,
    hidden: &[Mask],
    cwd: &str,
    network: bool,
) -> Vec<String> {
    let mut args: Vec<String> = ["--ro-bind", "/", "/", "--dev", "/dev", "--proc", "/proc"]
        .iter()
        .chain(&["--tmpfs", "/tmp"])
        .map(|arg| arg.to_string())
        .collect();
    for path in writable {
        args.extend(["--bind".into(), path.clone(), path.clone()]);
    }
    // The guarded folders are read-only even where a writable path contains them. Only after all
    // of them are bound (one may be another name for the same folder) are the writable paths that
    // stay writable inside them bound again.
    for guard in &guards.folders {
        let guard = guard.display().to_string();
        args.extend(["--ro-bind".into(), guard.clone(), guard]);
    }
    let mut rebound: Vec<&String> = Vec::new();
    for guard in &guards.folders {
        for path in writable {
            if guards.leaves_writable(Path::new(path), guard) && !rebound.contains(&path) {
                rebound.push(path);
                args.extend(["--bind".into(), path.clone(), path.clone()]);
            }
        }
    }
    for path in protected {
        args.extend(["--ro-bind".into(), path.clone(), path.clone()]);
    }
    for mask in hidden {
        if mask.dir {
            args.extend(["--tmpfs".into(), mask.path.clone()]);
        } else {
            args.extend(["--ro-bind".into(), "/dev/null".into(), mask.path.clone()]);
        }
    }
    args.extend(["--chdir".into(), cwd.to_string()]);
    args.extend(["--unshare-pid".into(), "--die-with-parent".into()]);
    if !network {
        args.push("--unshare-net".into());
    }
    args
}

#[cfg(target_os = "macos")]
fn os_command(
    cwd: &Path,
    writable: &[&Path],
    protected: &[&Path],
    command: &str,
    network: bool,
) -> Result<Command, ToolError> {
    let mut process = Command::new("sandbox-exec");
    let writable = writable_names(writable);
    let guards = relevant_guards(current_guards(), &writable);
    process
        .args([
            "-p",
            &seatbelt_profile(
                &shown(&writable),
                &displayed(protected),
                &guards,
                &hidden_masks(),
                &HIDDEN
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .loopback_ports,
                network,
            ),
        ])
        .args(["sh", "-c", command]);
    process.current_dir(cwd);
    Ok(process)
}

/// A Seatbelt profile allowing everything but writes outside the `writable` paths and
/// temporary folders, and optionally network access. Writes under the `protected` paths are
/// denied last, and the last matching rule wins. Writes under the `guarded` folders are denied
/// too, except under a writable path inside one. The `hidden` paths cannot be read (nor can a
/// local socket in a hidden folder be connected to), and connections to the daemon's loopback
/// `ports` are denied: a command can neither read the access token nor reach the daemon.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn seatbelt_profile(
    writable: &[String],
    protected: &[String],
    guards: &Guards,
    hidden: &[Mask],
    ports: &[u16],
    network: bool,
) -> String {
    let folders: String = writable
        .iter()
        .map(|root| {
            let quoted = root.replace('\\', "\\\\").replace('"', "\\\"");
            format!("(subpath \"{quoted}\") ")
        })
        .collect();
    let mut profile = format!(
        "(version 1)(allow default)(deny file-write*)\
         (allow file-write* {folders}(subpath \"/private/tmp\") \
         (subpath \"/private/var/folders\") (literal \"/dev/null\") (literal \"/dev/tty\"))"
    );
    // Guarded folders cannot be written even where a writable path contains them; only a writable
    // path under an open folder inside one is allowed again.
    for guard in &guards.folders {
        let quoted = guard
            .display()
            .to_string()
            .replace('\\', "\\\\")
            .replace('"', "\\\"");
        profile.push_str(&format!("(deny file-write* (subpath \"{quoted}\"))"));
    }
    for guard in &guards.folders {
        for path in writable {
            if guards.leaves_writable(Path::new(path), guard) {
                let inner = path.replace('\\', "\\\\").replace('"', "\\\"");
                profile.push_str(&format!("(allow file-write* (subpath \"{inner}\"))"));
            }
        }
    }
    for path in protected {
        let quoted = path.replace('\\', "\\\\").replace('"', "\\\"");
        profile.push_str(&format!("(deny file-write* (subpath \"{quoted}\"))"));
    }
    let mut ancestors: Vec<&Path> = Vec::new();
    for mask in hidden {
        let quoted = mask.path.replace('\\', "\\\\").replace('"', "\\\"");
        let filter = if mask.dir {
            format!("(subpath \"{quoted}\")")
        } else {
            format!("(literal \"{quoted}\")")
        };
        // Reading is denied by path, so writing, renaming (a rename away reopens the file under
        // another name) and linking are denied by the same path.
        profile.push_str(&format!(
            "(deny file-read* {filter})(deny file-write* {filter})(deny file-link {filter})"
        ));
        if mask.dir {
            profile.push_str(&format!(
                "(deny network-outbound (remote unix-socket (subpath \"{quoted}\")))"
            ));
        }
        for ancestor in Path::new(&mask.path).ancestors().skip(1) {
            if ancestor.parent().is_some() && !ancestors.contains(&ancestor) {
                ancestors.push(ancestor);
            }
        }
    }
    // Nor can a folder around them be renamed away, which would carry them to another path.
    for ancestor in ancestors {
        let quoted = ancestor
            .display()
            .to_string()
            .replace('\\', "\\\\")
            .replace('"', "\\\"");
        profile.push_str(&format!("(deny file-write-unlink (literal \"{quoted}\"))"));
    }
    for port in ports {
        profile.push_str(&format!(
            "(deny network-outbound (remote tcp \"localhost:{port}\"))"
        ));
    }
    if !network {
        profile.push_str("(deny network*)");
    }
    profile
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn os_command(
    _cwd: &Path,
    _writable: &[&Path],
    _protected: &[&Path],
    _command: &str,
    _network: bool,
) -> Result<Command, ToolError> {
    Err(ToolError::Io(
        "the OS sandbox tier is not available on this platform yet; set isolation.tier = \
         \"container\" or \"policy\""
            .into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn container_commands_mount_only_the_workspace() {
        let args = container_args(
            &[Path::new("/home/me/project")],
            &[],
            "ubuntu:24.04",
            false,
            "make test",
            false,
        );
        assert_eq!(
            args.join(" "),
            "run --rm -i --network none -v /home/me/project:/workspace -w /workspace ubuntu:24.04 sh -c make test"
        );
        let networked = container_args(&[Path::new("/p")], &[], "img", true, "x", false);
        assert!(!networked.contains(&"--network".to_string()));
        let both = container_args(
            &[Path::new("/p"), Path::new("/q")],
            &[],
            "img",
            true,
            "x",
            false,
        )
        .join(" ");
        assert!(
            both.contains("-v /p:/workspace -v /q:/folders/2 -w /workspace"),
            "{both}"
        );
    }

    #[test]
    fn a_read_only_mount_is_mounted_read_only_in_a_container() {
        let grants = [GrantedPath {
            path: "/home/me/.cargo".into(),
            writable: true,
        }];
        let args = container_args(
            &[Path::new("/p"), Path::new("/q")],
            &grants,
            "img",
            true,
            "x",
            true,
        )
        .join(" ");
        assert!(
            args.contains("-v /p:/workspace:ro -v /q:/folders/2:ro"),
            "{args}"
        );
        // A writable grant (the scratch folder) stays writable.
        assert!(
            args.contains("-v /home/me/.cargo:/home/me/.cargo -w"),
            "{args}"
        );
    }

    #[test]
    fn a_read_only_mount_is_not_writable_in_the_os_sandbox() {
        let scratch = vec!["/data/scratch".to_string()];
        let protected = vec!["/home/me/project".to_string()];
        let args = bwrap_args(
            &scratch,
            &protected,
            &Guards::default(),
            &[],
            "/home/me/project",
            false,
        )
        .join(" ");
        assert!(args.starts_with("--ro-bind / /"), "{args}");
        assert!(!args.contains(" --bind /home/me/project"), "{args}");
        assert!(
            args.contains("--ro-bind /home/me/project /home/me/project"),
            "{args}"
        );
        assert!(
            args.contains("--bind /data/scratch /data/scratch"),
            "{args}"
        );
        assert!(args.contains("--chdir /home/me/project"), "{args}");
        let profile = seatbelt_profile(&scratch, &protected, &Guards::default(), &[], &[], false);
        assert!(profile.contains("(subpath \"/data/scratch\")"), "{profile}");
        assert!(
            profile.ends_with("(deny file-write* (subpath \"/home/me/project\"))(deny network*)"),
            "{profile}"
        );
    }

    #[test]
    fn a_writable_grant_cannot_reopen_a_read_only_folder() {
        let grants = [
            GrantedPath {
                path: "/p/build".into(),
                writable: true,
            },
            GrantedPath {
                path: "/".into(),
                writable: true,
            },
            GrantedPath {
                path: "/elsewhere".into(),
                writable: true,
            },
        ];
        // No daemon folders registered: what is mounted does not depend on the host running this.
        let args = |read_only: bool| {
            container_args_guarding(
                &[Path::new("/p")],
                &grants,
                "img",
                true,
                "x",
                read_only,
                &Guards::default(),
                &[],
            )
            .join(" ")
        };
        let args_ro = args(true);
        assert!(args_ro.contains("-v /p/build:/p/build:ro"), "{args_ro}");
        assert!(args_ro.contains("-v /:/:ro"), "{args_ro}");
        assert!(args_ro.contains("-v /elsewhere:/elsewhere -w"), "{args_ro}");
        let writable = args(false);
        assert!(writable.contains("-v /p/build:/p/build -v"), "{writable}");
    }

    #[test]
    fn a_grant_of_the_whole_host_is_covered_where_the_daemons_files_are() {
        let grants = [GrantedPath {
            path: "/".into(),
            writable: false,
        }];
        let run = |guards: &Guards, hidden: &[Mask]| {
            container_args_guarding(
                &[Path::new("/p")],
                &grants,
                "img",
                true,
                "x",
                false,
                guards,
                hidden,
            )
            .join(" ")
        };
        // The credential files alone: a file by /dev/null, folders by tmpfs, at their host paths.
        let hidden = daemon_hidden();
        let args = run(&Guards::default(), &hidden);
        assert!(args.contains("-v /:/:ro"), "{args}");
        assert!(
            args.contains("-v /dev/null:/home/me/.local/share/imperium/daemon.json:ro"),
            "{args}"
        );
        assert!(
            args.contains("--tmpfs /home/me/.local/share/imperium/ipc"),
            "{args}"
        );
        assert!(
            args.contains("--tmpfs /home/me/.local/share/imperium/remotes"),
            "{args}"
        );
        // With the guarded folders, the data folder is covered whole, and the files in it with it.
        let args = run(&home_guards(), &hidden);
        assert!(
            args.contains("--tmpfs /home/me/.local/share/imperium -"),
            "{args}"
        );
        assert!(args.contains("--tmpfs /home/me/.config/imperium"), "{args}");
        assert!(!args.contains("daemon.json"), "{args}");
        // The runtime folder of the host is covered only where a mount contains it.
        let runtime = [Mask::dir("/run/user/1000")];
        assert!(run(&Guards::default(), &runtime).contains("--tmpfs /run/user/1000"));
        let none = container_args_guarding(
            &[Path::new("/p")],
            &[],
            "img",
            true,
            "x",
            false,
            &Guards::default(),
            &runtime,
        )
        .join(" ");
        assert!(!none.contains("/run/user"), "{none}");
    }

    #[test]
    fn tier_0_refuses_commands_in_a_read_only_mount() {
        let error = Sandbox::Policy
            .named_command(&[Path::new("/p")], &[], "ls", None, true)
            .unwrap_err();
        assert!(error.to_string().contains("read-only mount"), "{error}");
        assert!(
            Sandbox::Policy
                .named_command(&[Path::new("/p")], &[], "ls", None, false)
                .is_ok()
        );
    }

    #[test]
    fn containers_mount_grants_read_only_or_writable() {
        let grants = [
            GrantedPath {
                path: "/home/me/.cargo".into(),
                writable: true,
            },
            GrantedPath {
                path: "/home/me/.rustup".into(),
                writable: false,
            },
            GrantedPath {
                path: r"C:\Users\me\.npm".into(),
                writable: false,
            },
        ];
        let args = container_args(&[Path::new("/p")], &grants, "img", true, "x", false).join(" ");
        assert!(
            args.contains("-v /home/me/.cargo:/home/me/.cargo -v"),
            "{args}"
        );
        assert!(
            args.contains("-v /home/me/.rustup:/home/me/.rustup:ro"),
            "{args}"
        );
        assert!(args.contains(r"-v C:\Users\me\.npm:/grants/3:ro"), "{args}");
    }

    #[test]
    fn os_sandboxes_write_only_inside_the_workspace() {
        let roots = ["/home/me/project".to_string(), "/home/me/docs".to_string()];
        let args = bwrap_args(&roots, &[], &Guards::default(), &[], &roots[0], false).join(" ");
        assert!(args.starts_with("--ro-bind / /"), "{args}");
        assert!(
            args.contains("--bind /home/me/project /home/me/project"),
            "{args}"
        );
        assert!(
            args.contains("--bind /home/me/docs /home/me/docs --chdir /home/me/project"),
            "{args}"
        );
        assert!(args.ends_with("--unshare-net"), "{args}");
        let profile = seatbelt_profile(&roots, &[], &Guards::default(), &[], &[], true);
        assert!(profile.contains("(deny file-write*)"));
        assert!(profile.contains("(subpath \"/home/me/project\")"));
        assert!(profile.contains("(subpath \"/home/me/docs\")"));
        assert!(!profile.contains("deny network"));
        assert!(
            seatbelt_profile(&["/p".into()], &[], &Guards::default(), &[], &[], false)
                .ends_with("(deny network*)")
        );
    }

    #[test]
    fn bubblewrap_masks_credentials_after_every_other_mount() {
        let writable = vec!["/home/me".to_string()];
        let hidden = [
            Mask::file("/home/me/.local/share/imperium/daemon.json"),
            Mask::dir("/home/me/.local/share/imperium/ipc"),
        ];
        let args = bwrap_args(
            &writable,
            &[],
            &Guards::default(),
            &hidden,
            "/home/me",
            true,
        )
        .join(" ");
        // The endpoint file is replaced by /dev/null and the socket folder by an empty tmpfs.
        let file = "--ro-bind /dev/null /home/me/.local/share/imperium/daemon.json";
        let dir = "--tmpfs /home/me/.local/share/imperium/ipc";
        assert!(args.contains(file), "{args}");
        assert!(args.contains(dir), "{args}");
        // After the writable folder that contains them, so it cannot bring them back.
        let bound = args.find("--bind /home/me /home/me").unwrap();
        assert!(args.find(file).unwrap() > bound, "{args}");
        assert!(args.find(dir).unwrap() > bound, "{args}");
        assert!(
            args.find(dir).unwrap() < args.find("--chdir").unwrap(),
            "{args}"
        );
        // Nothing is masked when nothing is registered, and the network stays as configured.
        let plain =
            bwrap_args(&writable, &[], &Guards::default(), &[], "/home/me", false).join(" ");
        assert!(
            !plain.contains("/dev/null") && !plain.contains("--tmpfs /home"),
            "{plain}"
        );
    }

    #[test]
    fn seatbelt_denies_reading_credentials_and_reaching_the_daemon() {
        let hidden = [
            Mask::file("/Users/me/Library/Application Support/Imperium/data/daemon.json"),
            Mask::dir("/Users/me/Library/Application Support/Imperium/data/ipc"),
        ];
        let profile = seatbelt_profile(
            &["/p".into()],
            &[],
            &Guards::default(),
            &hidden,
            &[7878, 9000],
            true,
        );
        assert!(
            profile.contains(
                "(deny file-read* (literal \"/Users/me/Library/Application Support/Imperium/data/daemon.json\"))"
            ),
            "{profile}"
        );
        assert!(
            profile.contains(
                "(deny file-read* (subpath \"/Users/me/Library/Application Support/Imperium/data/ipc\"))"
            ),
            "{profile}"
        );
        assert!(
            profile.contains(
                "(deny network-outbound (remote unix-socket (subpath \"/Users/me/Library/Application Support/Imperium/data/ipc\")))"
            ),
            "{profile}"
        );
        assert!(
            profile.contains("(deny network-outbound (remote tcp \"localhost:7878\"))"),
            "{profile}"
        );
        assert!(
            profile.contains("(deny network-outbound (remote tcp \"localhost:9000\"))"),
            "{profile}"
        );
        // Denied after the broad allow, and the network stays open for everything else.
        assert!(
            profile.find("(allow default)").unwrap() < profile.find("deny file-read*").unwrap()
        );
        assert!(!profile.contains("(deny network*)"), "{profile}");
    }

    #[test]
    fn only_paths_that_exist_are_masked_and_resolved_names_are_added() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("daemon.json");
        std::fs::write(&file, "{}").unwrap();
        let folder = dir.path().join("ipc");
        std::fs::create_dir(&folder).unwrap();
        let missing = dir.path().join("secrets.key");
        let masks = masks_for(&[file.clone(), folder.clone(), missing.clone()]);
        let file_mask = masks.iter().find(|mask| !mask.dir).expect("a file mask");
        let dir_mask = masks.iter().find(|mask| mask.dir).expect("a folder mask");
        assert!(file_mask.path.ends_with("daemon.json"), "{masks:?}");
        assert!(dir_mask.path.ends_with("ipc"), "{masks:?}");
        assert!(
            masks.iter().all(|mask| !mask.path.ends_with("secrets.key")),
            "{masks:?}"
        );
    }

    #[test]
    fn a_command_inherits_nothing_that_names_the_daemon() {
        let names = [
            "PATH",
            "HOME",
            "IMPERIUM_HOME",
            "IMPERIUM_SECRETS_PASSPHRASE_FILE",
        ]
        .into_iter()
        .chain([
            "DBUS_SESSION_BUS_ADDRESS",
            "IMPERIUM_DAEMON__PORT",
            "NOT_IMPERIUM_X",
        ])
        .map(OsString::from);
        let removed: Vec<String> = daemon_env_names(names)
            .into_iter()
            .map(|name| name.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            removed,
            [
                "IMPERIUM_HOME",
                "IMPERIUM_SECRETS_PASSPHRASE_FILE",
                "DBUS_SESSION_BUS_ADDRESS",
                "IMPERIUM_DAEMON__PORT"
            ]
        );
    }

    fn home_guards() -> Guards {
        Guards {
            folders: vec![
                PathBuf::from("/home/me/.config/imperium"),
                PathBuf::from("/home/me/.local/share/imperium"),
            ],
            open: vec![PathBuf::from("/home/me/.local/share/imperium/workspaces")],
        }
    }

    #[test]
    fn a_workspace_of_home_leaves_config_and_data_read_only_but_not_its_own_workspace() {
        // The session's folder is the home folder, and the default sessions workspace (a
        // writable path) is inside `<data>`.
        let workspace = "/home/me/.local/share/imperium/workspaces/p";
        let writable = vec!["/home/me".to_string(), workspace.to_string()];
        let hidden = [Mask::file("/home/me/.local/share/imperium/daemon.json")];
        let args = bwrap_args(&writable, &[], &home_guards(), &hidden, "/home/me", true).join(" ");
        let home = args.find("--bind /home/me /home/me").unwrap();
        let config = args
            .find("--ro-bind /home/me/.config/imperium /home/me/.config/imperium")
            .unwrap();
        let data = args
            .find("--ro-bind /home/me/.local/share/imperium /home/me/.local/share/imperium")
            .unwrap();
        let inner = args
            .rfind(&format!("--bind {workspace} {workspace}"))
            .unwrap();
        let mask = args.find("--ro-bind /dev/null").unwrap();
        // Read-only after the folder that contains them, the workspace inside re-bound writable
        // after all of them, the credential masks last.
        assert!(home < config && home < data, "{args}");
        assert!(config < inner && data < inner && inner < mask, "{args}");
        assert_eq!(
            args[config..].matches("--bind ").count(),
            1,
            "only the workspace is re-bound: {args}"
        );
    }

    #[test]
    fn only_the_workspaces_root_stays_writable_inside_a_guarded_folder() {
        // A grant in `<config>`, and folders the daemon acts on in `<data>`, stay read-only.
        let writable = vec![
            "/home/me".to_string(),
            "/home/me/.config/imperium/sub".to_string(),
            "/home/me/.local/share/imperium/sessions/s".to_string(),
            "/home/me/.local/share/imperium/packs".to_string(),
            "/home/me/.local/share/imperium/workspaces/env/sessions/1".to_string(),
        ];
        let args = bwrap_args(&writable, &[], &home_guards(), &[], "/home/me", true).join(" ");
        let after = &args[args.find("--ro-bind /home/me/.local").unwrap()..];
        assert!(
            after.contains("--bind /home/me/.local/share/imperium/workspaces/env/sessions/1 "),
            "{args}"
        );
        for denied in [
            "/home/me/.config/imperium/sub",
            "/home/me/.local/share/imperium/sessions/s",
            "/home/me/.local/share/imperium/packs",
        ] {
            assert!(
                !after.contains(&format!("--bind {denied} ")),
                "{denied} was re-bound: {args}"
            );
        }
        let profile = seatbelt_profile(&writable, &[], &home_guards(), &[], &[], true);
        assert!(
            profile.contains("(allow file-write* (subpath \"/home/me/.local/share/imperium/workspaces/env/sessions/1\"))"),
            "{profile}"
        );
        assert!(
            !profile.contains("(allow file-write* (subpath \"/home/me/.config/imperium/sub\"))"),
            "{profile}"
        );
        assert!(
            !profile
                .contains("(allow file-write* (subpath \"/home/me/.local/share/imperium/packs\"))"),
            "{profile}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_linked_home_keeps_the_workspace_writable_under_both_names() {
        // /home -> /var/home: the guard is registered under one name, the workspace is given
        // under the other.
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        let workspace = real.join("data/workspaces/p");
        std::fs::create_dir_all(&workspace).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let via_link = link.join("data/workspaces/p");
        let writable = writable_names(&[via_link.as_path()]);
        let guards = Guards {
            folders: names_of(&real.join("data")),
            open: names_of(&real.join("data/workspaces")),
        };
        let args = bwrap_args(&shown(&writable), &[], &guards, &[], "/", true).join(" ");
        let resolved = std::fs::canonicalize(&workspace).unwrap();
        let resolved = resolved.display().to_string();
        let again = args
            .rfind(&format!("--bind {resolved} {resolved}"))
            .unwrap();
        let last_guard = args.rfind("--ro-bind ").unwrap();
        assert!(last_guard < again, "{args}");
        // And the name the workspace was given under is re-bound too, when it matches a guard.
        let guards = Guards {
            folders: names_of(&link.join("data")),
            open: names_of(&link.join("data/workspaces")),
        };
        let args = bwrap_args(&shown(&writable), &[], &guards, &[], "/", true).join(" ");
        let named = via_link.display().to_string();
        assert!(args.contains(&format!("--bind {named} {named}")), "{args}");
        assert!(
            args.contains(&format!("--bind {resolved} {resolved}")),
            "{args}"
        );
    }

    #[test]
    fn seatbelt_denies_writes_under_guarded_folders_except_the_workspace_inside() {
        let writable = vec![
            "/Users/me".to_string(),
            "/Users/me/data/workspaces/p".to_string(),
        ];
        let guards = Guards {
            folders: vec![PathBuf::from("/Users/me/data")],
            open: vec![PathBuf::from("/Users/me/data/workspaces")],
        };
        let profile = seatbelt_profile(&writable, &[], &guards, &[], &[], true);
        let deny = profile
            .find("(deny file-write* (subpath \"/Users/me/data\"))")
            .expect(&profile);
        let allow = profile
            .find("(allow file-write* (subpath \"/Users/me/data/workspaces/p\"))")
            .expect(&profile);
        assert!(deny < allow, "{profile}");
    }

    #[test]
    fn seatbelt_also_denies_writing_linking_and_renaming_away_a_hidden_path() {
        let hidden = [
            Mask::file("/Users/me/data/daemon.json"),
            Mask::dir("/Users/me/data/ipc"),
        ];
        let profile = seatbelt_profile(
            &["/Users/me".into()],
            &[],
            &Guards::default(),
            &hidden,
            &[],
            true,
        );
        for filter in [
            "(literal \"/Users/me/data/daemon.json\")",
            "(subpath \"/Users/me/data/ipc\")",
        ] {
            for operation in ["file-read*", "file-write*", "file-link"] {
                assert!(
                    profile.contains(&format!("(deny {operation} {filter})")),
                    "{operation} {filter}: {profile}"
                );
            }
        }
        // The folders around them cannot be renamed away, but the root cannot be named.
        assert!(
            profile.contains("(deny file-write-unlink (literal \"/Users/me/data\"))"),
            "{profile}"
        );
        assert!(
            profile.contains("(deny file-write-unlink (literal \"/Users/me\"))"),
            "{profile}"
        );
        assert!(!profile.contains("(literal \"/\"))"), "{profile}");
    }

    fn container(roots: &[&str], grants: &[(&str, bool)], hidden: &[Mask]) -> String {
        let roots: Vec<&Path> = roots.iter().map(Path::new).collect();
        let grants: Vec<GrantedPath> = grants
            .iter()
            .map(|(path, writable)| GrantedPath {
                path: (*path).into(),
                writable: *writable,
            })
            .collect();
        container_args_guarding(
            &roots,
            &grants,
            "img",
            true,
            "x",
            false,
            &home_guards(),
            hidden,
        )
        .join(" ")
    }

    fn daemon_hidden() -> Vec<Mask> {
        vec![
            Mask::file("/home/me/.local/share/imperium/daemon.json"),
            Mask::dir("/home/me/.local/share/imperium/ipc"),
            Mask::dir("/home/me/.local/share/imperium/remotes"),
        ]
    }

    #[test]
    fn a_container_never_mounts_the_daemons_folders_inside_a_mounted_one() {
        let args = container(&["/home/me"], &[], &daemon_hidden());
        assert!(args.contains("-v /home/me:/workspace -"), "{args}");
        assert!(
            args.contains("--tmpfs /workspace/.config/imperium"),
            "{args}"
        );
        assert!(
            args.contains("--tmpfs /workspace/.local/share/imperium"),
            "{args}"
        );
        // The hidden paths are inside the covered folder: no second mount of them.
        assert!(!args.contains("daemon.json"), "{args}");
        assert!(!args.contains("/ipc"), "{args}");
        // The default workspace is inside `<data>`: nothing around it is mounted, so no cover.
        let inside = container(
            &["/home/me/.local/share/imperium/workspaces/p"],
            &[],
            &daemon_hidden(),
        );
        assert!(!inside.contains("--tmpfs"), "{inside}");
        assert!(
            inside.contains("-v /home/me/.local/share/imperium/workspaces/p:/workspace -"),
            "{inside}"
        );
    }

    #[test]
    fn a_mount_that_is_the_data_folder_hides_the_credentials_in_it() {
        let data = "/home/me/.local/share/imperium";
        let args = container(&["/p"], &[(data, true)], &daemon_hidden());
        assert!(args.contains(&format!("-v {data}:{data}:ro")), "{args}");
        assert!(
            args.contains(&format!("-v /dev/null:{data}/daemon.json:ro")),
            "{args}"
        );
        assert!(args.contains(&format!("--tmpfs {data}/ipc")), "{args}");
        assert!(args.contains(&format!("--tmpfs {data}/remotes")), "{args}");
        // A folder in `<data>` the daemon acts on, and one in `<config>`, are read-only.
        for inside in [
            "/home/me/.local/share/imperium/sessions",
            "/home/me/.config/imperium/sub",
        ] {
            let args = container(&["/p"], &[(inside, true)], &daemon_hidden());
            assert!(args.contains(&format!("-v {inside}:{inside}:ro")), "{args}");
        }
        // A grant under the workspaces root stays writable.
        let workspace = "/home/me/.local/share/imperium/workspaces/env/sessions/1";
        let args = container(&["/p"], &[(workspace, true)], &daemon_hidden());
        assert!(
            args.contains(&format!("-v {workspace}:{workspace} -")),
            "{args}"
        );
    }

    #[test]
    fn a_grant_inside_a_hidden_folder_is_an_empty_folder_and_no_mount_point_repeats() {
        let remotes = "/home/me/.local/share/imperium/remotes";
        // The grant is the hidden folder, and the mount around it would cover it again.
        let args = container(&["/home/me"], &[(remotes, true)], &daemon_hidden());
        assert!(!args.contains(&format!("-v {remotes}")), "{args}");
        assert_eq!(
            args.matches(&format!("--tmpfs {remotes}")).count(),
            1,
            "{args}"
        );
        // A grant equal to a guard, with a mount around it: every target is used once.
        let config = "/home/me/.config/imperium";
        let args = container(&["/home/me"], &[(config, true)], &daemon_hidden());
        let targets: Vec<&str> = args
            .split(' ')
            .filter(|word| word.starts_with("/workspace") || word.starts_with(config))
            .collect();
        let mut unique = targets.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(targets.len(), unique.len(), "{args}");
    }

    /// Runs only when `IMPERIUM_TEST_CONTAINER_IMAGE` names an image already present locally: a
    /// real container starts, though a grant equals a guarded folder inside the workspace (a
    /// duplicate mount point would be refused), and sees none of the daemon's files.
    #[tokio::test]
    async fn a_real_container_starts_and_hides_the_daemons_files() {
        let Ok(image) = std::env::var("IMPERIUM_TEST_CONTAINER_IMAGE") else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        std::fs::create_dir_all(data.join("ipc")).unwrap();
        std::fs::write(data.join("daemon.json"), "TOKEN-THE-CONTAINER-MUST-NOT-SEE").unwrap();
        std::fs::write(dir.path().join("inside.txt"), "visible").unwrap();
        guard_from_commands([data.clone()]);
        hide_from_os_sandbox([data.join("daemon.json"), data.join("ipc")]);
        let sandbox = Sandbox::Container {
            runtime: "docker".into(),
            image,
            network: false,
        };
        let grants = [GrantedPath {
            path: data.clone(),
            writable: true,
        }];
        let output = sandbox
            .command(
                &[dir.path()],
                &grants,
                "cat inside.txt; cat data/daemon.json; ls data/ipc; echo started",
            )
            .unwrap()
            .output()
            .await
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("visible") && stdout.contains("started"),
            "{stdout}"
        );
        assert!(
            !stdout.contains("TOKEN-THE-CONTAINER-MUST-NOT-SEE"),
            "{stdout}"
        );
    }

    #[test]
    fn labels_name_the_tier() {
        assert_eq!(Sandbox::Policy.label(), "tier 0 · folder policy");
        assert_eq!(
            Sandbox::Os { network: false }.label(),
            "tier 1 · OS sandbox, no network"
        );
        let container = Sandbox::Container {
            runtime: "docker".into(),
            image: "ubuntu:24.04".into(),
            network: true,
        };
        assert_eq!(container.label(), "tier 2 · container ubuntu:24.04");
    }

    /// Runs only when `IMPERIUM_TEST_CONTAINER_IMAGE` names an image already present locally.
    #[tokio::test]
    async fn a_container_sees_the_workspace_and_nothing_else() {
        let Ok(image) = std::env::var("IMPERIUM_TEST_CONTAINER_IMAGE") else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("inside.txt"), "visible").unwrap();
        let sandbox = Sandbox::Container {
            runtime: "docker".into(),
            image,
            network: false,
        };
        let output = sandbox
            .command(
                &[dir.path()],
                &[],
                "cat inside.txt && echo made > out.txt && pwd",
            )
            .unwrap()
            .output()
            .await
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("visible") && stdout.contains("/workspace"),
            "{stdout}"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("out.txt"))
                .unwrap()
                .trim(),
            "made"
        );
    }
}
