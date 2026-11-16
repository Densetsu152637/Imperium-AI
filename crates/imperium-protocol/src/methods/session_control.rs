use super::*;

/// `session.suggestion_respond`: answers a `model_suggestion` event. `download` starts the
/// download (as `models.download` would) and returns its job; `dismiss` says not now (the
/// session does not suggest for that need again); `never` turns `models.suggest_downloads` off
/// in the user config file. Each is followed by a `model_suggestion_update` event.
#[derive(Debug)]
pub struct SessionSuggestionRespond;

impl Method for SessionSuggestionRespond {
    const NAME: &'static str = "session.suggestion_respond";
    type Params = SuggestionRespondParams;
    type Result = SuggestionRespondResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SuggestionRespondParams {
    pub session_id: String,
    pub suggestion_id: String,
    #[ts(type = "\"download\" | \"dismiss\" | \"never\"")]
    pub action: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SuggestionRespondResult {
    /// For `download`: the job, whose progress `models.download_progress` reports.
    #[serde(default)]
    pub job: Option<DownloadJob>,
}

/// `session.browser_control`: the user takes over the session's browser (`user` true) or gives
/// it back to the agent (`user` false). While the user drives, the agent's browser tools are
/// refused with a message saying so, and `browser_control` events tell every client. Taking
/// over starts the browser if it is not running, and returns its page.
#[derive(Debug)]
pub struct SessionBrowserControl;

impl Method for SessionBrowserControl {
    const NAME: &'static str = "session.browser_control";
    type Params = BrowserControlParams;
    type Result = BrowserControlResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct BrowserControlParams {
    pub session_id: String,
    pub user: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct BrowserControlResult {
    /// The browser's page after taking over; absent when giving back.
    #[serde(default)]
    pub view: Option<BrowserPage>,
}

/// The browser's page: URL, title, and a JPEG screenshot (base64), with the tab it belongs to
/// and every tab that is open.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct BrowserPage {
    pub url: String,
    pub title: String,
    pub screenshot: String,
    /// The tab this page is, as `browser_tabs` and `browser_switch` name it. Absent from a
    /// daemon older than protocol 0.35.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub tab: Option<String>,
    /// Every open tab, in the order they were opened, the one shown included. Absent from a
    /// daemon older than protocol 0.35.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(as = "Option<Vec<BrowserTab>>", optional)]
    pub tabs: Vec<BrowserTab>,
}

/// One tab of the session's browser.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct BrowserTab {
    /// Short, and unique among the open tabs for as long as the tab is open.
    pub id: String,
    pub url: String,
    pub title: String,
    /// Whether this is the tab the browser tools and `session.browser_act` work on.
    pub active: bool,
}

/// `session.browser_act`: an action of the user in the browser they have taken over
/// (`session.browser_control`); refused when the user is not driving. Returns the page after
/// the action, which is also sent as a `browser_view` event.
#[derive(Debug)]
pub struct SessionBrowserAct;

impl Method for SessionBrowserAct {
    const NAME: &'static str = "session.browser_act";
    type Params = BrowserActParams;
    type Result = BrowserActResult;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct BrowserActParams {
    pub session_id: String,
    pub action: BrowserAction,
}

/// What the user does in the browser. `click` positions are fractions (0 to 1) of the page's
/// visible width and height, so a client scales a click on the screenshot without knowing the
/// page's size.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export)]
pub enum BrowserAction {
    /// Go to an http or https URL.
    Navigate {
        url: String,
    },
    Back,
    Forward,
    Reload,
    Click {
        x: f64,
        y: f64,
    },
    /// Type text into the focused element.
    Type {
        text: String,
    },
    /// Press a named key such as `Enter`, `Tab`, `Backspace`, or `Escape`.
    Key {
        key: String,
    },
    /// Scroll the page by `dy` pixels (positive is down).
    Scroll {
        dy: f64,
    },
    /// Open a new tab (at `url` when given) and make it the active one. Refused when the
    /// browser already has its most tabs open.
    NewTab {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        url: Option<String>,
    },
    /// Make an open tab the active one.
    SwitchTab {
        tab: String,
    },
    /// Close a tab. The last tab cannot be closed; when the active tab is closed a neighbour
    /// becomes the active one.
    CloseTab {
        tab: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct BrowserActResult {
    pub view: BrowserPage,
}

/// Notification sent to every client when a session starts, whoever started it (a client, a
/// schedule, a project agent, or auto-fix). The payload is a [`SessionSummary`].
pub const SESSION_CREATED: &str = "session.created";

/// Notification sent to every client when a session's listing changes outside its event
/// stream (such as being pinned). The payload is a [`SessionSummary`].
pub const SESSION_UPDATED: &str = "session.updated";

/// `session.control`: change how a session waits for usage resets, resume it now, set what
/// pull request it watches, switch its model, compact its conversation, pin it, or rename it.
#[derive(Debug)]
pub struct SessionControl;

impl Method for SessionControl {
    const NAME: &'static str = "session.control";
    type Params = SessionControlParams;
    type Result = SessionControlResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionControlParams {
    pub session_id: String,
    /// `set_resume_policy` (with `resume_policy`, or none to use the default), `resume_now`,
    /// `watch` (with `repo` and `pull_request`), `unwatch`, `set_model` (with `model`, or none
    /// for `models.default`; refused while a turn runs), `set_effort` (with `effort`; applies
    /// from every agent's next model call, also while a turn runs; kept across restarts),
    /// `set_permission_mode` (with `permission_mode`: `ask`, `default`, `accept_edits`, `plan`,
    /// or `full`; applies from every agent's next call, also while a turn runs; kept across
    /// restarts), `compact` (summarise older turns
    /// now; refused while a turn runs), `set_pinned` (with `pinned`; kept across restarts),
    /// `rename` (with `title`; kept across restarts and never replaced by a generated title; an
    /// empty title lets the session be titled automatically again), `archive` (stop the session and keep its journal; announced with `session.updated`),
    /// `unarchive` (resume an archived session), `stop` (halt the root and every subagent at
    /// their next safe point; queued messages wait; kept across restarts; see `work_stopped`),
    /// or `resume` (continue a stopped session; not to be confused with `resume_now`, which ends
    /// a usage-limit wait).
    #[ts(
        type = "\"set_resume_policy\" | \"resume_now\" | \"watch\" | \"unwatch\" | \"set_model\" | \"set_effort\" | \"set_permission_mode\" | \"compact\" | \"set_pinned\" | \"rename\" | \"archive\" | \"unarchive\" | \"stop\" | \"resume\""
    )]
    pub action: String,
    #[serde(default)]
    pub resume_policy: Option<crate::session::ResumePolicy>,
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    #[ts(type = "number | null")]
    pub pull_request: Option<u64>,
    /// For `set_model`: a model table id or an `<endpoint>:<model>` spec.
    #[serde(default)]
    pub model: Option<String>,
    /// For `set_effort`: how hard models that can reason think.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "\"low\" | \"medium\" | \"high\" | \"max\"")]
    pub effort: Option<String>,
    /// For `set_permission_mode`: the session's new permission mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(
        optional,
        type = "\"ask\" | \"default\" | \"accept_edits\" | \"plan\" | \"full\""
    )]
    pub permission_mode: Option<String>,
    /// For `set_pinned`: whether the session is pinned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub pinned: Option<bool>,
    /// For `rename`: the new title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub title: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionControlResult {
    /// For `set_model` and `compact`: the session's model afterwards.
    #[serde(default)]
    pub model: Option<String>,
    /// For `set_effort`: the session's effort afterwards.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "\"low\" | \"medium\" | \"high\" | \"max\"")]
    pub effort: Option<String>,
    /// For `set_permission_mode`: the session's permission mode afterwards.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(
        optional,
        type = "\"ask\" | \"default\" | \"accept_edits\" | \"plan\" | \"full\""
    )]
    pub permission_mode: Option<String>,
    /// For `set_model` and `set_effort`: whether the session's model can reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub reasoning: Option<bool>,
    /// For `set_model` and `compact`: the conversation measured for that model's window. For
    /// `set_model` it is an estimate (`method` `estimated`) against the window the model is
    /// expected to have, as the model is not loaded or called until the session's next turn,
    /// which measures it exactly and compacts the conversation then if it does not fit.
    #[serde(default)]
    pub context: Option<ContextFit>,
}

/// A conversation measured against a model's context window, and any compaction it needed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ContextFit {
    /// The conversation's size as the model will read it (after any compaction).
    #[ts(type = "number")]
    pub tokens: u64,
    /// Absent when the window is unknown; the conversation was then not checked.
    #[ts(type = "number | null")]
    pub context_window: Option<u64>,
    /// The tokens the conversation may use: the window less room for the reply.
    #[ts(type = "number | null")]
    pub budget: Option<u64>,
    pub method: crate::session::TokenCountMethod,
    /// Present when older turns were summarised to fit.
    pub compaction: Option<Compaction>,
}

/// What compacting a conversation did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Compaction {
    #[ts(type = "number")]
    pub before_tokens: u64,
    #[ts(type = "number")]
    pub after_tokens: u64,
    pub summarised_messages: u32,
    /// The model that wrote the summary.
    pub summary_model: String,
}
