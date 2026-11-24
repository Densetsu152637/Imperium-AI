//! `imperium run --headless`: a session in a real daemon (the user's permission rules, isolation
//! tier and protected folders apply), driven against a scripted OpenAI-compatible endpoint, with
//! subagents answered from recordings.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::routing::post;
use clap::Parser;
use imperium_cli::client::Client;
use imperium_cli::eval::{Outcome, RunArgs, run};
use imperium_config::{Config, Layer, Paths, Source};
use imperium_core::{AccessToken, DaemonState};
use imperium_protocol::PROTOCOL_VERSION;
use imperium_protocol::discovery::DaemonEndpoint;
use imperium_protocol::jsonrpc::codes;
use imperium_protocol::methods::{
    EnvironmentList, EnvironmentListParams, EvalModel, EvalOptions, EvalVariant, Peer,
    SessionEvalCreate, SessionEvalCreateParams, SessionEvalResult, SessionEvalResultParams,
    SessionList, SessionListParams, SessionPromptPreview, SessionSend, SessionSendParams,
};
use serde_json::{Value, json};
use tokio::net::TcpListener;

#[derive(Parser)]
struct Wrap {
    #[command(flatten)]
    args: RunArgs,
}

fn args(list: &[String]) -> RunArgs {
    let all = std::iter::once("run".to_string()).chain(list.iter().cloned());
    Wrap::try_parse_from(all).expect("arguments").args
}

fn strings(list: &[&str]) -> Vec<String> {
    list.iter().map(|text| (*text).to_string()).collect()
}

/// A daemon of its own, in a folder of its own, with `config` as the user's settings.
struct Daemon {
    endpoint: DaemonEndpoint,
    data: tempfile::TempDir,
}

impl Daemon {
    async fn start(config: &str) -> Daemon {
        let data = tempfile::tempdir().unwrap();
        let settings = Config::from_layers([
            Layer::defaults(),
            Layer::new(Source::User("test".into()), config.parse().unwrap()),
        ])
        .settings()
        .unwrap();
        let state = DaemonState::new(Paths::under(data.path()), settings, None);
        // The tests reach the daemon over loopback TCP; the command uses the local socket.
        state.allow_eval_over_tcp();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let token = AccessToken::generate().unwrap();
        let endpoint = DaemonEndpoint {
            url: format!("ws://{}/rpc", listener.local_addr().unwrap()),
            token: token.as_str().to_string(),
            pid: std::process::id(),
            socket: None,
            protocol: PROTOCOL_VERSION,
        };
        tokio::spawn(imperium_core::serve(
            listener,
            token,
            state,
            std::future::pending(),
        ));
        Daemon { endpoint, data }
    }

    async fn client_named(&self, name: &str) -> Client {
        Client::connect(
            &self.endpoint,
            Peer {
                name: name.into(),
                version: "0".into(),
            },
        )
        .await
        .unwrap()
    }

    /// The `imperium` command's connection.
    async fn client(&self) -> Client {
        self.client_named("imperium-cli").await
    }

    /// `imperium run` with `list` as its arguments.
    async fn run(&self, list: &[String]) -> Outcome {
        let mut client = self.client().await;
        run(&mut client, args(list)).await.unwrap()
    }

    async fn print_prompt(&self, extra: &[&str]) -> Outcome {
        let mut list = strings(&["--print-prompt"]);
        list.extend(strings(extra));
        self.run(&list).await
    }

    /// The folder the user's configuration lives in (the daemon's `<config>`).
    fn config_dir(&self) -> std::path::PathBuf {
        Paths::under(self.data.path()).config_dir().to_path_buf()
    }
}

fn tool_call_body(name: &str, arguments: &Value) -> String {
    let chunk = json!({"choices":[{"delta":{"tool_calls":[{
        "index":0,"id":format!("call-{name}"),"function":{"name":name,"arguments":arguments.to_string()}}]}}]});
    format!(
        "data: {chunk}\n\ndata: {}\n\ndata: [DONE]\n\n",
        r#"{"choices":[],"usage":{"prompt_tokens":100,"completion_tokens":20}}"#
    )
}

fn text_body(text: &str) -> String {
    let chunk = json!({"choices":[{"delta":{"content":text}}]});
    format!(
        "data: {chunk}\n\ndata: {}\n\ndata: [DONE]\n\n",
        r#"{"choices":[],"usage":{"prompt_tokens":150,"completion_tokens":10}}"#
    )
}

/// Every request body the endpoint received.
type Seen = Arc<Mutex<Vec<Value>>>;

/// An endpoint that answers request `n` with `bodies[n]` (the last one from then on), and the
/// request bodies it received. Returns the base URL with the model id after `#`.
async fn endpoint(bodies: Vec<String>) -> (String, Seen) {
    endpoint_with(move |index, _| bodies[index.min(bodies.len() - 1)].clone()).await
}

