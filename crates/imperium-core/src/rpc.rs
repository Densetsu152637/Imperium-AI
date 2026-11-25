//! Request handling for one client connection.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use base64::Engine;
use imperium_engine::{QueueError, Session, SwitchError};
use imperium_protocol::agents::{
    EnvironmentAgentCreate, EnvironmentAgentCreateParams, EnvironmentAgentCreated,
    EnvironmentAgentDelete, EnvironmentAgentList, EnvironmentAgentListParams,
    EnvironmentAgentListResult, EnvironmentAgentRef, EnvironmentAgentUpdate,
    EnvironmentAgentUpdateParams,
};
use imperium_protocol::budgets::{BudgetsGet, BudgetsGetParams, BudgetsSet, BudgetsUsage};
use imperium_protocol::initiative::{
    InitiativeDismiss, InitiativeGet, InitiativeReview, InitiativeStart,
};
use imperium_protocol::jsonrpc::{ErrorObject, Request, RequestId, Response, codes};
use imperium_protocol::methods::{
    Accepted, ApprovalList, ApprovalListParams, ApprovalListResult, ApprovalRespond,
    ApprovalRespondParams, ApprovalRespondResult, BridgedToolInfo, Handshake, HandshakeParams,
    HandshakeResult, LocalModelList, LocalModelListParams, Method, ModInspect, ModInspectParams,
    ModInstall, ModInstallParams, ModList, ModListParams, ModelList, ModelListParams, Peer,
    PetInteract, PetInteractParams, PetInteractResult, PetList, PetListParams, PetListResult,
    PetStrip, PetStripParams, PetStripResult, PhoneDevices, PhoneDevicesParams, PhoneDevicesResult,
    PhonePair, PhonePairParams, PhonePairResult, PhoneRevoke, PhoneRevokeParams, PhoneRevokeResult,
    Ping, PingParams, PingResult, ProjectAgentRow, ProjectList, ProjectListParams,
    ProjectListResult, ProjectMessage, ProjectMessageParams, ProjectMessageResult, ScheduleList,
    ScheduleListParams, ScheduleListResult, ScheduleRun, ScheduleRunParams, ScheduleRunResult,
    SessionAnswer, SessionAnswerParams, SessionCreate, SessionCreateParams, SessionCreateResult,
    SessionDelete, SessionDeleteParams, SessionExport, SessionExportParams, SessionList,
    SessionListParams, SessionListResult, SessionRespond, SessionRespondParams, SessionRunTool,
    SessionRunToolParams, SessionRunToolResult, SessionSearch, SessionSearchParams, SessionSend,
    SessionSendParams, SessionSubscribe, SessionSubscribeParams, SessionSubscribeResult,
    SessionToolResult, SessionToolResultParams, SessionToolResultResult, SessionTools,
    SessionToolsParams, SessionToolsResult, Shutdown, ShutdownParams, ShutdownResult,
};
use imperium_protocol::methods::{ApiGet, ApiGetParams, ApiSet, ApiToken};
use imperium_protocol::methods::{
    EventsSubscribe, EventsSubscribeParams, EventsUnsubscribe, EventsUnsubscribeParams,
};
use imperium_protocol::methods::{
    PluginsApprovalChallenge, PluginsApprovalChallengeParams, PluginsApprove, PluginsApproveParams,
    PluginsBrowse, PluginsDeleteSecret, PluginsDeleteSecretParams, PluginsDismissSuggestion,
    PluginsDismissSuggestionParams, PluginsInspect, PluginsInspectFolder, PluginsInstall,
    PluginsList, PluginsListParams, PluginsMarketplaceAdd, PluginsMarketplaceList,
    PluginsMarketplaceListParams, PluginsMarketplaceRefresh, PluginsMarketplaceRemove,
    PluginsSetEnabled, PluginsSetEnabledParams, PluginsSetSecret, PluginsSetSecretParams,
    PluginsSuggestions, PluginsSuggestionsParams, PluginsUninstall, PluginsUpdate,
};
use imperium_protocol::methods::{
    SessionEvalCreate, SessionEvalCreateParams, SessionEvalResult, SessionEvalResultParams,
    SessionPromptPreview,
};
use imperium_protocol::methods::{
    SystemLease, SystemLeaseParams, SystemLeaseResult, SystemLifetime, SystemLifetimeParams,
};
use imperium_protocol::permissions::{
    PermissionsAddRule, PermissionsGet, PermissionsGetParams, PermissionsSet,
};
use imperium_protocol::session::AgentEvent;
use imperium_protocol::{PROTOCOL_VERSION, ProtocolVersion};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::broadcast;

use crate::environments::EnvironmentError;
use crate::state::{CreateError, DaemonState, SessionOptions};
use imperium_engine::ModelError;
use imperium_protocol::methods::{
    BuiltinCommand, SessionAttach, SessionBuiltin, SessionFiles, SessionInit, VoiceTranscribe,
};
use imperium_protocol::methods::{
    ConfigChanges, ConfigChangesParams, ConfigChangesResult, ConfigDescribe, ConfigDescribeParams,
    ConfigSet, ConfigSetParams, ConfigUndo, ConfigUndoParams, PersonalitiesList,
    PersonalitiesListParams,
};
use imperium_protocol::methods::{
    EnvironmentAddFolder, EnvironmentConfigGet, EnvironmentCreate, EnvironmentCreateParams,
    EnvironmentDelete, EnvironmentFolderParams, EnvironmentList, EnvironmentListParams,
    EnvironmentRef, EnvironmentRemoveFolder, EnvironmentTestRuntime, EnvironmentUpdate,
    EnvironmentUpdateParams,
};
use imperium_protocol::methods::{
    HfFiles, ModelDownload, ModelDownloadCancel, ModelDownloadCancelParams,
    ModelDownloadCancelResult, ModelDownloads, ModelDownloadsParams, ModelDownloadsResult,
};
use imperium_protocol::methods::{
    McpCatalog, McpCatalogParams, McpInstall, McpList, McpListParams, McpLogin, McpLogout,
    McpRemove, McpSave, McpSetEnabled, McpTest,
};
use imperium_protocol::methods::{
    ModelAddEndpoint, ModelAddEndpointParams, ModelFolderAdd, ModelFolderRemove, ModelTest,
    ModelUse, ModelUseParams, RuntimeInstall, RuntimeSetServer, RuntimeStatus,
    RuntimeUpdateExternal,
};
use imperium_protocol::methods::{
    ModelBench, ModelBenchCancel, ModelBenchCancelParams, ModelBenchCancelResult,
    ModelBenchResults, ModelBenchResultsParams,
};
use imperium_protocol::methods::{
    ModelConfig, ModelConfigParams, ModelSetConfig, ModelSetConfigParams,
};
use imperium_protocol::methods::{
    ModelUnload, ModelUnloadParams, ModelUnloadResult, SystemResources, SystemResourcesParams,
    SystemResourcesSubscribe, SystemResourcesSubscribeParams, SystemResourcesSubscribeResult,
};
use imperium_protocol::methods::{
    SessionAgentAsk, SessionAgentAskParams, SessionAgentAskResult, SessionAgentCancel,
    SessionAgentCancelParams, SessionAgentCancelResult, SessionAgentKnowledge,
    SessionAgentKnowledgeParams, SessionAgentKnowledgeResult, SessionAgentResume,
    SessionAgentResumeParams, SessionAgentResumeResult, SessionAgents, SessionAgentsParams,
    SessionAgentsResult,
};
use imperium_protocol::methods::{
    SessionBranch, SessionBranchParams, SessionBrowserAct, SessionBrowserControl, SessionCompare,
    SessionCompareParams, SessionComparePick, SessionComparePickParams, SessionSuggestionRespond,
};
use imperium_protocol::methods::{
    SessionCheckpoints, SessionCheckpointsParams, SessionReview, SessionReviewCommit,
    SessionReviewCommitParams, SessionReviewHunk, SessionReviewHunkParams, SessionReviewParams,
    SessionReviewPullRequest, SessionReviewPullRequestParams, SessionRewind, SessionRewindParams,
};
use imperium_protocol::methods::{SessionControl, SessionControlParams, SessionControlResult};
use imperium_protocol::methods::{
    SessionQueue, SessionQueueParams, SessionQueueResult, SessionSendResult,
};
use imperium_protocol::workspaces::{
    WorkspacesMove, WorkspacesOpen, WorkspacesWhere, WorkspacesWhereParams,
};

mod access;
mod config;
mod connectors;
mod environments;
mod errors;
mod models;
mod pets;
mod policy;
mod pull_requests;
mod sessions;
mod system;
use errors::*;

/// A session's events, to be forwarded to this connection as notifications.
#[derive(Debug)]
pub struct Subscription {
    pub session_id: String,
    pub events: broadcast::Receiver<AgentEvent>,
}

/// The next event of a subscribed session. When the session was restarted (its working folders
/// moved) the old one ends; this follows the session of the same id that took its place, so
/// the client keeps receiving. `None` when the session is gone.
pub async fn next_event(
    state: &DaemonState,
    subscription: &mut Subscription,
) -> Option<AgentEvent> {
    // One look at the replacement per end, so a session that really ended cannot loop.
    let mut followed = false;
    loop {
        match subscription.events.recv().await {
            Ok(event) => return Some(event),
            Err(broadcast::error::RecvError::Lagged(missed)) => {
                tracing::warn!(session = %subscription.session_id, missed, "client fell behind; events dropped");
            }
            Err(broadcast::error::RecvError::Closed) => {
                if followed {
                    return None;
                }
                followed = true;
                subscription.events = state.session(&subscription.session_id)?.subscribe();
            }
        }
    }
}

