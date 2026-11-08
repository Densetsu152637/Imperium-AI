//! `imperium env`: environments, the named sets of folders sessions work in.

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use imperium_cli::client::Client;
use imperium_protocol::methods::{
    EnvironmentAddFolder, EnvironmentCreate, EnvironmentCreateParams, EnvironmentDelete,
    EnvironmentFolderParams, EnvironmentInfo, EnvironmentList, EnvironmentListParams,
    EnvironmentRef, EnvironmentRemoveFolder, EnvironmentUpdate, EnvironmentUpdateParams,
};

use crate::{block_on, connect};

pub fn list() -> anyhow::Result<()> {
    block_on(async {
        let (mut client, _) = connect().await?;
        let listing = client
            .call::<EnvironmentList>(EnvironmentListParams {})
            .await?;
        if listing.environments.is_empty() {
            println!("No environments. Create one with: imperium env create <name> [folder...]");
        }
        for env in listing.environments {
            print!("{}", describe(&env));
        }
        Ok(())
    })
}

pub fn create(name: String, folders: Vec<PathBuf>) -> anyhow::Result<()> {
    let folders = folders
        .iter()
        .map(|folder| absolute(folder))
        .collect::<anyhow::Result<_>>()?;
    block_on(async {
        let (mut client, _) = connect().await?;
        let env = client
            .call::<EnvironmentCreate>(EnvironmentCreateParams {
                name,
                icon: None,
                color: None,
                folders,
            })
            .await?;
        print!("{}", describe(&env));
        Ok(())
    })
}

pub fn add_folder(env: String, folder: PathBuf) -> anyhow::Result<()> {
    let folder = absolute(&folder)?;
    block_on(async {
        let (mut client, _) = connect().await?;
        let id = resolve(&mut client, &env).await?.id;
        let env = client
            .call::<EnvironmentAddFolder>(EnvironmentFolderParams { id, folder })
            .await?;
        print!("{}", describe(&env));
        Ok(())
    })
}

pub fn remove_folder(env: String, folder: PathBuf) -> anyhow::Result<()> {
    let folder = absolute(&folder)?;
    block_on(async {
        let (mut client, _) = connect().await?;
        let id = resolve(&mut client, &env).await?.id;
        let env = client
            .call::<EnvironmentRemoveFolder>(EnvironmentFolderParams { id, folder })
            .await?;
        print!("{}", describe(&env));
        Ok(())
    })
}

pub fn rename(env: String, name: String) -> anyhow::Result<()> {
    block_on(async {
        let (mut client, _) = connect().await?;
        let id = resolve(&mut client, &env).await?.id;
        let env = client
            .call::<EnvironmentUpdate>(EnvironmentUpdateParams {
                id,
                name: Some(name),
                icon: None,
                color: None,
            })
            .await?;
        print!("{}", describe(&env));
        Ok(())
    })
}

pub fn delete(env: String) -> anyhow::Result<()> {
    block_on(async {
        let (mut client, _) = connect().await?;
        let found = resolve(&mut client, &env).await?;
        client
            .call::<EnvironmentDelete>(EnvironmentRef {
                id: found.id.clone(),
            })
            .await?;
        println!(
            "deleted {} ({}); its folders are untouched",
            found.name, found.id
        );
        Ok(())
    })
}

/// The environment named by `id_or_name`: an id, else a name (ignoring case) that only one
/// environment has.
pub async fn resolve(client: &mut Client, id_or_name: &str) -> anyhow::Result<EnvironmentInfo> {
    let listing = client
        .call::<EnvironmentList>(EnvironmentListParams {})
        .await?;
    pick(listing.environments, id_or_name)
}

fn pick(environments: Vec<EnvironmentInfo>, id_or_name: &str) -> anyhow::Result<EnvironmentInfo> {
    if let Some(env) = environments.iter().find(|env| env.id == id_or_name) {
        return Ok(env.clone());
    }
    let mut named: Vec<_> = environments
        .into_iter()
        .filter(|env| env.name.eq_ignore_ascii_case(id_or_name.trim()))
        .collect();
    match named.len() {
        1 => Ok(named.remove(0)),
        0 => bail!("no environment {id_or_name:?}; see imperium env list"),
        count => bail!("{count} environments are named {id_or_name:?}; use its id"),
    }
}

fn describe(env: &EnvironmentInfo) -> String {
    let mut text = format!("{}  {}\n", env.id, env.name);
    if env.folders.is_empty() {
        text.push_str("    (no folders yet: imperium env add-folder <env> <folder>)\n");
    }
    for (index, folder) in env.folders.iter().enumerate() {
        let note = if index == 0 {
            "  (working directory)"
        } else {
            ""
        };
        text.push_str(&format!("    {folder}{note}\n"));
    }
    text
}

fn absolute(folder: &Path) -> anyhow::Result<String> {
    Ok(std::path::absolute(folder)
        .with_context(|| format!("folder {}", folder.display()))?
        .to_string_lossy()
        .into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(id: &str, name: &str) -> EnvironmentInfo {
        EnvironmentInfo {
            id: id.into(),
            name: name.into(),
            icon: None,
            color: None,
            folders: vec!["/a".into(), "/b".into()],
        }
    }

    #[test]
    fn environments_are_found_by_id_or_unique_name() {
        let all = vec![env("a1", "Work"), env("b2", "Play"), env("c3", "play")];
        assert_eq!(pick(all.clone(), "a1").unwrap().name, "Work");
        assert_eq!(pick(all.clone(), "work").unwrap().id, "a1");
        assert!(pick(all.clone(), "PLAY").is_err(), "ambiguous");
        assert!(pick(all, "none").is_err());
        let shown = describe(&env("a1", "Work"));
        assert!(shown.contains("/a  (working directory)") && shown.contains("    /b\n"));
    }
}
