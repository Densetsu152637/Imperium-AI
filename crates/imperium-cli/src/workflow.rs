//! `imperium workflow resolve`: what an agent in a role would be asked to load for a task.

use std::path::PathBuf;

use anyhow::Context;
use imperium_config::Paths;
use imperium_workflow::update::PackStore;
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
    let mut named = Vec::new();
    let packs = if packs.is_empty() {
        let paths = Paths::from_env()?;
        // Packs the user accepted from an update source count like the folders they listed.
        for pinned in PackStore::new(paths.data_dir()).pinned().packs {
            named.push((pinned.name, pinned.dir));
        }
        imperium_config::load(&paths, Some(&project), std::env::vars())?
            .settings()?
            .workflow
            .packs
    } else {
        packs
    };
    let mut packs = packs
        .iter()
        .map(|root| Pack::load(root).with_context(|| format!("workflow pack {}", root.display())))
        .collect::<anyhow::Result<Vec<_>>>()?;
    for (name, dir) in named {
        let mut pack = Pack::load(&dir).with_context(|| format!("pinned pack {name}"))?;
        pack.name = name;
        packs.push(pack);
    }
    let manifest = ContextIndex::new(packs, &project).resolve(role, task);
    println!("{}", manifest.render());
    Ok(())
}
