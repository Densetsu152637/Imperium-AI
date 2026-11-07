//! `imperium project`: project agents.

use std::path::PathBuf;

use anyhow::{Context, bail};
use imperium_protocol::methods::{
    ProjectList, ProjectListParams, ProjectMessage, ProjectMessageParams,
};

use crate::{block_on, connect};

pub fn list() -> anyhow::Result<()> {
    block_on(async {
        let (mut client, _) = connect().await?;
        let listing = client.call::<ProjectList>(ProjectListParams {}).await?;
        if listing.agents.is_empty() {
            println!(
                "No project agents. Set agent = true under [[projects]] and restart the daemon."
            );
        }
        for agent in listing.agents {
            println!("{}  (agent session {})", agent.project, agent.session_id);
        }
        Ok(())
    })
}

pub fn tell(text: String, project: Option<PathBuf>) -> anyhow::Result<()> {
    let project = match project {
        Some(project) => project,
        None => std::env::current_dir().context("could not read the current directory")?,
    };
    block_on(async {
        let (mut client, _) = connect().await?;
        let result = client
            .call::<ProjectMessage>(ProjectMessageParams {
                project: project.display().to_string(),
                text,
            })
            .await?;
        if !result.delivered {
            bail!("{} has no running project agent", project.display());
        }
        println!("delivered to the project agent");
        Ok(())
    })
}