/// A reply that waits on something slow, such as the network.
pub type Deferred = Pin<Box<dyn Future<Output = Response> + Send>>;

/// A method's result that is still being worked out.
type Later = Pin<Box<dyn Future<Output = Result<Value, ErrorObject>> + Send>>;

/// The reply to one frame, plus any subscription the call started.
pub struct Handled {
    /// The reply, or `None` when `deferred` produces it.
    pub response: Option<Response>,
    pub subscription: Option<Subscription>,
    /// Sent when it resolves; the connection serves other calls meanwhile.
    pub deferred: Option<Deferred>,
}

impl std::fmt::Debug for Handled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Handled")
            .field("response", &self.response)
            .field("subscription", &self.subscription)
            .field("deferred", &self.deferred.is_some())
            .finish()
    }
}

/// One request on its way to the group of methods that handles it.
struct Dispatch<'a> {
    method: &'a str,
    params: Option<Value>,
    /// Work that replies later, set by [`call_later`].
    later: &'a mut Option<Later>,
    /// The subscription a call starts.
    subscription: &'a mut Option<Subscription>,
}

/// Per-connection protocol state. The first call must be `system.handshake`.
#[derive(Debug)]
pub struct Connection {
    state: Arc<DaemonState>,
    handshake_done: bool,
    /// For a paired phone: the only methods it may call.
    allowed: Option<Vec<&'static str>>,
    /// What the client called itself in the handshake.
    client_name: Option<String>,
    /// The evaluation sessions this connection made (a prompt preview's too, while it runs):
    /// deleted when it ends.
    eval_sessions: Arc<EvalSessions>,
    /// The process id of the other end and when (Unix seconds) the connection was accepted, for
    /// a connection on the local socket.
    peer: Option<(u32, u64)>,
    /// The identity of the other end of a local-socket connection, for plugin approvals.
    identity: Option<Arc<crate::app_identity::Peer>>,
    /// The plugin-approval challenges issued to this connection and not yet answered.
    challenges: crate::app_identity::Challenges,
    /// Set while this connection watches resources (`system.resources_subscribe`).
    resources: Option<crate::resources::Watch>,
    /// The topics chosen with `events.subscribe`; narrows what `receives` lets through.
    events: crate::events::EventFilter,
    /// Held from the first `pull_requests.*` call until the connection ends.
    pr_watch: Option<crate::pr_monitor::Watch>,
    /// Held from `system.lease` until the connection ends.
    lease: Option<crate::lifetime::LeaseGuard>,
}

impl Connection {
    /// The daemon this connection talks to.
    pub(crate) fn state(&self) -> Arc<DaemonState> {
        Arc::clone(&self.state)
    }

    pub fn new(state: Arc<DaemonState>) -> Self {
        Connection {
            state,
            handshake_done: false,
            allowed: None,
            client_name: None,
            eval_sessions: Arc::default(),
            peer: None,
            identity: None,
            challenges: crate::app_identity::Challenges::default(),
            resources: None,
            events: crate::events::EventFilter::default(),
            pr_watch: None,
            lease: None,
        }
    }

    /// This connection, with the process id of its other end and when it was accepted (a
    /// local-socket connection).
    #[must_use]
    pub fn with_peer(mut self, peer: Option<(u32, u64)>) -> Self {
        self.peer = peer;
        self
    }

    /// This connection, with the identity of its other end taken at accept (a local-socket
    /// connection).
    #[must_use]
    pub(crate) fn with_identity(
        mut self,
        identity: Option<Arc<crate::app_identity::Peer>>,
    ) -> Self {
        self.identity = identity;
        self
    }

    /// A connection limited to `allowed` methods (a paired phone's scopes).
    pub fn restricted(state: Arc<DaemonState>, allowed: Vec<&'static str>) -> Self {
        Connection {
            state,
            handshake_done: false,
            allowed: Some(allowed),
            client_name: None,
            eval_sessions: Arc::default(),
            peer: None,
            identity: None,
            challenges: crate::app_identity::Challenges::default(),
            resources: None,
            events: crate::events::EventFilter::default(),
            pr_watch: None,
            lease: None,
        }
    }

    /// Whether this connection may start evaluation sessions: it is not a paired phone's, it
    /// says it is the `imperium` command, and it came in on the local socket (a page in the
    /// app's window, such as a mod's, reaches the daemon over loopback TCP with the token and
    /// can call itself anything).
    pub(crate) fn may_eval(&self) -> bool {
        self.allowed.is_none()
            && self.client_name.as_deref() == Some(imperium_protocol::methods::EVAL_CLIENT)
            && (self.peer.is_some() || self.state.eval_over_tcp())
    }

    /// Whether this connection wants `system.resources_changed` notifications.
    pub fn watches_resources(&self) -> bool {
        self.resources.is_some()
    }

    /// Whether a notification goes to this connection: resource snapshots only to connections
    /// that asked for them, and benchmark progress (model ids, error text) never to a restricted
    /// connection such as a paired phone. The connection's own topic choice
    /// (`events.subscribe`) is applied on top of that, so it can only narrow the result.
    pub fn receives(&self, notification: &imperium_protocol::jsonrpc::Notification) -> bool {
        let permitted = match notification.method.as_str() {
            imperium_protocol::methods::RESOURCES_CHANGED => self.watches_resources(),
            imperium_protocol::methods::MODEL_BENCH_PROGRESS => self.allowed.is_none(),
            // Suggestions, reasons and sources are the user's own business, not a phone's.
            imperium_protocol::initiative::INITIATIVE_CHANGED
            | imperium_protocol::initiative::INITIATIVE_NOTICE => self.allowed.is_none(),
            _ => true,
        };
        permitted && self.events.allows(&notification.method)
    }

    /// Handles one text frame.
    pub fn handle_text(&mut self, text: &str) -> Handled {
        let value: Value = match serde_json::from_str(text) {
            Ok(value) => value,
            Err(error) => {
                return reply(Response::failure(
                    None,
                    ErrorObject::new(codes::PARSE_ERROR, format!("invalid JSON: {error}")),
                ));
            }
        };
        // Recover the id, if readable, so even an invalid request gets a matched error.
        let id = value
            .get("id")
            .and_then(|id| serde_json::from_value::<RequestId>(id.clone()).ok());
        match serde_json::from_value::<Request>(value) {
            Ok(request) => self.handle(request),
            Err(error) => reply(Response::failure(
                id,
                ErrorObject::new(codes::INVALID_REQUEST, format!("invalid request: {error}")),
            )),
        }
    }

    fn handle(&mut self, request: Request) -> Handled {
        let mut subscription: Option<Subscription> = None;
        let mut later: Option<Later> = None;
        let params = request.params;
        let outcome = match request.method.as_str() {
            Handshake::NAME => call::<Handshake>(params, |params| self.handshake(params)),
            _ if !self.handshake_done => Err(ErrorObject::new(
                codes::HANDSHAKE_REQUIRED,
                "call system.handshake first",
            )),
            method
                if self
                    .allowed
                    .as_ref()
                    .is_some_and(|allowed| !allowed.contains(&method)) =>
            {
                Err(ErrorObject::new(
                    codes::FORBIDDEN,
                    format!("{method} is not allowed for this device"),
                ))
            }
            method => self.dispatch(Dispatch {
                method,
                params,
                later: &mut later,
                subscription: &mut subscription,
            }),
        };
        let id = request.id;
        if let (Ok(_), Some(later)) = (&outcome, later) {
            return Handled {
                response: None,
                subscription,
                deferred: Some(Box::pin(async move {
                    match later.await {
                        Ok(result) => Response::success(id, result),
                        Err(error) => Response::failure(Some(id), error),
                    }
                })),
            };
        }
        let response = match outcome {
            Ok(result) => Response::success(id, result),
            Err(error) => Response::failure(Some(id), error),
        };
        Handled {
            response: Some(response),
            subscription,
            deferred: None,
        }
    }

    /// Hands the request to the group that knows its method.
    fn dispatch(&mut self, mut c: Dispatch<'_>) -> Result<Value, ErrorObject> {
        if let Some(outcome) = self.dispatch_system(&mut c) {
            return outcome;
        }
        if let Some(outcome) = self.dispatch_access(&mut c) {
            return outcome;
        }
        if let Some(outcome) = self.dispatch_config(&mut c) {
            return outcome;
        }
        if let Some(outcome) = self.dispatch_connectors(&mut c) {
            return outcome;
        }
        if let Some(outcome) = self.dispatch_policy(&mut c) {
            return outcome;
        }
        if let Some(outcome) = self.dispatch_pets(&mut c) {
            return outcome;
        }
        if let Some(outcome) = self.dispatch_models(&mut c) {
            return outcome;
        }
        if let Some(outcome) = self.dispatch_environments(&mut c) {
            return outcome;
        }
        if let Some(outcome) = self.dispatch_pull_requests(&mut c) {
            return outcome;
        }
        if let Some(outcome) = self.dispatch_sessions(&mut c) {
            return outcome;
        }
        Err(ErrorObject::new(
            codes::METHOD_NOT_FOUND,
            format!("unknown method {:?}", c.method),
        ))
    }

