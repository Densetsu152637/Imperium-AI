//! A model-style `read_file` call goes through the agent the way the app runs it: the argument
//! spellings small models write (`file_path`, a quoted `limit`, a drive path with stray
//! separators) still read the file, and a folder is pointed to `list_dir`.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::routing::post;
use imperium_core::{Connection, DaemonState};
use imperium_protocol::jsonrpc::{Outcome, Response};
use imperium_protocol::session::SessionEvent;
use serde_json::{Value, json};
use tokio::net::TcpListener;

fn sse(chunk: Value) -> String {
    format!("data: {chunk}\n\ndata: [DONE]\n\n")
}

fn read(id: &str, arguments: Value) -> String {
    sse(
        json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":id,
        "function":{"name":"read_file","arguments":arguments.to_string()}}]}}]}),
    )
}

/// Makes the given `read_file` calls one after another, then reports.
async fn fake_model(calls: Vec<Value>) -> String {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |axum::Json(body): axum::Json<Value>| {
            let calls = calls.clone();
            async move {
                let messages = body["messages"].as_array().cloned().unwrap_or_default();
                let done = messages.iter().filter(|m| m["role"] == "tool").count();
                let body = match calls.get(done) {
                    Some(arguments) => read(&format!("c{done}"), arguments.clone()),
                    None => sse(json!({"choices":[{"delta":{"content":"Done."}}]})),
                };
                ([("content-type", "text/event-stream")], body)
            }
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
    let settings = imperium_config::Config::from_layers([
        imperium_config::Layer::defaults(),
        imperium_config::Layer::new(
            imperium_config::Source::User("test".into()),
            config.parse().unwrap(),
        ),
    ])
    .settings()
    .unwrap();
    DaemonState::new(imperium_config::Paths::under(home), settings, None)
}

fn call(connection: &mut Connection, id: i64, method: &str, params: Value) -> Value {
    let frame = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
    let handled = connection.handle_text(&frame.to_string());
    let Some(Response { outcome, .. }) = handled.response else {
        panic!("no immediate reply to {method}");
    };
    match outcome {
        Outcome::Result(value) => value,
        Outcome::Error(error) => panic!("{method}: {error:?}"),
    }
}

/// The file's path as a model that mangles a Windows drive path writes it, and the plain path
/// elsewhere.
fn mangled(path: &std::path::Path) -> String {
    let plain = path.canonicalize().unwrap().display().to_string();
    let plain = plain.strip_prefix(r"\\?\").unwrap_or(&plain).to_string();
    if cfg!(windows) {
        let (drive, rest) = plain.split_at(2);
        format!(r"{drive}\://{}", rest.trim_start_matches('\\'))
    } else {
        plain
    }
}

#[tokio::test]
async fn a_model_style_read_file_call_reads_the_file_through_the_agent() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let lines: String = (1..=15).map(|n| format!("line {n}\n")).collect();
    std::fs::write(project.path().join("notes.txt"), lines).unwrap();
    std::fs::create_dir(project.path().join("src")).unwrap();
    let url = fake_model(vec![
        json!({"file_path": mangled(&project.path().join("notes.txt")), "limit": "10"}),
        json!({"path": "notes.txt", "offset": 14, "limit": 0}),
        json!({"path": "src"}),
    ])
    .await;

    let state = daemon(home.path(), &url);
    let mut connection = Connection::new(Arc::clone(&state));
    call(
        &mut connection,
        1,
        "system.handshake",
        json!({"client": {"name": "test", "version": "0"}, "protocol": {"major": 0, "minor": 2}}),
    );
    let created = call(
        &mut connection,
        2,
        "session.create",
        json!({"project_dir": project.path()}),
    );
    let session = state
        .session(created["session_id"].as_str().unwrap())
        .unwrap();
    let mut events = session.subscribe();
    session.send("read the notes".into()).unwrap();
    let mut results = Vec::new();
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            match events.recv().await.unwrap().event {
                SessionEvent::ToolFinished { ok, output, .. } => results.push((ok, output)),
                SessionEvent::TurnCompleted { .. } => break,
                SessionEvent::TurnFailed { message } => panic!("{message}"),
                _ => {}
            }
        }
    })
    .await
    .expect("the turn ends within 30 s");

    assert_eq!(results.len(), 3, "{results:?}");
    let (ok, output) = &results[0];
    assert!(*ok, "{results:?}");
    assert!(output.contains("     1\tline 1\n"), "{output}");
    assert!(output.contains("    10\tline 10\n"), "{output}");
    assert!(!output.contains("line 11"), "{output}");
    // A limit of 0 means no limit: the rest of the file from line 14.
    assert_eq!(
        results[1],
        (true, "    14\tline 14\n    15\tline 15\n".to_string())
    );
    assert!(
        !results[2].0 && results[2].1.contains("is a directory; use list_dir"),
        "{results:?}"
    );
}
