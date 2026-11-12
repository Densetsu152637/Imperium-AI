//! Persistent agents of an environment: zero or more per environment, each with a name and a
//! role, each keeping one session that is always available (see
//! docs/14-projects-and-automation.md#persistent-agents-implemented).

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::methods::{Accepted, EnvironmentInfo, Method};

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
    type Result = EnvironmentAgent;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct EnvironmentAgentCreateParams {
    pub environment_id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub role: Option<String>,
}

/// `environments.agent_update`: renames an agent or changes its role. A changed role applies
/// from the agent's next start, which happens at once; its conversation continues.
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
