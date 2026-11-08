//! Isolation tiers for commands agents run. Tier 0 (policy) confines only the file tools; the
//! OS tier runs commands in an OS sandbox that can write only inside the workspace (bubblewrap
//! on Linux, Seatbelt on macOS); the container tier runs them in a container with only the
//! workspace mounted. A tier that is not available fails the command rather than silently
//! running it with less isolation.

use std::path::Path;

use tokio::process::Command;

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
    /// folders). The OS sandbox lets it write in every folder; the container mounts them all.
    pub fn command(&self, roots: &[&Path], command: &str) -> Result<Command, ToolError> {
        let Some(root) = roots.first() else {
            return Err(ToolError::Io("the workspace has no folder".into()));
        };
        match self {
            Sandbox::Policy => {
                let mut process = shell(command);
                process.current_dir(root);
                Ok(process)
            }
            Sandbox::Os { network } => os_command(roots, command, *network),
            Sandbox::Container {
                runtime,
                image,
                network,
            } => {
                let mut process = Command::new(runtime);
                process.args(container_args(roots, image, *network, command));
                Ok(process)
            }
        }
    }
}

/// The first folder is mounted at `/workspace` (the working directory) and the others at
/// `/folders/2`, `/folders/3`, … in order.
fn container_args(roots: &[&Path], image: &str, network: bool, command: &str) -> Vec<String> {
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
        args.extend(["-v".into(), format!("{}:{target}", root.display())]);
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

#[cfg(target_os = "linux")]
fn os_command(roots: &[&Path], command: &str, network: bool) -> Result<Command, ToolError> {
    let roots = displayed(roots);
    let mut process = Command::new("bwrap");
    process
        .args(bwrap_args(&roots, network))
        .args(["sh", "-c", command]);
    process.current_dir(&roots[0]);
    Ok(process)
}

/// bubblewrap: the whole filesystem read-only, the workspace's folders and a private /tmp
/// writable, its own process namespace, and optionally no network. Runs in the first folder.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn bwrap_args(roots: &[String], network: bool) -> Vec<String> {
    let mut args: Vec<String> = ["--ro-bind", "/", "/", "--dev", "/dev", "--proc", "/proc"]
        .iter()
        .chain(&["--tmpfs", "/tmp"])
        .map(|arg| arg.to_string())
        .collect();
    for root in roots {
        args.extend(["--bind".into(), root.clone(), root.clone()]);
    }
    if let Some(first) = roots.first() {
        args.extend(["--chdir".into(), first.clone()]);
    }
    args.extend(["--unshare-pid".into(), "--die-with-parent".into()]);
    if !network {
        args.push("--unshare-net".into());
    }
    args
}

#[cfg(target_os = "macos")]
fn os_command(roots: &[&Path], command: &str, network: bool) -> Result<Command, ToolError> {
    let mut process = Command::new("sandbox-exec");
    process
        .args(["-p", &seatbelt_profile(&displayed(roots), network)])
        .args(["sh", "-c", command]);
    process.current_dir(roots[0]);
    Ok(process)
}

/// A Seatbelt profile allowing everything but writes outside the workspace's folders and
/// temporary folders, and optionally network access.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn seatbelt_profile(roots: &[String], network: bool) -> String {
    let folders: String = roots
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
fn os_command(_roots: &[&Path], _command: &str, _network: bool) -> Result<Command, ToolError> {
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
            "ubuntu:24.04",
            false,
            "make test",
        );
        assert_eq!(
            args.join(" "),
            "run --rm -i --network none -v /home/me/project:/workspace -w /workspace ubuntu:24.04 sh -c make test"
        );
        let networked = container_args(&[Path::new("/p")], "img", true, "x");
        assert!(!networked.contains(&"--network".to_string()));
        let both = container_args(&[Path::new("/p"), Path::new("/q")], "img", true, "x").join(" ");
        assert!(
            both.contains("-v /p:/workspace -v /q:/folders/2 -w /workspace"),
            "{both}"
        );
    }

    #[test]
    fn os_sandboxes_write_only_inside_the_workspace() {
        let roots = ["/home/me/project".to_string(), "/home/me/docs".to_string()];
        let args = bwrap_args(&roots, false).join(" ");
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
