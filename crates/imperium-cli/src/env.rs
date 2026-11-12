//! `imperium env`: environments, the named sets of folders sessions work in.

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use imperium_cli::client::Client;
use imperium_protocol::agents::{
    AgentPermissions, EnvironmentAgent, EnvironmentAgentCreate, EnvironmentAgentCreateParams,
    EnvironmentAgentDelete, EnvironmentAgentList, EnvironmentAgentListParams, EnvironmentAgentRef,
    EnvironmentAgentUpdate, EnvironmentAgentUpdateParams, ToolAccess,
};
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

/// Lists an environment's persistent agents.
pub fn agent_list(env: String) -> anyhow::Result<()> {
    block_on(async {
        let (mut client, _) = connect().await?;
        let environment_id = resolve(&mut client, &env).await?.id;
        let result = client
            .call::<EnvironmentAgentList>(EnvironmentAgentListParams { environment_id })
            .await?;
        if result.agents.is_empty() {
            println!("No persistent agents. Add one with: imperium env agent add <env> <name>");
        }
        for agent in &result.agents {
            print!("{}", describe_agent(agent));
        }
        Ok(())
    })
}

/// Permission profile flags of `imperium env agent add` and `set`. A flag not given keeps the
/// agent's profile as it is (a new agent has no restriction).
#[derive(Debug, Default, Clone, PartialEq, Eq, clap::Args)]
pub struct ProfileArgs {
    /// MCP servers the agent may use: `all`, `none`, or a comma-separated list of server names.
    #[arg(long, value_name = "all|none|NAMES")]
    pub mcp: Option<String>,
    /// Plugins the agent may activate and use: `all`, `none`, or a comma-separated list of
    /// plugin names.
    #[arg(long, value_name = "all|none|NAMES")]
    pub plugins: Option<String>,
    /// Limit the agent to local files: no web, browser, search, MCP, plugin or command tools, and
    /// file tools kept to the environment's folders and the agent's scratch folder.
    #[arg(long, conflicts_with = "no_local_only")]
    pub local_only: bool,
    /// Lift the local-files-only limit.
    #[arg(long)]
    pub no_local_only: bool,
}

impl ProfileArgs {
    /// Whether no flag was given.
    pub fn is_empty(&self) -> bool {
        *self == ProfileArgs::default()
    }

    /// `current` with the flags applied.
    pub fn apply(&self, current: &AgentPermissions) -> anyhow::Result<AgentPermissions> {
        let mut next = current.clone();
        if let Some(value) = &self.mcp {
            (next.mcp, next.mcp_servers) = parse_access(value).context("--mcp")?;
        }
        if let Some(value) = &self.plugins {
            (next.plugins, next.plugin_names) = parse_access(value).context("--plugins")?;
        }
        if self.local_only {
            next.local_files_only = true;
        }
        if self.no_local_only {
            next.local_files_only = false;
        }
        Ok(next)
    }
}

/// `all`, `none`, or a comma-separated list of names.
fn parse_access(value: &str) -> anyhow::Result<(ToolAccess, Vec<String>)> {
    match value.trim().to_ascii_lowercase().as_str() {
        "all" => Ok((ToolAccess::All, Vec::new())),
        "none" => Ok((ToolAccess::None, Vec::new())),
        _ => {
            let names: Vec<String> = value
                .split(',')
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_string)
                .collect();
            if names.is_empty() {
                bail!("expected all, none, or a comma-separated list of names");
            }
            Ok((ToolAccess::Named, names))
        }
    }
}