/// An endpoint whose answer to request `n` is `reply(n, body)`.
async fn endpoint_with(
    reply: impl Fn(usize, &Value) -> String + Send + Sync + 'static,
) -> (String, Seen) {
    let seen: Seen = Arc::default();
    let calls = Arc::new(AtomicUsize::new(0));
    let reply = Arc::new(reply);
    let record = Arc::clone(&seen);
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |axum::Json(body): axum::Json<Value>| {
            let index = calls.fetch_add(1, Ordering::SeqCst);
            let reply = reply(index, &body);
            record.lock().unwrap().push(body);
            async move { ([("content-type", "text/event-stream")], reply) }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1#fake-model", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    (url, seen)
}

fn plan_call() -> String {
    tool_call_body(
        "update_plan",
        &json!({"goal": "fix the bug", "steps": [{"title": "reproduce", "status": "pending"}]}),
    )
}

fn spawn_call() -> String {
    tool_call_body(
        "spawn_agent",
        &json!({"brief": "Find why the parser test fails"}),
    )
}

fn script() -> Vec<String> {
    vec![
        plan_call(),
        spawn_call(),
        tool_call_body("await_tasks", &json!({})),
        text_body("The parser test fails on empty input."),
    ]
}

fn write(dir: &tempfile::TempDir, name: &str, text: &str) -> String {
    let path = dir.path().join(name);
    std::fs::write(&path, text).unwrap();
    path.display().to_string()
}

fn tool_names(json: &Value) -> Vec<String> {
    json["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["function"]["name"].as_str().unwrap().to_string())
        .collect()
}

/// The arguments of a run of `task` in `workspace` against `url`, and then `extra`.
fn run_args(task: &str, workspace: &std::path::Path, url: &str, extra: &[&str]) -> Vec<String> {
    let mut list = strings(&["--headless", "--task", task, "--root-model", url]);
    list.push("--workspace".into());
    list.push(workspace.display().to_string());
    list.extend(strings(extra));
    list
}

#[tokio::test]
async fn print_prompt_hash_is_stable_and_names_the_real_prompt_and_tools() {
    let daemon = Daemon::start("").await;
    let first = daemon.print_prompt(&["--context-tokens", "32768"]).await;
    let second = daemon.print_prompt(&["--context-tokens", "32768"]).await;
    assert_eq!(first.code, 0);
    assert_eq!(first.json["prompt_hash"], second.json["prompt_hash"]);
    assert_eq!(first.json["system_prompt"], second.json["system_prompt"]);
    let hash = first.json["prompt_hash"].as_str().unwrap();
    assert!(hash.starts_with("sha256:") && hash.len() == "sha256:".len() + 64);
    let prompt = first.json["system_prompt"].as_str().unwrap();
    assert!(prompt.starts_with("You are Imperium, a coding agent"));
    assert!(prompt.contains("The fear of God is the beginning of all knowledge"));
    assert!(prompt.contains("{workspace}"));
    let names = tool_names(&first.json);
    for expected in [
        "spawn_agent",
        "await_tasks",
        "update_plan",
        "delegate_chunks",
        "ask_user",
        "web_search",
        "web_fetch",
    ] {
        assert!(
            names.iter().any(|name| name == expected),
            "{expected} in {names:?}"
        );
    }
    assert_eq!(first.json["variant"]["id"], "baseline");
}

#[tokio::test]
async fn a_variant_overlay_changes_the_prompt_and_the_hash() {
    let daemon = Daemon::start("").await;
    let dir = tempfile::tempdir().unwrap();
    let baseline = daemon.print_prompt(&[]).await;
    let overlay = write(
        &dir,
        "overlay.txt",
        "Context budget: {context_k}K tokens.\n",
    );
    let overlaid = daemon
        .print_prompt(&["--prompt-overlay", &overlay, "--context-tokens", "65536"])
        .await;
    assert_ne!(baseline.json["prompt_hash"], overlaid.json["prompt_hash"]);
    assert_ne!(
        baseline.json["variant"]["config_hash"],
        overlaid.json["variant"]["config_hash"]
    );
    let prompt = overlaid.json["system_prompt"].as_str().unwrap();
    assert!(prompt.ends_with("Context budget: 64K tokens."), "{prompt}");
    assert_eq!(overlaid.json["variant"]["overlay"], true);

    let file = write(
        &dir,
        "lean.toml",
        r#"
        description = "lean"
        [prompt]
        replace = [{ find = "Act through the tools", with = "Use the tools" }]
        [tools]
        drop = ["delegate_chunks"]
        rename = { ask_user = "ask_the_user" }
        [tools.description]
        update_plan = "Keep the checklist."
        "#,
    );
    let lean = daemon.print_prompt(&["--variant", &file]).await;
    assert_eq!(lean.json["variant"]["id"], "lean");
    assert_eq!(lean.json["variant"]["warnings"], json!([]));
    assert!(
        lean.json["system_prompt"]
            .as_str()
            .unwrap()
            .contains("Use the tools")
    );
    let tools = lean.json["tools"].as_array().unwrap();
    let find = |name: &str| tools.iter().find(|tool| tool["function"]["name"] == name);
    assert!(find("delegate_chunks").is_none());
    assert!(find("ask_user").is_none() && find("ask_the_user").is_some());
    assert_eq!(
        find("update_plan").unwrap()["function"]["description"],
        "Keep the checklist."
    );
    assert_ne!(baseline.json["prompt_hash"], lean.json["prompt_hash"]);

    let missed = write(
        &dir,
        "missed.toml",
        "[prompt]\nremove = [\"no such paragraph\"]\n",
    );
    let warned = daemon.print_prompt(&["--variant", &missed]).await;
    assert_eq!(
        warned.json["variant"]["warnings"].as_array().unwrap().len(),
        1
    );
}

#[tokio::test]
async fn removing_the_base_paragraph_is_refused_and_a_flag_drops_the_knowledge_line() {
    let daemon = Daemon::start("").await;
    let dir = tempfile::tempdir().unwrap();
    let file = write(
        &dir,
        "remove.toml",
        "[prompt]\nremove = [\"Research that several later tasks\"]\n",
    );
    let removed = daemon.print_prompt(&["--variant", &file]).await;
    let prompt = removed.json["system_prompt"].as_str().unwrap();
    assert!(prompt.starts_with("You are Imperium, a coding agent"));
    assert!(prompt.contains("The fear of God"));
    assert_eq!(
        removed.json["variant"]["warnings"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    let baseline = daemon.print_prompt(&[]).await;
    let off = daemon.print_prompt(&["--flag", "knowledge=false"]).await;
    let with = baseline.json["system_prompt"].as_str().unwrap();
    let without = off.json["system_prompt"].as_str().unwrap();
    assert!(with.contains("Research that several later tasks"));
    assert!(!without.contains("Research that several later tasks"));
    assert!(!tool_names(&off.json).contains(&"keep_as_knowledge".to_string()));
}

#[tokio::test]
async fn a_rename_onto_an_existing_tool_is_skipped_with_a_warning() {
    let daemon = Daemon::start("").await;
    let dir = tempfile::tempdir().unwrap();
    let file = write(
        &dir,
        "clash.toml",
        "[tools]\nrename = { ask_user = \"await_tasks\" }\n",
    );
    let clash = daemon.print_prompt(&["--variant", &file]).await;
    let names = tool_names(&clash.json);
    assert!(names.contains(&"ask_user".to_string()));
    assert_eq!(
        names.iter().filter(|name| *name == "await_tasks").count(),
        1
    );
    assert_eq!(
        clash.json["variant"]["warnings"].as_array().unwrap().len(),
        1
    );
    let twice = write(
        &dir,
        "twice.toml",
        "[tools.rename]\nask_user = \"x\"\nupdate_plan = \"x\"\n",
    );
    let both = strings(&["--print-prompt", "--variant", &twice]);
    let mut client = daemon.client().await;
    assert!(run(&mut client, args(&both)).await.is_err());
}

#[tokio::test]
async fn local_root_models_get_the_harness_aids_that_are_auto() {
    let daemon = Daemon::start("").await;
    let hosted = daemon.print_prompt(&["--flag", "attended=true"]).await;
    let local = daemon
        .print_prompt(&["--flag", "attended=true", "--local"])
        .await;
    assert!(!tool_names(&hosted.json).contains(&"request_clarification".to_string()));
    assert!(tool_names(&local.json).contains(&"request_clarification".to_string()));
}

#[tokio::test]
async fn a_run_replays_subagents_and_prints_the_documented_json() {
    let daemon = Daemon::start("").await;
    let workspace = tempfile::tempdir().unwrap();
    let files = tempfile::tempdir().unwrap();
    let recordings = write(
        &files,
        "recordings.json",
        r#"{"replies": [
            {"brief_contains": "parser test", "report": "The parser rejects empty input."}
        ]}"#,
    );
    let (url, seen) = endpoint(script()).await;
    let outcome = daemon
        .run(&run_args(
            "The parser tests fail. Find out why.",
            workspace.path(),
            &url,
            &[
                "--json",
                "--context-tokens",
                "32768",
                "--subagent-replay",
                &recordings,
                "--price-in",
                "1",
                "--price-out",
                "2",
            ],
        ))
        .await;
    let out = &outcome.json;
    assert_eq!(outcome.code, 0, "{out:#}");
    assert_eq!(out["exit_reason"], "completed");

    // The schema: these keys, in this form.
    let mut keys: Vec<&str> = out
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "agent",
            "compactions",
            "endpoint_errors",
            "engine",
            "error",
            "exit_reason",
            "failed_tool_calls",
            "final_reply",
            "ledger",
            "mode",
            "model",
            "notices",
            "permission_requests",
            "plan",
            "prompt_hash",
            "questions_asked",
            "replay",
            "root_tool_calls",
            "schema_version",
            "subagents",
            "system_prompt",
            "task",
            "tools",
            "transcript",
            "variant",
            "wall_time_ms",
        ]
    );
    assert_eq!(out["schema_version"], 1);
    assert_eq!(out["mode"], "run");
    assert_eq!(
        out["task"]["prompt"],
        "The parser tests fail. Find out why."
    );
    assert_eq!(out["model"]["model_id"], "fake-model");
    assert_eq!(out["model"]["context_tokens"], 32768);
    assert_eq!(out["variant"]["id"], "baseline");
    assert!(
        out["variant"]["config_hash"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );
    assert!(out["prompt_hash"].as_str().unwrap().starts_with("sha256:"));
    assert_eq!(out["final_reply"], "The parser test fails on empty input.");
    assert_eq!(out["plan"]["goal"], "fix the bug");
    assert_eq!(out["error"], Value::Null);

    // The transcript is in the OpenAI chat shape and holds the exact system prompt sent.
    let transcript = out["transcript"].as_array().unwrap();
    assert_eq!(transcript[0]["role"], "system");
    assert_eq!(transcript[0]["content"], out["system_prompt"]);
    assert_eq!(transcript[1]["role"], "user");
    let called: Vec<&str> = transcript
        .iter()
        .filter_map(|message| message["tool_calls"].as_array())
        .flatten()
        .map(|call| call["function"]["name"].as_str().unwrap())
        .collect();
    assert_eq!(called, ["update_plan", "spawn_agent", "await_tasks"]);
    let results: Vec<&str> = transcript
        .iter()
        .filter(|message| message["role"] == "tool")
        .map(|message| message["content"].as_str().unwrap())
        .collect();
    assert!(
        results
            .iter()
            .any(|text| text.contains("The parser rejects empty input.")),
        "{results:?}"
    );
    assert_eq!(transcript.last().unwrap()["role"], "assistant");

    // The subagent was answered from the recording: no model call for it.
    let subagent = &out["subagents"][0];
    assert_eq!(subagent["task_id"], "t1");
    assert_eq!(subagent["source"], "replay");
    assert_eq!(subagent["recording"], 0);
    assert_eq!(subagent["ok"], true);
    assert_eq!(subagent["report"], "The parser rejects empty input.");
    assert_eq!(out["replay"], json!({"replies": 1, "unused": []}));
    assert_eq!(seen.lock().unwrap().len(), 4);

    // The ledger counts the four root calls (100/20, 100/20, 100/20, 150/10 tokens).
    assert_eq!(out["ledger"]["root"]["model_calls"], 4);
    assert_eq!(out["ledger"]["root"]["prompt_tokens"], 450);
    assert_eq!(out["ledger"]["root"]["completion_tokens"], 70);
    assert_eq!(out["ledger"]["subagents"]["model_calls"], 0);
    assert_eq!(out["ledger"]["total"]["model_calls"], 4);
    let cost = out["ledger"]["total"]["cost_usd"].as_f64().unwrap();
    assert!(
        (cost - (450.0 + 140.0) / 1_000_000.0).abs() < 1e-12,
        "{cost}"
    );

    // The same run again gives the same hash and the same transcript: nothing random in it.
    let (url, _) = endpoint(script()).await;
    let again = daemon
        .run(&run_args(
            "The parser tests fail. Find out why.",
            workspace.path(),
            &url,
            &[
                "--context-tokens",
                "32768",
                "--subagent-replay",
                &recordings,
            ],
        ))
        .await;
    assert_eq!(again.json["prompt_hash"], out["prompt_hash"]);
    assert_eq!(again.json["transcript"], out["transcript"]);
}

#[tokio::test]
async fn an_unknown_replay_key_is_an_error_result_not_a_hang() {
    let daemon = Daemon::start("").await;
    let workspace = tempfile::tempdir().unwrap();
    let files = tempfile::tempdir().unwrap();
    let recordings = write(
        &files,
        "recordings.json",
        r#"{"replies": [{"task_id": "t9", "report": "never asked"}]}"#,
    );
    let (url, _) = endpoint(script()).await;
    let outcome = daemon
        .run(&run_args(
            "Investigate.",
            workspace.path(),
            &url,
            &["--subagent-replay", &recordings],
        ))
        .await;
    let out = &outcome.json;
    assert_eq!(out["exit_reason"], "completed", "{out:#}");
    let subagent = &out["subagents"][0];
    assert_eq!(subagent["ok"], false);
    assert_eq!(subagent["recording"], Value::Null);
    let report = subagent["report"].as_str().unwrap();
    assert!(report.starts_with("replay error:"), "{report}");
    let results: Vec<&str> = out["transcript"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|message| message["role"] == "tool")
        .map(|message| message["content"].as_str().unwrap())
        .collect();
    assert!(
        results.iter().any(|text| text.contains("replay error:")),
        "{results:?}"
    );
    assert_eq!(out["replay"], json!({"replies": 1, "unused": [0]}));
}

#[tokio::test]
async fn a_once_recording_answers_one_subagent_and_is_then_consumed() {
    let daemon = Daemon::start("").await;
    let workspace = tempfile::tempdir().unwrap();
    let files = tempfile::tempdir().unwrap();
    let recordings = write(
        &files,
        "recordings.json",
        r#"{"replies": [
            {"brief_contains": "parser test", "report": "first look", "once": true},
            {"brief_contains": "parser test", "report": "later looks"}
        ]}"#,
    );
    let (url, _) = endpoint(vec![
        spawn_call(),
        spawn_call(),
        tool_call_body("await_tasks", &json!({})),
        text_body("Both reported."),
    ])
    .await;
    let outcome = daemon
        .run(&run_args(
            "Investigate twice.",
            workspace.path(),
            &url,
            &["--subagent-replay", &recordings],
        ))
        .await;
    let out = &outcome.json;
    assert_eq!(out["exit_reason"], "completed", "{out:#}");
    let reports: Vec<&str> = out["subagents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|agent| agent["report"].as_str().unwrap())
        .collect();
    assert_eq!(reports, ["first look", "later looks"]);
}

