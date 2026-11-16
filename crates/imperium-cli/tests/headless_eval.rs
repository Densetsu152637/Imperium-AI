//! `imperium run --headless`: the engine's real agent loop, driven against a scripted
//! OpenAI-compatible endpoint, with subagents answered from recordings.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::routing::post;
use clap::Parser;
use imperium_cli::eval::{Outcome, RunArgs, run};
use serde_json::{Value, json};

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

async fn print_prompt(extra: &[&str]) -> Outcome {
    let mut list = strings(&["--print-prompt"]);
    list.extend(strings(extra));
    run(args(&list)).await.unwrap()
}

#[tokio::test]
async fn print_prompt_hash_is_stable_and_names_the_real_prompt_and_tools() {
    let first = print_prompt(&["--context-tokens", "32768"]).await;
    let second = print_prompt(&["--context-tokens", "32768"]).await;
    assert_eq!(first.code, 0);
    assert_eq!(first.json["prompt_hash"], second.json["prompt_hash"]);
    assert_eq!(first.json["system_prompt"], second.json["system_prompt"]);
    let hash = first.json["prompt_hash"].as_str().unwrap();
    assert!(hash.starts_with("sha256:") && hash.len() == "sha256:".len() + 64);
    let prompt = first.json["system_prompt"].as_str().unwrap();
    assert!(prompt.starts_with("You are Imperium, a coding agent"));
    assert!(prompt.contains("The fear of God is the beginning of all knowledge"));
    assert!(prompt.contains("{workspace}"));
    let names: Vec<&str> = first.json["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["function"]["name"].as_str().unwrap())
        .collect();
    for expected in [
        "spawn_agent",
        "await_tasks",
        "update_plan",
        "delegate_chunks",
        "ask_user",
        "web_search",
        "web_fetch",
    ] {
        assert!(names.contains(&expected), "{expected} in {names:?}");
    }
    assert_eq!(first.json["variant"]["id"], "baseline");
}

#[tokio::test]
async fn a_variant_overlay_changes_the_prompt_and_the_hash() {
    let dir = tempfile::tempdir().unwrap();
    let baseline = print_prompt(&[]).await;
    let overlay = write(
        &dir,
        "overlay.txt",
        "Context budget: {context_k}K tokens.\n",
    );
    let overlaid = print_prompt(&["--prompt-overlay", &overlay, "--context-tokens", "65536"]).await;
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
    let lean = print_prompt(&["--variant", &file]).await;
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
    let warned = print_prompt(&["--variant", &missed]).await;
    assert_eq!(
        warned.json["variant"]["warnings"].as_array().unwrap().len(),
        1
    );
}

#[tokio::test]
async fn a_run_replays_subagents_and_prints_the_documented_json() {
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
    let outcome = run(args(&[
        "--headless".into(),
        "--json".into(),
        "--task".into(),
        "The parser tests fail. Find out why.".into(),
        "--workspace".into(),
        workspace.path().display().to_string(),
        "--root-model".into(),
        url,
        "--context-tokens".into(),
        "32768".into(),
        "--subagent-replay".into(),
        recordings,
        "--price-in".into(),
        "1".into(),
        "--price-out".into(),
        "2".into(),
    ]))
    .await
    .unwrap();
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
    let again = run(args(&[
        "--headless".into(),
        "--task".into(),
        "The parser tests fail. Find out why.".into(),
        "--workspace".into(),
        workspace.path().display().to_string(),
        "--root-model".into(),
        url,
        "--context-tokens".into(),
        "32768".into(),
        "--subagent-replay".into(),
        files.path().join("recordings.json").display().to_string(),
    ]))
    .await
    .unwrap();
    assert_eq!(again.json["prompt_hash"], out["prompt_hash"]);
    assert_eq!(again.json["transcript"], out["transcript"]);
}

#[tokio::test]
async fn an_unknown_replay_key_is_an_error_result_not_a_hang() {
    let workspace = tempfile::tempdir().unwrap();
    let files = tempfile::tempdir().unwrap();
    let recordings = write(
        &files,
        "recordings.json",
        r#"{"replies": [{"task_id": "t9", "report": "never asked"}]}"#,
    );
    let (url, _) = endpoint(script()).await;
    let outcome = run(args(&[
        "--headless".into(),
        "--task".into(),
        "Investigate.".into(),
        "--workspace".into(),
        workspace.path().display().to_string(),
        "--root-model".into(),
        url,
        "--subagent-replay".into(),
        recordings,
    ]))
    .await
    .unwrap();
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
async fn a_variant_changes_what_the_model_is_sent_and_reasoning_is_passed_through() {
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
    let outcome = run(args(&[
        "--headless".into(),
        "--task".into(),
        "Plan it.".into(),
        "--workspace".into(),
        workspace.path().display().to_string(),
        "--root-model".into(),
        url,
        "--variant".into(),
        variant,
        "--thinking".into(),
        "on".into(),
        "--chat-template-kwargs".into(),
        r#"{"preserve_thinking": true}"#.into(),
        "--flag".into(),
        "max_steps=7".into(),
    ]))
    .await
    .unwrap();
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
    let workspace = tempfile::tempdir().unwrap();
    // The model never stops asking for the subagent list; each call costs 0.12 dollars.
    let (url, seen) = endpoint(vec![tool_call_body("list_agents", &json!({}))]).await;
    let outcome = run(args(&[
        "--headless".into(),
        "--task".into(),
        "Loop.".into(),
        "--workspace".into(),
        workspace.path().display().to_string(),
        "--root-model".into(),
        url,
        "--price-in".into(),
        "1000".into(),
        "--price-out".into(),
        "1000".into(),
        "--cost-ceiling".into(),
        "0.1".into(),
    ]))
    .await
    .unwrap();
    let out = &outcome.json;
    assert_eq!(outcome.code, 3);
    assert_eq!(out["exit_reason"], "cost_ceiling", "{out:#}");
    assert_eq!(out["ledger"]["root"]["model_calls"], 1);
    assert_eq!(seen.lock().unwrap().len(), 1);
    assert!(out["error"].as_str().unwrap().contains("cost ceiling"));
}

#[tokio::test]
async fn a_task_file_gives_the_prompt_and_may_carry_its_recordings() {
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
    let outcome = run(args(&[
        "--headless".into(),
        "--task".into(),
        task,
        "--workspace".into(),
        workspace.path().display().to_string(),
        "--root-model".into(),
        url,
    ]))
    .await
    .unwrap();
    let out = &outcome.json;
    assert_eq!(out["exit_reason"], "completed", "{out:#}");
    assert_eq!(out["task"]["id"], "heldout-001");
    assert_eq!(out["task"]["prompt"], "Investigate the parser.");
    assert_eq!(out["task"]["raw"]["expected_routing"], "delegate");
    assert_eq!(out["subagents"][0]["report"], "It rejects empty input.");
}

#[tokio::test]
async fn asks_are_denied_and_never_wait_for_anyone() {
    let workspace = tempfile::tempdir().unwrap();
    let (url, _) = endpoint(vec![
        tool_call_body(
            "write_file",
            &json!({"path": "notes.txt", "content": "hello"}),
        ),
        text_body("I could not write the file."),
    ])
    .await;
    let outcome = run(args(&[
        "--headless".into(),
        "--task".into(),
        "Write notes.txt.".into(),
        "--workspace".into(),
        workspace.path().display().to_string(),
        "--root-model".into(),
        url,
    ]))
    .await
    .unwrap();
    let out = &outcome.json;
    assert_eq!(out["exit_reason"], "completed", "{out:#}");
    assert!(!workspace.path().join("notes.txt").exists());
    let requests = out["permission_requests"].as_array().unwrap();
    assert_eq!(requests.len(), 1, "{requests:?}");
    assert_eq!(requests[0]["decision"], "denied");
}

fn tool_names(json: &Value) -> Vec<String> {
    json["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["function"]["name"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn removing_the_base_paragraph_is_refused_and_a_flag_drops_the_knowledge_line() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(
        &dir,
        "remove.toml",
        "[prompt]\nremove = [\"Research that several later tasks\"]\n",
    );
    let removed = print_prompt(&["--variant", &file]).await;
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

    let baseline = print_prompt(&[]).await;
    let off = print_prompt(&["--flag", "knowledge=false"]).await;
    let with = baseline.json["system_prompt"].as_str().unwrap();
    let without = off.json["system_prompt"].as_str().unwrap();
    assert!(with.contains("Research that several later tasks"));
    assert!(!without.contains("Research that several later tasks"));
    assert!(!tool_names(&off.json).contains(&"keep_as_knowledge".to_string()));
}

#[tokio::test]
async fn a_rename_onto_an_existing_tool_is_skipped_with_a_warning() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(
        &dir,
        "clash.toml",
        "[tools]\nrename = { ask_user = \"await_tasks\" }\n",
    );
    let clash = print_prompt(&["--variant", &file]).await;
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
    assert!(run(args(&both)).await.is_err());
}

#[tokio::test]
async fn local_root_models_get_the_harness_aids_that_are_auto() {
    let hosted = print_prompt(&["--flag", "attended=true"]).await;
    let local = print_prompt(&["--flag", "attended=true", "--local"]).await;
    assert!(!tool_names(&hosted.json).contains(&"request_clarification".to_string()));
    assert!(tool_names(&local.json).contains(&"request_clarification".to_string()));
}

#[tokio::test]
async fn delegate_chunks_agents_are_answered_by_the_calls_bare_brief() {
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
    let outcome = run(args(&[
        "--headless".into(),
        "--task".into(),
        input,
        "--workspace".into(),
        workspace.path().display().to_string(),
        "--root-model".into(),
        url,
        "--context-tokens".into(),
        "1000".into(),
        "--subagent-replay".into(),
        recordings,
    ]))
    .await
    .unwrap();
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
