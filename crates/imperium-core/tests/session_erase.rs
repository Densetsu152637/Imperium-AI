//! `session.erase`: the desktop app permanently deletes a session, running or archived, and what
//! the daemon stored for it. Nobody else may (an agent's command, a mod's page, a plain token
//! holder, a paired phone), and a daemon running as a server refuses it for everyone. Nothing
//! outside the session's own storage goes: the project's files stay.
//!
//! The desktop app is identified by the operating system from the process on the other end of the
//! local socket, which a test cannot be: `as_desktop_app_for_tests` (the `testing` feature) stands
//! in for it. Every other connection here is what another local process or a page reaching the
//! daemon over loopback looks like to the daemon: not the desktop app.

use std::path::PathBuf;
use std::sync::Arc;

use imperium_core::{Connection, DaemonState, SessionOptions};
use imperium_protocol::jsonrpc::{Outcome, Response, codes, server_features};
use serde_json::{Value, json};

fn settings(extra: &str) -> imperium_config::Settings {
    imperium_config::Config::from_layers([
        imperium_config::Layer::defaults(),
        imperium_config::Layer::new(
            imperium_config::Source::User("test".into()),
            format!(
                "[models]\ndefault = \"fake:m\"\n[endpoints.fake]\nbase_url = \"http://127.0.0.1:9/v1\"\n[workflow]\nenabled = false\n{extra}"
            )
            .parse()
            .unwrap(),
        ),
    ])
    .settings()
    .unwrap()
}

struct World {
    daemon: Arc<DaemonState>,
    /// The folder the sessions work in: the user's, never touched.
    project: tempfile::TempDir,
    home: tempfile::TempDir,
}

impl World {
    fn new(extra: &str) -> Self {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        std::fs::write(project.path().join("notes.md"), "the user's own file").unwrap();
        let daemon = DaemonState::new(
            imperium_config::Paths::under(home.path()),
            settings(extra),
            None,
        );
        Self {
            daemon,
            project,
            home,
        }
    }

    /// Starts a session in the project folder and gives it the kinds of files a long one has
    /// beside its journal; answers its id.
    fn session(&self) -> String {
        let (session, _) = self
            .daemon
            .create_session(&self.project.path().display().to_string(), None)
            .unwrap();
        let id = session.id().to_string();
        drop(session);
        let journal = self.stored(&id, "jsonl");
        if !journal.exists() {
            std::fs::write(&journal, "").unwrap();
        }
        std::fs::write(self.stored(&id, "messages.json"), "[]").unwrap();
        std::fs::write(self.stored(&id, "turns.jsonl"), "").unwrap();
        for folder in ["results", "agents"] {
            let folder = self.stored(&id, folder);
            std::fs::create_dir_all(&folder).unwrap();
            std::fs::write(folder.join("kept.txt"), "x").unwrap();
        }
        id
    }

    /// A file or folder the daemon keeps for a session.
    fn stored(&self, id: &str, extension: &str) -> PathBuf {
        imperium_config::Paths::under(self.home.path())
            .data_dir()
            .join("sessions")
            .join(format!("{id}.{extension}"))
    }

    /// Everything stored for the session that this test made or the daemon wrote.
    fn leftovers(&self, id: &str) -> Vec<String> {
        [
            "jsonl",
            "meta.json",
            "messages.json",
            "turns.jsonl",
            "results",
            "agents",
        ]
        .into_iter()
        .filter(|extension| self.stored(id, extension).exists())
        .map(str::to_string)
        .collect()
    }

    /// The desktop app's own connection.
    async fn app(&self) -> Connection {
        let mut connection = Connection::new(Arc::clone(&self.daemon)).as_desktop_app_for_tests();
        shake(&mut connection, "imperium-desktop").await;
        connection
    }

    /// Any other local or loopback client: a mod's page, a hook, a plugin's server, an agent's
    /// command, the CLI, a plain token holder. None is the desktop app.
    async fn other(&self) -> Connection {
        let mut connection = Connection::new(Arc::clone(&self.daemon));
        shake(&mut connection, "anyone").await;
        connection
    }

