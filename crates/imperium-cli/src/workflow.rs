//! `imperium workflow resolve`: what an agent in a role would be asked to load for a task.
//!
//! Without `--pack` the packs are layered as the daemon layers them for a session
//! ([`imperium_policy::packs::resolve`]): `workflow.enabled` is honoured, an unreadable pack is
//! skipped with a notice, listed folders are trusted, the packs pinned with `imperium pack` follow
//! them, and the bundled default pack stands in when nothing is listed or pinned. The bundled
//! pack is found by this process's own lookup, which can differ from the daemon's, so the pack
//! used is named. With `--pack` only the given folders are used, as before.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use imperium_config::Paths;
use imperium_policy::packs::{self, PackInputs};
use imperium_workflow::{ContextIndex, Pack};

pub fn resolve(
    role: &str,
    task: &str,
    project: Option<PathBuf>,
    packs: Vec<PathBuf>,
) -> anyhow::Result<()> {
    let project = match project {
        Some(project) => project,
        None => std::env::current_dir().context("could not read the current directory")?,
    };
    let index = if packs.is_empty() {
        configured(&project)?
    } else {
        Some(Arc::new(explicit(&packs, &project)?))
    };
    let Some(index) = index else {
        eprintln!("the workflow is turned off (workflow.enabled = false): no packs are used");
        return Ok(());
    };
    println!("{}", index.resolve(role, task).render());
    Ok(())
}

/// The packs the daemon would use for a session in `project`; `None` when the workflow is off.
fn configured(project: &Path) -> anyhow::Result<Option<Arc<ContextIndex>>> {
    let paths = Paths::from_env()?;
    let settings = imperium_config::load(&paths, Some(project), std::env::vars())?.settings()?;
    let workflow = &settings.workflow;
    let bundled = bundled_pack();
    let resolved = packs::resolve(
        &PackInputs {
            enabled: workflow.enabled,
            listed: &workflow.packs,
            default_pack: workflow.default_pack,
            data_dir: paths.data_dir(),
            config_dir: paths.config_dir(),
            bundled: bundled.clone(),
        },
        project,
    );
    for notice in &resolved.notices {
        eprintln!("{notice}");
    }
    let Some(index) = resolved.index else {
        return Ok(None);
    };
    if let Some(bundled) = &bundled
        && index.packs().iter().any(|pack| &pack.root == bundled)
    {
        eprintln!("using the bundled workflow pack {}", bundled.display());
    } else if index.packs().is_empty() && workflow.packs.is_empty() && workflow.default_pack {
        eprintln!(
            "no bundled workflow pack was found (set IMPERIUM_RESOURCE_DIR to a resource folder)"
        );
    }
    Ok(Some(index))
}

/// The folders named with `--pack`, and nothing else: no pinned packs, no bundled pack, and the
/// workflow switch is not consulted. A folder that cannot be read fails the command.
fn explicit(folders: &[PathBuf], project: &Path) -> anyhow::Result<ContextIndex> {
    let packs = folders
        .iter()
        .map(|root| Pack::load(root).with_context(|| format!("workflow pack {}", root.display())))
        .collect::<anyhow::Result<Vec<_>>>()?;
    Ok(ContextIndex::new(packs, project))
}

/// The bundled default pack, the way the daemon looks for it: the folder `IMPERIUM_RESOURCE_DIR`
/// names, then beside this executable and one level up, then the source tree. The desktop app sets
/// the variable only for the daemon it starts, so a command line run from a shell may find a
/// different copy, or none.
fn bundled_pack() -> Option<PathBuf> {
    const RELATIVE: &str = "policy/agent-custom-workflow";
    let mut candidates = Vec::new();
    if let Some(resources) = std::env::var_os("IMPERIUM_RESOURCE_DIR") {
        candidates.push(PathBuf::from(resources).join(RELATIVE));
    }
    if let Some(beside) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
    {
        candidates.push(beside.join(RELATIVE));
        candidates.push(beside.join("..").join(RELATIVE));
    }
    candidates.push(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(RELATIVE),
    );
    candidates
        .into_iter()
        .find(|candidate| candidate.join("AGENTS.md").is_file())
}
