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

    /// The process that runs `command` in `root` under this tier.
    pub fn command(&self, root: &Path, command: &str) -> Result<Command, ToolError> {
        match self {
            Sandbox::Policy => {
                let mut process = shell(command);
                process.current_dir(root);
                Ok(process)
            }
            Sandbox::Os { network } => os_command(root, command, *network),
            Sandbox::Container {
                runtime,
                image,
                network,
            } => {
                let mut process = Command::new(runtime);
                process.args(container_args(root, image, *network, command));
                Ok(process)
            }
        }
    }
}

fn container_args(root: &Path, image: &str, network: bool, command: &str) -> Vec<String> {
    let mut args = vec!["run".into(), "--rm".into(), "-i".into()];
    if !network {
        args.extend(["--network".into(), "none".into()]);
    }
    args.extend([
        "-v".into(),
        format!("{}:/workspace", root.display()),
        "-w".into(),
        "/workspace".into(),
        image.into(),
        "sh".into(),
        "-c".into(),
        command.into(),
    ]);
    args
}

#[cfg(target_os = "linux")]
fn os_command(root: &Path, command: &str, network: bool) -> Result<Command, ToolError> {
    let root = root.display().to_string();
    let mut process = Command::new("bwrap");
    process
        .args(bwrap_args(&root, network))
        .args(["sh", "-c", command]);
    process.current_dir(&root);
    Ok(process)
}

/// bubblewrap: the whole filesystem read-only, the workspace and a private /tmp writable, its
/// own process namespace, and optionally no network.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn bwrap_args(root: &str, network: bool) -> Vec<String> {
    let mut args: Vec<String> = [
        "--ro-bind",
        "/",
        "/",
        "--dev",
        "/dev",
        "--proc",
        "/proc",
        "--tmpfs",
        "/tmp",
        "--bind",
        root,
        root,
        "--chdir",
        root,
        "--unshare-pid",
        "--die-with-parent",
    ]
    .iter()
    .map(|arg| arg.to_string())
    .collect();
    if !network {
        args.push("--unshare-net".into());
    }
    args
}

#[cfg(target_os = "macos")]
fn os_command(root: &Path, command: &str, network: bool) -> Result<Command, ToolError> {
    let mut process = Command::new("sandbox-exec");
    process
        .args([
            "-p",
            &seatbelt_profile(&root.display().to_string(), network),
        ])
        .args(["sh", "-c", command]);
    process.current_dir(root);
    Ok(process)
}

/// A Seatbelt profile allowing everything but writes outside the workspace and temporary
/// folders, and optionally network access.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn seatbelt_profile(root: &str, network: bool) -> String {
    let quoted = root.replace('\\', "\\\\").replace('"', "\\\"");
    let mut profile = format!(
        "(version 1)(allow default)(deny file-write*)\
         (allow file-write* (subpath \"{quoted}\") (subpath \"/private/tmp\") \
         (subpath \"/private/var/folders\") (literal \"/dev/null\") (literal \"/dev/tty\"))"
    );
    if !network {
        profile.push_str("(deny network*)");
    }
    profile
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn os_command(_root: &Path, _command: &str, _network: bool) -> Result<Command, ToolError> {
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
            Path::new("/home/me/project"),
            "ubuntu:24.04",
            false,
            "make test",
        );
        assert_eq!(
            args.join(" "),
            "run --rm -i --network none -v /home/me/project:/workspace -w /workspace ubuntu:24.04 sh -c make test"
        );
        let networked = container_args(Path::new("/p"), "img", true, "x");
        assert!(!networked.contains(&"--network".to_string()));
    }

    #[test]
    fn os_sandboxes_write_only_inside_the_workspace() {
        let args = bwrap_args("/home/me/project", false).join(" ");
        assert!(args.starts_with("--ro-bind / /"), "{args}");
        assert!(
            args.contains("--bind /home/me/project /home/me/project"),
            "{args}"
        );
        assert!(args.ends_with("--unshare-net"), "{args}");
        let profile = seatbelt_profile("/Users/me/project", true);
        assert!(profile.contains("(deny file-write*)"));
        assert!(profile.contains("(subpath \"/Users/me/project\")"));
        assert!(!profile.contains("deny network"));
        assert!(seatbelt_profile("/p", false).ends_with("(deny network*)"));
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
            .command(dir.path(), "cat inside.txt && echo made > out.txt && pwd")
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
