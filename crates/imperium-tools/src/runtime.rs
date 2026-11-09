//! Remote runtimes: environments whose folders live in a running Docker container, a WSL
//! distribution, or a machine reached over SSH. The agent's file and command tools run there
//! through the runtime's own client (`docker exec`, `wsl.exe`, the system `ssh`), each call a
//! short-lived process; the approval layer above is unchanged.
//!
//! Commands are described as a [`ProcessSpec`] (built here, so it can be tested without any
//! of those programs) and run by a [`Runner`]: [`ProcessRunner`] in the daemon, a fake in tests.

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::ToolError;

/// Where a remote environment's tools run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Remote {
    /// A running container, reached with `docker exec`.
    Docker { container: String },
    /// A WSL distribution, reached with `wsl.exe` (Windows only).
    Wsl { distro: String },
    /// A host reached with the system `ssh` client: a saved alias from the SSH config, or
    /// `user@host[:port]`. Keys and agents come from the user's SSH configuration.
    Ssh { host: String },
}

impl Remote {
    /// A short label for the interface, such as `runtime · ssh build-box`.
    pub fn label(&self) -> String {
        match self {
            Remote::Docker { container } => format!("runtime · docker container {container}"),
            Remote::Wsl { distro } => format!("runtime · WSL {distro}"),
            Remote::Ssh { host } => format!("runtime · ssh {host}"),
        }
    }

    /// The process that runs `script` with `sh -c` in `cwd` inside the runtime. Input piped to
    /// it reaches the script (Docker needs `-i` for that).
    pub fn command(&self, cwd: &str, script: &str) -> ProcessSpec {
        match self {
            Remote::Docker { container } => ProcessSpec::new(
                "docker",
                ["exec", "-i", "-w", cwd, container, "sh", "-c", script],
            ),
            // `--exec` runs `sh` directly: after a plain `--`, wsl.exe joins the arguments into
            // one line for the distribution's login shell, which would split the script.
            Remote::Wsl { distro } => ProcessSpec::new(
                "wsl.exe",
                ["-d", distro, "--cd", cwd, "--exec", "sh", "-c", script],
            ),
            Remote::Ssh { host } => {
                let (host, port) = ssh_target(host);
                let mut args: Vec<String> = ["-o", "BatchMode=yes", "-T"]
                    .iter()
                    .map(|arg| arg.to_string())
                    .collect();
                if let Some(port) = port {
                    args.extend(["-p".into(), port]);
                }
                // ssh hands the remote side one command line, run by the login shell.
                let inner = format!("cd {} && {script}", sh_quote(cwd));
                args.extend([host, format!("sh -c {}", sh_quote(&inner))]);
                ProcessSpec {
                    program: "ssh".into(),
                    args,
                }
            }
        }
    }
}

/// A program and its arguments, not yet started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessSpec {
    pub program: String,
    pub args: Vec<String>,
}

impl ProcessSpec {
    pub fn new<S: AsRef<str>>(program: &str, args: impl IntoIterator<Item = S>) -> Self {
        ProcessSpec {
            program: program.into(),
            args: args
                .into_iter()
                .map(|arg| arg.as_ref().to_string())
                .collect(),
        }
    }
}

impl fmt::Display for ProcessSpec {
    /// The command line, for messages and tests (not for a shell: nothing is quoted).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.program)?;
        for arg in &self.args {
            write!(f, " {arg}")?;
        }
        Ok(())
    }
}

/// What a finished process left.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunOutput {
    /// The exit code; `None` when a signal ended it.
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl RunOutput {
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }

    /// Standard error (or output, when that is empty) as one trimmed line of text.
    pub fn message(&self) -> String {
        let stderr = decode(&self.stderr);
        let text = if stderr.trim().is_empty() {
            decode(&self.stdout)
        } else {
            stderr
        };
        text.trim().to_string()
    }
}

