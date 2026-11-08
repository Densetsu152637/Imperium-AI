//! Remote projects over SSH: `imperium remote connect <host>` finds (or starts) `imperium-core` on
//! the host, forwards its loopback port through SSH, and records the forwarded endpoint so
//! `imperium --remote <name> ...` talks to the remote daemon, which owns the host's files,
//! processes, models, and GPUs. Keys stay in the user's SSH agent; nothing is stored but the
//! remote daemon's per-launch token, in a file only the user can read.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use imperium_config::Paths;
use imperium_protocol::discovery::DaemonEndpoint;

/// Where forwarded remote endpoints are recorded.
pub fn endpoint_file(paths: &Paths, name: &str) -> PathBuf {
    paths
        .data_dir()
        .join("remotes")
        .join(format!("{name}.json"))
}

/// The remote daemon's port, from its `ws://127.0.0.1:<port>/rpc` URL.
fn port_of(url: &str) -> Option<u16> {
    url.strip_prefix("ws://127.0.0.1:")?
        .split('/')
        .next()?
        .parse()
        .ok()
}

/// `ssh` arguments that forward `local` on this machine to `remote` on the host's loopback.
pub fn forward_args(destination: &str, local: u16, remote: u16) -> Vec<String> {
    vec![
        "-N".into(),
        "-o".into(),
        "ExitOnForwardFailure=yes".into(),
        "-o".into(),
        "ServerAliveInterval=30".into(),
        "-L".into(),
        format!("127.0.0.1:{local}:127.0.0.1:{remote}"),
        destination.into(),
    ]
}

fn ssh_output(destination: &str, remote_command: &str) -> anyhow::Result<std::process::Output> {
    Command::new("ssh")
        .args(["-o", "BatchMode=yes", destination, remote_command])
        .stdin(Stdio::null())
        .output()
        .context("could not run ssh")
}

/// The host's daemon endpoint, starting the daemon there if it is not running.
fn remote_endpoint(destination: &str) -> anyhow::Result<DaemonEndpoint> {
    let mut output = ssh_output(destination, "imperium daemon endpoint")?;
    if !output.status.success() {
        let started = ssh_output(destination, "imperium daemon start")?;
        if !started.status.success() {
            bail!(
                "imperium is not installed on {destination}, or its daemon would not start: {}",
                String::from_utf8_lossy(&started.stderr).trim()
            );
        }
        output = ssh_output(destination, "imperium daemon endpoint")?;
    }
    if !output.status.success() {
        bail!(
            "could not read the daemon endpoint on {destination}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    serde_json::from_slice(&output.stdout).context("the remote endpoint was not valid JSON")
}

fn free_port() -> anyhow::Result<u16> {
    Ok(std::net::TcpListener::bind("127.0.0.1:0")?
        .local_addr()?
        .port())
}

fn write_private(path: &std::path::Path, text: &str) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    use std::io::Write;
    options.open(path)?.write_all(text.as_bytes())?;
    Ok(())
}

/// Connects to `destination` and keeps the SSH forward open until it ends or Ctrl+C.
pub fn connect(
    destination: &str,
    name: Option<String>,
    local_port: Option<u16>,
) -> anyhow::Result<()> {
    let name = name.unwrap_or_else(|| destination.replace(['@', ':', '/', '\\'], "_"));
    let remote = remote_endpoint(destination)?;
    let remote_port = port_of(&remote.url)
        .with_context(|| format!("unexpected remote daemon URL {}", remote.url))?;
    let local = match local_port {
        Some(port) => port,
        None => free_port()?,
    };
    let paths = Paths::from_env()?;
    let file = endpoint_file(&paths, &name);
    let forwarded = DaemonEndpoint {
        url: format!("ws://127.0.0.1:{local}/rpc"),
        ..remote
    };
    write_private(&file, &serde_json::to_string_pretty(&forwarded)?)?;
    let mut tunnel = Command::new("ssh")
        .args(forward_args(destination, local, remote_port))
        .spawn()
        .context("could not start the SSH forward")?;
    println!(
        "connected to imperium-core on {destination} (protocol {}); use `imperium --remote {name} ...`. \
         Press Ctrl+C to disconnect.",
        forwarded.protocol
    );
    let status = tunnel.wait();
    let _ = std::fs::remove_file(&file);
    let status = status.context("the SSH forward failed")?;
    if !status.success() {
        bail!("the SSH forward to {destination} ended: {status}");
    }
    Ok(())
}

/// `imperium daemon endpoint`: prints this machine's daemon endpoint as JSON (used over SSH).
pub fn print_endpoint() -> anyhow::Result<()> {
    let paths = Paths::from_env()?;
    let endpoint = imperium_protocol::discovery::read_endpoint(&paths.daemon_endpoint_file())
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    println!("{}", serde_json::to_string(&endpoint)?);
    Ok(())
}

/// `imperium daemon start`: starts `imperium-core` in the background and waits for it to listen.
pub fn start_daemon() -> anyhow::Result<()> {
    let paths = Paths::from_env()?;
    let endpoint_file = paths.daemon_endpoint_file();
    if endpoint_file.is_file() {
        println!("imperium-core is already running");
        return Ok(());
    }
    let exe = std::env::current_exe()?;
    let daemon = exe.with_file_name(format!("imperium-core{}", std::env::consts::EXE_SUFFIX));
    Command::new(&daemon)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("could not start {}", daemon.display()))?;
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if endpoint_file.is_file() {
            println!("imperium-core started");
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    bail!("imperium-core did not start within 15 s")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forwards_target_the_remote_loopback() {
        assert_eq!(port_of("ws://127.0.0.1:52011/rpc"), Some(52011));
        assert_eq!(port_of("ws://example.com:1/rpc"), None);
        let args = forward_args("gpu-box", 40000, 52011).join(" ");
        assert_eq!(
            args,
            "-N -o ExitOnForwardFailure=yes -o ServerAliveInterval=30 -L 127.0.0.1:40000:127.0.0.1:52011 gpu-box"
        );
    }
}
