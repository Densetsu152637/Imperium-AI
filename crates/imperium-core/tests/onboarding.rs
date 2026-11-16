//! Setting up models from the app: `models.add_folder` / `models.remove_folder` (saved to
//! config.toml and scanned at once), `models.use`, `models.test`, and `runtime.status` /
//! `runtime.set_server`.

use std::path::Path;
use std::sync::Arc;

use axum::Json;
use axum::routing::post;
use futures_util::{SinkExt, StreamExt};
use imperium_config::Paths;
use imperium_core::AccessToken;
use imperium_protocol::PROTOCOL_VERSION;
use serde_json::{Value, json};
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

/// A daemon started with `config` as its user file and no model folders.
async fn start_daemon(home: &Path, config: &str) -> (Socket, Arc<imperium_core::DaemonState>) {
    let paths = Paths::under(home);
    std::fs::create_dir_all(paths.config_dir()).unwrap();
    // A llama-server installed on the machine running the tests is not looked for, and none is
    // downloaded.
    let config =
        format!("{config}\n[runtime.llamacpp]\nfind_existing = false\nauto_install = false\n");
    std::fs::write(paths.user_config_file(), config).unwrap();
    let loaded = imperium_config::load(&paths, None, []).unwrap();
    // Secrets stay in memory, away from the machine's keychain.
    let daemon = imperium_core::DaemonState::with_secrets(
        paths,
        loaded.settings().unwrap(),
        None,
        Arc::new(imperium_config::MemorySecrets::new()),
    );
    daemon.remember_config(&loaded);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/rpc", listener.local_addr().unwrap());
    tokio::spawn(imperium_core::serve(
        listener,
        AccessToken::from("t".to_string()),
        Arc::clone(&daemon),
        std::future::pending(),
    ));
    let mut request = url.into_client_request().unwrap();
    request
        .headers_mut()
        .insert("Authorization", "Bearer t".parse().unwrap());
    let (mut socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    let handshake =
        json!({"client": {"name": "test", "version": "0"}, "protocol": PROTOCOL_VERSION});
    call(&mut socket, "system.handshake", handshake)
        .await
        .unwrap();
    (socket, daemon)
}

static NEXT_ID: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(1);

async fn call(socket: &mut Socket, method: &str, params: Value) -> Result<Value, Value> {
    let id = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let request = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
    socket
        .send(Message::Text(request.to_string().into()))
        .await
        .unwrap();
    loop {
        let message = tokio::time::timeout(std::time::Duration::from_secs(60), socket.next())
            .await
            .expect("the daemon answers")
            .unwrap()
            .unwrap();
        let message: Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
        if message["id"] == id {
            return match message.get("result") {
                Some(result) => Ok(result.clone()),
                None => Err(message["error"].clone()),
            };
        }
    }
}

fn user_file(home: &Path) -> toml::Table {
    std::fs::read_to_string(Paths::under(home).user_config_file())
        .unwrap()
        .parse()
        .unwrap()
}

fn ids(listing: &Value) -> Vec<String> {
    listing["models"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["id"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn folders_are_added_scanned_and_removed_without_a_restart() {
    let home = tempfile::tempdir().unwrap();
    let models = tempfile::tempdir().unwrap();
    let repo = models.path().join("unsloth").join("Tiny-GGUF");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(repo.join("tiny-Q4_K_M.gguf"), gguf()).unwrap();
    let (mut socket, daemon) = start_daemon(home.path(), "").await;

    let listing = call(&mut socket, "models.local", json!({})).await.unwrap();
    assert_eq!(listing["enabled"], false);

    let folder = models.path().display().to_string();
    let listing = call(&mut socket, "models.add_folder", json!({"path": folder}))
        .await
        .unwrap();
    assert_eq!(listing["enabled"], true);
    assert_eq!(ids(&listing), ["tiny-Q4_K_M"]);
    assert_eq!(listing["models"][0]["repo"], "unsloth/Tiny-GGUF");
    assert_eq!(listing["configured_folders"], json!([folder]));
    assert_eq!(
        user_file(home.path())["models"]["folders"],
        toml::Value::Array(vec![folder.clone().into()])
    );
    assert_eq!(
        daemon.settings().models.folders,
        [models.path().to_path_buf()]
    );

    // Adding it again changes nothing; a new file shows up on the next add or rescan.
    std::fs::write(models.path().join("second.gguf"), gguf()).unwrap();
    let listing = call(
        &mut socket,
        "models.add_folder",
        json!({"path": format!("{folder}/")}),
    )
    .await
    .unwrap();
    assert_eq!(listing["configured_folders"], json!([folder]));
    assert_eq!(ids(&listing), ["second", "tiny-Q4_K_M"]);

    for bad in ["relative/models", &format!("{folder}/missing")] {
        let error = call(&mut socket, "models.add_folder", json!({"path": bad}))
            .await
            .unwrap_err();
        assert_eq!(error["code"], -32602, "{error}");
    }

    let listing = call(&mut socket, "models.remove_folder", json!({"path": folder}))
        .await
        .unwrap();
    assert_eq!(listing["enabled"], false);
    assert!(ids(&listing).is_empty());
    assert_eq!(
        user_file(home.path())["models"]["folders"],
        toml::Value::Array(Vec::new())
    );
    // The files are untouched.
    assert!(repo.join("tiny-Q4_K_M.gguf").is_file());
    let error = call(&mut socket, "models.remove_folder", json!({"path": folder}))
        .await
        .unwrap_err();
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .contains("not one of your model folders")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn use_sets_the_default_and_a_later_model_save_keeps_it() {
    let home = tempfile::tempdir().unwrap();
    let models = tempfile::tempdir().unwrap();
    std::fs::write(models.path().join("tiny.gguf"), gguf()).unwrap();
    let config = r#"
[[models.catalog]]
id = "coder"
spec = "cloud:coder-1"
location = "cloud"
tier = "basic"
[endpoints.cloud]
base_url = "https://example.invalid/v1"
"#;
    let (mut socket, daemon) = start_daemon(home.path(), config).await;
    call(
        &mut socket,
        "models.add_folder",
        json!({"path": models.path().display().to_string()}),
    )
    .await
    .unwrap();

    let used = call(&mut socket, "models.use", json!({"model": "tiny"}))
        .await
        .unwrap();
    assert_eq!(used["default"], "llamacpp:tiny");
    assert_eq!(
        user_file(home.path())["models"]["default"].as_str(),
        Some("llamacpp:tiny")
    );
    assert_eq!(
        daemon.settings().models.default.as_deref(),
        Some("llamacpp:tiny")
    );
    let listing = call(&mut socket, "models.local", json!({})).await.unwrap();
    assert_eq!(listing["default_model"], "llamacpp:tiny");

    // Saving model settings rebuilds the settings from the layers: the new default and
    // folders survive it.
    call(
        &mut socket,
        "models.set_config",
        json!({"local": [{"id": "tiny", "settings": {"slots": 2}}]}),
    )
    .await
    .unwrap();
    let settings = daemon.settings();
    assert_eq!(settings.models.default.as_deref(), Some("llamacpp:tiny"));
    assert_eq!(settings.models.folders, [models.path().to_path_buf()]);

    // A table row is used by its spec.
    let used = call(&mut socket, "models.use", json!({"model": "coder"}))
        .await
        .unwrap();
    assert_eq!(used["default"], "cloud:coder-1");
    for unknown in ["nope", "llamacpp:nope", "elsewhere:model"] {
        let error = call(&mut socket, "models.use", json!({"model": unknown}))
            .await
            .unwrap_err();
        assert_eq!(error["code"], -32602, "{unknown}: {error}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn test_reports_the_reply_and_speed_or_why_it_failed() {
    let app = axum::Router::new().route(
        "/v1/chat/completions",
        post(
            |headers: axum::http::HeaderMap, Json(body): Json<Value>| async move {
                assert_eq!(body["model"], "echo-1");
                assert_eq!(headers["authorization"], "Bearer sk-test");
                Json(json!({
                    "choices": [{"message": {"role": "assistant", "content": "Hello!"}}],
                    "usage": {"prompt_tokens": 9, "completion_tokens": 3}
                }))
            },
        ),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    let home = tempfile::tempdir().unwrap();
    let models = tempfile::tempdir().unwrap();
    std::fs::write(models.path().join("tiny.gguf"), gguf()).unwrap();
    let (mut socket, daemon) = start_daemon(home.path(), "").await;

    // Connecting the endpoint keeps its key in the keychain, not the file.
    let added = call(
        &mut socket,
        "models.add_endpoint",
        json!({"id": "fake", "base_url": base, "api_key": "sk-test", "model": "echo-1",
               "make_default": true}),
    )
    .await
    .unwrap();
    assert_eq!(added["default"], "fake:echo-1");
    let file = std::fs::read_to_string(Paths::under(home.path()).user_config_file()).unwrap();
    assert!(!file.contains("sk-test"), "{file}");
    assert!(file.contains("fake-api-key"), "{file}");
    assert_eq!(
        daemon.settings().models.default.as_deref(),
        Some("fake:echo-1")
    );

    let tested = call(&mut socket, "models.test", json!({"model": "fake:echo-1"}))
        .await
        .unwrap();
    assert_eq!(tested["ok"], true, "{tested}");
    assert_eq!(tested["reply"], "Hello!");
    assert_eq!(tested["completion_tokens"], 3);
    assert!(tested["tokens_per_s"].as_f64().unwrap() > 0.0);

    // A local model with no llama-server says so instead of failing the call.
    call(
        &mut socket,
        "models.add_folder",
        json!({"path": models.path().display().to_string()}),
    )
    .await
    .unwrap();
    let tested = call(&mut socket, "models.test", json!({"model": "tiny"}))
        .await
        .unwrap();
    assert_eq!(tested["ok"], false);
    let message = tested["message"].as_str().unwrap();
    assert!(message.contains("No llama.cpp runtime yet"), "{message}");
    assert!(message.contains("Local models → Runtime"), "{message}");
    assert!(message.contains("imperium runtime install"), "{message}");
}

#[tokio::test(flavor = "multi_thread")]
async fn runtime_status_explains_a_missing_server_and_set_server_saves_the_path() {
    let home = tempfile::tempdir().unwrap();
    let (mut socket, daemon) = start_daemon(home.path(), "").await;

    let status = call(&mut socket, "runtime.status", json!({}))
        .await
        .unwrap();
    assert_eq!(status["source"], "pack");
    assert_eq!(status["installed"], false);
    assert_eq!(status["runs"], false);
    assert!(
        status["problem"]
            .as_str()
            .unwrap()
            .contains("No llama.cpp runtime yet")
    );

    let error = call(
        &mut socket,
        "runtime.set_server",
        json!({"path": home.path().join("missing.exe").display().to_string()}),
    )
    .await
    .unwrap_err();
    assert_eq!(error["code"], -32602);

    // A file that is not a program is saved but explained.
    let fake = home.path().join("llama-server.exe");
    std::fs::write(&fake, "not a program").unwrap();
    let status = call(
        &mut socket,
        "runtime.set_server",
        json!({"path": fake.display().to_string()}),
    )
    .await
    .unwrap();
    assert_eq!(status["source"], "custom");
    assert_eq!(status["installed"], true);
    assert_eq!(status["runs"], false);
    assert!(
        status["problem"]
            .as_str()
            .unwrap()
            .contains("could not be started")
    );
    assert_eq!(
        user_file(home.path())["runtime"]["llamacpp"]["custom_path"].as_str(),
        Some(fake.display().to_string().as_str())
    );
    assert_eq!(
        daemon.settings().runtime.llamacpp.custom_path.as_deref(),
        Some(fake.as_path())
    );

    let status = call(&mut socket, "runtime.set_server", json!({"path": null}))
        .await
        .unwrap();
    assert_eq!(status["source"], "pack");
    assert_eq!(daemon.settings().runtime.llamacpp.custom_path, None);
}