/// Text a program wrote: UTF-8, or UTF-16LE as `wsl.exe` writes its own messages.
pub fn decode(bytes: &[u8]) -> String {
    let (pairs, rest) = bytes.as_chunks::<2>();
    if !pairs.is_empty() && rest.is_empty() && pairs.iter().all(|pair| pair[1] == 0) {
        let units: Vec<u16> = pairs.iter().map(|pair| u16::from_le_bytes(*pair)).collect();
        return String::from_utf16_lossy(&units)
            .trim_start_matches('\u{feff}')
            .to_string();
    }
    String::from_utf8_lossy(bytes).into_owned()
}

pub type RunFuture<'a> = Pin<Box<dyn Future<Output = Result<RunOutput, ToolError>> + Send + 'a>>;

/// Runs processes for remote runtimes.
pub trait Runner: Send + Sync + fmt::Debug {
    /// Runs `spec` to completion, writing `stdin` to it when given, and gives up after
    /// `timeout`. Failing to start the program is an error; a non-zero exit is not.
    fn run<'a>(
        &'a self,
        spec: &'a ProcessSpec,
        stdin: Option<&'a [u8]>,
        timeout: Duration,
    ) -> RunFuture<'a>;
}

/// Starts real processes.
#[derive(Debug, Default, Clone, Copy)]
pub struct ProcessRunner;

impl Runner for ProcessRunner {
    fn run<'a>(
        &'a self,
        spec: &'a ProcessSpec,
        stdin: Option<&'a [u8]>,
        timeout: Duration,
    ) -> RunFuture<'a> {
        Box::pin(async move {
            let mut child = Command::new(&spec.program)
                .args(&spec.args)
                .stdin(if stdin.is_some() {
                    Stdio::piped()
                } else {
                    Stdio::null()
                })
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true)
                .spawn()
                .map_err(|error| {
                    ToolError::Io(format!("could not start {}: {error}", spec.program))
                })?;
            if let (Some(bytes), Some(mut pipe)) = (stdin, child.stdin.take()) {
                pipe.write_all(bytes)
                    .await
                    .map_err(|error| ToolError::Io(format!("{}: {error}", spec.program)))?;
                // Closing the pipe ends the input.
                drop(pipe);
            }
            let output = tokio::time::timeout(timeout, child.wait_with_output())
                .await
                .map_err(|_| ToolError::Timeout(timeout.as_secs()))?
                .map_err(|error| ToolError::Io(format!("{}: {error}", spec.program)))?;
            Ok(RunOutput {
                code: output.status.code(),
                stdout: output.stdout,
                stderr: output.stderr,
            })
        })
    }
}

/// Quotes `text` as one word for a POSIX shell.
pub fn sh_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

/// Splits `user@host:port` into the destination and port; an alias or `user@host` has no port.
pub fn ssh_target(host: &str) -> (String, Option<String>) {
    match host.rsplit_once(':') {
        Some((destination, port))
            if !destination.contains(':')
                && !port.is_empty()
                && port.chars().all(|c| c.is_ascii_digit()) =>
        {
            (destination.to_string(), Some(port.to_string()))
        }
        _ => (host.to_string(), None),
    }
}

/// A path as WSL sees it: `C:\work\x` becomes `/mnt/c/work/x`, and a `\\wsl$\<distro>\…` or
/// `\\wsl.localhost\<distro>\…` path becomes the path inside the distribution. Linux paths are
/// returned unchanged.
pub fn wsl_path(path: &str) -> String {
    let path = path.trim();
    let bytes = path.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        let drive = (bytes[0] as char).to_ascii_lowercase();
        let rest = path[2..].replace('\\', "/");
        let rest = rest.trim_start_matches('/');
        return if rest.is_empty() {
            format!("/mnt/{drive}")
        } else {
            format!("/mnt/{drive}/{}", rest.trim_end_matches('/'))
        };
    }
    let unix = path.replace('\\', "/");
    for prefix in ["//wsl$/", "//wsl.localhost/"] {
        if let Some(rest) = unix
            .get(..prefix.len())
            .filter(|head| head.eq_ignore_ascii_case(prefix))
            .map(|_| &unix[prefix.len()..])
        {
            // Drop the distribution's name.
            let inside = rest.split_once('/').map_or("", |(_, inside)| inside);
            return format!("/{}", inside.trim_end_matches('/'));
        }
    }
    path.to_string()
}

