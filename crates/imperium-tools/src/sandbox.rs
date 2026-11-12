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

use std::path::Path;

use tokio::process::Command;

use crate::GrantedPath;

use crate::ToolError;
use crate::exec::shell;

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
                os_command(root, &writable, command, *network)
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
                process.args(args);
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
        let mode = if read_only { ":ro" } else { "" };
        args.extend(["-v".into(), format!("{}:{target}{mode}", root.display())]);
    }
    for (index, grant) in grants.iter().enumerate() {
        let source = grant.display_path();
        let target = if source.starts_with('/') {
            source.clone()
        } else {
            format!("/grants/{}", index + 1)
        };
        let mode = if grant.writable { "" } else { ":ro" };
        args.extend(["-v".into(), format!("{source}:{target}{mode}")]);
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

#[cfg_attr(not(any(target_os = "linux", target_os = "macos")), allow(dead_code))]
fn displayed(roots: &[&Path]) -> Vec<String> {
    roots
        .iter()
        .map(|root| root.display().to_string())
        .collect()
}

/// Runs `command` in `cwd` with writes allowed only in `writable` (and temporary files).
#[cfg(target_os = "linux")]
fn os_command(
    cwd: &Path,
    writable: &[&Path],
    command: &str,
    network: bool,
) -> Result<Command, ToolError> {
    let mut process = Command::new("bwrap");
    process
        .args(bwrap_args(
            &displayed(writable),
            &cwd.display().to_string(),
            network,
        ))
        .args(["sh", "-c", command]);
    process.current_dir(cwd);
    Ok(process)
}

/// bubblewrap: the whole filesystem read-only, the `writable` paths and a private /tmp
/// writable, its own process namespace, and optionally no network. Runs in `cwd`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn bwrap_args(writable: &[String], cwd: &str, network: bool) -> Vec<String> {
    let mut args: Vec<String> = ["--ro-bind", "/", "/", "--dev", "/dev", "--proc", "/proc"]
        .iter()
        .chain(&["--tmpfs", "/tmp"])
        .map(|arg| arg.to_string())
        .collect();
    for path in writable {
        args.extend(["--bind".into(), path.clone(), path.clone()]);
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
    command: &str,
    network: bool,
) -> Result<Command, ToolError> {
    let mut process = Command::new("sandbox-exec");
    process
        .args(["-p", &seatbelt_profile(&displayed(writable), network)])
        .args(["sh", "-c", command]);
    process.current_dir(cwd);
    Ok(process)
}

/// A Seatbelt profile allowing everything but writes outside the `writable` paths and
/// temporary folders, and optionally network access.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn seatbelt_profile(writable: &[String], network: bool) -> String {
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
    if !network {
        profile.push_str("(deny network*)");
    }
    profile
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn os_command(
    _cwd: &Path,
    _writable: &[&Path],
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
        let args = bwrap_args(&scratch, "/home/me/project", false).join(" ");
        assert!(args.starts_with("--ro-bind / /"), "{args}");
        assert!(!args.contains("--bind /home/me/project"), "{args}");
        assert!(
            args.contains("--bind /data/scratch /data/scratch"),
            "{args}"
        );
        assert!(args.contains("--chdir /home/me/project"), "{args}");
        let profile = seatbelt_profile(&scratch, false);
        assert!(!profile.contains("/home/me/project"), "{profile}");
        assert!(profile.contains("(subpath \"/data/scratch\")"), "{profile}");
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
        let args = bwrap_args(&roots, &roots[0], false).join(" ");
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
        let profile = seatbelt_profile(&roots, true);
        assert!(profile.contains("(deny file-write*)"));
        assert!(profile.contains("(subpath \"/home/me/project\")"));
        assert!(profile.contains("(subpath \"/home/me/docs\")"));
        assert!(!profile.contains("deny network"));
        assert!(seatbelt_profile(&["/p".into()], false).ends_with("(deny network*)"));
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