#[tokio::test]
async fn a_variant_changes_what_the_model_is_sent_and_reasoning_is_passed_through() {
    let daemon = Daemon::start("").await;
    let workspace = tempfile::tempdir().unwrap();
    let files = tempfile::tempdir().unwrap();
    let variant = write(
        &files,
        "terse.toml",
        r#"
        [prompt]
        append = "Answer in one line."
        [tools]
        drop = ["delegate_chunks"]
        rename = { update_plan = "plan" }
        "#,
    );
    let (url, seen) = endpoint(vec![
        tool_call_body(
            "plan",
            &json!({"goal": "g", "steps": [{"title": "s", "status": "pending"}]}),
        ),
        text_body("Done."),
    ])
    .await;
    let outcome = daemon
        .run(&run_args(
            "Plan it.",
            workspace.path(),
            &url,
            &[
                "--variant",
                &variant,
                "--thinking",
                "on",
                "--chat-template-kwargs",
                r#"{"preserve_thinking": true}"#,
                "--flag",
                "max_steps=7",
            ],
        ))
        .await;
    let out = &outcome.json;
    assert_eq!(out["exit_reason"], "completed", "{out:#}");
    // The renamed tool ran as the engine's update_plan: there is a plan.
    assert_eq!(out["plan"]["goal"], "g");
    assert_eq!(out["variant"]["id"], "terse");
    assert_eq!(out["variant"]["flags"]["max_steps"], "7");
    assert_eq!(out["variant"]["warnings"], json!([]));
    assert!(
        out["system_prompt"]
            .as_str()
            .unwrap()
            .ends_with("Answer in one line.")
    );
    let requests = seen.lock().unwrap();
    let first = &requests[0];
    let names: Vec<&str> = first["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["function"]["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"plan") && !names.contains(&"update_plan"));
    assert!(!names.contains(&"delegate_chunks"));
    assert_eq!(first["chat_template_kwargs"]["enable_thinking"], true);
    assert_eq!(first["chat_template_kwargs"]["preserve_thinking"], true);
    // The second request carries the renamed call back under its new name.
    let called: Vec<&str> = requests[1]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|message| message["tool_calls"].as_array())
        .flatten()
        .map(|call| call["function"]["name"].as_str().unwrap())
        .collect();
    assert_eq!(called, ["plan"]);
}

