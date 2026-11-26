//! `imperium runtime ncm use|status`: the `ncm-server` that serves NCM (Princeps) models. There
//! are no release builds yet, so the user builds it and approves it here; nothing is searched
//! for on `PATH` or downloaded ([`imperium_local::ncm_runtime`]).

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, bail};
use imperium_config::Paths;
use imperium_local::backend::{NCM_ENV, allowed_env};
use imperium_local::ncm_runtime;

use crate::vllm::confirm;

/// Longest `ncm-server --version` may take.
const VERSION_TIMEOUT: Duration = Duration::from_secs(10);

/// Folders agents write to, where an approved binary must not be: the session workspaces and
/// the worktree folder.
fn agent_folders(paths: &Paths, settings: &imperium_config::Settings) -> Vec<PathBuf> {
    let mut folders = vec![settings.workspaces.root(paths)];
    folders.extend(settings.sessions.worktree_dir.clone());
    folders
}

/// What `program --version` prints, run with the same environment allowlist as a model's
/// server; `None` when it does not answer within [`VERSION_TIMEOUT`] or fails.
fn version_of(program: &Path) -> Option<String> {
    let mut command = Command::new(program);
    command
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .env_clear()
        .envs(allowed_env(
            NCM_ENV,
            std::env::vars_os().filter_map(|(name, value)| Some((name.into_string().ok()?, value))),
            cfg!(windows),
        ));
    let mut child = command.spawn().ok()?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut out = String::new();
                child
                    .stdout
                    .take()?
                    .take(4096)
                    .read_to_string(&mut out)
                    .ok()?;
                return status.success().then(|| out.trim().to_string());
            }
            Ok(None) if started.elapsed() < VERSION_TIMEOUT => {
                std::thread::sleep(Duration::from_millis(50));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

/// `imperium runtime ncm use <path>`: shows the binary's path, size, SHA-256 and version, and
/// once the user approves, copies it into the data folder and sets `runtime.ncm.custom_path`.
pub fn use_server(path: PathBuf, yes: bool) -> anyhow::Result<()> {
    let paths = Paths::from_env()?;
    let settings = imperium_config::load(&paths, None, std::env::vars())?.settings()?;
    let path = std::path::absolute(&path)
        .with_context(|| format!("could not resolve {}", path.display()))?;
    let candidate = ncm_runtime::inspect(&path, &agent_folders(&paths, &settings))
        .map_err(anyhow::Error::msg)?;
    let version = version_of(&candidate.path);
    println!("ncm-server to approve:");
    println!("  path:    {}", candidate.path.display());
    println!("  size:    {} bytes", candidate.size);
    println!("  SHA-256: {}", candidate.sha256);
    println!(
        "  version: {}",
        version.as_deref().unwrap_or("(no answer to --version)")
    );
    println!(
        "Imperium copies it to {} and runs only that copy, checked against this hash at every \
         launch.",
        ncm_runtime::copy_dir(paths.data_dir(), &candidate.sha256).display()
    );
    if !confirm("Approve this ncm-server?", yes)? {
        bail!("not approved; nothing changed");
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    let approval = ncm_runtime::approve(paths.data_dir(), &candidate, version, now)
        .map_err(anyhow::Error::msg)?;
    imperium_config::set_user_setting(
        &paths,
        "runtime.ncm.custom_path",
        &serde_json::Value::from(approval.source.display().to_string()),
        std::env::vars(),
    )
    .map_err(|error| anyhow::anyhow!("{error}"))?;
    println!(
        "Approved. NCM models load with it from their next load (SHA-256 {}).",
        approval.sha256
    );
    Ok(())
}

/// `imperium runtime ncm status`: the approved build, and whether its copy is intact.
pub fn status() -> anyhow::Result<()> {
    let paths = Paths::from_env()?;
    match ncm_runtime::approved(paths.data_dir()).map_err(anyhow::Error::msg)? {
        None => println!("{}.", ncm_runtime::HOW_TO_INSTALL),
        Some(approval) => {
            println!("Approved ncm-server: {}", approval.source.display());
            println!("  SHA-256: {}", approval.sha256);
            if let Some(version) = &approval.version {
                println!("  version: {version}");
            }
            match ncm_runtime::verify(paths.data_dir()) {
                Ok(verified) => println!("  runs from: {}", verified.program.display()),
                Err(problem) => println!("  {problem}"),
            }
        }
    }
    Ok(())
}
