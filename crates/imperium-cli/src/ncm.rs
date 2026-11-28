//! `imperium runtime ncm use|status`: the `ncm-server` that serves NCM (Princeps) models. There
//! are no release builds yet, so the user builds it and approves it here, at a terminal; nothing
//! is searched for on `PATH` or downloaded ([`imperium_local::ncm_runtime`]).

use std::io::IsTerminal;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, bail};
use imperium_config::Paths;
use imperium_local::ncm_runtime;

use crate::vllm::confirm;

/// Why an approval cannot happen here: only a person at a terminal approves an `ncm-server`
/// (an agent's command, a script or a pipe cannot), so there is no `--yes`.
fn refuse_unless_interactive(interactive: bool) -> anyhow::Result<()> {
    if !interactive {
        bail!(
            "approving an ncm-server needs you at a terminal: run `imperium runtime ncm use \
             <path>` yourself in an interactive shell (it cannot be approved from a script, a \
             pipe or an agent)"
        );
    }
    Ok(())
}

/// Folders agents write to, where an approved binary must not be: the session workspaces, the
/// worktree folder and the temporary folders.
fn agent_folders(paths: &Paths, settings: &imperium_config::Settings) -> Vec<PathBuf> {
    let mut folders = vec![settings.workspaces.root(paths)];
    folders.extend(settings.sessions.worktree_dir.clone());
    folders.extend(ncm_runtime::temp_folders());
    folders
}

/// `imperium runtime ncm use <path>`: shows the binary's path, size and SHA-256 without running
/// it; once the user approves at a terminal, copies it into the data folder, asks the copy for
/// its version (and, when that does not say, the user whether it has CUDA), and sets
/// `runtime.ncm.custom_path`.
pub fn use_server(path: PathBuf) -> anyhow::Result<()> {
    refuse_unless_interactive(std::io::stdin().is_terminal())?;
    let paths = Paths::from_env()?;
    let settings = imperium_config::load(&paths, None, std::env::vars())?.settings()?;
    let path = std::path::absolute(&path)
        .with_context(|| format!("could not resolve {}", path.display()))?;
    let candidate = ncm_runtime::inspect(&path, &agent_folders(&paths, &settings))
        .map_err(anyhow::Error::msg)?;
    println!("ncm-server to approve (not run yet):");
    println!("  path:    {}", candidate.path.display());
    println!("  size:    {} bytes", candidate.size);
    println!("  SHA-256: {}", candidate.sha256);
    println!(
        "Imperium copies it to {} and runs only that copy, checked against this hash at every \
         launch.",
        ncm_runtime::copy_dir(paths.data_dir(), &candidate.sha256).display()
    );
    if !confirm("Approve this ncm-server?", false)? {
        bail!("not approved; nothing changed");
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    let approval = ncm_runtime::approve(
        paths.data_dir(),
        &candidate,
        now,
        ncm_runtime::probe,
        |version| {
            let question = format!(
                "Its version ({}) does not say whether it was built with `--features cuda`. Was \
                 it?",
                version.unwrap_or("no answer to --version")
            );
            // Asked before anything is recorded: an answer that cannot be read leaves no
            // approval behind.
            confirm(&question, false).map_err(|error| error.to_string())
        },
    )
    .map_err(anyhow::Error::msg)?;
    imperium_config::set_user_setting(
        &paths,
        "runtime.ncm.custom_path",
        &serde_json::Value::from(approval.source.display().to_string()),
        std::env::vars(),
    )
    .map_err(|error| anyhow::anyhow!("{error}"))?;
    let (build, gpu) = if approval.cuda {
        ("with CUDA", "use")
    } else {
        ("CPU only", "leave")
    };
    println!(
        "Approved (SHA-256 {}, {build}). NCM models load with it from their next load; restart \
         the daemon for runtime.ncm.device = auto to {gpu} the GPU.",
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
            println!(
                "  CUDA:    {}",
                if approval.cuda {
                    "yes"
                } else {
                    "no (CPU only)"
                }
            );
            match ncm_runtime::verify(paths.data_dir()) {
                Ok(verified) => println!("  runs from: {}", verified.program.display()),
                Err(problem) => println!("  {problem}"),
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn approving_needs_a_terminal() {
        let error = refuse_unless_interactive(false).unwrap_err().to_string();
        assert!(error.contains("terminal"), "{error}");
        assert!(refuse_unless_interactive(true).is_ok());
    }

    #[test]
    fn there_is_no_way_to_approve_without_being_asked() {
        // `--yes` is not a flag of `runtime ncm use`: a run without a terminal cannot pass one.
        let with_yes = [
            "imperium",
            "runtime",
            "ncm",
            "use",
            "/x/ncm-server",
            "--yes",
        ];
        assert!(crate::Cli::try_parse_from(with_yes).is_err());
        let plain = ["imperium", "runtime", "ncm", "use", "/x/ncm-server"];
        assert!(crate::Cli::try_parse_from(plain).is_ok());
    }

    #[test]
    fn the_temporary_folders_are_agent_folders() {
        assert!(ncm_runtime::temp_folders().contains(&std::env::temp_dir()));
    }
}
