//! `imperium mcp-serve`: the Claude harness, an MCP server on standard input and output that
//! lets an external agent (Claude Code) drive the running daemon.
//!
//! It is a client of the daemon like the CLI is: it connects with the endpoint file's token
//! and calls the ordinary protocol methods, so the daemon's permission rules, safety floor,
//! budgets and environment folders apply to everything it starts exactly as they do to a
//! session started from the terminal. It grants nothing extra: a permission request an agent
//! raises stays pending until the caller answers it with `respond_permission`, and nothing here
//! answers one on its own. The token never appears in a tool result.
//!
//! The transport is newline-delimited JSON-RPC 2.0 (the MCP stdio transport), handled by hand:
//! the harness needs only `initialize`, `ping`, `tools/list` and `tools/call`.

use std::sync::Arc;
use std::time::Duration;

use imperium_client::Client;
use imperium_protocol::session::SESSION_EVENT;
use serde_json::{Map, Value, json};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{Notify, mpsc, oneshot};
use tokio::task::JoinSet;
use tokio::time::Instant;

/// MCP revisions this server speaks, newest first; it answers a client's request for one of
/// them with the same, and anything else with the newest.
const SUPPORTED: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];
/// Longest a single `wait_for_turn` (or `wait_timeout_s`) may block, in seconds.
const MAX_WAIT_S: u64 = 1800;
/// Wait used when the caller names none, in seconds.
const DEFAULT_WAIT_S: u64 = 60;
/// How often a waiting call re-reads the session when no event wakes it.
const POLL: Duration = Duration::from_millis(500);
/// Characters of one tool output kept in a result.
const OUTPUT_LIMIT: usize = 1500;
/// Characters of a transcript returned unless the caller asks for more.
const TRANSCRIPT_LIMIT: usize = 60_000;

/// One daemon call queued for the task that owns the connection.
struct Call {
    method: String,
    params: Value,
    reply: oneshot::Sender<Result<Value, String>>,
}

/// The daemon connection, shared by the requests being handled. One task owns the
/// [`Client`], which makes calls one at a time, and keeps reading notifications so none pile up;
/// a session event wakes the calls waiting for a turn.
#[derive(Clone)]
struct Hub {
    calls: mpsc::UnboundedSender<Call>,
    changed: Arc<Notify>,
}

impl Hub {
    fn start(client: Client) -> (Hub, tokio::task::JoinHandle<()>) {
        let (calls, receiver) = mpsc::unbounded_channel();
        let changed = Arc::new(Notify::new());
        let pump = tokio::spawn(pump(client, receiver, Arc::clone(&changed)));
        (Hub { calls, changed }, pump)
    }

    async fn call(&self, method: &str, params: Value) -> Result<Value, String> {
        let (reply, answer) = oneshot::channel();
        self.calls
            .send(Call {
                method: method.to_string(),
                params,
                reply,
            })
            .map_err(|_| closed())?;
        answer.await.map_err(|_| closed())?
    }

    /// Every event the session has emitted so far, oldest first.
    async fn history(&self, session_id: &str) -> Result<Vec<Value>, String> {
        let subscribed = self
            .call("session.subscribe", json!({"session_id": session_id}))
            .await?;
        Ok(subscribed
            .get("history")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }

    /// The daemon's summary of one session.
    async fn summary(&self, session_id: &str) -> Result<Value, String> {
        let listed = self.call("session.list", json!({})).await?;
        listed
            .get("sessions")
            .and_then(Value::as_array)
            .and_then(|sessions| {
                sessions
                    .iter()
                    .find(|session| session["session_id"].as_str() == Some(session_id))
            })
            .cloned()
            .ok_or_else(|| format!("no such session: {session_id}"))
    }
}

fn closed() -> String {
    "the connection to imperium-core is closed; restart the MCP server (in Claude Code, /mcp)"
        .into()
}

async fn pump(mut client: Client, mut calls: mpsc::UnboundedReceiver<Call>, changed: Arc<Notify>) {
    loop {
        tokio::select! {
            call = calls.recv() => {
                let Some(call) = call else { return };
                let result = client
                    .call_value(&call.method, call.params)
                    .await
                    .map_err(|error| error.to_string());
                let _ = call.reply.send(result);
            }
            notification = client.next_notification() => {
                match notification {
                    Ok(notification) => {
                        if notification.method == SESSION_EVENT {
                            changed.notify_waiters();
                        }
                    }
                    Err(_) => break,
                }
            }
        }
    }
    // The daemon went away: answer what is still queued, and what comes later, with an error.
    changed.notify_waiters();
    while let Some(call) = calls.recv().await {
        let _ = call.reply.send(Err(closed()));
    }
}

/// Serves MCP over `input` and `output` until `input` ends. `client` is a connected daemon
/// client; nothing but protocol messages is ever written to `output`.
pub async fn serve<R, W>(client: Client, input: R, output: W) -> anyhow::Result<()>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (hub, pump) = Hub::start(client);
    let (out, mut replies) = mpsc::unbounded_channel::<String>();
    let writer = tokio::spawn(async move {
        let mut output = output;
        while let Some(line) = replies.recv().await {
            let written = output.write_all(line.as_bytes()).await.is_ok()
                && output.write_all(b"\n").await.is_ok()
                && output.flush().await.is_ok();
            if !written {
                break;
            }
        }
    });

