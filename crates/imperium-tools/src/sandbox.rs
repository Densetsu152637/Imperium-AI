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
    loopback_ports: Vec<u16>,
}

static HIDDEN: Mutex<Hidden> = Mutex::new(Hidden {
    paths: Vec::new(),
    guarded: Vec::new(),
    loopback_ports: Vec::new(),
});

/// Registers folders (the daemon's configuration and data folders) that commands in the OS and
/// container tiers must not write, even when a session folder or a writable grant contains them
/// (a workspace of `~`): the OS tier mounts them read-only, the container tier mounts an empty
/// folder over them. A session folder inside one (the default workspace is inside `<data>`)
/// stays writable.
pub fn guard_from_commands(paths: impl IntoIterator<Item = PathBuf>) {
    let mut hidden = HIDDEN.lock().unwrap_or_else(PoisonError::into_inner);
    for path in paths {
        if !hidden.guarded.contains(&path) {
            hidden.guarded.push(path);
        }
    }
}

/// The guarded folders that exist now, each also under its resolved name (Unix).
fn guarded_paths() -> Vec<PathBuf> {
    let registered = HIDDEN
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .guarded
        .clone();
    let mut guarded: Vec<PathBuf> = Vec::new();
    for path in registered {
        if !path.is_dir() {
            continue;
        }
        let mut names = vec![path.clone()];
        #[cfg(unix)]
        {
            if let Ok(resolved) = std::fs::canonicalize(&path) {
                names.push(resolved);
            }
        }
        for name in names {
            if !guarded.contains(&name) {
                guarded.push(name);
            }
        }
    }
    guarded
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
        &guarded_paths(),
    )
}

