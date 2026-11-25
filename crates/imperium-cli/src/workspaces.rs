//! `imperium workspaces`: where environments' working folders are, and moving them.

use std::path::Path;

use anyhow::Context;
use imperium_protocol::workspaces::{
    WorkspacesMove, WorkspacesMoveParams, WorkspacesWhere, WorkspacesWhereParams,
};

use crate::{block_on, connect};

pub fn show() -> anyhow::Result<()> {
    block_on(async {
        let (mut client, _) = connect().await?;
        let found = client
            .call::<WorkspacesWhere>(WorkspacesWhereParams {})
            .await?;
        println!("{}", found.root);
        if found.root != found.default_root {
            println!("  (the default is {})", found.default_root);
        }
        if let Some(warning) = &found.warning {
            println!("  warning: {warning}");
        }
        for env in &found.environments {
            let memory = if env.memory_exists {
                "Memory.md"
            } else {
                "no Memory.md yet"
            };
            println!(
                "  {}  {}  ({memory}, {} session folders)",
                env.environment_id, env.dir, env.session_folders
            );
        }
        Ok(())
    })
}

pub fn move_to(path: &Path) -> anyhow::Result<()> {
    let written = path.to_string_lossy();
    // An empty path goes back to the default; anything else is made absolute here, since the
    // daemon may run somewhere else.
    let path = if written.is_empty() || written.starts_with('/') {
        written.into_owned()
    } else {
        std::path::absolute(path)
            .with_context(|| format!("folder {}", path.display()))?
            .to_string_lossy()
            .into_owned()
    };
    block_on(async {
        let (mut client, _) = connect().await?;
        let moved = client
            .call::<WorkspacesMove>(WorkspacesMoveParams { path })
            .await?;
        if moved.method == "nothing" && moved.files == 0 {
            println!("The working folders are at {}", moved.to);
        } else {
            println!(
                "Moved {} files ({} bytes) from {} to {} ({}).",
                moved.files, moved.bytes, moved.from, moved.to, moved.method
            );
        }
        for id in &moved.restarted_sessions {
            println!("Restarted session {id} to use the new folder.");
        }
        for warning in &moved.warnings {
            eprintln!("warning: {warning}");
        }
        Ok(())
    })
}