/// `path` with `.` and `..` resolved and `/` separators, if it is absolute and does not climb
/// above `/`.
pub fn normalize_posix(path: &str) -> Option<String> {
    if !path.starts_with('/') {
        return None;
    }
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            name => parts.push(name),
        }
    }
    Some(format!("/{}", parts.join("/")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docker_runs_in_the_container_at_the_folder() {
        let spec = Remote::Docker {
            container: "web".into(),
        }
        .command("/srv/app", "ls -1");
        assert_eq!(
            spec.to_string(),
            "docker exec -i -w /srv/app web sh -c ls -1"
        );
        assert_eq!(
            spec.args.last().unwrap(),
            "ls -1",
            "the script is one argument"
        );
    }

    #[test]
    fn wsl_runs_sh_in_the_distribution_at_the_folder() {
        let spec = Remote::Wsl {
            distro: "Ubuntu".into(),
        }
        .command(&wsl_path(r"C:\Users\me\project"), "cargo test");
        assert_eq!(
            spec.to_string(),
            "wsl.exe -d Ubuntu --cd /mnt/c/Users/me/project --exec sh -c cargo test"
        );
    }

    #[test]
    fn ssh_quotes_the_folder_and_script_for_the_remote_shell() {
        let spec = Remote::Ssh {
            host: "me@build:2222".into(),
        }
        .command("/home/me/it's here", "echo 'hi'");
        assert_eq!(
            spec.args,
            [
                "-o",
                "BatchMode=yes",
                "-T",
                "-p",
                "2222",
                "me@build",
                r#"sh -c 'cd '\''/home/me/it'\''\'\'''\''s here'\'' && echo '\''hi'\'''"#,
            ]
        );
        let alias = Remote::Ssh {
            host: "devbox".into(),
        }
        .command("/w", "true");
        assert_eq!(alias.args[3], "devbox", "{alias}");
    }

    #[test]
    fn ssh_targets_split_off_a_port() {
        assert_eq!(ssh_target("devbox"), ("devbox".into(), None));
        assert_eq!(ssh_target("me@host"), ("me@host".into(), None));
        assert_eq!(
            ssh_target("me@host:22"),
            ("me@host".into(), Some("22".into()))
        );
        assert_eq!(ssh_target("host:x"), ("host:x".into(), None));
    }

    #[test]
    fn windows_paths_are_translated_for_wsl() {
        assert_eq!(wsl_path(r"C:\Users\me\x\"), "/mnt/c/Users/me/x");
        assert_eq!(wsl_path("d:/data"), "/mnt/d/data");
        assert_eq!(wsl_path(r"E:\"), "/mnt/e");
        assert_eq!(wsl_path(r"\\wsl$\Ubuntu\home\me"), "/home/me");
        assert_eq!(wsl_path(r"\\wsl.localhost\Debian\srv\"), "/srv");
        assert_eq!(wsl_path("/home/me"), "/home/me");
    }

    #[test]
    fn posix_paths_normalise_without_escaping_the_root() {
        assert_eq!(normalize_posix("/a/./b/../c/").as_deref(), Some("/a/c"));
        assert_eq!(normalize_posix("/").as_deref(), Some("/"));
        assert_eq!(normalize_posix("/.."), None);
        assert_eq!(normalize_posix("relative"), None);
    }

    #[test]
    fn utf16_output_from_wsl_is_decoded() {
        let bytes: Vec<u8> = "Ubuntu\r\n"
            .encode_utf16()
            .flat_map(|unit| unit.to_le_bytes())
            .collect();
        assert_eq!(decode(&bytes), "Ubuntu\r\n");
        assert_eq!(decode(b"plain"), "plain");
    }
}