    fn handshake(&mut self, params: HandshakeParams) -> Result<HandshakeResult, ErrorObject> {
        if !PROTOCOL_VERSION.is_compatible_with(params.protocol) {
            return Err(incompatible(params.protocol));
        }
        self.handshake_done = true;
        self.client_name = Some(params.client.name.clone());
        Ok(HandshakeResult {
            daemon: Peer {
                name: "imperium-core".into(),
                version: env!("CARGO_PKG_VERSION").into(),
            },
            protocol: PROTOCOL_VERSION,
        })
    }
}

impl Drop for Connection {
    /// The evaluation sessions this connection made go with it, so a client that vanishes
    /// leaves no session running (or the model's key held).
    fn drop(&mut self) {
        self.eval_sessions.close(&self.state);
    }
}

/// The evaluation sessions one connection made, shared with the previews it started (which run
/// on after the request returns). Closing it deletes them all, and any registered later.
#[derive(Debug, Default)]
pub(crate) struct EvalSessions {
    inner: std::sync::Mutex<EvalSessionList>,
}

#[derive(Debug, Default)]
struct EvalSessionList {
    closed: bool,
    ids: Vec<String>,
}

impl EvalSessions {
    fn list(&self) -> std::sync::MutexGuard<'_, EvalSessionList> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Takes `id` on; or, when the connection has already ended, deletes it at once and says
    /// so (`false`).
    pub(crate) fn add(&self, state: &DaemonState, id: &str) -> bool {
        let mut list = self.list();
        if list.closed {
            drop(list);
            let _ = state.delete_session(id, false);
            false
        } else {
            list.ids.push(id.to_string());
            true
        }
    }

    /// Lets go of `id`: its owner deleted it.
    pub(crate) fn remove(&self, id: &str) {
        self.list().ids.retain(|known| known != id);
    }

    /// The connection ended: its sessions go, and so does any made after this.
    fn close(&self, state: &DaemonState) {
        let ids = {
            let mut list = self.list();
            list.closed = true;
            std::mem::take(&mut list.ids)
        };
        for id in ids {
            let _ = state.delete_session(&id, false);
        }
    }
}

fn reply(response: Response) -> Handled {
    Handled {
        response: Some(response),
        subscription: None,
        deferred: None,
    }
}
/// The longest `session.tool_result` waits, in milliseconds.
const TOOL_RESULT_WAIT_MS: u64 = 5_000;

/// Decodes a method's params (absent params mean `{}`), runs it, and encodes its result.
fn call<M: Method>(
    params: Option<Value>,
    run: impl FnOnce(M::Params) -> Result<M::Result, ErrorObject>,
) -> Result<Value, ErrorObject> {
    run(decode::<M>(params)?).map(|result| to_value(&result))
}

/// Runs blocking work (git, file reads) on the blocking pool; its error message becomes an
/// invalid-params error.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, ErrorObject> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|error| ErrorObject::new(codes::INTERNAL_ERROR, error.to_string()))?
        .map_err(|message| ErrorObject::new(codes::INVALID_PARAMS, message))
}

/// Like [`call`] for a method whose result takes a while: decodes the params now and leaves
/// the work in `later`, for the connection to reply when it is done.
fn call_later<M: Method, F>(
    params: Option<Value>,
    later: &mut Option<Later>,
    run: impl FnOnce(M::Params) -> F,
) -> Result<Value, ErrorObject>
where
    F: Future<Output = Result<M::Result, ErrorObject>> + Send + 'static,
    M::Result: Send,
{
    let work = run(decode::<M>(params)?);
    *later = Some(Box::pin(async move {
        work.await.map(|result| to_value(&result))
    }));
    Ok(Value::Null)
}

/// A working-folder request the daemon refused, with the reason.
fn refused(message: String) -> ErrorObject {
    ErrorObject::new(codes::INVALID_PARAMS, message)
}

fn decode<M: Method>(params: Option<Value>) -> Result<M::Params, ErrorObject> {
    serde_json::from_value(params.unwrap_or_else(|| json!({}))).map_err(|error| {
        ErrorObject::new(
            codes::INVALID_PARAMS,
            format!("invalid params for {}: {error}", M::NAME),
        )
    })
}

fn to_value(result: &impl Serialize) -> Value {
    serde_json::to_value(result).expect("protocol result types serialise to JSON")
}

#[cfg(test)]
mod tests {
    use super::*;
    use imperium_config::{Config, Layer, Paths};
    use imperium_protocol::jsonrpc::Outcome;

    fn connection() -> Connection {
        let settings = Config::from_layers([Layer::defaults()]).settings().unwrap();
        // Environments are saved in the data folder, so each connection gets its own.
        let home: &'static tempfile::TempDir = Box::leak(Box::new(tempfile::tempdir().unwrap()));
        Connection::new(DaemonState::new(Paths::under(home.path()), settings, None))
    }

    fn result_of(handled: &Handled) -> Value {
        match &handled
            .response
            .as_ref()
            .expect("an immediate reply")
            .outcome
        {
            Outcome::Result(result) => result.clone(),
            Outcome::Error(error) => panic!("expected a result, got {error:?}"),
        }
    }

    /// The reply of a call that may answer later (an awaited deferred reply).
    async fn settle(handled: Handled) -> Handled {
        match handled.deferred {
            Some(later) => Handled {
                response: Some(later.await),
                subscription: None,
                deferred: None,
            },
            None => handled,
        }
    }

    fn request(id: i64, method: &str, params: Value) -> String {
        json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string()
    }

    fn handshake_params(major: u32) -> Value {
        json!({
            "client": {"name": "test", "version": "0"},
            "protocol": {"major": major, "minor": 0}
        })
    }

    fn error_code(handled: &Handled) -> Option<i32> {
        let response = handled.response.as_ref().expect("an immediate reply");
        match &response.outcome {
            Outcome::Error(error) => Some(error.code),
            Outcome::Result(_) => None,
        }
    }

    #[test]
    fn a_paired_phone_cannot_inspect_or_install_mods() {
        let settings = Config::from_layers([Layer::defaults()]).settings().unwrap();
        let home = tempfile::tempdir().unwrap();
        let state = DaemonState::new(Paths::under(home.path()), settings, None);
        // A phone's scopes are an allowlist; mods.* is never in it.
        let mut phone = Connection::restricted(state, vec![Handshake::NAME, "session.list"]);
        let handled = phone.handle_text(&request(
            1,
            Handshake::NAME,
            handshake_params(PROTOCOL_VERSION.major),
        ));
        assert_eq!(error_code(&handled), None);
        for method in ["mods.inspect", "mods.install"] {
            let handled = phone.handle_text(&request(2, method, json!({"source": "x"})));
            assert_eq!(error_code(&handled), Some(codes::FORBIDDEN), "{method}");
        }
    }

    #[test]
    fn a_paired_phone_cannot_inspect_a_plugin_folder() {
        let settings = Config::from_layers([Layer::defaults()]).settings().unwrap();
        let home = tempfile::tempdir().unwrap();
        let state = DaemonState::new(Paths::under(home.path()), settings, None);
        let mut phone = Connection::restricted(state, vec![Handshake::NAME, "session.list"]);
        let handled = phone.handle_text(&request(
            1,
            Handshake::NAME,
            handshake_params(PROTOCOL_VERSION.major),
        ));
        assert_eq!(error_code(&handled), None);
        let handled = phone.handle_text(&request(
            2,
            PluginsInspectFolder::NAME,
            json!({"name": "p"}),
        ));
        assert_eq!(error_code(&handled), Some(codes::FORBIDDEN));
    }

    /// Evaluation sessions (replay, variants, a model of their own) are for the `imperium`
    /// command: neither another client nor a phone may start one or read one's record.
    #[test]
    fn evaluation_sessions_are_for_the_imperium_command_only() {
        let eval = json!({
            "project_dir": ".",
            "options": {"model": {"base_url": "http://127.0.0.1:1/v1", "model_id": "m"}}
        });
        let named = |name: &str| {
            json!({
                "client": {"name": name, "version": "0"},
                "protocol": {"major": PROTOCOL_VERSION.major, "minor": 0}
            })
        };
        let methods = [
            (SessionEvalCreate::NAME, eval.clone()),
            (SessionEvalResult::NAME, json!({"session_id": "x"})),
        ];
        // (`session.prompt_preview` answers later; the command-level test covers it.)
        // Another full-token client.
        let mut other = connection();
        other.handle_text(&request(1, Handshake::NAME, named("imperium-desktop")));
        for (method, params) in &methods {
            let handled = other.handle_text(&request(2, method, params.clone()));
            assert_eq!(error_code(&handled), Some(codes::FORBIDDEN), "{method}");
        }
        // A phone that calls itself the command: its scopes do not list the methods.
        let settings = Config::from_layers([Layer::defaults()]).settings().unwrap();
        let home = tempfile::tempdir().unwrap();
        let state = DaemonState::new(Paths::under(home.path()), settings, None);
        let mut phone = Connection::restricted(state, vec![Handshake::NAME, "session.list"]);
        phone.handle_text(&request(1, Handshake::NAME, named("imperium-cli")));
        for (method, params) in &methods {
            let handled = phone.handle_text(&request(2, method, params.clone()));
            assert_eq!(error_code(&handled), Some(codes::FORBIDDEN), "{method}");
        }
        // The command's own name over loopback TCP is not enough: a page in the app's window
        // reaches the daemon that way with the token.
        let mut tcp = connection();
        tcp.handle_text(&request(1, Handshake::NAME, named("imperium-cli")));
        let handled = tcp.handle_text(&request(2, SessionEvalResult::NAME, methods[1].1.clone()));
        assert_eq!(error_code(&handled), Some(codes::FORBIDDEN));
        // The command itself, on the local socket, is let in (here only as far as the missing
        // session).
        let mut cli = connection().with_peer(Some((std::process::id(), 0)));
        cli.handle_text(&request(1, Handshake::NAME, named("imperium-cli")));
        let handled = cli.handle_text(&request(2, SessionEvalResult::NAME, methods[1].1.clone()));
        assert_eq!(error_code(&handled), Some(codes::INVALID_PARAMS));
    }