#[tokio::test]
async fn the_cost_ceiling_stops_the_run_with_its_exit_reason() {
    let daemon = Daemon::start("").await;
    let workspace = tempfile::tempdir().unwrap();
    // The model never stops asking for the subagent list; each call costs 0.12 dollars.
    let (url, seen) = endpoint(vec![tool_call_body("list_agents", &json!({}))]).await;
    let outcome = daemon
        .run(&run_args(
            "Loop.",
            workspace.path(),
            &url,
            &[
                "--price-in",
                "1000",
                "--price-out",
                "1000",
                "--cost-ceiling",
                "0.1",
            ],
        ))
        .await;
    let out = &outcome.json;
    assert_eq!(outcome.code, 3);
    assert_eq!(out["exit_reason"], "cost_ceiling", "{out:#}");
    assert_eq!(out["ledger"]["root"]["model_calls"], 1);
    assert_eq!(seen.lock().unwrap().len(), 1);
    assert!(out["error"].as_str().unwrap().contains("cost ceiling"));
}

#[tokio::test]
async fn a_task_file_gives_the_prompt_and_may_carry_its_recordings() {
    let daemon = Daemon::start("").await;
    let workspace = tempfile::tempdir().unwrap();
    let files = tempfile::tempdir().unwrap();
    let task = write(
        &files,
        "task.json",
        r#"{
            "id": "heldout-001",
            "prompt": "Investigate the parser.",
            "expected_routing": "delegate",
            "subagent_replay": {"replies": [{"task_id": "t1", "report": "It rejects empty input."}]}
        }"#,
    );
    let (url, _) = endpoint(script()).await;
    let outcome = daemon
        .run(&run_args(&task, workspace.path(), &url, &[]))
        .await;
    let out = &outcome.json;
    assert_eq!(out["exit_reason"], "completed", "{out:#}");
    assert_eq!(out["task"]["id"], "heldout-001");
    assert_eq!(out["task"]["prompt"], "Investigate the parser.");
    assert_eq!(out["task"]["raw"]["expected_routing"], "delegate");
    assert_eq!(out["subagents"][0]["report"], "It rejects empty input.");
}

