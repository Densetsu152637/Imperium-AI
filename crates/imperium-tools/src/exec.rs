use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;

use crate::{ToolError, Workspace};

/// Longest stdout or stderr returned to the model, in characters; the tail is kept.
const MAX_STREAM_CHARS: usize = 30_000;
pub(crate) const DEFAULT_TIMEOUT_S: u64 = 120;

/// A command line run through the platform shell: `sh -c`, or `cmd /C` on Windows, where the
/// line is passed verbatim because cmd does not understand the quoting other programs expect.
pub fn shell(command: &str) -> Command {
    #[cfg(windows)]
    {
        let mut process = Command::new("cmd");
        process.arg("/C").raw_arg(command);
        process
    }
    #[cfg(not(windows))]
    {
        let mut process = Command::new("sh");
        process.arg("-c").arg(command);
        process
    }
}

/// Runs a shell command in the workspace root under the workspace's isolation tier (`cmd /C`
/// on Windows, `sh -c` elsewhere, or inside the OS sandbox or container).
///
/// On timeout the shell is killed; processes it started may keep running until they exit
/// (in the container tier the container is removed with them).
pub async fn run_command(
    workspace: &Workspace,
    command: &str,
    timeout: Duration,
) -> Result<String, ToolError> {
    if workspace.runtime().is_some() {
        let output = crate::remote_files::command(workspace, command, timeout).await?;
        let status = match output.code {
            Some(code) => format!("exit code {code}"),
            None => "terminated by a signal".into(),
        };
        return Ok(format!(
            "{status}\n--- stdout ---\n{}\n--- stderr ---\n{}",
            tail(&String::from_utf8_lossy(&output.stdout)),
            tail(&String::from_utf8_lossy(&output.stderr)),
        ));
    }
    let child = workspace
        .sandbox()
        .command(&workspace.roots(), command)?
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| ToolError::Io(format!("could not start the command: {error}")))?;
    let output = tokio::time::timeout(timeout, child.wait_with_output())
        .await
        .map_err(|_| ToolError::Timeout(timeout.as_secs()))?
        .map_err(|error| ToolError::Io(format!("command failed: {error}")))?;

    let status = match output.status.code() {
        Some(code) => format!("exit code {code}"),
        None => "terminated by a signal".into(),
    };
    Ok(format!(
        "{status}\n--- stdout ---\n{}\n--- stderr ---\n{}",
        tail(&String::from_utf8_lossy(&output.stdout)),
        tail(&String::from_utf8_lossy(&output.stderr)),
    ))
}

fn tail(text: &str) -> String {
    let count = text.chars().count();
    if count <= MAX_STREAM_CHARS {
        return text.to_string();
    }
    let kept: String = text.chars().skip(count - MAX_STREAM_CHARS).collect();
    format!(
        "[{} earlier characters omitted]\n{kept}",
        count - MAX_STREAM_CHARS
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn output_and_exit_code_are_reported() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = Workspace::new(dir.path()).unwrap();
        let output = run_command(&workspace, "echo hello && exit 3", Duration::from_secs(30))
            .await
            .unwrap();
        assert!(output.starts_with("exit code 3"), "{output}");
        assert!(output.contains("hello"), "{output}");
    }

    #[tokio::test]
    async fn quoted_arguments_reach_the_shell_intact() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = Workspace::new(dir.path()).unwrap();
        std::fs::write(dir.path().join("two words.txt"), "inside").unwrap();
        let read = if cfg!(windows) {
            r#"type "two words.txt""#
        } else {
            r#"cat "two words.txt""#
        };
        let output = run_command(&workspace, read, Duration::from_secs(30))
            .await
            .unwrap();
        assert!(output.contains("inside"), "{output}");
    }

    #[tokio::test]
    async fn commands_past_the_timeout_are_stopped() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = Workspace::new(dir.path()).unwrap();
        let slow = if cfg!(windows) {
            "ping -n 4 127.0.0.1 > NUL"
        } else {
            "sleep 4"
        };
        assert!(matches!(
            run_command(&workspace, slow, Duration::from_millis(500)).await,
            Err(ToolError::Timeout(_))
        ));
    }

    #[test]
    fn long_output_keeps_the_tail() {
        let long = "a".repeat(MAX_STREAM_CHARS) + "END";
        let kept = tail(&long);
        assert!(kept.starts_with("[3 earlier characters omitted]"));
        assert!(kept.ends_with("END"));
    }
}