    fn handshaken() -> Connection {
        let mut connection = connection();
        let handled = connection.handle_text(&request(
            1,
            Handshake::NAME,
            handshake_params(PROTOCOL_VERSION.major),
        ));
        assert_eq!(error_code(&handled), None);
        connection
    }

    /// Every method the protocol defines reaches its own handler: a number is no method's
    /// params, so each answers "invalid params for <method>", never "unknown method". A method
    /// missing from the dispatch (or sent to the wrong group) fails here.
    #[test]
    fn every_protocol_method_is_routed_to_a_handler() {
        let names = include_str!("../../imperium-protocol/snapshots/methods.txt");
        let methods: Vec<&str> = names
            .lines()
            .filter_map(|line| line.strip_prefix("method "))
            .filter(|name| *name != Handshake::NAME)
            .collect();
        assert!(methods.len() > 100, "{} methods", methods.len());
        let mut connection = handshaken();
        for name in methods {
            let handled = connection.handle_text(&request(2, name, json!(1)));
            let response = handled.response.as_ref().expect("an immediate reply");
            let Outcome::Error(error) = &response.outcome else {
                panic!("{name} accepted a bare number as its params");
            };
            assert_eq!(
                error.code,
                codes::INVALID_PARAMS,
                "{name}: {}",
                error.message
            );
            assert!(
                error
                    .message
                    .starts_with(&format!("invalid params for {name}")),
                "{name}: {}",
                error.message
            );
        }
    }

    #[test]
    fn methods_other_than_handshake_require_a_handshake_first() {
        let mut connection = connection();
        let handled = connection.handle_text(&request(1, Ping::NAME, json!({})));
        assert_eq!(error_code(&handled), Some(codes::HANDSHAKE_REQUIRED));
        assert_eq!(
            handled.response.as_ref().unwrap().id,
            Some(RequestId::Number(1))
        );
    }

    #[test]
    fn ping_succeeds_after_a_compatible_handshake() {
        let mut connection = handshaken();
        let ping = connection.handle_text(&request(2, Ping::NAME, Value::Null));
        assert_eq!(error_code(&ping), None, "{:?}", ping.response);
    }