fn write_script() -> Vec<String> {
    vec![
        tool_call_body("write_file", &json!({"path": "notes.txt", "content": "hi"})),
        text_body("Done."),
    ]
}

#[tokio::test]
async fn asks_are_denied_and_never_wait_for_anyone() {
    let daemon = Daemon::start("").await;
    let workspace = tempfile::tempdir().unwrap();
    let (url, _) = endpoint(write_script()).await;
    let outcome = daemon
        .run(&run_args("Write notes.txt.", workspace.path(), &url, &[]))
        .await;
    let out = &outcome.json;
    assert_eq!(out["exit_reason"], "completed", "{out:#}");
    assert!(!workspace.path().join("notes.txt").exists());
    let requests = out["permission_requests"].as_array().unwrap();
    assert_eq!(requests.len(), 1, "{requests:?}");
    assert_eq!(requests[0]["decision"], "denied");
}

#[tokio::test]
async fn the_users_deny_rules_apply_even_in_full_mode() {
    let daemon =
        Daemon::start("[[permissions.rules]]\ntool = \"write_file\"\naction = \"deny\"\n").await;
    let workspace = tempfile::tempdir().unwrap();
    let (url, _) = endpoint(write_script()).await;
    let outcome = daemon
        .run(&run_args(
            "Write notes.txt.",
            workspace.path(),
            &url,
            &["--permission-mode", "full"],
        ))
        .await;
    let out = &outcome.json;
    assert_eq!(out["exit_reason"], "completed", "{out:#}");
    assert!(!workspace.path().join("notes.txt").exists());
    assert_eq!(out["failed_tool_calls"], 1);
    assert_eq!(out["permission_requests"], json!([]));
}