    let mut lines = input.lines();
    let mut running = JoinSet::new();
    while let Some(line) = lines.next_line().await? {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let message: Value = match serde_json::from_str(line) {
            Ok(message) => message,
            Err(error) => {
                let reply = error_reply(Value::Null, -32700, &format!("parse error: {error}"));
                let _ = out.send(reply.to_string());
                continue;
            }
        };
        let hub = hub.clone();
        let out = out.clone();
        running.spawn(async move {
            if let Some(reply) = handle(&hub, message).await {
                let _ = out.send(reply.to_string());
            }
        });
    }
    running.abort_all();
    while running.join_next().await.is_some() {}
    drop(out);
    let _ = writer.await;
    pump.abort();
    Ok(())
}

fn error_reply(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

/// Answers one message; notifications (no id) get no reply.
async fn handle(hub: &Hub, message: Value) -> Option<Value> {
    let id = message.get("id").cloned()?;
    let method = message
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    let result = match method {
        "initialize" => initialize(&params),
        "ping" => json!({}),
        "tools/list" => json!({"tools": tools()}),
        "tools/call" => call_tool(hub, &params).await,
        other => {
            return Some(error_reply(
                id,
                -32601,
                &format!("method not found: {other}"),
            ));
        }
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

fn initialize(params: &Value) -> Value {
    let asked = params["protocolVersion"].as_str().unwrap_or_default();
    let version = SUPPORTED
        .iter()
        .find(|version| **version == asked)
        .copied()
        .unwrap_or(SUPPORTED[0]);
    json!({
        "protocolVersion": version,
        "capabilities": {"tools": {}},
        "serverInfo": {"name": "imperium", "version": env!("CARGO_PKG_VERSION")},
        "instructions": "Drives the running Imperium daemon. Start a session in an environment \
            with start_session, follow it with wait_for_turn (a timeout is not a failure: call \
            again with the returned cursor), read what it did with get_tool_log and \
            get_transcript, and answer its permission requests yourself with respond_permission: \
            nothing is approved automatically, and the daemon's own permission rules and safety \
            floor apply to everything an agent does."
    })
}

fn tool_result(value: &Value, is_error: bool) -> Value {
    let text = serde_json::to_string_pretty(value).unwrap_or_default();
    json!({"content": [{"type": "text", "text": text}], "isError": is_error})
}

async fn call_tool(hub: &Hub, params: &Value) -> Value {
    let name = params["name"].as_str().unwrap_or_default();
    let empty = json!({});
    let args = params.get("arguments").unwrap_or(&empty);
    match run_tool(hub, name, args).await {
        Ok(value) => tool_result(&value, false),
        Err(message) => tool_result(&json!({"error": message}), true),
    }
}

async fn run_tool(hub: &Hub, name: &str, args: &Value) -> Result<Value, String> {
    match name {
        "list_environments" => list_environments(hub).await,
        "list_sessions" => list_sessions(hub, args).await,
        "start_session" => start_session(hub, args).await,
        "send_message" => send_message(hub, args).await,
        "wait_for_turn" => wait_for_turn(hub, args).await,
        "get_session" => get_session(hub, args).await,
        "get_tool_log" => get_tool_log(hub, args).await,
        "get_transcript" => get_transcript(hub, args).await,
        "respond_permission" => respond_permission(hub, args).await,
        "answer_questions" => answer_questions(hub, args).await,
        "stop_session" => stop_session(hub, args).await,
        other => Err(format!("unknown tool {other}")),
    }
}

// Arguments ---------------------------------------------------------------------------------

/// A non-empty string argument, trimmed.
fn text_arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

fn required<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    text_arg(args, key).ok_or_else(|| format!("{key} is required"))
}

/// A string argument kept exactly as given (a message), when it is not blank.
fn raw_arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
}

fn number_arg(args: &Value, key: &str) -> Option<u64> {
    args.get(key).and_then(Value::as_u64)
}

fn flag(args: &Value, key: &str) -> bool {
    args.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn wait_arg(args: &Value, key: &str) -> Option<Duration> {
    number_arg(args, key).map(|seconds| Duration::from_secs(seconds.clamp(1, MAX_WAIT_S)))
}

/// The listed keys of an object, those it has.
fn pick(value: &Value, keys: &[&str]) -> Value {
    let mut picked = Map::new();
    for key in keys {
        if let Some(found) = value.get(*key) {
            picked.insert((*key).to_string(), found.clone());
        }
    }
    Value::Object(picked)
}

fn clip(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let kept: String = text.chars().take(limit).collect();
    format!("{kept}... (truncated)")
}

/// An environment's id, from its id or (case-insensitively) its name.
async fn resolve_environment(hub: &Hub, name: &str) -> Result<String, String> {
    let listed = hub.call("environments.list", json!({})).await?;
    let all = listed["environments"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if let Some(found) = all.iter().find(|env| env["id"].as_str() == Some(name)) {
        return Ok(found["id"].as_str().unwrap_or(name).to_string());
    }
    let matches: Vec<&Value> = all
        .iter()
        .filter(|env| {
            env["name"]
                .as_str()
                .is_some_and(|candidate| candidate.eq_ignore_ascii_case(name))
        })
        .collect();
    match matches.as_slice() {
        [one] => Ok(one["id"].as_str().unwrap_or_default().to_string()),
        [] => Err(format!(
            "no environment has the id or name {name}; list_environments shows them"
        )),
        _ => Err(format!(
            "more than one environment is named {name}; use its id (list_environments)"
        )),
    }
}

// Tools -------------------------------------------------------------------------------------

async fn list_environments(hub: &Hub) -> Result<Value, String> {
    let listed = hub.call("environments.list", json!({})).await?;
    let environments: Vec<Value> = listed["environments"]
        .as_array()
        .map(|all| {
            all.iter()
                .map(|env| pick(env, &["id", "name", "folders", "builtin", "runtime"]))
                .collect()
        })
        .unwrap_or_default();
    Ok(json!({
        "environments": environments,
        "git": listed.get("git").cloned().unwrap_or_else(|| json!([])),
    }))
}

async fn list_sessions(hub: &Hub, args: &Value) -> Result<Value, String> {
    let environment = match text_arg(args, "environment") {
        Some(name) => Some(resolve_environment(hub, name).await?),
        None => None,
    };
    let include_archived = flag(args, "include_archived");
    let listed = hub.call("session.list", json!({})).await?;
    let sessions: Vec<Value> = listed["sessions"]
        .as_array()
        .map(|all| {
            all.iter()
                .filter(|session| include_archived || session["archived"] != true)
                .filter(|session| {
                    environment
                        .as_deref()
                        .is_none_or(|id| session["environment_id"].as_str() == Some(id))
                })
                .map(session_row)
                .collect()
        })
        .unwrap_or_default();
    Ok(json!({"sessions": sessions}))
}

fn session_row(session: &Value) -> Value {
    pick(
        session,
        &[
            "session_id",
            "environment_id",
            "title",
            "status",
            "status_detail",
            "model",
            "permission_mode",
            "folders",
            "archived",
            "read_only",
        ],
    )
}

async fn start_session(hub: &Hub, args: &Value) -> Result<Value, String> {
    let mut params = Map::new();
    match (text_arg(args, "environment"), text_arg(args, "project_dir")) {
        (Some(_), Some(_)) => return Err("give environment or project_dir, not both".into()),
        (Some(name), None) => {
            let id = resolve_environment(hub, name).await?;
            params.insert("environment_id".into(), json!(id));
        }
        (None, Some(dir)) => {
            params.insert("project_dir".into(), json!(dir));
        }
        (None, None) => {}
    }
    if let Some(model) = text_arg(args, "model") {
        params.insert("model".into(), json!(model));
    }
    if let Some(subfolder) = text_arg(args, "subfolder") {
        params.insert("subfolder".into(), json!(subfolder));
    }
    if flag(args, "read_only") {
        params.insert("read_only".into(), json!(true));
    }
    if flag(args, "worktree") {
        params.insert("worktree".into(), json!(true));
    }
    let created = hub.call("session.create", Value::Object(params)).await?;
    let session_id = created["session_id"]
        .as_str()
        .ok_or("the daemon did not return a session id")?
        .to_string();
    let mut started = json!({
        "session_id": session_id,
        "model": created["model"],
        "environment_id": created["environment_id"],
        "folders": created["folders"],
        "isolation": created["isolation"],
        "worktrees": created["worktrees"],
        "cursor": 0,
    });
    if let Some(prompt) = raw_arg(args, "prompt") {
        let cursor = send(hub, &session_id, prompt, false).await?;
        started["cursor"] = json!(cursor);
        if let Some(limit) = wait_arg(args, "wait_timeout_s") {
            started["turn"] = wait_turn(hub, &session_id, Some(cursor), limit).await?;
        }
    }
    Ok(started)
}

/// Sends a message and returns the cursor (the session's event count) from before it, so a
/// wait starting there sees the turn the message starts and no earlier one.
async fn send(hub: &Hub, session_id: &str, text: &str, steer: bool) -> Result<usize, String> {
    let cursor = hub.history(session_id).await?.len();
    let mut params = json!({"session_id": session_id, "text": text});
    if steer {
        params["mode"] = json!("steer");
    }
    hub.call("session.send", params).await?;
    Ok(cursor)
}

async fn send_message(hub: &Hub, args: &Value) -> Result<Value, String> {
    let session_id = required(args, "session_id")?;
    let text = raw_arg(args, "text").ok_or("text is required")?;
    let cursor = send(hub, session_id, text, flag(args, "steer")).await?;
    let mut sent = json!({"session_id": session_id, "cursor": cursor});
    if let Some(limit) = wait_arg(args, "wait_timeout_s") {
        sent["turn"] = wait_turn(hub, session_id, Some(cursor), limit).await?;
    }
    Ok(sent)
}

async fn wait_for_turn(hub: &Hub, args: &Value) -> Result<Value, String> {
    let session_id = required(args, "session_id")?;
    let since = number_arg(args, "since").map(|cursor| cursor as usize);
    let limit = wait_arg(args, "timeout_s").unwrap_or(Duration::from_secs(DEFAULT_WAIT_S));
    wait_turn(hub, session_id, since, limit).await
}

/// Waits for the turn that began at or after `since` to end, or for something that needs the
/// caller (a permission request or questions), or for `limit`.
async fn wait_turn(
    hub: &Hub,
    session_id: &str,
    since: Option<usize>,
    limit: Duration,
) -> Result<Value, String> {
    let deadline = Instant::now() + limit;
    loop {
        let history = hub.history(session_id).await?;
        let turn = analyze(&history, since);
        if turn.outcome != "running" {
            return Ok(turn.report);
        }
        let now = Instant::now();
        if now >= deadline {
            let mut report = turn.report;
            report["outcome"] = json!("timeout");
            if let Ok(summary) = hub.summary(session_id).await {
                report["status"] = summary["status"].clone();
            }
            return Ok(report);
        }
        let pause = (deadline - now).min(POLL);
        tokio::select! {
            _ = hub.changed.notified() => {}
            _ = tokio::time::sleep(pause) => {}
        }
    }
}

async fn get_session(hub: &Hub, args: &Value) -> Result<Value, String> {
    let session_id = required(args, "session_id")?;
    let summary = hub.summary(session_id).await?;
    let history = hub.history(session_id).await?;
    let turn = analyze(&history, None);
    Ok(json!({
        "session": session_row(&summary),
        "latest_turn": turn.outcome,
        "pending_permissions": turn.report["pending_permissions"],
        "pending_questions": turn.report["pending_questions"],
        "cursor": history.len(),
    }))
}

async fn get_tool_log(hub: &Hub, args: &Value) -> Result<Value, String> {
    let session_id = required(args, "session_id")?;
    let since = number_arg(args, "since").map_or(0, |cursor| cursor as usize);
    let limit = number_arg(args, "limit").map_or(200, |limit| limit.clamp(1, 2000) as usize);
    let history = hub.history(session_id).await?;
    let turn = analyze(&history, Some(since));
    let entries = turn.report["tool_log"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let skipped = entries.len().saturating_sub(limit);
    Ok(json!({
        "entries": entries.into_iter().skip(skipped).collect::<Vec<_>>(),
        "omitted_older": skipped,
        "cursor": history.len(),
    }))
}

async fn get_transcript(hub: &Hub, args: &Value) -> Result<Value, String> {
    let session_id = required(args, "session_id")?;
    let format = match text_arg(args, "format").unwrap_or("markdown") {
        "markdown" | "md" => "markdown",
        "json" => "json",
        other => return Err(format!("unknown format {other}: use markdown or json")),
    };
    let limit = number_arg(args, "max_chars").map_or(TRANSCRIPT_LIMIT, |limit| limit as usize);
    let exported = hub
        .call(
            "session.export",
            json!({"session_id": session_id, "format": format}),
        )
        .await?;
    let content = exported["content"].as_str().unwrap_or_default();
    let total = content.chars().count();
    // The end is what a caller reading a long run wants, so a long transcript loses its start.
    let (shown, omitted) = if total > limit {
        let tail: String = content.chars().skip(total - limit).collect();
        (tail, total - limit)
    } else {
        (content.to_string(), 0)
    };
    let mut result = json!({
        "format": format,
        "omitted_chars": omitted,
        "content": shown,
    });
    if let Some(warning) = exported.get("warning").filter(|warning| !warning.is_null()) {
        result["warning"] = warning.clone();
    }
    Ok(result)
}

async fn respond_permission(hub: &Hub, args: &Value) -> Result<Value, String> {
    let session_id = required(args, "session_id")?;
    let request_id = number_arg(args, "request_id").ok_or("request_id is required")?;
    let decision = match required(args, "decision")? {
        decision @ ("allow" | "allow_for_session" | "deny") => decision,
        other => {
            return Err(format!(
                "unknown decision {other}: use allow, allow_for_session or deny"
            ));
        }
    };
    hub.call(
        "session.respond",
        json!({"session_id": session_id, "request_id": request_id, "decision": decision}),
    )
    .await?;
    Ok(json!({"session_id": session_id, "request_id": request_id, "decision": decision}))
}

async fn answer_questions(hub: &Hub, args: &Value) -> Result<Value, String> {
    let session_id = required(args, "session_id")?;
    let request_id = number_arg(args, "request_id").ok_or("request_id is required")?;
    let answers: Vec<String> = args
        .get("answers")
        .and_then(Value::as_array)
        .ok_or("answers is required: one string per question, in order")?
        .iter()
        .map(|answer| answer.as_str().unwrap_or_default().to_string())
        .collect();
    hub.call(
        "session.answer",
        json!({"session_id": session_id, "request_id": request_id, "answers": answers}),
    )
    .await?;
    Ok(json!({"session_id": session_id, "request_id": request_id, "answered": answers.len()}))
}

async fn stop_session(hub: &Hub, args: &Value) -> Result<Value, String> {
    let session_id = required(args, "session_id")?;
    hub.call(
        "session.control",
        json!({"session_id": session_id, "action": "stop"}),
    )
    .await?;
    Ok(json!({"session_id": session_id, "stopped": true}))
}

// Reading a session's events ----------------------------------------------------------------

/// What a window of a session's events says about the turn in it.
struct Turn {
    /// `completed`, `failed`, `stopped`, `permission_required`, `questions_asked`, or
    /// `running` (waiting calls turn that into `timeout` when their time is up).
    outcome: &'static str,
    report: Value,
}

fn entry(kind: &str, agent: &str, body: &Value, keys: &[&str]) -> Value {
    let mut entry = pick(body, keys);
    entry["event"] = json!(kind);
    entry["agent_id"] = json!(agent);
    entry
}

/// Reads `history` from `since` (default: the latest root turn start) on. A turn ends only if
/// the window holds the root agent's `turn_started`, so a turn that was already finishing when
/// a message was sent does not count as that message's turn. A permission request or question
/// is pending until its agent moves on (a tool starting or finishing) or it is answered.
fn analyze(history: &[Value], since: Option<usize>) -> Turn {
    let start = match since {
        Some(cursor) => cursor.min(history.len()),
        None => history
            .iter()
            .rposition(|event| {
                event["agent_id"] == "root" && event["event"]["type"] == "turn_started"
            })
            .unwrap_or(0),
    };
    let mut started = false;
    let mut text = String::new();
    let mut log: Vec<Value> = Vec::new();
    let mut permissions: Vec<(String, Value)> = Vec::new();
    let mut questions: Vec<(u64, Value)> = Vec::new();
    let mut notices: Vec<Value> = Vec::new();
    let mut detail = Map::new();
    let mut outcome: Option<&'static str> = None;
    for event in &history[start..] {
        let agent = event["agent_id"].as_str().unwrap_or_default();
        let body = &event["event"];
        let root = agent == "root";
        match body["type"].as_str().unwrap_or_default() {
            "turn_started" if root => {
                started = true;
                text.clear();
            }
            "text_delta" if root => text.push_str(body["text"].as_str().unwrap_or_default()),
            "tool_started" => {
                permissions.retain(|(waiting, _)| waiting != agent);
                log.push(entry(
                    "tool_started",
                    agent,
                    body,
                    &["call_id", "tool", "summary", "arguments"],
                ));
            }
            "tool_finished" => {
                permissions.retain(|(waiting, _)| waiting != agent);
                let mut finished = entry("tool_finished", agent, body, &["call_id", "tool", "ok"]);
                finished["output"] = json!(clip(
                    body["output"].as_str().unwrap_or_default(),
                    OUTPUT_LIMIT
                ));
                log.push(finished);
            }
            "permission_requested" => {
                let request = entry(
                    "permission_requested",
                    agent,
                    body,
                    &["request_id", "tool", "summary", "suggested_rule"],
                );
                permissions.retain(|(waiting, _)| waiting != agent);
                permissions.push((agent.to_string(), request.clone()));
                log.push(request);
            }
            "questions_asked" => {
                let request_id = body["request_id"].as_u64().unwrap_or_default();
                questions.push((
                    request_id,
                    entry("questions_asked", agent, body, &["request_id", "questions"]),
                ));
            }
            "questions_answered" => {
                let request_id = body["request_id"].as_u64().unwrap_or_default();
                questions.retain(|(waiting, _)| *waiting != request_id);
            }
            "notice" => notices.push(body["message"].clone()),
            "turn_completed" if root && started => {
                outcome = Some("completed");
                for key in ["usage", "cost_usd", "steps"] {
                    detail.insert(key.to_string(), body[key].clone());
                }
                break;
            }
            "turn_failed" if root && started => {
                outcome = Some("failed");
                detail.insert("message".into(), body["message"].clone());
                break;
            }
            "work_stopped" if root && started => {
                outcome = Some("stopped");
                break;
            }
            _ => {}
        }
    }
    let outcome = outcome.unwrap_or(if !permissions.is_empty() {
        "permission_required"
    } else if !questions.is_empty() {
        "questions_asked"
    } else {
        "running"
    });
    let mut report = json!({
        "outcome": outcome,
        "cursor": history.len(),
        "since": start,
        "assistant_text": text,
        "tool_log": log,
        "pending_permissions": permissions.iter().map(|(_, request)| request.clone()).collect::<Vec<_>>(),
        "pending_questions": questions.iter().map(|(_, asked)| asked.clone()).collect::<Vec<_>>(),
        "notices": notices,
    });
    for (key, value) in detail {
        report[key] = value;
    }
    Turn { outcome, report }
}

// Tool descriptions -------------------------------------------------------------------------

fn tool(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": {"type": "object", "properties": properties, "required": required},
    })
}

fn tools() -> Vec<Value> {
    let session = json!({"type": "string", "description": "The session id (from start_session or list_sessions)."});
    let since = json!({"type": "integer", "description": "A cursor from start_session or send_message: look only at events from there on. Leave out to look at the latest turn."});
    let wait = json!({"type": "integer", "description": "Also wait up to this many seconds (1 to 1800) for the turn to end and return its result as `turn`."});
    vec![
        tool(
            "list_environments",
            "List Imperium's environments: the named sets of folders a session works in, with their ids.",
            json!({}),
            &[],
        ),
        tool(
            "list_sessions",
            "List the daemon's sessions with their status (idle, working, needs_attention, ...), model and environment.",
            json!({
                "environment": {"type": "string", "description": "Only sessions in this environment (name or id)."},
                "include_archived": {"type": "boolean", "description": "Also list archived sessions."},
            }),
            &[],
        ),
        tool(
            "start_session",
            "Start an Imperium agent session in an environment (or a project folder) and optionally give it a first prompt. The agent runs under the daemon's own permission rules: anything it needs approval for waits for respond_permission.",
            json!({
                "environment": {"type": "string", "description": "Environment name or id; where the agent works. Give this or project_dir; with neither, the Home environment."},
                "project_dir": {"type": "string", "description": "Absolute folder to work in (its environment is made if needed)."},
                "prompt": {"type": "string", "description": "The first message; the agent starts on it at once."},
                "model": {"type": "string", "description": "A model table id or <endpoint>:<model>; the daemon's default when left out. The runtime is the environment's."},
                "subfolder": {"type": "string", "description": "Narrow the session to this folder inside the environment."},
                "read_only": {"type": "boolean", "description": "Mount the folders read-only."},
                "worktree": {"type": "boolean", "description": "Work on new git worktrees of the environment's repositories."},
                "wait_timeout_s": wait,
            }),
            &[],
        ),
        tool(
            "send_message",
            "Send a message to a session. It starts a turn when the agent is idle and queues behind the running one otherwise (steer: true joins the running turn instead).",
            json!({
                "session_id": session,
                "text": {"type": "string"},
                "steer": {"type": "boolean"},
                "wait_timeout_s": wait,
            }),
            &["session_id", "text"],
        ),
        tool(
            "wait_for_turn",
            "Wait for a session's turn to end. Returns early with outcome permission_required (answer with respond_permission) or questions_asked (answer_questions); outcome timeout means it is still running: call again with the returned cursor. Also returns the agent's reply text, its tool calls and its token usage.",
            json!({
                "session_id": session,
                "since": since,
                "timeout_s": {"type": "integer", "description": "Seconds to wait, 1 to 1800 (default 60)."},
            }),
            &["session_id"],
        ),
        tool(
            "get_session",
            "A session's status, pending permission requests and questions, and the latest turn's outcome.",
            json!({"session_id": session}),
            &["session_id"],
        ),
        tool(
            "get_tool_log",
            "The tool calls a session's agents made, with arguments, results and permission requests, oldest first.",
            json!({
                "session_id": session,
                "since": since,
                "limit": {"type": "integer", "description": "Newest entries to return (default 200)."},
            }),
            &["session_id"],
        ),
        tool(
            "get_transcript",
            "A session's full transcript as the daemon exports it: messages, tool calls and results, subagent logs. Verbatim, so it may hold secrets the agents read. A long transcript loses its start.",
            json!({
                "session_id": session,
                "format": {"type": "string", "enum": ["markdown", "json"]},
                "max_chars": {"type": "integer", "description": "Characters to return (default 60000)."},
            }),
            &["session_id"],
        ),
        tool(
            "respond_permission",
            "Answer a pending permission request, as the user would. Only you decide: nothing is approved automatically. allow_for_session also allows that tool for the rest of the session.",
            json!({
                "session_id": session,
                "request_id": {"type": "integer", "description": "The request_id from pending_permissions."},
                "decision": {"type": "string", "enum": ["allow", "allow_for_session", "deny"]},
            }),
            &["session_id", "request_id", "decision"],
        ),
        tool(
            "answer_questions",
            "Answer an agent's ask_user questions: one answer per question, in order.",
            json!({
                "session_id": session,
                "request_id": {"type": "integer"},
                "answers": {"type": "array", "items": {"type": "string"}},
            }),
            &["session_id", "request_id", "answers"],
        ),
        tool(
            "stop_session",
            "Stop a session's work now (the session stays and can be messaged again).",
            json!({"session_id": session}),
            &["session_id"],
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(agent: &str, body: Value) -> Value {
        json!({"agent_id": agent, "event": body})
    }

    fn started() -> Value {
        event("root", json!({"type": "turn_started", "text": "go"}))
    }

    fn completed() -> Value {
        event(
            "root",
            json!({"type": "turn_completed", "usage": {"prompt_tokens": 1}, "cost_usd": null, "steps": 1}),
        )
    }

    fn ask(request_id: u64) -> Value {
        event(
            "root",
            json!({"type": "permission_requested", "request_id": request_id, "tool": "write_file", "summary": "write a"}),
        )
    }

    #[test]
    fn a_completed_turn_reports_its_reply_and_tools() {
        let history = vec![
            started(),
            event("root", json!({"type": "text_delta", "text": "He"})),
            event(
                "root",
                json!({"type": "tool_started", "call_id": "c0", "tool": "read_file", "summary": "read a"}),
            ),
            event(
                "root",
                json!({"type": "tool_finished", "call_id": "c0", "ok": true, "output": "x", "tool": "read_file"}),
            ),
            event("root", json!({"type": "text_delta", "text": "llo"})),
            completed(),
        ];
        let turn = analyze(&history, None);
        assert_eq!(turn.outcome, "completed");
        assert_eq!(turn.report["assistant_text"], "Hello");
        assert_eq!(turn.report["tool_log"].as_array().unwrap().len(), 2);
        assert_eq!(turn.report["tool_log"][1]["ok"], true);
        assert_eq!(turn.report["cursor"], 6);
    }

    #[test]
    fn a_permission_request_is_pending_until_the_agent_moves_on() {
        let mut history = vec![started(), ask(7)];
        let turn = analyze(&history, None);
        assert_eq!(turn.outcome, "permission_required");
        assert_eq!(turn.report["pending_permissions"][0]["request_id"], 7);

        history.push(event(
            "root",
            json!({"type": "tool_finished", "call_id": "c0", "ok": false, "output": "denied"}),
        ));
        assert_eq!(analyze(&history, None).outcome, "running");
        history.push(completed());
        assert_eq!(analyze(&history, None).outcome, "completed");
    }

    #[test]
    fn a_turn_already_ending_is_not_the_turn_of_a_new_message() {
        // The cursor was taken after the old turn's start but before its end.
        let history = vec![started(), completed()];
        assert_eq!(analyze(&history, Some(1)).outcome, "running");
        let mut later = history;
        later.push(started());
        later.push(event(
            "root",
            json!({"type": "turn_failed", "message": "no model"}),
        ));
        let turn = analyze(&later, Some(1));
        assert_eq!(turn.outcome, "failed");
        assert_eq!(turn.report["message"], "no model");
    }

    #[test]
    fn questions_are_pending_until_answered() {
        let asked = event(
            "root",
            json!({"type": "questions_asked", "request_id": 2, "questions": []}),
        );
        let answered = event(
            "root",
            json!({"type": "questions_answered", "request_id": 2}),
        );
        assert_eq!(
            analyze(&[started(), asked.clone()], None).outcome,
            "questions_asked"
        );
        assert_eq!(
            analyze(&[started(), asked, answered], None).outcome,
            "running"
        );
    }

    #[test]
    fn a_decision_must_be_stated() {
        assert_eq!(clip("abcdef", 3), "abc... (truncated)");
        assert_eq!(clip("abc", 3), "abc");
        assert_eq!(text_arg(&json!({"a": "  "}), "a"), None);
        assert_eq!(
            initialize(&json!({"protocolVersion": "2024-11-05"}))["protocolVersion"],
            "2024-11-05"
        );
        assert_eq!(
            initialize(&json!({"protocolVersion": "1999"}))["protocolVersion"],
            SUPPORTED[0]
        );
    }
}
