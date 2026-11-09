//! `models.config` and `models.set_config`: per-model settings saved to the app's file
//! (`<config>/models.toml`), loaded back by the configuration layers, and applied live.

use std::path::Path;
use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use imperium_config::Paths;
use imperium_protocol::PROTOCOL_VERSION;
use imperiumd::AccessToken;
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

/// A daemon with one local model (`tiny`) and one hosted row (`coder`).
async fn start_daemon(home: &Path) -> (Socket, Arc<imperiumd::DaemonState>) {
    let models = home.join("models");
    std::fs::create_dir_all(&models).unwrap();
    std::fs::write(models.join("tiny.gguf"), gguf()).unwrap();
    let text = format!(
        r#"
[models]
folders = ['{}']
[[models.catalog]]
id = "coder"
spec = "cloud:coder-1"
location = "cloud"
tier = "basic"
[endpoints.cloud]
base_url = "https://example.invalid/v1"
"#,
        models.display()
    );
    let paths = Paths::under(home);
    std::fs::create_dir_all(paths.config_dir()).unwrap();
    std::fs::write(paths.user_config_file(), text).unwrap();
    let config = imperium_config::load(&paths, None, []).unwrap();
    let daemon = imperiumd::DaemonState::new(paths, config.settings().unwrap(), None);
    daemon.remember_config(&config);
    daemon.rescan_local_models();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/rpc", listener.local_addr().unwrap());
    tokio::spawn(imperiumd::serve(
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
        let message = tokio::time::timeout(std::time::Duration::from_secs(20), socket.next())
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

#[tokio::test]
async fn set_config_saves_reloads_and_applies() {
    let home = tempfile::tempdir().unwrap();
    let (mut socket, daemon) = start_daemon(home.path()).await;
    let paths = Paths::under(home.path());
    let file = paths.app_models_file();

    // Nothing is saved yet: the defaults are in effect.
    let config = call(&mut socket, "models.config", json!({})).await.unwrap();
    assert!(config["file"].as_str().unwrap().ends_with("models.toml"));
    let tiny = &config["local"][0];
    assert_eq!(tiny["id"], "tiny");
    assert_eq!(tiny["saved"]["slots"], Value::Null);
    assert_eq!(tiny["effective"]["slots"], 8);
    assert_eq!(config["defaults"]["slots"], 8);
    assert_eq!(config["table"][0]["effective"]["enabled"], true);

    // A dry run computes the result and writes nothing.
    let update = json!({
        "local": [{"id": "tiny", "settings": {"slots": 3, "cache_type_k": "q8_0"}}],
        "table": [{"id": "coder", "settings": {"enabled": false, "tier": "strong"}}],
    });
    let mut dry = update.clone();
    dry["dry_run"] = json!(true);
    let result = call(&mut socket, "models.set_config", dry).await.unwrap();
    assert_eq!(result["local"][0]["effective"]["slots"], 3);
    assert!(!file.exists(), "a dry run writes nothing");
    assert!(daemon.model_table().models[0].enabled);

    // Saving writes the file, which the configuration layers read back.
    let result = call(&mut socket, "models.set_config", update)
        .await
        .unwrap();
    assert!(file.is_file());
    let tiny = &result["local"][0];
    assert_eq!(tiny["saved"]["slots"], 3);
    assert_eq!(tiny["effective"]["slots"], 3);
    assert_eq!(tiny["effective"]["cache_type_k"], "q8_0");
    assert_eq!(result["table"][0]["saved"]["tier"], "strong");
    assert_eq!(result["table"][0]["effective"]["enabled"], false);
    let loaded = imperium_config::load(&paths, None, [])
        .unwrap()
        .settings()
        .unwrap();
    assert_eq!(loaded.launch_of("tiny").slots, 3);
    assert_eq!(loaded.launch_of("tiny").cache_type_k, "q8_0");
    assert!(!loaded.models.catalog[0].enabled);
    // It is in effect in the running daemon, and `models.config` reports it.
    assert!(!daemon.model_table().models[0].enabled);
    let config = call(&mut socket, "models.config", json!({})).await.unwrap();
    assert_eq!(config["local"][0]["saved"]["slots"], 3);
    assert_eq!(config["local"][0]["effective"]["slots"], 3);

    // An invalid value is refused, and the file does not change.
    let before = std::fs::read_to_string(&file).unwrap();
    for (params, expected) in [
        (
            json!({"local": [{"id": "tiny", "settings": {"slots": 0}}]}),
            "local model tiny",
        ),
        (
            json!({"local": [{"id": "nope", "settings": {"slots": 2}}]}),
            "local model nope",
        ),
        (
            json!({"table": [{"id": "coder", "settings": {"input_usd_per_mtok": 1.0}}]}),
            "model table row coder",
        ),
    ] {
        let error = call(&mut socket, "models.set_config", params)
            .await
            .unwrap_err();
        assert_eq!(error["code"], -32602, "{error}");
        assert!(
            error["message"].as_str().unwrap().contains(expected),
            "{error}"
        );
        assert_eq!(std::fs::read_to_string(&file).unwrap(), before);
    }

    // Settings that are all null remove the model's saved settings.
    let reset = json!({"local": [{"id": "tiny", "settings": {}}]});
    let result = call(&mut socket, "models.set_config", reset).await.unwrap();
    assert_eq!(result["local"][0]["saved"]["slots"], Value::Null);
    assert_eq!(result["local"][0]["effective"]["slots"], 8);
    let saved = std::fs::read_to_string(&file).unwrap();
    assert!(!saved.contains("tiny"), "{saved}");
    assert!(
        saved.contains("coder"),
        "the other model keeps its settings: {saved}"
    );
}

#[tokio::test]
async fn a_save_applies_only_the_app_file_not_later_config_toml_edits() {
    let home = tempfile::tempdir().unwrap();
    let (mut socket, daemon) = start_daemon(home.path()).await;
    let paths = Paths::under(home.path());
    // config.toml is edited by hand while the daemon runs: those edits wait for a restart.
    let user = paths.user_config_file();
    let edited = std::fs::read_to_string(&user)
        .unwrap()
        .replace("tier = \"basic\"", "tier = \"strong\"")
        + "[runtime.llamacpp]
slots = 5
";
    std::fs::write(&user, edited).unwrap();

    let update = json!({"table": [{"id": "coder", "settings": {"enabled": false}}]});
    let result = call(&mut socket, "models.set_config", update)
        .await
        .unwrap();
    assert_eq!(result["table"][0]["effective"]["enabled"], false);
    assert_eq!(result["table"][0]["effective"]["tier"], "basic");
    assert_eq!(result["local"][0]["effective"]["slots"], 8);
    let live = daemon.settings();
    assert_eq!(live.models.catalog[0].tier.as_str(), "basic");
    assert!(!live.models.catalog[0].enabled);
    assert_eq!(live.launch_of("tiny").slots, 8);
}

#[tokio::test]
async fn an_app_file_that_is_not_valid_toml_is_not_overwritten() {
    let home = tempfile::tempdir().unwrap();
    let (mut socket, _daemon) = start_daemon(home.path()).await;
    let file = Paths::under(home.path()).app_models_file();
    std::fs::write(
        &file,
        "[runtime.llamacpp.models.tiny
slots = 3
",
    )
    .unwrap();
    let update = json!({"local": [{"id": "tiny", "settings": {"slots": 2}}]});
    let error = call(&mut socket, "models.set_config", update)
        .await
        .unwrap_err();
    let message = error["message"].as_str().unwrap();
    assert!(message.contains("models.toml"), "{error}");
    assert!(message.contains("not valid TOML"), "{error}");
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "[runtime.llamacpp.models.tiny
slots = 3
"
    );
}