    #[tokio::test]
    async fn shutdown_replies_then_signals_the_daemon() {
        let mut connection = handshaken();
        let state = connection.state.clone();
        let stopped = connection.handle_text(&request(2, Shutdown::NAME, Value::Null));
        assert_eq!(error_code(&stopped), None, "{:?}", stopped.response);
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            state.shutdown_requested(),
        )
        .await
        .expect("the daemon is asked to stop");
    }

    #[test]
    fn an_incompatible_major_version_is_refused() {
        let mut connection = connection();
        let handled = connection.handle_text(&request(
            1,
            Handshake::NAME,
            handshake_params(PROTOCOL_VERSION.major + 1),
        ));
        assert_eq!(error_code(&handled), Some(codes::INCOMPATIBLE_PROTOCOL));
        let ping = connection.handle_text(&request(2, Ping::NAME, json!({})));
        assert_eq!(error_code(&ping), Some(codes::HANDSHAKE_REQUIRED));
    }

    #[test]
    fn malformed_input_gets_the_matching_error() {
        let mut connection = connection();
        assert_eq!(
            error_code(&connection.handle_text("{not json")),
            Some(codes::PARSE_ERROR)
        );

        let invalid = connection.handle_text(r#"{"jsonrpc":"2.0","id":5}"#);
        assert_eq!(error_code(&invalid), Some(codes::INVALID_REQUEST));
        assert_eq!(
            invalid.response.as_ref().unwrap().id,
            Some(RequestId::Number(5))
        );

        let bad_params = connection.handle_text(&request(3, Handshake::NAME, json!({"x": 1})));
        assert_eq!(error_code(&bad_params), Some(codes::INVALID_PARAMS));
    }

    #[test]
    fn unknown_methods_and_sessions_are_reported() {
        let mut connection = handshaken();
        let unknown = connection.handle_text(&request(2, "system.nope", json!({})));
        assert_eq!(error_code(&unknown), Some(codes::METHOD_NOT_FOUND));

        let no_session = connection.handle_text(&request(
            3,
            SessionSend::NAME,
            json!({"session_id": "missing", "text": "hi"}),
        ));
        assert_eq!(error_code(&no_session), Some(codes::SESSION_NOT_FOUND));
    }

    #[test]
    fn the_model_table_lists_rows_with_prices() {
        let config: toml::Table = r#"
            [endpoints.cloud]
            base_url = "https://example.com/v1"
            pricing."big" = { input_usd_per_mtok = 2.0, output_usd_per_mtok = 8.0 }

            [[models.catalog]]
            id = "big"
            spec = "cloud:big"
            location = "cloud"
            tier = "strong"

            [[models.catalog]]
            id = "laptop"
            spec = "nowhere:small"
            location = "local"
            tier = "basic"
            tags = ["code"]
        "#
        .parse()
        .unwrap();
        let settings = Config::from_layers([
            Layer::defaults(),
            Layer::new(imperium_config::Source::User("u".into()), config),
        ])
        .settings()
        .unwrap();
        let mut connection = Connection::new(DaemonState::new(
            Paths::under(std::path::Path::new("unused")),
            settings,
            None,
        ));
        connection.handle_text(&request(
            1,
            Handshake::NAME,
            handshake_params(PROTOCOL_VERSION.major),
        ));
        let handled = connection.handle_text(&request(2, ModelList::NAME, json!({})));
        let Some(Outcome::Result(result)) = handled.response.map(|response| response.outcome)
        else {
            panic!("models.list failed");
        };
        assert_eq!(result["prefer"], "cheapest");
        assert_eq!(result["models"][0]["input_usd_per_mtok"], 2.0);
        assert_eq!(result["models"][0]["endpoint_configured"], true);
        assert_eq!(result["models"][1]["location"], "local");
        assert_eq!(result["models"][1]["endpoint_configured"], false);
    }

    fn result(handled: Handled) -> Value {
        match handled.response.map(|response| response.outcome) {
            Some(Outcome::Result(result)) => result,
            other => panic!("expected a result, got {other:?}"),
        }
    }

    #[test]
    fn personalities_are_listed_and_a_new_choice_applies_to_new_sessions_at_once() {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::under(home.path());
        let settings = Config::from_layers([Layer::defaults()]).settings().unwrap();
        let state = DaemonState::new(paths.clone(), settings, None);
        let mut connection = Connection::new(Arc::clone(&state));
        connection.handle_text(&request(
            1,
            Handshake::NAME,
            handshake_params(PROTOCOL_VERSION.major),
        ));
        let local = paths.config_dir().join("personalities");
        std::fs::create_dir_all(&local).unwrap();
        std::fs::write(local.join("luxion.md"), "# My Luxion\n\nLocal.\n").unwrap();

        let listed =
            result(connection.handle_text(&request(2, PersonalitiesList::NAME, json!({}))));
        assert_eq!(listed["default"], "marcus_aurelius");
        let list = listed["personalities"].as_array().unwrap();
        let find = |id: &str| list.iter().find(|p| p["id"] == id).unwrap().clone();
        let marcus = find("marcus_aurelius");
        assert_eq!(
            (&marcus["name"], &marcus["source"]),
            (&json!("Marcus Aurelius"), &json!("builtin"))
        );
        let luxion = find("luxion");
        assert_eq!(
            (&luxion["name"], &luxion["source"], &luxion["overrides"]),
            (&json!("My Luxion"), &json!("local"), &json!(true))
        );

        assert_eq!(state.settings().policy.personality, "marcus_aurelius");
        let set = result(connection.handle_text(&request(
            3,
            ConfigSet::NAME,
            json!({"key": "policy.personality", "value": "luxion"}),
        )));
        assert_eq!(set["restart"], false);
        assert_eq!(state.settings().policy.personality, "luxion");
    }

    #[test]
    fn settings_are_described_and_written_to_the_user_file() {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::under(home.path());
        let settings = Config::from_layers([Layer::defaults()]).settings().unwrap();
        let mut connection = Connection::new(DaemonState::new(paths.clone(), settings, None));
        connection.handle_text(&request(
            1,
            Handshake::NAME,
            handshake_params(PROTOCOL_VERSION.major),
        ));

        let described =
            result(connection.handle_text(&request(2, ConfigDescribe::NAME, json!({}))));
        let entries = described["entries"].as_array().unwrap();
        let fidget = entries
            .iter()
            .find(|entry| entry["key"] == "pets.fidget")
            .unwrap();
        assert_eq!(fidget["section"], "pets");
        assert_eq!(fidget["kind"], "bool");
        assert_eq!(fidget["default"], true);
        assert_eq!(fidget["restart"], true);

        let set = result(connection.handle_text(&request(
            3,
            ConfigSet::NAME,
            json!({"key": "pets.fidget", "value": false}),
        )));
        assert_eq!(set["entry"]["value"], false);
        assert_eq!(set["entry"]["source"], "user");
        assert_eq!(set["restart"], true);
        assert_eq!(set["overridden"], false);
        let text = std::fs::read_to_string(paths.user_config_file()).unwrap();
        assert!(text.contains("fidget = false"), "{text}");

        let refused = connection.handle_text(&request(
            4,
            ConfigSet::NAME,
            json!({"key": "pets.fidget", "value": "sometimes"}),
        ));
        assert_eq!(error_code(&refused), Some(codes::INVALID_PARAMS));
        let unknown = connection.handle_text(&request(
            5,
            ConfigSet::NAME,
            json!({"key": "endpoints.x.base_url", "value": "u"}),
        ));
        assert_eq!(error_code(&unknown), Some(codes::INVALID_PARAMS));
    }

    fn copy_dir(from: &std::path::Path, to: &std::path::Path) {
        std::fs::create_dir_all(to).unwrap();
        for entry in std::fs::read_dir(from).unwrap().flatten() {
            let target = to.join(entry.file_name());
            if entry.path().is_dir() {
                copy_dir(&entry.path(), &target);
            } else {
                std::fs::copy(entry.path(), target).unwrap();
            }
        }
    }

    #[test]
    fn pets_are_listed_and_stripped_without_the_pet_engine() {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::under(home.path());
        let bundled =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../pets/pixel-clawd");
        copy_dir(
            &bundled,
            &paths.config_dir().join("pets").join("pixel-clawd"),
        );
        let settings = Config::from_layers([Layer::defaults()]).settings().unwrap();
        // The engine is never started here, as when `pets.enabled` is false.
        let mut connection = Connection::new(DaemonState::new(paths, settings, None));
        connection.handle_text(&request(
            1,
            Handshake::NAME,
            handshake_params(PROTOCOL_VERSION.major),
        ));

        let listed = result(connection.handle_text(&request(2, PetList::NAME, json!({}))));
        assert!(listed["selected"].is_null());
        assert!(listed["state"].is_null());
        let pet = listed["pets"]
            .as_array()
            .unwrap()
            .iter()
            .find(|pet| pet["id"] == "pixel-clawd")
            .expect("the installed pet is listed");
        assert!(pet["drawn"].as_array().unwrap().iter().any(|a| a == "idle"));
        assert!(pet["pet_json"].as_str().unwrap().contains("pixel-clawd"));

        let strip = result(connection.handle_text(&request(
            3,
            PetStrip::NAME,
            json!({"pet_id": "pixel-clawd", "animation": "idle"}),
        )));
        assert!(!strip["png_base64"].as_str().unwrap().is_empty());
        // Without the engine there is no selected pet; unknown pets and animations are refused,
        // including names that try to leave the strips folder.
        for params in [
            json!({"animation": "idle"}),
            json!({"pet_id": "nobody", "animation": "idle"}),
            json!({"pet_id": "pixel-clawd", "animation": "../pet"}),
            json!({"pet_id": "../pixel-clawd", "animation": "idle"}),
        ] {
            let refused = connection.handle_text(&request(4, PetStrip::NAME, params));
            assert_eq!(error_code(&refused), Some(codes::INVALID_PARAMS));
        }
    }

    fn connectors_connection() -> (
        tempfile::TempDir,
        Paths,
        Arc<imperium_config::MemorySecrets>,
        Connection,
    ) {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::under(home.path());
        let settings = Config::from_layers([Layer::defaults()]).settings().unwrap();
        let secrets = Arc::new(imperium_config::MemorySecrets::new());
        let mut connection = Connection::new(DaemonState::with_secrets(
            paths.clone(),
            settings,
            None,
            secrets.clone(),
        ));
        connection.handle_text(&request(
            1,
            Handshake::NAME,
            handshake_params(PROTOCOL_VERSION.major),
        ));
        (home, paths, secrets, connection)
    }

    #[test]
    fn connectors_are_installed_from_the_catalogue_with_secrets_kept_out_of_the_config() {
        use imperium_config::SecretStore;
        let (_home, paths, secrets, mut connection) = connectors_connection();
        std::fs::create_dir_all(paths.config_dir()).unwrap();
        std::fs::write(paths.user_config_file(), "# mine\n[daemon]\nport = 7000\n").unwrap();

        let catalog = result(connection.handle_text(&request(2, McpCatalog::NAME, json!({}))));
        let slack = catalog["entries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["id"] == "slack")
            .unwrap();
        assert_eq!(slack["fields"][0]["kind"], "secret");

        let installed = result(connection.handle_text(&request(
            3,
            McpInstall::NAME,
            json!({"catalog_id": "slack", "values": {"bot_token": "xoxb-secret", "team_id": "T1"}}),
        )));
        let server = &installed["server"];
        assert_eq!(server["name"], "slack");
        assert_eq!(server["config"]["catalog"], "slack");
        assert_eq!(
            server["config"]["secret_env"]["SLACK_BOT_TOKEN"],
            "mcp.slack.bot-token"
        );
        assert_eq!(server["status"]["state"], "untested");
        assert_eq!(server["status"]["missing_secrets"], json!([]));
        let text = std::fs::read_to_string(paths.user_config_file()).unwrap();
        assert!(
            text.starts_with("# mine\n[daemon]\nport = 7000\n"),
            "{text}"
        );
        assert!(
            text.contains("SLACK_BOT_TOKEN = { secret = \"mcp.slack.bot-token\" }"),
            "{text}"
        );
        assert!(!text.contains("xoxb-secret"), "{text}");
        let name: imperium_config::SecretName = "mcp.slack.bot-token".parse().unwrap();
        assert_eq!(secrets.load(&name).unwrap().as_deref(), Some("xoxb-secret"));

        // A second install under the same name is refused; a missing field too.
        let again = connection.handle_text(&request(
            4,
            McpInstall::NAME,
            json!({"catalog_id": "slack", "values": {"bot_token": "x", "team_id": "T"}}),
        ));
        assert_eq!(error_code(&again), Some(codes::INVALID_PARAMS));
        let missing = connection.handle_text(&request(
            5,
            McpInstall::NAME,
            json!({"catalog_id": "slack", "name": "slack2", "values": {}}),
        ));
        assert_eq!(error_code(&missing), Some(codes::INVALID_PARAMS));

        // A custom server, then edits: disable, update, and the list reflects each.
        let saved = result(connection.handle_text(&request(
            6,
            McpSave::NAME,
            json!({
                "name": "remote",
                "create": true,
                "config": {"url": "https://example.com/mcp", "bearer_secret": "mcp.remote.token"},
                "secrets": {"mcp.remote.token": "tok"}
            }),
        )));
        assert_eq!(
            saved["server"]["config"]["bearer_secret"],
            "mcp.remote.token"
        );
        let disabled = result(connection.handle_text(&request(
            7,
            McpSetEnabled::NAME,
            json!({"name": "remote", "enabled": false}),
        )));
        assert_eq!(disabled["server"]["status"]["state"], "disabled");
        let listed = result(connection.handle_text(&request(8, McpList::NAME, json!({}))));
        let names: Vec<&str> = listed["servers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|server| server["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["remote", "slack"]);
        assert_eq!(listed["servers"][0]["config"]["enabled"], false);
        let text = std::fs::read_to_string(paths.user_config_file()).unwrap();
        assert!(text.contains("enabled = false"), "{text}");
        assert!(!text.contains("tok\""), "{text}");

        // Removing forgets the server and, when asked, its secrets.
        let removed = result(connection.handle_text(&request(
            9,
            McpRemove::NAME,
            json!({"name": "slack", "forget_secrets": true}),
        )));
        assert_eq!(removed["removed"], true);
        assert_eq!(secrets.load(&name).unwrap(), None);
        let text = std::fs::read_to_string(paths.user_config_file()).unwrap();
        assert!(!text.contains("slack"), "{text}");
        assert!(text.starts_with("# mine\n"), "{text}");
        let again =
            result(connection.handle_text(&request(10, McpRemove::NAME, json!({"name": "slack"}))));
        assert_eq!(again["removed"], false);
    }

    #[tokio::test]
    async fn testing_a_connector_records_its_status() {
        let (_home, _paths, _secrets, mut connection) = connectors_connection();
        result(connection.handle_text(&request(
            2,
            McpSave::NAME,
            json!({"name": "broken", "config": {"command": "definitely-not-an-mcp-server-xyz"}}),
        )));
        let handled = connection.handle_text(&request(3, McpTest::NAME, json!({"name": "broken"})));
        let response = handled.deferred.expect("a deferred reply").await;
        let Outcome::Result(status) = response.outcome else {
            panic!("mcp.test failed: {response:?}");
        };
        assert_eq!(status["state"], "error");
        assert!(
            status["error"]
                .as_str()
                .unwrap()
                .contains("could not start"),
            "{status}"
        );
        assert!(status["checked_at"].is_number());

        let listed = result(connection.handle_text(&request(4, McpList::NAME, json!({}))));
        assert_eq!(listed["servers"][0]["status"]["state"], "error");

        let unknown = connection.handle_text(&request(5, McpTest::NAME, json!({"name": "nope"})));
        let response = unknown.deferred.expect("a deferred reply").await;
        assert!(
            matches!(response.outcome, Outcome::Error(ref error) if error.code == codes::INVALID_PARAMS)
        );
    }

    #[tokio::test]
    async fn signing_in_is_offered_only_to_remote_servers_without_a_token_of_their_own() {
        let (_home, _paths, _secrets, mut connection) = connectors_connection();
        result(connection.handle_text(&request(
            2,
            McpSave::NAME,
            json!({"name": "local", "config": {"command": "x"}}),
        )));
        result(connection.handle_text(&request(
            3,
            McpSave::NAME,
            json!({
                "name": "fixed",
                "config": {"url": "https://example.com/mcp", "bearer_secret": "mcp.fixed.token"},
                "secrets": {"mcp.fixed.token": "t"}
            }),
        )));
        result(connection.handle_text(&request(
            4,
            McpSave::NAME,
            json!({"name": "open", "config": {"url": "https://example.com/open"}}),
        )));
        for (id, name) in [(5, "local"), (6, "fixed"), (7, "missing")] {
            let handled =
                connection.handle_text(&request(id, McpLogin::NAME, json!({"name": name})));
            let response = handled.deferred.expect("a deferred reply").await;
            assert!(
                matches!(response.outcome, Outcome::Error(ref error) if error.code == codes::INVALID_PARAMS),
                "{name}: {response:?}"
            );
        }

        // Signing out of a server that was never signed in changes nothing.
        let handled = connection.handle_text(&request(8, McpLogout::NAME, json!({"name": "open"})));
        let response = handled.deferred.expect("a deferred reply").await;
        let Outcome::Result(signed_out) = response.outcome else {
            panic!("mcp.logout failed: {response:?}");
        };
        assert_eq!(signed_out["signed_out"], false);

        // Only a remote server without a token of its own has a sign-in state.
        let listed = result(connection.handle_text(&request(9, McpList::NAME, json!({}))));
        let state_of = |name: &str| {
            listed["servers"]
                .as_array()
                .unwrap()
                .iter()
                .find(|server| server["name"] == name)
                .map(|server| server["status"]["sign_in"].clone())
                .unwrap()
        };
        assert!(state_of("local").is_null());
        assert!(state_of("fixed").is_null());
        assert!(state_of("open").is_null());
    }

    #[tokio::test]
    async fn attachments_are_staged_inside_the_session_and_checked_again_on_send() {
        let mut connection = handshaken();
        let project = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(project.path().join("notes.md"), "# Plan").unwrap();
        std::fs::write(outside.path().join("secret.txt"), "key").unwrap();
        let created = result_of(&connection.handle_text(&request(
            2,
            SessionCreate::NAME,
            json!({"project_dir": project.path()}),
        )));
        let id = created["session_id"].as_str().unwrap().to_string();
        let store = connection
            .state
            .data_dir()
            .join("attachments")
            .join(&id)
            .canonicalize()
            .unwrap();

        // Inside the project: referenced where it is.
        let inside = result_of(
            &settle(connection.handle_text(&request(
                3,
                SessionAttach::NAME,
                json!({"session_id": id, "path": project.path().join("notes.md")}),
            )))
            .await,
        );
        assert_eq!(inside["attachment"]["kind"], "text");
        assert_eq!(inside["attachment"]["copied"], false);
        // Pasted bytes and outside files are copied into the session's attachments folder.
        let pasted = result_of(
            &settle(connection.handle_text(&request(
                4,
                SessionAttach::NAME,
                json!({"session_id": id, "name": "shot.png",
                       "data": base64::engine::general_purpose::STANDARD.encode(b"\x89PNG\r\n\x1a\nx")}),
            )))
            .await,
        );
        assert_eq!(pasted["attachment"]["kind"], "image");
        assert_eq!(pasted["attachment"]["copied"], true);
        assert!(
            pasted["image_data"]
                .as_str()
                .unwrap()
                .starts_with("data:image/png;base64,")
        );
        let copied = result_of(
            &settle(connection.handle_text(&request(
                5,
                SessionAttach::NAME,
                json!({"session_id": id, "path": outside.path().join("secret.txt")}),
            )))
            .await,
        );
        assert_eq!(copied["attachment"]["copied"], true);
        let copy = std::path::Path::new(copied["attachment"]["path"].as_str().unwrap());
        assert!(copy.canonicalize().unwrap().starts_with(&store));
        // Neither or both sources, and oversized files, are refused.
        let neither = settle(connection.handle_text(&request(
            6,
            SessionAttach::NAME,
            json!({"session_id": id}),
        )))
        .await;
        assert_eq!(error_code(&neither), Some(codes::INVALID_PARAMS));

        // A sent attachment pointing outside the session is refused; the staged ones are sent.
        let mut smuggled = inside["attachment"].clone();
        smuggled["path"] = json!(outside.path().join("secret.txt"));
        let refused = connection.handle_text(&request(
            7,
            SessionSend::NAME,
            json!({"session_id": id, "text": "", "attachments": [smuggled]}),
        ));
        assert_eq!(error_code(&refused), Some(codes::INVALID_PARAMS));
        let sent = connection.handle_text(&request(
            8,
            SessionSend::NAME,
            json!({"session_id": id, "text": "", "attachments": [
                inside["attachment"], pasted["attachment"], copied["attachment"]
            ]}),
        ));
        assert_eq!(error_code(&sent), None, "{:?}", sent.response);

        // Deleting the session deletes its attachments folder.
        let deleted =
            connection.handle_text(&request(9, SessionDelete::NAME, json!({"session_id": id})));
        assert_eq!(error_code(&deleted), None, "{:?}", deleted.response);
        assert!(!store.exists());
    }

    /// Calls `method` and waits for its reply, which may come later.
    async fn ask(connection: &mut Connection, id: i64, method: &str, params: Value) -> Handled {
        settle(connection.handle_text(&request(id, method, params))).await
    }

    #[tokio::test]
    async fn mentions_list_and_stage_only_what_the_session_may_read() {
        let mut connection = handshaken();
        let project = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(project.path().join(".gitignore"), "ignored.txt\n").unwrap();
        std::fs::write(project.path().join("ignored.txt"), "no").unwrap();
        std::fs::write(project.path().join("notes.md"), "# Plan").unwrap();
        std::fs::create_dir_all(project.path().join("docs")).unwrap();
        std::fs::create_dir_all(project.path().join("src")).unwrap();
        std::fs::write(project.path().join("src/lib.rs"), "fn main() {}").unwrap();
        std::fs::write(project.path().join("big.txt"), "x".repeat(150 * 1024)).unwrap();
        std::fs::write(outside.path().join("secret.txt"), "key").unwrap();
        let created = result_of(&connection.handle_text(&request(
            2,
            SessionCreate::NAME,
            json!({"project_dir": project.path()}),
        )));
        let id = created["session_id"].as_str().unwrap().to_string();

        // The picker lists files and directories, skips ignored files and ranks by match.
        let listed = result_of(
            &ask(
                &mut connection,
                3,
                SessionFiles::NAME,
                json!({"session_id": id, "query": ""}),
            )
            .await,
        );
        let names: Vec<&str> = listed["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|file| file["relative"].as_str().unwrap())
            .collect();
        assert!(
            names.contains(&"notes.md") && names.contains(&"docs"),
            "{names:?}"
        );
        assert!(!names.contains(&"ignored.txt"), "{names:?}");
        let found = result_of(
            &ask(
                &mut connection,
                4,
                SessionFiles::NAME,
                json!({"session_id": id, "query": "lib"}),
            )
            .await,
        );
        assert_eq!(found["files"][0]["relative"], "src/lib.rs");
        assert_eq!(found["files"][0]["directory"], false);

        // A mention outside the session's folders is refused, never copied.
        let refused = ask(
            &mut connection,
            5,
            SessionAttach::NAME,
            json!({"session_id": id, "path": outside.path().join("secret.txt"), "mention": true}),
        )
        .await;
        assert_eq!(error_code(&refused), Some(codes::INVALID_PARAMS));
        let store = connection.state.data_dir().join("attachments").join(&id);
        assert!(
            !store.exists() || std::fs::read_dir(&store).unwrap().next().is_none(),
            "nothing was copied"
        );
        // A directory is a path, not an attachment.
        let directory = ask(
            &mut connection,
            6,
            SessionAttach::NAME,
            json!({"session_id": id, "path": project.path().join("docs"), "mention": true}),
        )
        .await;
        assert_eq!(error_code(&directory), Some(codes::INVALID_PARAMS));

        // A file inside is referenced where it is, and a long one is cut with a marker.
        let notes = result_of(
            &ask(
                &mut connection,
                7,
                SessionAttach::NAME,
                json!({"session_id": id, "path": project.path().join("notes.md"), "mention": true}),
            )
            .await,
        );
        assert_eq!(notes["attachment"]["copied"], false);
        assert_eq!(notes["attachment"]["kind"], "text");
        let big = result_of(
            &ask(
                &mut connection,
                8,
                SessionAttach::NAME,
                json!({"session_id": id, "path": project.path().join("big.txt"), "mention": true}),
            )
            .await,
        );
        assert_eq!(big["attachment"]["copied"], false);
        let attachment: imperium_protocol::session::Attachment =
            serde_json::from_value(big["attachment"].clone()).unwrap();
        let rendered = imperium_media::attachments::render(&[attachment]);
        assert!(
            rendered.text.contains("[… truncated: showing the first"),
            "{}",
            rendered.text
        );
        let sent = ask(
            &mut connection,
            9,
            SessionSend::NAME,
            json!({"session_id": id, "text": "look", "attachments": [notes["attachment"], big["attachment"]]}),
        )
        .await;
        assert_eq!(error_code(&sent), None, "{:?}", sent.response);
    }

    #[tokio::test]
    async fn a_mention_a_deny_rule_covers_is_not_listed_or_staged() {
        let mut connection = handshaken();
        let project = tempfile::tempdir().unwrap();
        std::fs::write(project.path().join(".env"), "KEY=1").unwrap();
        std::fs::write(project.path().join("a.txt"), "a").unwrap();
        let added = ask(
            &mut connection,
            2,
            "permissions.add_rule",
            json!({"rule": {"tool": "*", "pattern": ".env", "action": "deny"}}),
        )
        .await;
        assert_eq!(error_code(&added), None, "{:?}", added.response);
        let created = result_of(&connection.handle_text(&request(
            3,
            SessionCreate::NAME,
            json!({"project_dir": project.path()}),
        )));
        let id = created["session_id"].as_str().unwrap().to_string();
        let listed = result_of(
            &ask(
                &mut connection,
                4,
                SessionFiles::NAME,
                json!({"session_id": id, "query": "env"}),
            )
            .await,
        );
        assert!(
            listed["files"]
                .as_array()
                .unwrap()
                .iter()
                .all(|file| file["relative"] != ".env")
        );
        let refused = ask(
            &mut connection,
            5,
            SessionAttach::NAME,
            json!({"session_id": id, "path": project.path().join(".env"), "mention": true}),
        )
        .await;
        assert_eq!(error_code(&refused), Some(codes::INVALID_PARAMS));
    }

    #[tokio::test]
    async fn init_sends_the_built_in_prompt_as_a_user_turn() {
        let mut connection = handshaken();
        let project = tempfile::tempdir().unwrap();
        let created = result_of(&connection.handle_text(&request(
            2,
            SessionCreate::NAME,
            json!({"project_dir": project.path()}),
        )));
        let id = created["session_id"].as_str().unwrap().to_string();
        let sent = connection.handle_text(&request(
            3,
            SessionInit::NAME,
            json!({"session_id": id, "guidance": "the build commands"}),
        ));
        assert_eq!(error_code(&sent), None, "{:?}", sent.response);
        assert!(result_of(&sent)["id"].is_string());
        let missing = connection.handle_text(&request(
            4,
            SessionInit::NAME,
            json!({"session_id": "nope"}),
        ));
        assert_eq!(error_code(&missing), Some(codes::SESSION_NOT_FOUND));
    }

    #[tokio::test]
    async fn builtin_review_commands_send_a_user_turn() {
        let mut connection = handshaken();
        let project = tempfile::tempdir().unwrap();
        let created = result_of(&connection.handle_text(&request(
            2,
            SessionCreate::NAME,
            json!({"project_dir": project.path()}),
        )));
        let id = created["session_id"].as_str().unwrap().to_string();
        for (n, command, args) in [
            (3, "init", "the tests"),
            (4, "code_review", "high 123"),
            (5, "security_review", ""),
        ] {
            let sent = connection.handle_text(&request(
                n,
                SessionBuiltin::NAME,
                json!({"session_id": id, "command": command, "args": args}),
            ));
            assert_eq!(error_code(&sent), None, "{command}: {:?}", sent.response);
            assert!(result_of(&sent)["id"].is_string());
        }
        // The text each command sends as the user's message (this session has no model, so no
        // turn starts to read it back from).
        let init = super::sessions::builtin_text(BuiltinCommand::Init, "the tests");
        assert!(init.contains("AGENTS.md"), "{init}");
        assert!(init.ends_with("the tests"), "{init}");
        let review = super::sessions::builtin_text(BuiltinCommand::CodeReview, "high 123");
        assert!(review.contains("Effort: high"), "{review}");
        assert!(review.contains("`gh pr diff 123`"), "{review}");
        let security = super::sessions::builtin_text(BuiltinCommand::SecurityReview, "");
        assert!(security.contains("exploitability"), "{security}");
        let unknown = connection.handle_text(&request(
            6,
            SessionBuiltin::NAME,
            json!({"session_id": id, "command": "nope"}),
        ));
        assert_eq!(error_code(&unknown), Some(codes::INVALID_PARAMS));
        let missing = connection.handle_text(&request(
            7,
            SessionBuiltin::NAME,
            json!({"session_id": "nope", "command": "code_review"}),
        ));
        assert_eq!(error_code(&missing), Some(codes::SESSION_NOT_FOUND));
    }

    #[tokio::test]
    async fn sessions_are_archived_and_unarchived_over_rpc() {
        let mut connection = handshaken();
        let mut notices = connection.state.announcements();
        let dir = tempfile::tempdir().unwrap();
        let created = result_of(&connection.handle_text(&request(
            2,
            SessionCreate::NAME,
            json!({"project_dir": dir.path()}),
        )));
        let id = created["session_id"].as_str().unwrap().to_string();
        let control = |connection: &mut Connection, n: i64, action: &str| {
            connection
                .handle_text(&request(
                    n,
                    SessionControl::NAME,
                    json!({"session_id": id, "action": action}),
                ))
                .deferred
                .expect("a deferred reply")
        };

        let archived = control(&mut connection, 3, "archive").await;
        assert!(
            matches!(archived.outcome, Outcome::Result(_)),
            "{archived:?}"
        );
        let listed = result_of(&connection.handle_text(&request(4, SessionList::NAME, json!({}))));
        assert_eq!(listed["sessions"].as_array().unwrap().len(), 1);
        assert_eq!(listed["sessions"][0]["archived"], true);
        assert!(listed["sessions"][0]["archived_at"].is_u64());
        // Every client hears of it.
        let mut updated = false;
        while let Ok(notice) = notices.try_recv() {
            if notice.method == imperium_protocol::methods::SESSION_UPDATED
                && notice.params["archived"] == true
            {
                updated = true;
            }
        }
        assert!(updated, "no session.updated for the archive");
        // Its history is still readable, with nothing live to follow.
        let subscribed = connection.handle_text(&request(
            5,
            SessionSubscribe::NAME,
            json!({"session_id": id}),
        ));
        assert!(subscribed.subscription.is_none());
        assert!(result_of(&subscribed)["history"].is_array());
        // It no longer takes messages.
        let send = connection.handle_text(&request(
            6,
            SessionSend::NAME,
            json!({"session_id": id, "text": "hi"}),
        ));
        assert_eq!(error_code(&send), Some(codes::SESSION_NOT_FOUND));

        let unarchived = control(&mut connection, 7, "unarchive").await;
        assert!(
            matches!(unarchived.outcome, Outcome::Result(_)),
            "{unarchived:?}"
        );
        let listed = result_of(&connection.handle_text(&request(8, SessionList::NAME, json!({}))));
        assert_eq!(listed["sessions"][0]["archived"], false);
        assert!(connection.state.session(&id).is_some());

        let missing = connection
            .handle_text(&request(
                9,
                SessionControl::NAME,
                json!({"session_id": "nope", "action": "archive"}),
            ))
            .deferred
            .expect("a deferred reply")
            .await;
        assert!(
            matches!(missing.outcome, Outcome::Error(ref error) if error.code == codes::SESSION_NOT_FOUND)
        );
    }

    #[tokio::test]
    async fn sessions_start_without_a_model_and_wait_for_one() {
        let mut connection = handshaken();
        let missing_folder = connection.handle_text(&request(
            2,
            SessionCreate::NAME,
            json!({"project_dir": "definitely/not/a/folder"}),
        ));
        assert_eq!(error_code(&missing_folder), Some(codes::INVALID_PARAMS));

        // No model given and no models.default: the session is created anyway, without one.
        let dir = tempfile::tempdir().unwrap();
        let created = connection.handle_text(&request(
            3,
            SessionCreate::NAME,
            json!({"project_dir": dir.path()}),
        ));
        assert!(created.subscription.is_some());
        let created = result_of(&created);
        assert_eq!(created["model"], Value::Null);
        let id = created["session_id"].as_str().unwrap().to_string();
        let listed = result_of(&connection.handle_text(&request(4, SessionList::NAME, json!({}))));
        assert_eq!(listed["sessions"][0]["model"], Value::Null);
        assert_eq!(listed["sessions"][0]["status"], "idle");

        // A message waits for a model instead of failing.
        let session = connection.state.session(&id).unwrap();
        session.send("hello".into()).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while session.status().0 != imperium_protocol::session::SessionStatus::NeedsModel {
            assert!(
                std::time::Instant::now() < deadline,
                "the message never waited"
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(session.queue(None).unwrap().len(), 1);

        // A model that cannot be resolved is refused, and the session keeps waiting.
        let refused = connection.handle_text(&request(
            5,
            SessionControl::NAME,
            json!({"session_id": id, "action": "set_model", "model": "nowhere:m"}),
        ));
        let refused = refused.deferred.expect("a deferred reply").await;
        assert!(
            matches!(refused.outcome, Outcome::Error(ref error) if error.code == codes::MODEL_UNAVAILABLE)
        );
        assert_eq!(session.model(), None);

        // Neither an environment nor a project: the built-in Home environment.
        let neither =
            result_of(&connection.handle_text(&request(6, SessionCreate::NAME, json!({}))));
        assert_eq!(neither["environment_id"], "home");
    }

    #[test]
    fn environments_are_managed_over_rpc() {
        let mut connection = handshaken();
        let one = tempfile::tempdir().unwrap();
        let two = tempfile::tempdir().unwrap();
        let created = result_of(&connection.handle_text(&request(
            2,
            "environments.create",
            json!({"name": "Work", "folders": [one.path()]}),
        )));
        let id = created["id"].as_str().unwrap().to_string();
        assert_eq!(created["folders"].as_array().unwrap().len(), 1);
        // A second environment is just as easy (one per folder, or several folders each).
        result_of(&connection.handle_text(&request(
            3,
            "environments.create",
            json!({"name": "Other", "folders": [two.path()]}),
        )));
        let added = result_of(&connection.handle_text(&request(
            4,
            "environments.add_folder",
            json!({"id": id, "folder": two.path()}),
        )));
        assert_eq!(added["folders"].as_array().unwrap().len(), 2);
        let renamed = result_of(&connection.handle_text(&request(
            5,
            "environments.update",
            json!({"id": id, "name": "Day job", "icon": "code"}),
        )));
        assert_eq!(renamed["name"], "Day job");
        assert_eq!(renamed["icon"], "code");
        let removed = result_of(&connection.handle_text(&request(
            6,
            "environments.remove_folder",
            json!({"id": id, "folder": two.path()}),
        )));
        assert_eq!(removed["folders"].as_array().unwrap().len(), 1);
        let listed =
            result_of(&connection.handle_text(&request(7, "environments.list", json!({}))));
        assert_eq!(listed["environments"].as_array().unwrap().len(), 3);

        let missing = connection.handle_text(&request(
            8,
            "environments.add_folder",
            json!({"id": id, "folder": one.path().join("missing")}),
        ));
        assert_eq!(error_code(&missing), Some(codes::INVALID_PARAMS));
        let unknown = connection.handle_text(&request(
            9,
            SessionCreate::NAME,
            json!({"environment_id": "nope"}),
        ));
        assert_eq!(error_code(&unknown), Some(codes::INVALID_PARAMS));
        let deleted =
            connection.handle_text(&request(10, "environments.delete", json!({"id": id})));
        assert_eq!(error_code(&deleted), None);
        let listed =
            result_of(&connection.handle_text(&request(11, "environments.list", json!({}))));
        assert_eq!(listed["environments"].as_array().unwrap().len(), 2);
        let bogus = connection.handle_text(&request(12, "environments.nothing", json!({})));
        assert_eq!(error_code(&bogus), Some(codes::METHOD_NOT_FOUND));
    }

    #[tokio::test]
    async fn a_grant_needs_a_process_of_the_users_on_the_local_socket() {
        let source = tempfile::tempdir().unwrap();
        std::fs::write(
            source.path().join("mod.json"),
            r#"{"id": "grant-demo", "permissions": ["storage"], "main_context": true}"#,
        )
        .unwrap();
        std::fs::write(source.path().join("index.js"), "export default {};").unwrap();
        let location = source.path().display().to_string();
        // A connection with no peer process to vouch for: loopback TCP, or none known.
        let mut connection = handshaken();
        let handled =
            connection.handle_text(&request(2, ModInspect::NAME, json!({"source": location})));
        let response = handled.deferred.expect("a deferred reply").await;
        let Outcome::Result(preview) = response.outcome else {
            panic!("mods.inspect failed: {response:?}");
        };
        let params = |trusted: bool, interactive: bool| {
            json!({
                "source": location,
                "sha256": preview["sha256"],
                "permissions": ["storage"],
                "trusted": trusted,
                "interactive": interactive,
            })
        };
        // Trust is refused, and nothing is installed.
        let handled = connection.handle_text(&request(3, ModInstall::NAME, params(true, false)));
        let response = handled.deferred.expect("a deferred reply").await;
        let Outcome::Error(error) = response.outcome else {
            panic!("trust should be refused: {response:?}");
        };
        assert!(
            error
                .message
                .contains(imperium_protocol::errors::TRUST_NOT_GRANTED),
            "{}",
            error.message
        );
        // An approval is taken as given without a prompt: installed, but off.
        let handled = connection.handle_text(&request(4, ModInstall::NAME, params(false, true)));
        let response = handled.deferred.expect("a deferred reply").await;
        let Outcome::Result(done) = response.outcome else {
            panic!("the install should go ahead: {response:?}");
        };
        assert_eq!(done["interactive"], false);
        assert_eq!(done["trusted"], false);
        let note = done["notes"][0].as_str().unwrap_or_default();
        assert!(note.contains("not counted"), "{done}");
    }

    fn preview_params(folder: &std::path::Path) -> Value {
        json!({
            "project_dir": folder.to_string_lossy(),
            "options": {"model": {"base_url": "http://127.0.0.1:1/v1", "model_id": "m"}}
        })
    }

    /// The daemon's environments, minus Home.
    fn made_environments(state: &DaemonState) -> usize {
        state
            .environments()
            .list()
            .iter()
            .filter(|env| !env.builtin)
            .count()
    }

    #[tokio::test]
    async fn a_preview_session_goes_with_a_connection_that_ends_first() {
        let folder = tempfile::tempdir().unwrap();
        let mut connection = connection().with_peer(Some((std::process::id(), 0)));
        connection.handle_text(&request(
            1,
            Handshake::NAME,
            json!({
                "client": {"name": imperium_protocol::methods::EVAL_CLIENT, "version": "0"},
                "protocol": {"major": PROTOCOL_VERSION.major, "minor": 0}
            }),
        ));
        let state = connection.state();
        let params: SessionEvalCreateParams =
            serde_json::from_value(preview_params(folder.path())).unwrap();
        let prepared = Arc::new(crate::eval::Prepared::new(&params.options, true).unwrap());
        // The preview is under way: its session exists, in an environment made for the run.
        let (session, _events) = state
            .create_eval(None, params.project_dir.as_deref(), prepared)
            .unwrap();
        let id = session.id().to_string();
        drop(session);
        assert!(connection.eval_sessions.add(&state, &id));
        assert_eq!(made_environments(&state), 1);
        // The client disconnects.
        drop(connection);
        assert!(state.sessions().is_empty(), "the preview session is gone");
        assert_eq!(made_environments(&state), 0, "so is its environment");
    }

    #[tokio::test]
    async fn a_preview_that_registers_after_its_connection_ended_is_deleted_at_once() {
        let folder = tempfile::tempdir().unwrap();
        let connection = connection();
        let state = connection.state();
        let owner = Arc::clone(&connection.eval_sessions);
        drop(connection);
        let params: SessionEvalCreateParams =
            serde_json::from_value(preview_params(folder.path())).unwrap();
        let prepared = Arc::new(crate::eval::Prepared::new(&params.options, true).unwrap());
        // The connection ended between the request and the session's creation.
        let preview = state
            .prompt_preview(None, params.project_dir.as_deref(), prepared, &owner)
            .await;
        assert!(preview.is_err(), "no preview for a client that has gone");
        assert!(state.sessions().is_empty());
        assert_eq!(made_environments(&state), 0);
    }

    /// A plugin approval over loopback TCP (no local-socket peer) is refused before the plugin
    /// is looked up or any process is examined, and records nothing.
    #[test]
    fn a_plugin_approval_over_tcp_is_refused_first() {
        let mut connection = handshaken();
        let challenge = connection.handle_text(&request(
            2,
            PluginsApprovalChallenge::NAME,
            json!({"name": "missing", "sha256": "0", "runs_digest": "0"}),
        ));
        let approve = connection.handle_text(&request(
            3,
            PluginsApprove::NAME,
            json!({"name": "missing", "sha256": "0", "runs_digest": "0", "challenge": "0"}),
        ));
        for handled in [challenge, approve] {
            let Some(Outcome::Error(error)) = handled.response.map(|response| response.outcome)
            else {
                panic!("an approval over TCP must be refused");
            };
            assert!(error.message.contains("local socket"), "{}", error.message);
        }
    }
}
