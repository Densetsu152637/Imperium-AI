//! File and command tools for a remote workspace: every call is a short `sh -c` script run in
//! the runtime (`cat`, `ls`, `mkdir -p` with `cat >` fed from standard input), so nothing but a
//! POSIX shell is needed there.

use std::time::Duration;

use crate::runtime::{RunOutput, sh_quote};
use crate::workspace::RemoteFolders;
use crate::{ToolError, Workspace};

/// How long one file operation may take in the runtime.
const FILE_TIMEOUT: Duration = Duration::from_secs(60);

async fn run(
    remote: &RemoteFolders,
    script: &str,
    stdin: Option<&[u8]>,
    timeout: Duration,
) -> Result<RunOutput, ToolError> {
    let spec = remote.target.command(&remote.folders[0], script);
    remote.runner.run(&spec, stdin, timeout).await
}

fn remote(workspace: &Workspace) -> Result<&RemoteFolders, ToolError> {
    workspace
        .remote_folders()
        .ok_or_else(|| ToolError::Io("not a remote workspace".into()))
}

/// A failed operation's message, naming the path the agent gave.
fn failed(shown: &str, output: &RunOutput) -> ToolError {
    let message = output.message();
    ToolError::Io(if message.is_empty() {
        format!("{shown}: failed in the runtime")
    } else {
        format!("{shown}: {message}")
    })
}

pub(crate) async fn read_bytes(workspace: &Workspace, path: &str) -> Result<Vec<u8>, ToolError> {
    let remote = remote(workspace)?;
    let full = workspace.resolve_remote(path, false)?;
    let output = run(
        remote,
        &format!("cat -- {}", sh_quote(&full)),
        None,
        FILE_TIMEOUT,
    )
    .await?;
    if !output.success() {
        return Err(failed(path, &output));
    }
    Ok(output.stdout)
}

pub(crate) async fn read_text(workspace: &Workspace, path: &str) -> Result<String, ToolError> {
    String::from_utf8(read_bytes(workspace, path).await?)
        .map_err(|_| ToolError::Invalid(format!("{path} is not UTF-8 text")))
}

/// Writes `content` to `path` (making its folder), returning the path as shown to the model.
pub(crate) async fn write(
    workspace: &Workspace,
    path: &str,
    content: &[u8],
) -> Result<String, ToolError> {
    let remote = remote(workspace)?;
    let full = workspace.resolve_remote(path, true)?;
    let parent = full.rsplit_once('/').map_or("/", |(parent, _)| parent);
    let parent = if parent.is_empty() { "/" } else { parent };
    let script = format!(
        "mkdir -p -- {} && cat > {}",
        sh_quote(parent),
        sh_quote(&full)
    );
    let output = run(remote, &script, Some(content), FILE_TIMEOUT).await?;
    if !output.success() {
        return Err(failed(path, &output));
    }
    Ok(workspace.display_remote(&full))
}

pub(crate) async fn list(workspace: &Workspace, path: &str) -> Result<String, ToolError> {
    let remote = remote(workspace)?;
    let full = workspace.resolve_remote(path, false)?;
    let output = run(
        remote,
        &format!("ls -1Ap -- {}", sh_quote(&full)),
        None,
        FILE_TIMEOUT,
    )
    .await?;
    if !output.success() {
        return Err(failed(path, &output));
    }
    let mut entries: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| line.trim_end_matches('\r').to_string())
        .filter(|line| !line.is_empty())
        .collect();
    entries.sort();
    if entries.is_empty() {
        return Ok(format!("{} is empty", workspace.display_remote(&full)));
    }
    Ok(entries.join("\n"))
}

/// Runs a command line in the root folder inside the runtime.
pub(crate) async fn command(
    workspace: &Workspace,
    line: &str,
    timeout: Duration,
) -> Result<RunOutput, ToolError> {
    run(remote(workspace)?, line, None, timeout).await
}
