//! `imperium workflow resolve`: what an agent in a role would be asked to load for a task.

use std::path::PathBuf;

use anyhow::Context;
use imperium_config::Paths;
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
    let packs = if packs.is_empty() {
        let paths = Paths::from_env()?;
        imperium_config::load(&paths, Some(&project), std::env::vars())?
            .settings()?
            .workflow
            .packs
    } else {
        packs
    };
    let packs = packs
        .iter()
        .map(|root| Pack::load(root).with_context(|| format!("workflow pack {}", root.display())))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let manifest = ContextIndex::new(packs, &project).resolve(role, task);
    println!("{}", manifest.render());
    Ok(())
}