    /// The ids `session.list` shows, running and archived.
    async fn listed(&self) -> Vec<String> {
        let mut connection = self.other().await;
        let listed = result(call(&mut connection, "session.list", json!({})).await);
        listed["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|session| session["session_id"].as_str().unwrap().to_string())
            .collect()
    }

    fn project_file_is_intact(&self) -> bool {
        std::fs::read_to_string(self.project.path().join("notes.md"))
            .is_ok_and(|text| text == "the user's own file")
    }
}

async fn call(connection: &mut Connection, method: &str, params: Value) -> Response {
    let frame = json!({"jsonrpc": "2.0", "id": 5, "method": method, "params": params});
    let handled = connection.handle_text(&frame.to_string());
    match (handled.response, handled.deferred) {
        (Some(response), _) => response,
        (None, Some(deferred)) => {
            tokio::time::timeout(std::time::Duration::from_secs(20), deferred)
                .await
                .expect("an answer within 20 s")
        }
        (None, None) => panic!("no reply"),
    }
}

async fn shake(connection: &mut Connection, name: &str) {
    let handshake = json!({"client": {"name": name, "version": "0"},
                           "protocol": {"major": 0, "minor": 67}});
    result(call(connection, "system.handshake", handshake).await);
}

fn result(response: Response) -> Value {
    match response.outcome {
        Outcome::Result(value) => value,
        Outcome::Error(error) => panic!("{error:?}"),
    }
}

fn error(response: Response) -> (i32, String, Option<Value>) {
    match response.outcome {
        Outcome::Error(error) => (error.code, error.message, error.data),
        Outcome::Result(value) => panic!("expected an error, got {value}"),
    }
}

fn erase(id: &str) -> Value {
    json!({"session_id": id})
}

#[tokio::test]
async fn erasing_a_session_removes_what_was_stored_for_it_and_nothing_else() {
    let world = World::new("");
    let id = world.session();
    let other = world.session();
    assert!(world.listed().await.contains(&id));
    assert!(world.leftovers(&id).contains(&"jsonl".to_string()));

    let mut app = world.app().await;
    let mut notices = world.daemon.announcements();
    result(call(&mut app, "session.erase", erase(&id)).await);

    assert!(world.daemon.session(&id).is_none());
    assert_eq!(world.leftovers(&id), Vec::<String>::new());
    let listed = world.listed().await;
    assert!(!listed.contains(&id), "{listed:?}");
    // Every client hears of it.
    let mut announced = false;
    while let Ok(notice) = notices.try_recv() {
        announced |= notice.method == imperium_protocol::methods::SESSION_DELETED
            && notice.params["session_id"] == id;
    }
    assert!(announced, "no session.deleted for the erase");
    // Another session, and the project's own files, are untouched.
    assert!(listed.contains(&other));
    assert!(world.leftovers(&other).contains(&"jsonl".to_string()));
    assert!(world.project_file_is_intact());
    // It is gone: a second erase names no session.
    let (code, _, _) = error(call(&mut app, "session.erase", erase(&id)).await);
    assert_eq!(code, codes::INVALID_PARAMS);
}

#[tokio::test]
async fn an_archived_session_can_be_erased_too() {
    let world = World::new("");
    let id = world.session();
    world.daemon.archive(&id).unwrap();
    assert!(world.listed().await.contains(&id));
    assert_eq!(world.daemon.archived_sessions().len(), 1);

    let mut app = world.app().await;
    result(call(&mut app, "session.erase", erase(&id)).await);

    assert!(world.daemon.archived_sessions().is_empty());
    assert!(world.daemon.archived_history(&id).is_none());
    assert_eq!(world.leftovers(&id), Vec::<String>::new());
    assert!(!world.listed().await.contains(&id));
    assert!(world.project_file_is_intact());
}

#[tokio::test]
async fn nobody_but_the_desktop_app_may_erase_a_session() {
    let world = World::new("");
    let id = world.session();

    // An agent's command, a hook, a mod's page and a plain token holder all look like this.
    let mut anyone = world.other().await;
    let (code, message, _) = error(call(&mut anyone, "session.erase", erase(&id)).await);
    assert_eq!(code, codes::FORBIDDEN, "{message}");
    assert!(message.contains("permission denied"), "{message}");

    // A paired phone is limited to the methods its scopes name, and a paired device is refused
    // by name even where a scope were to list the method.
    for allowed in [vec!["session.list"], vec!["session.list", "session.erase"]] {
        let mut phone = Connection::restricted(Arc::clone(&world.daemon), allowed);
        shake(&mut phone, "phone").await;
        let (code, message, _) = error(call(&mut phone, "session.erase", erase(&id)).await);
        assert!(
            code == codes::FORBIDDEN || code == codes::METHOD_NOT_FOUND,
            "{code} {message}"
        );
    }

    // Nothing was removed, and the session still lists.
    assert!(world.daemon.session(&id).is_some());
    assert!(world.leftovers(&id).contains(&"jsonl".to_string()));
    assert!(world.listed().await.contains(&id));
    assert!(world.project_file_is_intact());
}

#[tokio::test]
async fn a_server_refuses_to_erase_a_session_even_for_the_desktop_app() {
    let world = World::new("[daemon]\nserver = true");
    let id = world.session();

    for mut connection in [world.app().await, world.other().await] {
        let (code, _, data) = error(call(&mut connection, "session.erase", erase(&id)).await);
        assert_eq!(code, codes::UNAVAILABLE_ON_SERVER);
        assert_eq!(
            data,
            Some(json!({"feature": server_features::SESSION_ERASE}))
        );
    }
    assert!(world.daemon.session(&id).is_some());
    assert!(world.leftovers(&id).contains(&"jsonl".to_string()));
}

#[tokio::test]
async fn an_environments_persistent_agent_is_not_erased() {
    let world = World::new("");
    let (agent, _) = world
        .daemon
        .create_session_with(
            &world.project.path().display().to_string(),
            None,
            SessionOptions {
                persistent: true,
                ..SessionOptions::default()
            },
        )
        .unwrap();
    let id = agent.id().to_string();
    let mut app = world.app().await;
    let (code, message, _) = error(call(&mut app, "session.erase", erase(&id)).await);
    assert_eq!(code, codes::INVALID_PARAMS, "{message}");
    assert!(world.daemon.session(&id).is_some());
}