/// Adds a persistent agent to an environment.
pub fn agent_add(
    env: String,
    name: String,
    role: Option<String>,
    initiative: Option<String>,
    profile: ProfileArgs,
) -> anyhow::Result<()> {
    let initiative = parse_initiative(initiative)?;
    let permissions = if profile.is_empty() {
        None
    } else {
        Some(profile.apply(&AgentPermissions::default())?)
    };
    block_on(async {
        let (mut client, _) = connect().await?;
        let environment_id = resolve(&mut client, &env).await?.id;
        let agent = client
            .call::<EnvironmentAgentCreate>(EnvironmentAgentCreateParams {
                environment_id,
                name,
                role,
                initiative,
                permissions,
            })
            .await?;
        print!("{}", describe_agent(&agent.agent));
        if !agent.started {
            println!(
                "Added, but not running yet: {}",
                agent.reason.as_deref().unwrap_or("it has not started")
            );
        }
        Ok(())
    })
}

/// Renames an agent or changes its role or permission profile. The agent is a name or an id.
pub fn agent_set(
    env: String,
    agent: String,
    name: Option<String>,
    role: Option<String>,
    initiative: Option<String>,
    profile: ProfileArgs,
) -> anyhow::Result<()> {
    let initiative = parse_initiative(initiative)?;
    block_on(async {
        let (mut client, _) = connect().await?;
        let environment_id = resolve(&mut client, &env).await?.id;
        let current = resolve_agent(&mut client, &environment_id, &agent).await?;
        let permissions = if profile.is_empty() {
            None
        } else {
            Some(profile.apply(&current.permissions)?)
        };
        let agent = client
            .call::<EnvironmentAgentUpdate>(EnvironmentAgentUpdateParams {
                environment_id,
                agent_id: current.id,
                name,
                role,
                initiative,
                permissions,
            })
            .await?;
        print!("{}", describe_agent(&agent.agent));
        if !agent.started {
            println!(
                "Saved, but not running: {}",
                agent.reason.as_deref().unwrap_or("it has not started")
            );
        }
        Ok(())
    })
}

/// Deletes an agent; its session is archived.
pub fn agent_remove(env: String, agent: String) -> anyhow::Result<()> {
    block_on(async {
        let (mut client, _) = connect().await?;
        let environment_id = resolve(&mut client, &env).await?.id;
        let agent_id = resolve_agent(&mut client, &environment_id, &agent)
            .await?
            .id;
        client
            .call::<EnvironmentAgentDelete>(EnvironmentAgentRef {
                environment_id,
                agent_id,
            })
            .await?;
        println!("Deleted. Its session is archived.");
        Ok(())
    })
}

/// `off`, `suggest` or `act`, if given.
fn parse_initiative(
    text: Option<String>,
) -> anyhow::Result<Option<imperium_protocol::initiative::Initiative>> {
    text.map(|text| {
        imperium_protocol::initiative::Initiative::parse(&text)
            .ok_or_else(|| anyhow::anyhow!("initiative is off, suggest or act"))
    })
    .transpose()
}

pub(crate) async fn resolve_agent(
    client: &mut Client,
    environment_id: &str,
    name_or_id: &str,
) -> anyhow::Result<EnvironmentAgent> {
    let result = client
        .call::<EnvironmentAgentList>(EnvironmentAgentListParams {
            environment_id: environment_id.to_string(),
        })
        .await?;
    result
        .agents
        .iter()
        .find(|agent| agent.id == name_or_id || agent.name.eq_ignore_ascii_case(name_or_id))
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("no agent {name_or_id:?}; see imperium env agent list"))
}

fn describe_agent(agent: &EnvironmentAgent) -> String {
    let session = agent.session_id.as_deref().unwrap_or("not started");
    let mut text = format!(
        "{}  {}  (session: {session}, initiative: {})\n",
        crate::initiative::plain(&agent.id),
        crate::initiative::plain(&agent.name),
        agent.initiative.as_str()
    );
    if !agent.role.is_empty() {
        text.push_str(&format!(
            "    role: {}\n",
            crate::initiative::plain(&agent.role)
        ));
    }
    if let Some(limits) = describe_permissions(&agent.permissions) {
        text.push_str(&format!("    limited to: {limits}\n"));
    }
    text
}