#[tokio::test]
async fn a_write_into_a_protected_folder_asks_even_in_full_mode_and_is_denied() {
    let daemon = Daemon::start("").await;
    let mods = daemon.config_dir().join("mods");
    std::fs::create_dir_all(&mods).unwrap();
    let (url, _) = endpoint(write_script()).await;
    let outcome = daemon
        .run(&run_args(
            "Write notes.txt.",
            &mods,
            &url,
            &["--permission-mode", "full"],
        ))
        .await;
    let out = &outcome.json;
    assert_eq!(out["exit_reason"], "completed", "{out:#}");
    assert!(!mods.join("notes.txt").exists());
    let requests = out["permission_requests"].as_array().unwrap();
    assert_eq!(requests.len(), 1, "{requests:?}");
    assert_eq!(requests[0]["decision"], "denied");
}

fn eval_params(workspace: &std::path::Path) -> SessionEvalCreateParams {
    SessionEvalCreateParams {
        environment_id: None,
        project_dir: Some(workspace.display().to_string()),
        options: EvalOptions {
            model: EvalModel {
                base_url: "http://127.0.0.1:1/v1".into(),
                model_id: "m".into(),
                api_key: None,
                chat_template_kwargs: None,
                local: false,
                effort: None,
                context_tokens: None,
                price_in: None,
                price_out: None,
            },
            variant: EvalVariant {
                id: "baseline".into(),
                ..EvalVariant::default()
            },
            replay: None,
            agent: None,
            permission_mode: None,
            cost_ceiling_usd: None,
            token_ceiling: None,
            timeout_s: None,
        },
    }
}

#[tokio::test]
async fn the_users_isolation_tier_applies_to_a_headless_session() {
    let workspace = tempfile::tempdir().unwrap();
    let plain = Daemon::start("").await;
    let isolated = Daemon::start("[isolation]\ntier = \"os\"\n").await;
    let mut client = plain.client().await;
    let policy = client
        .call::<SessionEvalCreate>(eval_params(workspace.path()))
        .await
        .unwrap();
    let mut client = isolated.client().await;
    let os = client
        .call::<SessionEvalCreate>(eval_params(workspace.path()))
        .await
        .unwrap();
    assert!(policy.isolation.contains("tier 0"), "{}", policy.isolation);
    assert!(os.isolation.contains("tier 1"), "{}", os.isolation);
}

#[tokio::test]
async fn only_the_imperium_command_may_start_an_evaluation_session() {
    let daemon = Daemon::start("").await;
    let workspace = tempfile::tempdir().unwrap();
    let mut other = daemon.client_named("imperium-desktop").await;
    let error = other
        .call::<SessionEvalCreate>(eval_params(workspace.path()))
        .await
        .unwrap_err();
    match error {
        imperium_cli::client::ClientError::Rpc(error) => assert_eq!(error.code, codes::FORBIDDEN),
        other => panic!("expected an RPC error, got {other:?}"),
    }
    let error = other
        .call::<SessionPromptPreview>(eval_params(workspace.path()))
        .await
        .unwrap_err();
    match error {
        imperium_cli::client::ClientError::Rpc(error) => assert_eq!(error.code, codes::FORBIDDEN),
        other => panic!("expected an RPC error, got {other:?}"),
    }
}

/// A daemon state that never calls `allow_eval_over_tcp`, as the real daemon never does.
fn plain_state(data: &std::path::Path) -> DaemonState {
    let settings = Config::from_layers([Layer::defaults()]).settings().unwrap();
    DaemonState::new(Paths::under(data), settings, None)
}

fn command_peer() -> Peer {
    Peer {
        name: "imperium-cli".into(),
        version: "0".into(),
    }
}

