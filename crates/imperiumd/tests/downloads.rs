//! `models.hf_files` and `models.download` against a fake Hugging Face Hub: the downloaded model
//! shows up in `models.local`.

use std::sync::Arc;

use axum::Router;
use axum::extract::Path as UrlPath;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::get;
use futures_util::{SinkExt, StreamExt};
use imperium_protocol::PROTOCOL_VERSION;
use imperiumd::AccessToken;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// The smallest GGUF the scanner accepts: a version 3 header with no tensors or keys.
fn gguf() -> Vec<u8> {
    let mut bytes = b"GGUF".to_vec();
    bytes.extend(3u32.to_le_bytes());
    bytes.extend(0u64.to_le_bytes());
    bytes.extend(0u64.to_le_bytes());
    bytes
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// A Hub with `acme/tiny-GGUF` (two quantisations) and `acme/gated` (needs a token).
async fn fake_hub() -> String {
    let files: Arc<Vec<(String, Vec<u8>)>> = Arc::new(vec![
        ("tiny-Q4_K_M.gguf".into(), gguf()),
        ("tiny-Q8_0.gguf".into(), [gguf(), vec![0; 64]].concat()),
    ]);
    let listed = Arc::clone(&files);
    let tree = move |UrlPath((_, name, _)): UrlPath<(String, String, String)>,
                     headers: HeaderMap| {
        let files = Arc::clone(&listed);
        async move {
            if name == "gated" && !headers.contains_key(header::AUTHORIZATION) {
                return StatusCode::UNAUTHORIZED.into_response();
            }
            let entries: Vec<Value> = files
                .iter()
                .map(|(path, body)| {
                    json!({"type": "file", "path": path, "size": body.len(),
                           "lfs": {"oid": hex(&Sha256::digest(body))}})
                })
                .collect();
            axum::Json(entries).into_response()
        }
    };
    let resolve = move |UrlPath((_, _, _, path)): UrlPath<(String, String, String, String)>| async move {
        match files.iter().find(|(name, _)| *name == path) {
            Some((_, body)) => body.clone().into_response(),
            None => StatusCode::NOT_FOUND.into_response(),
        }
    };
    let app: Router = Router::new()
        .route("/api/models/{owner}/{name}/tree/{revision}", get(tree))
        .route("/{owner}/{name}/resolve/{revision}/{*path}", get(resolve));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    url
}

async fn start_daemon(home: &std::path::Path, hub: String) -> Socket {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/rpc", listener.local_addr().unwrap());
    let settings = imperium_config::Config::from_layers([imperium_config::Layer::defaults()])
        .settings()
        .unwrap();
    let daemon = imperiumd::DaemonState::new(imperium_config::Paths::under(home), settings, None);
    daemon.use_hub(imperium_local::hf::Hub::new(hub, None));
    tokio::spawn(imperiumd::serve(
        listener,
        AccessToken::from("t".to_string()),
        daemon,
        std::future::pending(),
    ));
    let mut request = url.into_client_request().unwrap();
    request
        .headers_mut()
        .insert("Authorization", "Bearer t".parse().unwrap());
    let (mut socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    let handshake =
        json!({"client": {"name": "test", "version": "0"}, "protocol": PROTOCOL_VERSION});
    call(
        &mut socket,
        1,
        "system.handshake",
        handshake,
        &mut Vec::new(),
    )
    .await
    .unwrap();
    socket
}

/// Calls a method, keeping notifications that arrive before the reply in `notifications`.
async fn call(
    socket: &mut Socket,
    id: i64,
    method: &str,
    params: Value,
    notifications: &mut Vec<Value>,
) -> Result<Value, Value> {
    let request = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
    socket
        .send(Message::Text(request.to_string().into()))
        .await
        .unwrap();
    loop {
        let message: Value = next(socket).await;
        if message["id"] == id {
            return match message.get("result") {
                Some(result) => Ok(result.clone()),
                None => Err(message["error"].clone()),
            };
        }
        notifications.push(message);
    }
}

async fn next(socket: &mut Socket) -> Value {
    let message = tokio::time::timeout(std::time::Duration::from_secs(20), socket.next())
        .await
        .expect("the daemon answers")
        .unwrap()
        .unwrap();
    serde_json::from_str(message.to_text().unwrap()).unwrap()
}

#[tokio::test]
async fn a_downloaded_model_appears_in_the_local_listing() {
    let home = tempfile::tempdir().unwrap();
    let mut socket = start_daemon(home.path(), fake_hub().await).await;
    let mut seen = Vec::new();

    let listing = call(
        &mut socket,
        2,
        "models.hf_files",
        json!({"repo": "acme/tiny-GGUF"}),
        &mut seen,
    )
    .await
    .unwrap();
    assert_eq!(listing["default"], "Q4_K_M");
    assert_eq!(listing["variants"][1]["name"], "Q8_0");
    assert_eq!(listing["variants"][1]["model_id"], "tiny-Q8_0");
    assert_eq!(listing["variants"][1]["size_bytes"], 88);
    assert!(
        ["gpu", "partial", "cpu", "none"]
            .contains(&listing["variants"][0]["fit"].as_str().unwrap())
    );

    let local = call(&mut socket, 3, "models.local", json!({}), &mut seen)
        .await
        .unwrap();
    assert_eq!(
        local["enabled"], false,
        "nothing to scan before the download"
    );

    let job = call(
        &mut socket,
        4,
        "models.download",
        json!({"repo": "acme/tiny-GGUF", "quant": "q8_0"}),
        &mut seen,
    )
    .await
    .unwrap();
    assert_eq!(job["state"], "running");
    assert_eq!(job["model"], "llamacpp:tiny-Q8_0");
    let job_id = job["job_id"].clone();

    // Progress notifications end with `done`.
    let mut done = None;
    for notification in seen.drain(..) {
        if notification["params"]["state"] == "done" {
            done = Some(notification);
        }
    }
    while done.is_none() {
        let message = next(&mut socket).await;
        assert_eq!(message["method"], "models.download_progress", "{message}");
        assert_eq!(message["params"]["job_id"], job_id);
        assert_ne!(message["params"]["state"], "failed", "{message}");
        if message["params"]["state"] == "done" {
            done = Some(message);
        }
    }
    assert_eq!(done.unwrap()["params"]["done_bytes"], 88);
    let file = home
        .path()
        .join("data/models/acme/tiny-GGUF/tiny-Q8_0.gguf");
    assert!(file.is_file());

    let local = call(&mut socket, 5, "models.local", json!({}), &mut seen)
        .await
        .unwrap();
    assert_eq!(local["enabled"], true);
    assert_eq!(local["models"][0]["id"], "tiny-Q8_0", "{local}");
    assert_eq!(local["models"][0]["runtime"], "llamacpp");

    let jobs = call(&mut socket, 6, "models.downloads", json!({}), &mut seen)
        .await
        .unwrap();
    assert_eq!(jobs["jobs"][0]["state"], "done");
    let cancel = call(
        &mut socket,
        7,
        "models.download_cancel",
        json!({"job_id": job_id}),
        &mut seen,
    )
    .await
    .unwrap();
    assert_eq!(cancel["found"], false, "a finished job cannot be cancelled");
}

#[tokio::test]
async fn gated_repositories_say_a_token_is_needed() {
    let home = tempfile::tempdir().unwrap();
    let mut socket = start_daemon(home.path(), fake_hub().await).await;
    let error = call(
        &mut socket,
        2,
        "models.download",
        json!({"repo": "acme/gated"}),
        &mut Vec::new(),
    )
    .await
    .unwrap_err();
    let message = error["message"].as_str().unwrap();
    assert!(message.contains("needs a Hugging Face token"), "{message}");
    assert!(message.contains("HF_TOKEN"), "{message}");

    let error = call(
        &mut socket,
        3,
        "models.download",
        json!({"repo": "acme/tiny-GGUF", "quant": "Q2_K"}),
        &mut Vec::new(),
    )
    .await
    .unwrap_err();
    assert!(
        error["message"].as_str().unwrap().contains("Q4_K_M, Q8_0"),
        "{error}"
    );
}