/// What a profile restricts, or `None` when it restricts nothing.
fn describe_permissions(permissions: &AgentPermissions) -> Option<String> {
    if *permissions == AgentPermissions::default() {
        return None;
    }
    if permissions.local_files_only {
        return Some(
            "local files only (no web, MCP, plugin or command tools; files kept to the environment's folders and its scratch folder)"
                .into(),
        );
    }
    let part = |label: &str, access: ToolAccess, names: &[String]| match access {
        ToolAccess::All => None,
        ToolAccess::None => Some(format!("{label}: none")),
        ToolAccess::Named => Some(format!("{label}: {}", names.join(", "))),
    };
    let parts: Vec<String> = [
        part("MCP servers", permissions.mcp, &permissions.mcp_servers),
        part("plugins", permissions.plugins, &permissions.plugin_names),
    ]
    .into_iter()
    .flatten()
    .collect();
    Some(parts.join("; "))
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
    if env.builtin {
        text.push_str(
            "    built in: the default for sessions started with no environment or project\n",
        );
    }
    if env.runtime != EnvironmentRuntime::Local {
        text.push_str(&format!("    runtime: {}\n", runtime_label(&env.runtime)));
    }
    for agent in &env.agents {
        let session = agent.session_id.as_deref().unwrap_or("not started");
        text.push_str(&format!("    agent {}: {session}\n", agent.name));
    }
    if env.folders.is_empty() && env.builtin {
        text.push_str(
            "    (no project folders: sessions work in their own scratch folders; add paths with imperium env add-folder)\n",
        );
    } else if env.folders.is_empty() {
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
            agents: Vec::new(),
            pet: None,
            pinned: false,
            permissions: Default::default(),
            project_worktrees: true,
            builtin: false,
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
        remote
            .agents
            .push(imperium_protocol::agents::EnvironmentAgent {
                id: "g1".into(),
                name: "Planner".into(),
                role: String::new(),
                session_id: None,
                initiative: Default::default(),
                permissions: AgentPermissions::default(),
            });
        let shown = describe(&remote);
        assert!(shown.contains("runtime: wsl Ubuntu") && shown.contains("agent Planner"));
        assert_eq!(absolute(Path::new("/srv/app")).unwrap(), "/srv/app");
    }

    #[test]
    fn profile_flags_build_a_permission_profile() {
        assert!(ProfileArgs::default().is_empty());
        let base = AgentPermissions::default();
        let both = ProfileArgs {
            mcp: Some("github, Slack".into()),
            plugins: Some("none".into()),
            ..ProfileArgs::default()
        }
        .apply(&base)
        .unwrap();
        assert_eq!(both.mcp, ToolAccess::Named);
        assert_eq!(both.mcp_servers, ["github", "Slack"]);
        assert_eq!(both.plugins, ToolAccess::None);
        assert!(!both.local_files_only);

        // A flag not given keeps what the agent has; others change it.
        let locked = ProfileArgs {
            local_only: true,
            ..ProfileArgs::default()
        }
        .apply(&both)
        .unwrap();
        assert_eq!(locked.mcp_servers, ["github", "Slack"]);
        assert!(locked.local_files_only);
        let open = ProfileArgs {
            mcp: Some("ALL".into()),
            no_local_only: true,
            ..ProfileArgs::default()
        }
        .apply(&locked)
        .unwrap();
        assert_eq!(open.mcp, ToolAccess::All);
        assert!(open.mcp_servers.is_empty());
        assert!(!open.local_files_only);
        assert!(
            ProfileArgs {
                plugins: Some(" , ".into()),
                ..ProfileArgs::default()
            }
            .apply(&base)
            .is_err()
        );

        assert_eq!(describe_permissions(&base), None);
        assert_eq!(
            describe_permissions(&both).unwrap(),
            "MCP servers: github, Slack; plugins: none"
        );
        assert!(
            describe_permissions(&locked)
                .unwrap()
                .starts_with("local files only")
        );
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