/// Like [`container_args`]; a `guarded` folder that a mounted folder or grant contains is covered
/// by an empty tmpfs at its path in the container (never mounted), and one that is itself mounted
/// is mounted read-only. A mounted folder inside a guarded one (the default workspace is inside
/// `<data>`) is unaffected: nothing around it is mounted.
fn container_args_guarding(
    roots: &[&Path],
    grants: &[GrantedPath],
    image: &str,
    network: bool,
    command: &str,
    read_only: bool,
    guarded: &[PathBuf],
) -> Vec<String> {
    let mut masks: Vec<String> = Vec::new();
    let mut guard_mounts = |source: &Path, target: &str| -> bool {
        let mut is_guarded = false;
        for guard in guarded {
            if source == guard.as_path() {
                is_guarded = true;
            } else if let Ok(relative) = guard.strip_prefix(source) {
                let inside: Vec<String> = relative
                    .components()
                    .map(|part| part.as_os_str().to_string_lossy().into_owned())
                    .collect();
                let at = format!("{}/{}", target.trim_end_matches('/'), inside.join("/"));
                if !masks.contains(&at) {
                    masks.push(at);
                }
            }
        }
        is_guarded
    };
    let mut args = vec!["run".into(), "--rm".into(), "-i".into()];
    if !network {
        args.extend(["--network".into(), "none".into()]);
    }
    for (index, root) in roots.iter().enumerate() {
        let target = if index == 0 {
            "/workspace".to_string()
        } else {
            format!("/folders/{}", index + 1)
        };
        let guarded_root = guard_mounts(*root, &target);
        let mode = if read_only || guarded_root { ":ro" } else { "" };
        args.extend(["-v".into(), format!("{}:{target}{mode}", root.display())]);
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
        let guarded_grant = guard_mounts(&grant.path, &target);
        let mode = if grant.writable && !reopens && !guarded_grant {
            ""
        } else {
            ":ro"
        };
        args.extend(["-v".into(), format!("{source}:{target}{mode}")]);
    }
    for at in masks {
        args.extend(["--tmpfs".into(), at]);
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
fn relevant_guards(writable: &[&Path]) -> Vec<String> {
    guarded_paths()
        .into_iter()
        .filter(|guard| {
            writable
                .iter()
                .any(|path| path.starts_with(guard) || guard.starts_with(path))
        })
        .map(|guard| guard.display().to_string())
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
    process
        .args(bwrap_args(
            &displayed(writable),
            &displayed(protected),
            &relevant_guards(writable),
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
    guarded: &[String],
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
    // The guarded folders are read-only even where a writable path contains them; a writable
    // path inside one (the default workspace is inside `<data>`) is bound writable again.
    for guard in guarded {
        args.extend(["--ro-bind".into(), guard.clone(), guard.clone()]);
        for path in writable {
            if Path::new(path).starts_with(guard) && path != guard {
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
    process
        .args([
            "-p",
            &seatbelt_profile(
                &displayed(writable),
                &displayed(protected),
                &relevant_guards(writable),
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
    guarded: &[String],
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
    // Guarded folders cannot be written even where a writable path contains them; a writable path
    // inside one is allowed again.
    for guard in guarded {
        let quoted = guard.replace('\\', "\\\\").replace('"', "\\\"");
        profile.push_str(&format!("(deny file-write* (subpath \"{quoted}\"))"));
        for path in writable {
            if Path::new(path).starts_with(guard) && path != guard {
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
        let args = bwrap_args(&scratch, &protected, &[], &[], "/home/me/project", false).join(" ");
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
        let profile = seatbelt_profile(&scratch, &protected, &[], &[], &[], false);
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
        let args = container_args(&[Path::new("/p")], &grants, "img", true, "x", true).join(" ");
        assert!(args.contains("-v /p/build:/p/build:ro"), "{args}");
        assert!(args.contains("-v /:/:ro"), "{args}");
        assert!(args.contains("-v /elsewhere:/elsewhere -w"), "{args}");
        let writable =
            container_args(&[Path::new("/p")], &grants, "img", true, "x", false).join(" ");
        assert!(writable.contains("-v /p/build:/p/build -v"), "{writable}");
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
        let args = bwrap_args(&roots, &[], &[], &[], &roots[0], false).join(" ");
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
        let profile = seatbelt_profile(&roots, &[], &[], &[], &[], true);
        assert!(profile.contains("(deny file-write*)"));
        assert!(profile.contains("(subpath \"/home/me/project\")"));
        assert!(profile.contains("(subpath \"/home/me/docs\")"));
        assert!(!profile.contains("deny network"));
        assert!(
            seatbelt_profile(&["/p".into()], &[], &[], &[], &[], false)
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
        let args = bwrap_args(&writable, &[], &[], &hidden, "/home/me", true).join(" ");
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
        let plain = bwrap_args(&writable, &[], &[], &[], "/home/me", false).join(" ");
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
        let profile = seatbelt_profile(&["/p".into()], &[], &[], &hidden, &[7878, 9000], true);
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

    #[test]
    fn a_workspace_of_home_leaves_config_and_data_read_only_but_not_its_own_workspace() {
        // The session's folder is the home folder, and the default sessions workspace (a
        // writable path) is inside `<data>`.
        let writable = vec![
            "/home/me".to_string(),
            "/home/me/.local/share/imperium/workspaces/p".to_string(),
        ];
        let guarded = vec![
            "/home/me/.config/imperium".to_string(),
            "/home/me/.local/share/imperium".to_string(),
        ];
        let hidden = [Mask::file("/home/me/.local/share/imperium/daemon.json")];
        let args = bwrap_args(&writable, &[], &guarded, &hidden, "/home/me", true).join(" ");
        let home = args.find("--bind /home/me /home/me").unwrap();
        let config = args
            .find("--ro-bind /home/me/.config/imperium /home/me/.config/imperium")
            .unwrap();
        let data = args
            .find("--ro-bind /home/me/.local/share/imperium /home/me/.local/share/imperium")
            .unwrap();
        let inner = args
            .rfind("--bind /home/me/.local/share/imperium/workspaces/p /home/me/.local/share/imperium/workspaces/p")
            .unwrap();
        let mask = args.find("--ro-bind /dev/null").unwrap();
        // Read-only after the folder that contains them, the workspace inside re-bound writable
        // after that, the credential masks last.
        assert!(home < config && home < data, "{args}");
        assert!(data < inner && inner < mask, "{args}");
        // The re-bound workspace is the only writable bind after the guard.
        assert_eq!(
            args[data..].matches("--bind ").count(),
            1,
            "only the workspace is re-bound: {args}"
        );
    }

    #[test]
    fn seatbelt_denies_writes_under_guarded_folders_except_the_workspace_inside() {
        let writable = vec![
            "/Users/me".to_string(),
            "/Users/me/Library/Application Support/Imperium/data/workspaces/p".to_string(),
        ];
        let guarded = vec!["/Users/me/Library/Application Support/Imperium/data".to_string()];
        let profile = seatbelt_profile(&writable, &[], &guarded, &[], &[], true);
        let deny = profile
            .find("(deny file-write* (subpath \"/Users/me/Library/Application Support/Imperium/data\"))")
            .expect(&profile);
        let allow = profile
            .find("(allow file-write* (subpath \"/Users/me/Library/Application Support/Imperium/data/workspaces/p\"))")
            .expect(&profile);
        assert!(deny < allow, "{profile}");
    }

    #[test]
    fn seatbelt_also_denies_writing_linking_and_renaming_away_a_hidden_path() {
        let hidden = [
            Mask::file("/Users/me/data/daemon.json"),
            Mask::dir("/Users/me/data/ipc"),
        ];
        let profile = seatbelt_profile(&["/Users/me".into()], &[], &[], &hidden, &[], true);
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

    #[test]
    fn a_container_never_mounts_guarded_folders_inside_a_mounted_one() {
        let guarded = [
            PathBuf::from("/home/me/.config/imperium"),
            PathBuf::from("/home/me/.local/share/imperium"),
        ];
        let args = container_args_guarding(
            &[Path::new("/home/me")],
            &[],
            "img",
            true,
            "x",
            false,
            &guarded,
        )
        .join(" ");
        assert!(args.contains("-v /home/me:/workspace -"), "{args}");
        assert!(
            args.contains("--tmpfs /workspace/.config/imperium"),
            "{args}"
        );
        assert!(
            args.contains("--tmpfs /workspace/.local/share/imperium"),
            "{args}"
        );
        // A grant around them, and one that is a guarded folder itself.
        let grants = [
            GrantedPath {
                path: "/home/me".into(),
                writable: true,
            },
            GrantedPath {
                path: "/home/me/.config/imperium".into(),
                writable: true,
            },
        ];
        let args = container_args_guarding(
            &[Path::new("/p")],
            &grants,
            "img",
            true,
            "x",
            false,
            &guarded,
        )
        .join(" ");
        assert!(args.contains("--tmpfs /home/me/.config/imperium"), "{args}");
        assert!(
            args.contains("-v /home/me/.config/imperium:/home/me/.config/imperium:ro"),
            "{args}"
        );
        // The default workspace is inside `<data>`: nothing around it is mounted, so no mask.
        let inside = container_args_guarding(
            &[Path::new("/home/me/.local/share/imperium/workspaces/p")],
            &[],
            "img",
            true,
            "x",
            false,
            &guarded,
        )
        .join(" ");
        assert!(!inside.contains("--tmpfs"), "{inside}");
        assert!(
            inside.contains("-v /home/me/.local/share/imperium/workspaces/p:/workspace -"),
            "{inside}"
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