#[tokio::test]
async fn the_command_evaluates_over_the_local_socket_but_not_over_tcp() {
    let data = tempfile::tempdir().unwrap();
    let listener = imperium_ipc::Listener::bind(data.path()).unwrap();
    let token = AccessToken::generate().unwrap();
    let endpoint = DaemonEndpoint {
        url: String::new(),
        token: token.as_str().to_string(),
        pid: std::process::id(),
        socket: Some(listener.address()),
        protocol: PROTOCOL_VERSION,
    };
    tokio::spawn(imperium_core::serve_ipc(
        listener,
        token,
        plain_state(data.path()),
        std::future::pending(),
    ));
    let mut client = Client::connect(&endpoint, command_peer()).await.unwrap();
    let outcome = run(&mut client, args(&strings(&["--print-prompt"])))
        .await
        .unwrap();
    assert!(
        outcome.json["system_prompt"].is_string(),
        "{:#}",
        outcome.json
    );

    // The same kind of state over loopback TCP refuses the command.
    let tcp_data = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let token = AccessToken::generate().unwrap();
    let endpoint = DaemonEndpoint {
        url: format!("ws://{}/rpc", listener.local_addr().unwrap()),
        token: token.as_str().to_string(),
        pid: std::process::id(),
        socket: None,
        protocol: PROTOCOL_VERSION,
    };
    tokio::spawn(imperium_core::serve(
        listener,
        token,
        plain_state(tcp_data.path()),
        std::future::pending(),
    ));
    let mut client = Client::connect(&endpoint, command_peer()).await.unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let error = client
        .call::<SessionEvalCreate>(eval_params(workspace.path()))
        .await
        .unwrap_err();
    match error {
        imperium_cli::client::ClientError::Rpc(error) => {
            assert_eq!(error.code, codes::FORBIDDEN);
            assert!(error.message.contains("daemon.socket"), "{}", error.message);
        }
        other => panic!("expected an RPC error, got {other:?}"),
    }
}

#[tokio::test]
async fn an_agent_file_runs_as_the_root_and_none_listed_means_no_tools() {
    let daemon = Daemon::start("").await;
    let workspace = tempfile::tempdir().unwrap();
    let agents = daemon.config_dir().join("agents");
    std::fs::create_dir_all(&agents).unwrap();
    std::fs::write(
        agents.join("critic.md"),
        "---\nname: critic\ndescription: Reviews one artefact.\ntools: []\n---\nYou are a strict reviewer. List the defects you find.\n",
    )
    .unwrap();
    let (url, seen) = endpoint(vec![text_body("Defect 1: the loop never ends.")]).await;
    let outcome = daemon
        .run(&run_args(
            "Review this code.",
            workspace.path(),
            &url,
            &["--agent", "critic"],
        ))
        .await;
    let out = &outcome.json;
    assert_eq!(out["exit_reason"], "completed", "{out:#}");
    assert_eq!(out["agent"], "critic");
    assert_eq!(out["final_reply"], "Defect 1: the loop never ends.");
    assert!(
        out["system_prompt"]
            .as_str()
            .unwrap()
            .contains("You are a strict reviewer")
    );
    assert_eq!(out["tools"], json!([]));
    let requests = seen.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert!(
        requests[0]
            .get("tools")
            .is_none_or(|tools| tools.as_array().is_some_and(Vec::is_empty)),
        "{}",
        requests[0]
    );
}

#[tokio::test]
async fn delegate_chunks_agents_are_answered_by_the_calls_bare_brief() {
    let daemon = Daemon::start("").await;
    let workspace = tempfile::tempdir().unwrap();
    let files = tempfile::tempdir().unwrap();
    let recordings = write(
        &files,
        "recordings.json",
        r#"{"replies": [
            {"role": "chunk", "chunk": 1, "brief": "list the rows", "report": "first chunk rows"},
            {"role": "chunk", "brief": "list the rows", "report": "chunk rows"},
            {"role": "reduce", "brief": "merge the lists", "report": "merged lists"}
        ]}"#,
    );
    let mut input = String::new();
    for row in 0..200 {
        input.push_str(&format!("NEEDLE row {row:06} lorem ipsum dolor sit\n"));
    }
    let (url, _) = endpoint_with(|index, body| match index {
        0 => {
            let text = body.to_string();
            let id = text
                .split("stored as artefact ")
                .nth(1)
                .and_then(|rest| rest.split(' ').next())
                .unwrap_or("missing")
                .to_string();
            tool_call_body(
                "delegate_chunks",
                &json!({"artefact_id": id, "chunk_tokens": 100,
                    "brief": "list the rows", "reduce": "merge the lists"}),
            )
        }
        1 => tool_call_body("await_tasks", &json!({})),
        _ => text_body("Merged."),
    })
    .await;
    let outcome = daemon
        .run(&run_args(
            &input,
            workspace.path(),
            &url,
            &["--context-tokens", "1000", "--subagent-replay", &recordings],
        ))
        .await;
    let out = &outcome.json;
    assert_eq!(out["exit_reason"], "completed", "{out:#}");
    let agents = out["subagents"].as_array().unwrap();
    let chunks: Vec<&Value> = agents.iter().filter(|a| a["role"] == "chunk").collect();
    assert!(chunks.len() >= 2, "{agents:?}");
    assert!(chunks.iter().all(|chunk| chunk["ok"] == true));
    assert!(
        chunks
            .iter()
            .any(|chunk| chunk["report"] == "first chunk rows")
    );
    assert!(chunks.iter().any(|chunk| chunk["report"] == "chunk rows"));
    let reduce: Vec<&Value> = agents.iter().filter(|a| a["role"] == "reduce").collect();
    assert_eq!(reduce.len(), 1);
    assert_eq!(reduce[0]["report"], "merged lists");
    assert_eq!(out["replay"]["unused"], json!([]));
}

