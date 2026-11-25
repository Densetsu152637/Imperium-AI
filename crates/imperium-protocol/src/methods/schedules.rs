use super::*;

/// `schedules.list`: configured schedules with their next and last runs.
#[derive(Debug)]
pub struct ScheduleList;

impl Method for ScheduleList {
    const NAME: &'static str = "schedules.list";
    type Params = ScheduleListParams;
    type Result = ScheduleListResult;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ScheduleListParams {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ScheduleListResult {
    pub schedules: Vec<ScheduleRow>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ScheduleRow {
    pub id: String,
    pub enabled: bool,
    /// The trigger as written, such as `cron 0 8 * * MON-FRI`.
    pub trigger: String,
    /// `prompt` or `command`.
    pub action: String,
    pub project: String,
    /// The environment the schedule runs in (`action.environment`); none for a schedule that
    /// works in a bare `project` folder.
    #[serde(default)]
    pub environment: Option<String>,
    /// RFC 3339 local times.
    pub next_run: Option<String>,
    pub last_run: Option<String>,
    pub last_ok: Option<bool>,
    pub running: bool,
    /// Set while a run waits for a quiet machine (`prefer_quiet`, `when_quiet`): why. Run it
    /// now with `schedules.run`.
    pub deferred: Option<String>,
    /// Why the schedule cannot run, if it cannot.
    pub problem: Option<String>,
}

/// `schedules.run`: start a schedule now.
#[derive(Debug)]
pub struct ScheduleRun;

impl Method for ScheduleRun {
    const NAME: &'static str = "schedules.run";
    type Params = ScheduleRunParams;
    type Result = ScheduleRunResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ScheduleRunParams {
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ScheduleRunResult {
    /// False when the schedule was already running and does not allow overlap.
    pub started: bool,
}

/// Notification sent to every client when a scheduled run starts or ends.
pub const SCHEDULE_EVENT: &str = "schedule.event";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ScheduleEventParams {
    pub schedule_id: String,
    #[ts(type = "\"deferred\" | \"started\" | \"completed\" | \"failed\"")]
    pub status: String,
    /// The session a prompt run created.
    pub session_id: Option<String>,
    pub summary: String,
}

/// Notification sent to every client when unattended work needs a yes or no.
pub const APPROVAL_REQUESTED: &str = "approval.requested";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ApprovalInfo {
    #[ts(type = "number")]
    pub id: u64,
    /// What is being approved, such as `push`.
    pub kind: String,
    pub summary: String,
    pub requested_at: String,
}

/// `approvals.list`: pending approval requests.
#[derive(Debug)]
pub struct ApprovalList;

impl Method for ApprovalList {
    const NAME: &'static str = "approvals.list";
    type Params = ApprovalListParams;
    type Result = ApprovalListResult;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ApprovalListParams {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ApprovalListResult {
    pub approvals: Vec<ApprovalInfo>,
}

/// `approvals.respond`: approve or decline a pending request.
#[derive(Debug)]
pub struct ApprovalRespond;

impl Method for ApprovalRespond {
    const NAME: &'static str = "approvals.respond";
    type Params = ApprovalRespondParams;
    type Result = ApprovalRespondResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ApprovalRespondParams {
    #[ts(type = "number")]
    pub id: u64,
    pub approve: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ApprovalRespondResult {
    /// False when the request was already answered or never existed.
    pub found: bool,
}

/// Notification sent to every client as auto-fix works on a pull request.
pub const AUTOFIX_EVENT: &str = "autofix.event";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AutofixEventParams {
    pub repo: String,
    #[ts(type = "number")]
    pub pull_request: u64,
    /// `started`, `awaiting_approval`, `pushed`, `declined`, `nothing`, `failed`, or `stopped`.
    pub status: String,
    pub session_id: Option<String>,
    pub summary: String,
}

/// `projects.list`: projects with a running project agent.
#[derive(Debug)]
pub struct ProjectList;

impl Method for ProjectList {
    const NAME: &'static str = "projects.list";
    type Params = ProjectListParams;
    type Result = ProjectListResult;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ProjectListParams {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ProjectListResult {
    pub agents: Vec<ProjectAgentRow>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ProjectAgentRow {
    pub project: String,
    /// The agent's own session, which can be opened like any other.
    pub session_id: String,
}

/// `projects.message`: wake a project agent with a message.
#[derive(Debug)]
pub struct ProjectMessage;

impl Method for ProjectMessage {
    const NAME: &'static str = "projects.message";
    type Params = ProjectMessageParams;
    type Result = ProjectMessageResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ProjectMessageParams {
    pub project: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ProjectMessageResult {
    /// False when the project has no running agent.
    pub delivered: bool,
}
