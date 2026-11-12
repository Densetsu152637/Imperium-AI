//! Initiative: what an environment's persistent agent does on its own (see
//! docs/14-projects-and-automation.md#initiative-implemented). Each environment sets how much
//! initiative its agent takes; the agent reviews the environment when the machine and the
//! environment are idle and either suggests work for the user to start or, in `act`, starts
//! sessions itself within limits.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::methods::{Accepted, Method};

/// How much initiative an environment's persistent agent takes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum Initiative {
    /// The agent does nothing unless it is messaged.
    Off,
    /// The agent reviews the environment now and then and suggests work, and reminds the user
    /// what is left. It starts nothing.
    #[default]
    Suggest,
    /// As `suggest`, and the agent may start sessions itself, within limits.
    Act,
}

impl Initiative {
    pub fn as_str(self) -> &'static str {
        match self {
            Initiative::Off => "off",
            Initiative::Suggest => "suggest",
            Initiative::Act => "act",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "off" => Some(Initiative::Off),
            "suggest" => Some(Initiative::Suggest),
            "act" => Some(Initiative::Act),
            _ => None,
        }
    }
}

/// Where a suggestion stands.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum SuggestionStatus {
    /// Waiting for the user.
    #[default]
    Open,
    /// The user started it (`session_id` says where).
    Started,
    /// The user dismissed it; the agent does not suggest the same thing again for a while.
    Dismissed,
}

/// One piece of work the agent suggests: a prompt the user can start as a session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct WorkSuggestion {
    pub id: String,
    /// A short name for the work.
    pub title: String,
    /// What the session is told, worded as the user would ("improve my timeline").
    pub prompt: String,
    /// Why now, in a sentence.
    pub reason: String,
    /// What the agent read to find it: a file and section, an issue, a session.
    pub source: String,
    /// When it was suggested, in milliseconds since the Unix epoch.
    #[ts(type = "number")]
    pub created_at: u64,
    #[serde(default)]
    pub status: SuggestionStatus,
    /// The session it was started as.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub session_id: Option<String>,
}

/// One line of a work-left digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct DigestItem {
    pub text: String,
    /// Where it comes from: a roadmap entry, a plan file, an unfinished session.
    pub source: String,
}

/// The agent's reminder of the work left in an environment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct WorkDigest {
    #[ts(type = "number")]
    pub created_at: u64,
    pub summary: String,
    pub items: Vec<DigestItem>,
}

/// Where an environment's initiative stands, for its panel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct InitiativeState {
    pub environment_id: String,
    pub mode: Initiative,
    /// A review is running now.
    pub reviewing: bool,
    /// Why the agent is not reviewing at the moment (for example the machine is busy or the
    /// persistent agent is off); none when it will on its next look.
    #[serde(default)]
    pub waiting: Option<String>,
    #[serde(default)]
    #[ts(type = "number | null")]
    pub last_review_at: Option<u64>,
    #[serde(default)]
    #[ts(type = "number | null")]
    pub next_review_at: Option<u64>,
    /// Why the last review failed, if it did.
    #[serde(default)]
    pub last_error: Option<String>,
    /// Open suggestions first, then the latest started and dismissed ones.
    pub suggestions: Vec<WorkSuggestion>,
    /// The latest work-left digest.
    #[serde(default)]
    pub digest: Option<WorkDigest>,
    /// Sessions the agent started on its own today, and the limits (`act` mode).
    pub started_today: u32,
    pub max_per_day: u32,
    /// Sessions it started that are still running, and the limit.
    pub running: u32,
    pub max_concurrent: u32,
}

/// `initiative.get`: where an environment's initiative stands.
#[derive(Debug)]
pub struct InitiativeGet;

impl Method for InitiativeGet {
    const NAME: &'static str = "initiative.get";
    type Params = InitiativeRef;
    type Result = InitiativeState;
}

/// Names one environment.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct InitiativeRef {
    pub environment_id: String,
}

/// `initiative.start`: starts an open suggestion as a session, as if the user had typed its
/// prompt. The session is the user's own (not background) and runs in their permission mode.
#[derive(Debug)]
pub struct InitiativeStart;

impl Method for InitiativeStart {
    const NAME: &'static str = "initiative.start";
    type Params = InitiativeSuggestionRef;
    type Result = InitiativeStartResult;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct InitiativeSuggestionRef {
    pub environment_id: String,
    pub suggestion_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct InitiativeStartResult {
    pub session_id: String,
}

/// `initiative.dismiss`: dismisses a suggestion.
#[derive(Debug)]
pub struct InitiativeDismiss;

impl Method for InitiativeDismiss {
    const NAME: &'static str = "initiative.dismiss";
    type Params = InitiativeSuggestionRef;
    type Result = Accepted;
}

/// `initiative.review`: asks the agent to review the environment now. A person's request does
/// not wait for the machine or the environment to be idle, nor for the review interval. The
/// result arrives as `initiative.changed`.
#[derive(Debug)]
pub struct InitiativeReview;

impl Method for InitiativeReview {
    const NAME: &'static str = "initiative.review";
    type Params = InitiativeRef;
    type Result = Accepted;
}

/// Notification: an environment's initiative changed; params are [`InitiativeState`].
pub const INITIATIVE_CHANGED: &str = "initiative.changed";

/// Notification: something the user should hear about; params are [`InitiativeNotice`].
pub const INITIATIVE_NOTICE: &str = "initiative.notice";

/// What a notice is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum NoticeKind {
    /// New suggestions are waiting.
    Suggestions,
    /// A work-left digest.
    Digest,
    /// The agent started a session.
    Started,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct InitiativeNotice {
    pub environment_id: String,
    pub environment_name: String,
    pub kind: NoticeKind,
    pub title: String,
    pub body: String,
    /// The session the agent started, for `started`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub session_id: Option<String>,
}

/// Who started a session besides the user: shown as "started by <environment> agent".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct StartedBy {
    pub environment_id: String,
    pub environment_name: String,
    /// `initiative` (the agent chose to, in `act`), `suggestion` (the user started one of its
    /// suggestions), or `agent` (the agent started it while working for the user).
    pub kind: String,
    /// Why, in a sentence.
    pub reason: String,
    /// What the agent read to decide it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub source: Option<String>,
}
