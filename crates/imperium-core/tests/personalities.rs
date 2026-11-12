//! `personalities.list` against the daemon: personalities are found whatever the workflow
//! switches say (docs/08-agent-policy.md#personalities).

use std::path::Path;
use std::sync::Arc;

use imperium_core::{Connection, DaemonState};
use imperium_protocol::jsonrpc::{Outcome, Response};
use imperium_protocol::methods::PersonalitiesListResult;
use serde_json::{Value, json};

fn daemon(home: &Path, workflow: &str) -> Arc<DaemonState> {
    let config = format!(
        "[models]\ndefault = \"fake:m\"\n[workflow]\n{workflow}\n[memory]\nenabled = false\n"
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

fn call(connection: &mut Connection, method: &str, params: Value) -> Value {
    let frame = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
    let handled = connection.handle_text(&frame.to_string());
    let response: Response = handled.response.expect("an immediate reply");
    match response.outcome {
        Outcome::Result(value) => value,
        Outcome::Error(error) => panic!("{method}: {}", error.message),
    }
}

#[test]
fn personalities_load_with_the_default_pack_or_the_workflow_switched_off() {
    for workflow in ["default_pack = false", "enabled = false"] {
        let home = tempfile::tempdir().unwrap();
        let folder = imperium_config::Paths::under(home.path())
            .config_dir()
            .join("personalities");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("mine.md"), "# Mine\n\nPlain.\n").unwrap();
        let state = daemon(home.path(), workflow);
        let mut connection = Connection::new(Arc::clone(&state));
        call(
            &mut connection,
            "system.handshake",
            json!({"client": {"name": "test", "version": "0"}, "protocol": {"major": 0, "minor": 13}}),
        );
        let list: PersonalitiesListResult =
            serde_json::from_value(call(&mut connection, "personalities.list", json!({}))).unwrap();
        let ids: Vec<&str> = list.personalities.iter().map(|p| p.id.as_str()).collect();
        assert!(ids.contains(&"mine"), "{workflow}: {ids:?}");
        assert!(ids.contains(&"marcus_aurelius"), "{workflow}: {ids:?}");
        // The bundled pack's personalities are still listed.
        assert!(ids.len() > 10, "{workflow}: {ids:?}");
    }
}
