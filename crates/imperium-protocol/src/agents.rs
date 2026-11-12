//! Persistent agents of an environment: zero or more per environment, each with a name and a
//! role, each keeping one session that is always available (see
//! docs/14-projects-and-automation.md#persistent-agents-implemented).

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::initiative::Initiative;
use crate::methods::{Accepted, EnvironmentInfo, Method};

/// Which of a kind of outside tool an agent may use.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ToolAccess {
    // Everything the environment and session offer: no extra restriction.
    #[default]
    All,
    // None of them.
    None,
    // Only the ones named next to it.
    Named,
}

/// What an agent may use beyond the environment's own permissions. A profile only narrows: it
/// hides tools the agent would have had and refuses calls to them, and grants nothing the
/// environment, its permission mode or the safety floor deny. The default restricts nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AgentPermissions {
    // MCP servers (by their names in the configuration).
    #[serde(default)]
    pub mcp: ToolAccess,
    // The servers allowed when `mcp` is `named`.
    #[serde(default)]
    pub mcp_servers: Vec<String>,
    // Plugins the agent may activate and use the tools of.
    #[serde(default)]
    pub plugins: ToolAccess,
    // The plugins allowed when `plugins` is `named`.
    #[serde(default)]
    pub plugin_names: Vec<String>,
    // Local files only: no web, browser, search, media, MCP, plugin or other integration tools
    // and no commands, with file tools kept to the environment's folders and the agent's
    // scratch folder. Overrides the two lists above.
    #[serde(default)]
    pub local_files_only: bool,
}

/// One persistent agent of an environment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct EnvironmentAgent {
    pub id: String,
    /// Shown as the agent's session title and in the environment window; unique within the
    /// environment (ignoring case).
    pub name: String,
    /// What the agent is for, added to its instructions ("Keep the roadmap current"); may be
    /// empty.
    #[serde(default)]
    pub role: String,
    /// The agent's session id once it has started; kept when its session is archived.
    #[serde(default)]
    pub session_id: Option<String>,
    /// How much initiative the agent takes: `off` (the default, for new agents too), `suggest`
    /// or `act` (docs/14, Initiative). Nothing proactive happens unless it is turned on.
    #[serde(default)]
    pub initiative: Initiative,
    /// What the agent may use beyond the environment's own permissions; also binds the
    /// sessions and subagents it starts.
    #[serde(default)]
    pub permissions: AgentPermissions,
}

/// `environments.agent_list`: the environment's persistent agents.
#[derive(Debug)]
pub struct EnvironmentAgentList;

impl Method for EnvironmentAgentList {
    const NAME: &'static str = "environments.agent_list";
    type Params = EnvironmentAgentListParams;
    type Result = EnvironmentAgentListResult;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct EnvironmentAgentListParams {
    pub environment_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct EnvironmentAgentListResult {
    pub agents: Vec<EnvironmentAgent>,
}

/// `environments.agent_create`: adds a persistent agent. It starts at once (or as soon as the
/// environment has a folder). A new environment has none.
#[derive(Debug)]
pub struct EnvironmentAgentCreate;

impl Method for EnvironmentAgentCreate {
    const NAME: &'static str = "environments.agent_create";
    type Params = EnvironmentAgentCreateParams;
    type Result = EnvironmentAgentCreated;
}

/// The agent that was added, and whether it is running. An agent that could not start (the
/// environment has no folder yet, no model is configured) is still added and starts when the
/// cause is fixed; `reason` says what is missing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct EnvironmentAgentCreated {
    #[serde(flatten)]
    #[ts(flatten)]
    pub agent: EnvironmentAgent,
    pub started: bool,
    /// Why it is not running, when `started` is false.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct EnvironmentAgentCreateParams {
    pub environment_id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub role: Option<String>,
    /// Initiative to start with (`off` when left out).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub initiative: Option<Initiative>,
    /// The agent's permission profile; unrestricted when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub permissions: Option<AgentPermissions>,
}

/// `environments.agent_update`: renames an agent, changes its role or its permission profile. A
/// changed role applies from the agent's next start, which happens at once; its conversation
/// continues. A changed profile applies from the agent's next tool call, without a restart.
#[derive(Debug)]
pub struct EnvironmentAgentUpdate;

impl Method for EnvironmentAgentUpdate {
    const NAME: &'static str = "environments.agent_update";
    type Params = EnvironmentAgentUpdateParams;
    type Result = EnvironmentAgent;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct EnvironmentAgentUpdateParams {
    pub environment_id: String,
    pub agent_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub role: Option<String>,
    /// A new initiative; it applies from the agent's next look and restarts nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub initiative: Option<Initiative>,
    /// Replaces the agent's permission profile; it applies at once, to the agent's running
    /// session and to the sessions it started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub permissions: Option<AgentPermissions>,
}

/// `environments.agent_delete`: removes an agent. Its session stops and is archived, so its
/// history stays listed with the other archived sessions.
#[derive(Debug)]
pub struct EnvironmentAgentDelete;

impl Method for EnvironmentAgentDelete {
    const NAME: &'static str = "environments.agent_delete";
    type Params = EnvironmentAgentRef;
    type Result = Accepted;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct EnvironmentAgentRef {
    pub environment_id: String,
    pub agent_id: String,
}

/// The agent whose session is `session_id`, if the environment has one.
pub fn agent_of_session<'a>(
    env: &'a EnvironmentInfo,
    session_id: &str,
) -> Option<&'a EnvironmentAgent> {
    env.agents
        .iter()
        .find(|agent| agent.session_id.as_deref() == Some(session_id))
}