/// The folders of every environment the daemon has.
async fn environment_folders(client: &mut Client) -> Vec<Vec<String>> {
    client
        .call::<EnvironmentList>(EnvironmentListParams::default())
        .await
        .unwrap()
        .environments
        .into_iter()
        .map(|env| env.folders)
        .collect()
}

/// Asks for the record of `session_id` until `ready` likes it, within ten seconds.
async fn record_when(
    client: &mut Client,
    session_id: &str,
    ready: impl Fn(&imperium_protocol::methods::EvalResult) -> bool,
) -> Option<imperium_protocol::methods::EvalResult> {
    for _ in 0..100 {
        let asked = client
            .call::<SessionEvalResult>(SessionEvalResultParams {
                session_id: session_id.to_string(),
            })
            .await;
        match asked {
            Ok(result) if ready(&result) => return Some(result),
            Ok(_) => {}
            Err(_) => return None,
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    None
}

/// Whether the record of `session_id` is gone within ten seconds.
async fn session_gone(client: &mut Client, session_id: &str) -> bool {
    for _ in 0..100 {
        let asked = client
            .call::<SessionEvalResult>(SessionEvalResultParams {
                session_id: session_id.to_string(),
            })
            .await;
        if asked.is_err() {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    false
}

#[tokio::test]
async fn a_run_leaves_no_environment_and_no_listed_session_behind() {
    let daemon = Daemon::start("").await;
    let workspace = tempfile::tempdir().unwrap();
    let mut watcher = daemon.client().await;
    let before = environment_folders(&mut watcher).await;
    let (url, _) = endpoint(vec![text_body("Done.")]).await;

    // While the run lives, the session is not listed to other clients.
    let mut client = daemon.client().await;
    let mut params = eval_params(workspace.path());
    params.options.model.base_url = url.split('#').next().unwrap().to_string();
    params.options.model.model_id = "fake-model".into();
    let created = client.call::<SessionEvalCreate>(params).await.unwrap();
    let listed = watcher
        .call::<SessionList>(SessionListParams::default())
        .await
        .unwrap();
    assert!(
        listed
            .sessions
            .iter()
            .all(|row| row.session_id != created.session_id),
        "an evaluation session is listed"
    );

    // The daemon, not the notifications, says how the turn ended and what the plan was.
    client
        .call::<SessionSend>(SessionSendParams {
            session_id: created.session_id.clone(),
            text: "Say done.".into(),
            mode: None,
            agent_id: None,
            confirm_budget: None,
            attachments: Vec::new(),
        })
        .await
        .unwrap();
    let record = record_when(&mut watcher, &created.session_id, |r| r.ended.is_some())
        .await
        .expect("the daemon records the end of the turn");
    assert_eq!(record.ended.unwrap().reason, "completed");

    // The end of the run takes its environment with it.
    drop(client);
    assert!(session_gone(&mut watcher, &created.session_id).await);
    assert_eq!(environment_folders(&mut watcher).await, before);
}

#[tokio::test]
async fn an_evaluation_session_goes_when_its_connection_drops() {
    let daemon = Daemon::start("").await;
    let workspace = tempfile::tempdir().unwrap();
    let mut client = daemon.client().await;
    let created = client
        .call::<SessionEvalCreate>(eval_params(workspace.path()))
        .await
        .unwrap();
    let mut other = daemon.client().await;
    assert!(
        record_when(&mut other, &created.session_id, |_| true)
            .await
            .is_some()
    );
    drop(client);
    assert!(
        session_gone(&mut other, &created.session_id).await,
        "the session outlived its connection"
    );
}

#[tokio::test]
async fn the_daemon_stops_a_session_at_its_deadline() {
    let daemon = Daemon::start("").await;
    let workspace = tempfile::tempdir().unwrap();
    // A model that accepts the request and never answers.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((stream, _)) = listener.accept().await {
            held.push(stream);
        }
    });
    let mut client = daemon.client().await;
    let mut params = eval_params(workspace.path());
    params.options.model.base_url = url;
    params.options.timeout_s = Some(1);
    let created = client.call::<SessionEvalCreate>(params).await.unwrap();
    client
        .call::<SessionSend>(SessionSendParams {
            session_id: created.session_id.clone(),
            text: "Wait.".into(),
            mode: None,
            agent_id: None,
            confirm_budget: None,
            attachments: Vec::new(),
        })
        .await
        .unwrap();
    let record = record_when(&mut client, &created.session_id, |r| r.ended.is_some())
        .await
        .expect("the daemon ends the run at its deadline");
    assert_eq!(record.ended.unwrap().reason, "timeout");
}
