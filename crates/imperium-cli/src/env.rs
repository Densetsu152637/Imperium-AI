//! `imperium env`: environments, the named sets of folders sessions work in.

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use imperium_cli::client::Client;
use imperium_protocol::methods::{
    EnvironmentAddFolder, EnvironmentCreate, EnvironmentCreateParams, EnvironmentDelete,
    EnvironmentFolderParams, EnvironmentInfo, EnvironmentList, EnvironmentListParams,
    EnvironmentRef, EnvironmentRemoveFolder, EnvironmentRuntime, EnvironmentTestRuntime,
    EnvironmentUpdate, EnvironmentUpdateParams, SessionDelete, SessionDeleteParams,
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
                folders,
                ..EnvironmentCreateParams::default()
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
                ..EnvironmentUpdateParams::default()
            })
            .await?;
        print!("{}", describe(&env));
        Ok(())
    })
}

/// The runtime `kind` names, with the options it needs.
pub fn runtime(
    kind: &str,
    image: Option<String>,
    container: Option<String>,
    distro: Option<String>,
    host: Option<String>,
) -> anyhow::Result<EnvironmentRuntime> {
    let need = |value: Option<String>, flag: &str| {
        value.with_context(|| format!("the {kind} runtime needs --{flag}"))
    };
    Ok(match kind {
        "local" => EnvironmentRuntime::Local,
        "docker" => EnvironmentRuntime::Docker {
            image: need(image, "image")?,
            container,
        },
        "wsl" => EnvironmentRuntime::Wsl {
            distro: need(distro, "distro")?,
        },
        "ssh" => EnvironmentRuntime::Ssh {
            host: need(host, "host")?,
        },
        other => bail!("unknown runtime {other:?}: use local, docker, wsl, or ssh"),
    })
}

pub fn set_runtime(env: String, runtime: EnvironmentRuntime) -> anyhow::Result<()> {
    block_on(async {
        let (mut client, _) = connect().await?;
        let id = resolve(&mut client, &env).await?.id;
        let env = client
            .call::<EnvironmentUpdate>(EnvironmentUpdateParams {
                id,
                runtime: Some(runtime),
                ..EnvironmentUpdateParams::default()
            })
            .await?;
        print!("{}", describe(&env));
        println!("Check it with: imperium env test {}", env.id);
        Ok(())
    })
}

pub fn set_agent(env: String, on: bool) -> anyhow::Result<()> {
    block_on(async {
        let (mut client, _) = connect().await?;
        let id = resolve(&mut client, &env).await?.id;
        let env = client
            .call::<EnvironmentUpdate>(EnvironmentUpdateParams {
                id,
                persistent_agent: Some(on),
                ..EnvironmentUpdateParams::default()
            })
            .await?;
        print!("{}", describe(&env));
        Ok(())
    })
}

pub fn test(env: String) -> anyhow::Result<()> {
    block_on(async {
        let (mut client, _) = connect().await?;
        let id = resolve(&mut client, &env).await?.id;
        let result = client
            .call::<EnvironmentTestRuntime>(EnvironmentRef { id })
            .await?;
        if result.ok {
            println!("ok: {}", result.message);
            Ok(())
        } else {
            bail!("{}", result.message)
        }
    })
}

pub fn delete_session(session_id: String, remove_worktrees: bool) -> anyhow::Result<()> {
    block_on(async {
        let (mut client, _) = connect().await?;
        let result = client
            .call::<SessionDelete>(SessionDeleteParams {
                session_id: session_id.clone(),
                remove_worktrees,
            })
            .await?;
        println!("deleted session {session_id}");
        for removed in result.removed_worktrees {
            println!("removed worktree {removed} (its branch is kept)");
        }
        for failure in &result.failed {
            eprintln!("could not remove worktree {failure}");
        }
        Ok(())
    })
}

fn runtime_label(runtime: &EnvironmentRuntime) -> String {
    match runtime {
        EnvironmentRuntime::Local => "local".into(),
        EnvironmentRuntime::Docker {
            image,
            container: Some(container),
        } => format!("docker container {container} (image {image})"),
        EnvironmentRuntime::Docker { image, .. } => format!("docker image {image}"),
        EnvironmentRuntime::Wsl { distro } => format!("wsl {distro}"),
        EnvironmentRuntime::Ssh { host } => format!("ssh {host}"),
    }
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
    if env.runtime != EnvironmentRuntime::Local {
        text.push_str(&format!("    runtime: {}\n", runtime_label(&env.runtime)));
    }
    if env.persistent_agent {
        let session = env.agent_session_id.as_deref().unwrap_or("not started");
        text.push_str(&format!("    persistent agent: {session}\n"));
    }
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

/// A folder as the daemon expects it: absolute on this machine, or a POSIX path (starting with
/// `/`) kept as written, since for a remote runtime it names a folder there.
fn absolute(folder: &Path) -> anyhow::Result<String> {
    let written = folder.to_string_lossy();
    if written.starts_with('/') {
        return Ok(written.into_owned());
    }
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
            runtime: EnvironmentRuntime::Local,
            persistent_agent: false,
            agent_session_id: None,
            pet: None,
            pinned: false,
            permissions: Default::default(),
        }
    }

    #[test]
    fn runtimes_are_built_from_their_options() {
        assert_eq!(
            runtime("ssh", None, None, None, Some("box".into())).unwrap(),
            EnvironmentRuntime::Ssh { host: "box".into() }
        );
        assert_eq!(
            runtime(
                "docker",
                Some("rust:1".into()),
                Some("web".into()),
                None,
                None
            )
            .unwrap(),
            EnvironmentRuntime::Docker {
                image: "rust:1".into(),
                container: Some("web".into())
            }
        );
        assert!(runtime("wsl", None, None, None, None).is_err());
        assert!(runtime("vm", None, None, None, None).is_err());
        let mut remote = env("a1", "Work");
        remote.runtime = EnvironmentRuntime::Wsl {
            distro: "Ubuntu".into(),
        };
        remote.persistent_agent = true;
        let shown = describe(&remote);
        assert!(shown.contains("runtime: wsl Ubuntu") && shown.contains("persistent agent"));
        assert_eq!(absolute(Path::new("/srv/app")).unwrap(), "/srv/app");
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
