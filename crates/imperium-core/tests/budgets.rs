//! Budgets over the protocol: a model call counts against the session's environment, a used-up
//! budget that asks refuses `session.send` until it is confirmed, one that blocks refuses it
//! outright, an alert goes to clients once, and usage survives a restart.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::routing::post;
use imperium_core::{Connection, DaemonState};
use imperium_protocol::jsonrpc::{ErrorObject, Outcome, Response, codes};
use imperium_protocol::session::SessionEvent;
use serde_json::{Value, json};
use tokio::net::TcpListener;

/// Answers every call with "Done." and 600 prompt plus 400 completion tokens.
async fn fake_model() -> String {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(|| async {
            let text = json!({"choices":[{"delta":{"content":"Done."}}]});
            let usage = json!({"choices":[],"usage":{"prompt_tokens":600,"completion_tokens":400}});
            (
                [("content-type", "text/event-stream")],
                format!("data: {text}\n\ndata: {usage}\n\ndata: [DONE]\n\n"),
            )
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    url
}

fn daemon(home: &std::path::Path, base_url: &str) -> Arc<DaemonState> {
    let config = format!(
        "[models]\ndefault = \"fake:m\"\n[endpoints.fake]\nbase_url = \"{base_url}\"\n\
         [workflow]\nenabled = false\n[memory]\nenabled = false"
    );
    let paths = imperium_config::Paths::under(home);
    if !paths.user_config_file().exists() {
        std::fs::create_dir_all(paths.config_dir()).unwrap();
        std::fs::write(paths.user_config_file(), config).unwrap();
    }
    let settings = imperium_config::load(&paths, None, Vec::new())
        .unwrap()
        .settings()
        .unwrap();
    DaemonState::new(paths, settings, None)
}

fn request(connection: &mut Connection, id: i64, method: &str, params: Value) -> Outcome {
    let frame = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
    let handled = connection.handle_text(&frame.to_string());
    let Some(Response { outcome, .. }) = handled.response else {
        panic!("no immediate reply to {method}");
    };
    outcome
}

fn call(connection: &mut Connection, id: i64, method: &str, params: Value) -> Value {
    match request(connection, id, method, params) {
        Outcome::Result(value) => value,
        Outcome::Error(error) => panic!("{method}: {error:?}"),
    }
}

fn refused(connection: &mut Connection, id: i64, method: &str, params: Value) -> ErrorObject {
    match request(connection, id, method, params) {
        Outcome::Result(value) => panic!("{method} succeeded: {value}"),
        Outcome::Error(error) => error,
    }
}

fn handshake(connection: &mut Connection) {
    call(
        connection,
        1,
        "system.handshake",
        json!({"client": {"name": "test", "version": "0"}, "protocol": {"major": 0, "minor": 6}}),
    );
}

async fn finish_turn(
    state: &DaemonState,
    session_id: &str,
    text: &str,
    connection: &mut Connection,
    id: i64,
    confirm: bool,
) {
    let session = state.session(session_id).unwrap();
    let mut events = session.subscribe();
    let mut params = json!({"session_id": session_id, "text": text});
    if confirm {
        params["confirm_budget"] = json!(true);
    }
    call(connection, id, "session.send", params);
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            match events.recv().await.unwrap().event {
                SessionEvent::TurnCompleted { .. } => break,
                SessionEvent::TurnFailed { message } => panic!("{message}"),
                _ => {}
            }
        }
    })
    .await
    .expect("the turn ends within 30 s");
}

#[tokio::test]
async fn used_up_budgets_ask_then_block_new_turns_and_usage_survives_a_restart() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let url = fake_model().await;
    let state = daemon(home.path(), &url);
    let mut connection = Connection::new(Arc::clone(&state));
    handshake(&mut connection);
    let mut alerts = state.announcements();
    let env = call(
        &mut connection,
        2,
        "environments.create",
        json!({"name": "Work", "folders": [project.path()]}),
    );
    let env_id = env["id"].as_str().unwrap().to_string();
    let saved = call(
        &mut connection,
        3,
        "budgets.set",
        json!({
            "week_start": "monday",
            "month_start_day": 1,
            "thresholds": [50, 80, 100],
            "limits": [{
                "id": "", "name": "", "scope": "environment", "target": env_id,
                "window": "weekly", "tokens": 1500, "cost_usd": null,
                "thresholds": null, "action": "ask"
            }]
        }),
    );
    let budget_id = saved["settings"]["limits"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(!budget_id.is_empty());
    assert_eq!(saved["status"][0]["label"], "Work, weekly");

    let created = call(
        &mut connection,
        4,
        "session.create",
        json!({"environment_id": env_id}),
    );
    let session_id = created["session_id"].as_str().unwrap().to_string();

    // 1000 of 1500 tokens: the 50% alert.
    finish_turn(&state, &session_id, "one", &mut connection, 5, false).await;
    let alert = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let notification = alerts.recv().await.unwrap();
            if notification.method == "budgets.alert" {
                break notification.params;
            }
        }
    })
    .await
    .expect("an alert within 10 s");
    assert_eq!(alert["threshold"], 50);
    assert_eq!(alert["budget_id"], budget_id.as_str());
    assert_eq!(alert["session_id"], session_id.as_str());

    // 2000 tokens: used up. The turn that crossed it finished.
    finish_turn(&state, &session_id, "two", &mut connection, 6, false).await;
    let used = loop {
        let status = call(&mut connection, 7, "budgets.get", json!({}));
        if status["status"][0]["tokens_used"] == 2000 {
            break status;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(used["status"][0]["exceeded"], true);
    assert_eq!(used["status"][0]["alerted"], json!([50, 80, 100]));

    // Asking: refused until confirmed.
    let error = refused(
        &mut connection,
        8,
        "session.send",
        json!({"session_id": session_id, "text": "three"}),
    );
    assert_eq!(error.code, codes::BUDGET_EXCEEDED);
    let data = error.data.unwrap();
    assert_eq!(data["action"], "ask");
    assert_eq!(data["budget_id"], budget_id.as_str());
    finish_turn(&state, &session_id, "three", &mut connection, 9, true).await;

    // Blocking: refused even when confirmed.
    let mut settings = used["settings"].clone();
    settings["limits"][0]["action"] = json!("block");
    call(&mut connection, 10, "budgets.set", settings);
    let error = refused(
        &mut connection,
        11,
        "session.send",
        json!({"session_id": session_id, "text": "four", "confirm_budget": true}),
    );
    assert_eq!(error.code, codes::BUDGET_EXCEEDED);
    assert_eq!(error.data.unwrap()["action"], "block");

    // The dashboard sees the environment and the endpoint.
    let usage = call(&mut connection, 12, "budgets.usage", json!({"days": 7}));
    assert_eq!(usage["days"].as_array().unwrap().len(), 7);
    assert_eq!(usage["environments"][0]["label"], "Work");
    assert_eq!(usage["endpoints"][0]["key"], "fake");
    assert_eq!(usage["models"][0]["calls"], 3);

    // After a restart the window's usage and alerts are still there.
    drop(connection);
    drop(state);
    let state = daemon(home.path(), &url);
    let mut connection = Connection::new(Arc::clone(&state));
    handshake(&mut connection);
    let status = call(&mut connection, 2, "budgets.get", json!({}));
    assert_eq!(status["status"][0]["tokens_used"], 3000);
    assert_eq!(status["status"][0]["budget"]["action"], "block");
    assert_eq!(status["status"][0]["alerted"], json!([50, 80, 100]));
}
