//! Auto-merge, desktop only, against the fake GitHub. It is off for every pull request until the
//! desktop app turns it on with a single-use challenge; nobody else can turn it on (a paired
//! phone, a mod's page, an agent, a hook, a plugin's server); anybody local can turn it off; the
//! only things ever sent to GitHub that change anything are the two auto-merge mutations, never a
//! merge; and the daemon's own push turns it off first.
//!
//! The desktop app is identified by the operating system from the process on the other end of the
//! local socket, which a test cannot be: `as_desktop_app_for_tests` (the `testing` feature) stands
//! in for it. Every other connection here is what a phone, a mod's page or another local process
//! looks like to the daemon: not the desktop app.

use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use imperium_core::{Connection, DaemonState};
use imperium_github::{FailingPr, GhFuture, GitHub, HttpGitHub, StaticToken};
use imperium_github_fake::FakeGithub;
use imperium_protocol::jsonrpc::{Outcome, Response, codes};
use serde_json::{Value, json};

const TOKEN: &str = "ghp_AUTOMERGESENTINEL0123456789abcdef";
const REPO: &str = "owner/repo";
const URL: &str = "https://github.com/owner/repo/pull/7";
const HEAD: &str = "abc123";

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

/// A folder that is a git repository whose `origin` is `url`.
fn repository(url: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for args in [vec!["init", "-q"], vec!["remote", "add", "origin", url]] {
        let status = Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(args)
            .status()
            .unwrap();
        assert!(status.success());
    }
    dir
}

struct World {
    fake: FakeGithub,
    daemon: Arc<DaemonState>,
    session: String,
    // Held so the data folder and the checkout outlive the test body.
    _home: tempfile::TempDir,
    _folder: tempfile::TempDir,
}

impl World {
    async fn new(extra: &str) -> Self {
        Self::prepared(extra, |_| {}).await
    }

    /// A world whose GitHub is set up by `prepare` before the session binds: the daemon reads a
    /// repository's merge settings once and keeps them for a few minutes.
    async fn prepared(extra: &str, prepare: impl FnOnce(&FakeGithub)) -> Self {
        let fake = FakeGithub::start(TOKEN).await;
        fake.add_repo(REPO);
        fake.add_pr(REPO, 7, "Add the thing", "feature", HEAD);
        prepare(&fake);
        let home = tempfile::tempdir().unwrap();
        let daemon = DaemonState::new(
            imperium_config::Paths::under(home.path()),
            settings(extra),
            None,
        );
        daemon.pr_monitor().attach(Arc::new(github(&fake)));
        let folder = repository("https://github.com/owner/repo.git");
        let (session, _) = daemon
            .create_session(&folder.path().display().to_string(), None)
            .unwrap();
        let session = session.id().to_string();
        let world = Self {
            fake,
            daemon,
            session,
            _home: home,
            _folder: folder,
        };
        let mut anyone = world.other("test").await;
        result(
            call(
                &mut anyone,
                "pull_requests.bind",
                json!({"session_id": world.session, "url": URL}),
            )
            .await,
        );
        world
    }

    /// The desktop app's own connection.
    async fn app(&self) -> Connection {
        let mut connection = Connection::new(Arc::clone(&self.daemon)).as_desktop_app_for_tests();
        shake(&mut connection, "imperium-desktop").await;
        connection
    }

    /// Any other local or loopback client: a mod's page, a hook, a plugin's server, an agent's
    /// command, the CLI. None is the desktop app.
    async fn other(&self, name: &str) -> Connection {
        let mut connection = Connection::new(Arc::clone(&self.daemon));
        shake(&mut connection, name).await;
        connection
    }

    /// The card's status, as a client sees it.
    async fn status(&self) -> Value {
        let mut connection = self.other("status").await;
        result(
            call(
                &mut connection,
                "pull_requests.status",
                json!({"session_id": self.session}),
            )
            .await,
        )
    }

    /// `pull_requests.auto_merge_challenge` as the desktop app, for `head` and `method`.
    async fn challenge(&self, app: &mut Connection, head: &str, method: &str) -> String {
        let answer = result(
            call(
                app,
                "pull_requests.auto_merge_challenge",
                json!({"session_id": self.session, "url": URL, "head_sha": head,
                       "merge_method": method}),
            )
            .await,
        );
        answer["challenge"].as_str().unwrap().to_string()
    }

    fn enable(&self, head: &str, method: &str, challenge: Option<&str>) -> Value {
        json!({"session_id": self.session, "url": URL, "head_sha": head, "enabled": true,
               "merge_method": method, "challenge": challenge})
    }

    /// Turns auto-merge on as the desktop app does, and answers the new status.
    async fn turn_on(&self, method: &str) -> Value {
        let mut app = self.app().await;
        let challenge = self.challenge(&mut app, HEAD, method).await;
        result(
            call(
                &mut app,
                "pull_requests.set_auto_merge",
                self.enable(HEAD, method, Some(challenge.as_str())),
            )
            .await,
        )
    }

    fn disable(&self) -> Value {
        json!({"session_id": self.session, "url": URL, "head_sha": HEAD, "enabled": false})
    }

    /// The names of the GraphQL mutations GitHub accepted.
    fn mutations(&self) -> Vec<String> {
        self.fake
            .mutations()
            .into_iter()
            .map(|(operation, _)| operation)
            .collect()
    }
}

fn github(fake: &FakeGithub) -> HttpGitHub {
    HttpGitHub::new(&fake.url(), Arc::new(StaticToken::new(TOKEN))).unwrap()
}

async fn call(connection: &mut Connection, method: &str, params: Value) -> Response {
    let frame = json!({"jsonrpc": "2.0", "id": 5, "method": method, "params": params});
    let handled = connection.handle_text(&frame.to_string());
    match (handled.response, handled.deferred) {
        (Some(response), _) => response,
        (None, Some(deferred)) => tokio::time::timeout(Duration::from_secs(20), deferred)
            .await
            .expect("an answer within 20 s"),
        (None, None) => panic!("no reply"),
    }
}

async fn shake(connection: &mut Connection, name: &str) {
    let handshake = json!({"client": {"name": name, "version": "0"},
                           "protocol": {"major": 0, "minor": 60}});
    result(call(connection, "system.handshake", handshake).await);
}

fn result(response: Response) -> Value {
    match response.outcome {
        Outcome::Result(value) => value,
        Outcome::Error(error) => panic!("{error:?}"),
    }
}

fn error(response: Response) -> (i32, String) {
    match response.outcome {
        Outcome::Error(error) => (error.code, error.message),
        Outcome::Result(value) => panic!("expected an error, got {value}"),
    }
}

/// An error that says the caller may not do this.
fn denied(response: Response) {
    let (code, message) = error(response);
    assert_eq!(code, codes::FORBIDDEN, "{message}");
    assert!(message.contains("permission denied"), "{message}");
}

#[tokio::test]
async fn auto_merge_is_off_until_the_desktop_app_turns_it_on() {
    let world = World::new("").await;
    let status = world.status().await;
    assert_eq!(status["auto_merge"]["enabled"], false);
    assert!(status["auto_merge"].get("reason_unavailable").is_none());
    assert_eq!(status["allowed_methods"], json!(["squash", "merge"]));
    assert!(world.fake.mutations().is_empty());
    assert!(world.fake.pr(REPO, 7).auto_merge.is_none());
}

#[tokio::test]
async fn turning_it_on_sends_the_method_and_the_head_the_user_saw() {
    let world = World::new("").await;
    let status = world.turn_on("merge").await;
    assert_eq!(status["auto_merge"]["enabled"], true);
    assert_eq!(status["auto_merge"]["method"], "merge");
    assert_eq!(world.mutations(), ["EnableAutoMerge"]);
    let (_, variables) = &world.fake.mutations()[0];
    assert_eq!(variables["method"], "MERGE");
    assert_eq!(variables["oid"], HEAD);
    assert_eq!(
        world.fake.pr(REPO, 7).auto_merge,
        Some(("MERGE".to_string(), HEAD.to_string()))
    );
    // The poller's view agrees with GitHub.
    assert_eq!(world.status().await["auto_merge"]["method"], "merge");
}

#[tokio::test]
async fn turning_it_on_needs_a_valid_single_use_challenge() {
    let world = World::new("").await;
    let mut app = world.app().await;

    // No challenge.
    denied(
        call(
            &mut app,
            "pull_requests.set_auto_merge",
            world.enable(HEAD, "squash", None),
        )
        .await,
    );
    // No method.
    let challenge = world.challenge(&mut app, HEAD, "squash").await;
    denied(
        call(
            &mut app,
            "pull_requests.set_auto_merge",
            json!({"session_id": world.session, "url": URL, "head_sha": HEAD, "enabled": true,
                   "challenge": challenge}),
        )
        .await,
    );
    // Nothing that is not a challenge this connection issued.
    denied(
        call(
            &mut app,
            "pull_requests.set_auto_merge",
            world.enable(HEAD, "squash", Some("not-a-challenge")),
        )
        .await,
    );

    // Another connection's challenge: the app's other connection did not ask for it.
    let mut another_app = world.app().await;
    let challenge = world.challenge(&mut app, HEAD, "squash").await;
    denied(
        call(
            &mut another_app,
            "pull_requests.set_auto_merge",
            world.enable(HEAD, "squash", Some(challenge.as_str())),
        )
        .await,
    );

    // Another head's and another method's (each uses its challenge up).
    for (head, method) in [("def456", "squash"), (HEAD, "merge")] {
        let challenge = world.challenge(&mut app, HEAD, "squash").await;
        denied(
            call(
                &mut app,
                "pull_requests.set_auto_merge",
                world.enable(head, method, Some(challenge.as_str())),
            )
            .await,
        );
        // The used-up challenge is gone, for the right content too.
        denied(
            call(
                &mut app,
                "pull_requests.set_auto_merge",
                world.enable(HEAD, "squash", Some(challenge.as_str())),
            )
            .await,
        );
    }

    // Another pull request's address.
    let challenge = world.challenge(&mut app, HEAD, "squash").await;
    let mut other_url = world.enable(HEAD, "squash", Some(challenge.as_str()));
    other_url["url"] = json!("https://github.com/owner/repo/pull/8");
    denied(call(&mut app, "pull_requests.set_auto_merge", other_url).await);

    assert!(
        world.fake.mutations().is_empty(),
        "nothing was sent to GitHub"
    );

    // The right one works once and only once.
    let challenge = world.challenge(&mut app, HEAD, "squash").await;
    let on = result(
        call(
            &mut app,
            "pull_requests.set_auto_merge",
            world.enable(HEAD, "squash", Some(challenge.as_str())),
        )
        .await,
    );
    assert_eq!(on["auto_merge"]["enabled"], true);
    denied(
        call(
            &mut app,
            "pull_requests.set_auto_merge",
            world.enable(HEAD, "squash", Some(challenge.as_str())),
        )
        .await,
    );
    assert_eq!(world.mutations(), ["EnableAutoMerge"]);
}

#[tokio::test]
async fn nobody_but_the_desktop_app_can_turn_it_on_or_ask_for_a_challenge() {
    let world = World::new("").await;
    let mut app = world.app().await;
    let challenge = world.challenge(&mut app, HEAD, "squash").await;

    // A mod's page (loopback TCP), a hook's request, a plugin's server and an agent's command
    // all reach the daemon as an ordinary client that is not the installed app, whatever they
    // call themselves, and even with a challenge the app was given.
    for name in [
        "imperium-desktop",
        "mod-frame",
        "imperium-hook",
        "plugin-tool",
        "imperium-agent",
        "imperium",
    ] {
        let mut stranger = world.other(name).await;
        denied(
            call(
                &mut stranger,
                "pull_requests.auto_merge_challenge",
                json!({"session_id": world.session, "url": URL, "head_sha": HEAD,
                       "merge_method": "squash"}),
            )
            .await,
        );
        denied(
            call(
                &mut stranger,
                "pull_requests.set_auto_merge",
                world.enable(HEAD, "squash", Some(challenge.as_str())),
            )
            .await,
        );
    }

    // A paired phone's connection never reaches it: its scopes do not list the methods.
    let mut phone = Connection::restricted(
        Arc::clone(&world.daemon),
        vec!["system.handshake", "pull_requests.status"],
    );
    shake(&mut phone, "phone").await;
    for method in [
        "pull_requests.auto_merge_challenge",
        "pull_requests.set_auto_merge",
    ] {
        let (code, message) = error(
            call(
                &mut phone,
                method,
                json!({"session_id": world.session, "url": URL, "head_sha": HEAD,
                       "merge_method": "squash", "enabled": true, "challenge": challenge}),
            )
            .await,
        );
        assert_eq!(code, codes::FORBIDDEN, "{message}");
    }
    // Even a device whose scopes were widened by mistake, and that somehow passed for the app,
    // is refused.
    let mut widened = Connection::restricted(
        Arc::clone(&world.daemon),
        vec![
            "system.handshake",
            "pull_requests.auto_merge_challenge",
            "pull_requests.set_auto_merge",
        ],
    )
    .as_desktop_app_for_tests();
    shake(&mut widened, "phone").await;
    denied(
        call(
            &mut widened,
            "pull_requests.set_auto_merge",
            world.enable(HEAD, "squash", Some(challenge.as_str())),
        )
        .await,
    );

    assert!(world.fake.mutations().is_empty());
    assert_eq!(world.status().await["auto_merge"]["enabled"], false);
}

/// Agents act through tools, and no tool turns auto-merge on: none is named for it or for pull
/// requests, and `session.run_tool` (the daemon's bridge to the tools) refuses anything that is
/// not one of them, whatever the method it is asked to run is called.
#[tokio::test]
async fn no_agent_tool_turns_it_on_and_run_tool_refuses_the_method() {
    let world = World::new("").await;
    let mut tools: Vec<(String, String)> = imperium_engine::bridge::tools()
        .into_iter()
        .map(|tool| {
            (
                tool.name,
                format!("{} {}", tool.description, tool.parameters),
            )
        })
        .collect();
    tools.extend(imperium_tools::specs().into_iter().map(|spec| {
        (
            spec.name,
            format!("{} {}", spec.description, spec.parameters),
        )
    }));
    assert!(!tools.is_empty());
    for (name, text) in &tools {
        let name = name.to_ascii_lowercase();
        let text = text.to_ascii_lowercase();
        for word in ["auto_merge", "automerge", "auto-merge", "pull_request"] {
            assert!(!name.contains(word), "tool {name} is about {word}");
        }
        for word in ["set_auto_merge", "pull_requests."] {
            assert!(!text.contains(word), "tool {name} names {word}: {text}");
        }
    }

    // The bridge runs only the tools it lists, with writes allowed or not.
    let mut agent = world.other("imperium").await;
    for tool in [
        "pull_requests.set_auto_merge",
        "pull_requests.auto_merge_challenge",
        "set_auto_merge",
    ] {
        let (code, message) = error(
            call(
                &mut agent,
                "session.run_tool",
                json!({"session_id": world.session, "tool": tool,
                       "arguments": {"enabled": true}, "allow_write": true}),
            )
            .await,
        );
        assert_eq!(code, codes::INVALID_PARAMS, "{message}");
    }
    assert!(world.fake.mutations().is_empty());
}

#[tokio::test]
async fn anyone_local_can_turn_it_off_and_nothing_is_sent_when_it_is_already_off() {
    let world = World::new("").await;
    // Off already: no mutation.
    let mut cli = world.other("imperium").await;
    let status = result(call(&mut cli, "pull_requests.set_auto_merge", world.disable()).await);
    assert_eq!(status["auto_merge"]["enabled"], false);
    assert!(world.fake.mutations().is_empty());

    for name in ["imperium", "imperium-agent", "mod-frame", "imperium-hook"] {
        world.turn_on("squash").await;
        assert!(world.fake.pr(REPO, 7).auto_merge.is_some());
        let mut caller = world.other(name).await;
        let status =
            result(call(&mut caller, "pull_requests.set_auto_merge", world.disable()).await);
        assert_eq!(status["auto_merge"]["enabled"], false, "{name}");
        assert!(world.fake.pr(REPO, 7).auto_merge.is_none(), "{name}");
    }
    assert_eq!(
        world.mutations(),
        [
            "EnableAutoMerge",
            "DisableAutoMerge",
            "EnableAutoMerge",
            "DisableAutoMerge",
            "EnableAutoMerge",
            "DisableAutoMerge",
            "EnableAutoMerge",
            "DisableAutoMerge",
        ]
    );

    // A disable names the bound pull request, not another one.
    let mut other_url = world.disable();
    other_url["url"] = json!("https://github.com/owner/repo/pull/8");
    let (code, _) = error(call(&mut cli, "pull_requests.set_auto_merge", other_url).await);
    assert_eq!(code, codes::INVALID_PARAMS);
}

#[tokio::test]
async fn only_the_two_auto_merge_mutations_and_reads_are_ever_sent() {
    let world = World::new("").await;
    world.turn_on("squash").await;
    let mut cli = world.other("imperium").await;
    result(call(&mut cli, "pull_requests.set_auto_merge", world.disable()).await);
    // A refused request too.
    let mut app = world.app().await;
    let challenge = world.challenge(&mut app, "stale", "squash").await;
    let (code, _) = error(
        call(
            &mut app,
            "pull_requests.set_auto_merge",
            world.enable("stale", "squash", Some(challenge.as_str())),
        )
        .await,
    );
    assert_eq!(code, codes::INVALID_PARAMS);

    let requests = world.fake.requests();
    assert!(!requests.is_empty());
    for request in &requests {
        match request.method.as_str() {
            "GET" => assert!(!request.path.contains("merge"), "{request:?}"),
            "POST" => {
                assert_eq!(request.path, "/graphql", "{request:?}");
                let operation = request.operation.as_deref().unwrap_or_default();
                assert!(
                    ["PrStatus", "EnableAutoMerge", "DisableAutoMerge"].contains(&operation),
                    "{request:?}"
                );
            }
            // No PUT (a merge is `PUT /pulls/{n}/merge`), PATCH or DELETE.
            other => panic!("{other} {}", request.path),
        }
        assert!(!request.path.ends_with("/merge"), "{request:?}");
    }
    assert_eq!(world.mutations(), ["EnableAutoMerge", "DisableAutoMerge"]);
    let (_, variables) = &world.fake.mutations()[0];
    assert_eq!(variables["oid"], HEAD);
    assert!(world.fake.pr(REPO, 7).auto_merge.is_none());
}

#[tokio::test]
async fn a_branch_that_moved_is_refused_before_github_is_asked() {
    let world = World::new("").await;
    let mut app = world.app().await;
    let challenge = world.challenge(&mut app, HEAD, "squash").await;
    world.fake.force_push(REPO, 7, "def456");
    let (code, message) = error(
        call(
            &mut app,
            "pull_requests.set_auto_merge",
            world.enable(HEAD, "squash", Some(challenge.as_str())),
        )
        .await,
    );
    assert_eq!(code, codes::INVALID_PARAMS);
    assert!(message.contains("branch moved"), "{message}");
    assert!(world.fake.mutations().is_empty());
    // The card now shows the new head, and the user confirms again for it.
    assert_eq!(world.status().await["head_sha"], "def456");
}

#[tokio::test]
async fn a_draft_a_repository_that_forbids_it_and_a_method_it_does_not_allow_refuse() {
    // A repository that does not allow auto-merge.
    let world = World::prepared("", |fake| {
        fake.set_merge_settings(REPO, true, true, false, false);
    })
    .await;
    let mut app = world.app().await;
    let challenge = world.challenge(&mut app, HEAD, "squash").await;
    let (code, message) = error(
        call(
            &mut app,
            "pull_requests.set_auto_merge",
            world.enable(HEAD, "squash", Some(challenge.as_str())),
        )
        .await,
    );
    assert_eq!(code, codes::INVALID_PARAMS);
    assert!(message.contains("does not allow auto-merge"), "{message}");
    assert!(
        world.status().await["auto_merge"]["reason_unavailable"]
            .as_str()
            .unwrap()
            .contains("does not allow auto-merge")
    );

    // A draft.
    let world = World::prepared("", |fake| fake.set_draft(REPO, 7, true)).await;
    let mut app = world.app().await;
    let challenge = world.challenge(&mut app, HEAD, "squash").await;
    let (_, message) = error(
        call(
            &mut app,
            "pull_requests.set_auto_merge",
            world.enable(HEAD, "squash", Some(challenge.as_str())),
        )
        .await,
    );
    assert!(message.contains("draft"), "{message}");

    // A method the repository does not allow (the fake's repository allows squash and merge).
    let world = World::new("").await;
    let mut app = world.app().await;
    let challenge = world.challenge(&mut app, HEAD, "rebase").await;
    let (_, message) = error(
        call(
            &mut app,
            "pull_requests.set_auto_merge",
            world.enable(HEAD, "rebase", Some(challenge.as_str())),
        )
        .await,
    );
    assert!(message.contains("does not allow rebase"), "{message}");
    assert!(world.fake.mutations().is_empty());
}

#[tokio::test]
async fn the_users_setting_can_forbid_it_and_asks_for_nothing_else() {
    let world = World::new("[github.automerge]\nallowed = false\n").await;
    let mut app = world.app().await;
    let challenge = world.challenge(&mut app, HEAD, "squash").await;
    let (code, message) = error(
        call(
            &mut app,
            "pull_requests.set_auto_merge",
            world.enable(HEAD, "squash", Some(challenge.as_str())),
        )
        .await,
    );
    assert_eq!(code, codes::INVALID_PARAMS);
    assert!(message.contains("github.automerge.allowed"), "{message}");
    assert!(world.fake.mutations().is_empty());

    // A preference for a method turns nothing on.
    let world = World::new("[github.automerge]\ndefault_method = \"rebase\"\n").await;
    assert_eq!(world.status().await["auto_merge"]["enabled"], false);
    assert!(world.fake.mutations().is_empty());
}

/// A GitHub source that implements only what auto-fix needs: it has no pull-request status, so
/// the daemon cannot have asked it for auto-merge.
#[derive(Debug)]
struct AutofixOnly;

impl GitHub for AutofixOnly {
    fn failing_prs<'a>(&'a self, _repo: &'a str) -> GhFuture<'a, Vec<FailingPr>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn failure_log<'a>(&'a self, _repo: &'a str, _url: &'a str) -> GhFuture<'a, String> {
        Box::pin(async { Ok(String::new()) })
    }

    fn pr_state<'a>(&'a self, _repo: &'a str, _number: u64) -> GhFuture<'a, String> {
        Box::pin(async { Ok("OPEN".to_string()) })
    }
}

#[tokio::test]
async fn the_daemons_own_push_turns_it_off_first_and_never_back_on() {
    let world = World::new("").await;
    let client = github(&world.fake);

    // Off already: nothing is sent.
    imperium_core::disable_auto_merge_before_push(&world.daemon, &client, REPO, 7)
        .await
        .unwrap();
    assert!(world.fake.mutations().is_empty());

    world.turn_on("squash").await;
    assert_eq!(world.status().await["auto_merge"]["enabled"], true);

    imperium_core::disable_auto_merge_before_push(&world.daemon, &client, REPO, 7)
        .await
        .unwrap();
    assert!(world.fake.pr(REPO, 7).auto_merge.is_none());
    // The card shows it at once, without waiting for a poll.
    assert_eq!(world.status().await["auto_merge"]["enabled"], false);

    // A second push finds it off; nothing re-enables it.
    imperium_core::disable_auto_merge_before_push(&world.daemon, &client, REPO, 7)
        .await
        .unwrap();
    assert_eq!(world.mutations(), ["EnableAutoMerge", "DisableAutoMerge"]);

    // A source with no pull-request status cannot have been asked, so a push is not held up.
    imperium_core::disable_auto_merge_before_push(&world.daemon, &AutofixOnly, REPO, 7)
        .await
        .unwrap();

    // A source that cannot say whether it is on blocks the push.
    world.fake.fail_next(500);
    let error = imperium_core::disable_auto_merge_before_push(&world.daemon, &client, REPO, 7)
        .await
        .unwrap_err();
    assert!(error.contains("could not check"), "{error}");
    assert!(!error.contains(TOKEN));
}

impl World {
    async fn poll(&self) {
        self.daemon.pr_monitor().poll(&self.daemon, true).await;
    }
}

/// GitHub keeps auto-merge on after a push, so a push after the user turned it on (by the agent
/// through `gh`, by a collaborator) would be merged unconfirmed. The poll turns it off.
#[tokio::test]
async fn a_head_that_moves_after_enabling_turns_it_off_at_the_next_poll() {
    let world = World::new("").await;
    world.turn_on("squash").await;
    world.fake.force_push(REPO, 7, "def456");
    world.poll().await;

    assert_eq!(world.mutations(), ["EnableAutoMerge", "DisableAutoMerge"]);
    assert!(world.fake.pr(REPO, 7).auto_merge.is_none());
    let status = world.status().await;
    assert_eq!(status["auto_merge"]["enabled"], false);
    assert_eq!(status["head_sha"], "def456");
    assert!(
        status["auto_merge"]["reason_unavailable"]
            .as_str()
            .unwrap()
            .contains("new commits were pushed after you enabled it"),
        "{status}"
    );

    // Said once, and never turned on again by the daemon.
    world.poll().await;
    world.poll().await;
    assert_eq!(world.mutations(), ["EnableAutoMerge", "DisableAutoMerge"]);
    let status = world.status().await;
    assert_eq!(status["auto_merge"]["enabled"], false);
    assert!(status["auto_merge"].get("reason_unavailable").is_none());
}

#[tokio::test]
async fn a_head_that_stays_the_same_sends_nothing() {
    let world = World::new("").await;
    world.turn_on("squash").await;
    for _ in 0..3 {
        world.poll().await;
    }
    assert_eq!(world.mutations(), ["EnableAutoMerge"]);
    assert_eq!(world.status().await["auto_merge"]["enabled"], true);
}

/// Auto-merge that Imperium did not turn on (enabled on GitHub itself, or before this was
/// recorded) is not Imperium's to turn off.
#[tokio::test]
async fn auto_merge_enabled_elsewhere_is_left_alone_when_the_head_moves() {
    let world = World::new("").await;
    let enable = imperium_github::AutoMerge::Enable {
        method: imperium_github::MergeMethod::Squash,
        expected_head_oid: HEAD.into(),
    };
    github(&world.fake)
        .set_auto_merge(REPO, 7, &enable)
        .await
        .unwrap();
    world.fake.force_push(REPO, 7, "def456");
    // The fake drops nothing on a push: GitHub keeps the request.
    world.poll().await;
    world.poll().await;
    assert_eq!(world.mutations(), ["EnableAutoMerge"]);
    assert_eq!(world.status().await["auto_merge"]["enabled"], true);
}

impl World {
    fn status_reads(&self) -> usize {
        self.fake
            .requests()
            .iter()
            .filter(|r| r.operation.as_deref() == Some("PrStatus"))
            .count()
    }

    /// Calls `pull_requests.unbind` as an ordinary client.
    async fn unbind(&self) -> Response {
        let mut connection = self.other("imperium").await;
        call(
            &mut connection,
            "pull_requests.unbind",
            json!({"session_id": self.session}),
        )
        .await
    }
}

/// A poll whose read was in flight before the user turned auto-merge on comes back saying it is
/// off. It must not erase the head the user confirmed, or a later push is never caught.
#[tokio::test]
async fn a_poll_in_flight_before_enabling_does_not_erase_the_confirmed_head() {
    let world = World::new("").await;
    world.fake.delay_next_status(Duration::from_millis(700));
    let reads = world.status_reads();
    let daemon = Arc::clone(&world.daemon);
    let stale = tokio::spawn(async move {
        daemon.pr_monitor().poll(&daemon, true).await;
    });
    while world.status_reads() == reads {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // The in-flight read is held back with auto-merge off; meanwhile the user turns it on.
    world.turn_on("squash").await;
    stale.await.unwrap();

    world.fake.force_push(REPO, 7, "def456");
    world.poll().await;
    assert_eq!(world.mutations(), ["EnableAutoMerge", "DisableAutoMerge"]);
    assert!(world.fake.pr(REPO, 7).auto_merge.is_none());
}

#[tokio::test]
async fn unbinding_turns_auto_merge_off_and_a_failed_turn_off_keeps_the_binding() {
    let world = World::new("").await;
    world.turn_on("squash").await;

    // The turn-off fails: the unbind fails with a reason and the binding stays.
    world.fake.fail_next(500);
    let (code, message) = error(world.unbind().await);
    assert_eq!(code, codes::INVALID_PARAMS);
    assert!(message.contains("was not unbound"), "{message}");
    assert!(world.fake.pr(REPO, 7).auto_merge.is_some());
    assert_eq!(world.status().await["auto_merge"]["enabled"], true);

    // Retried, it turns auto-merge off and then unbinds.
    let answer = result(world.unbind().await);
    assert_eq!(answer["was_bound"], true);
    assert!(world.fake.pr(REPO, 7).auto_merge.is_none());
    assert_eq!(world.mutations(), ["EnableAutoMerge", "DisableAutoMerge"]);

    // Nothing is sent for a pull request Imperium did not turn on.
    let other = World::new("").await;
    let answer = result(other.unbind().await);
    assert_eq!(answer["was_bound"], true);
    assert!(other.fake.mutations().is_empty());
}

#[tokio::test]
async fn deleting_the_session_turns_auto_merge_off() {
    let world = World::new("").await;
    world.turn_on("squash").await;
    world.daemon.delete_session(&world.session, false).unwrap();
    // The poller (not running in a test) works through the turn-off list.
    world.poll().await;
    assert_eq!(world.mutations(), ["EnableAutoMerge", "DisableAutoMerge"]);
    assert!(world.fake.pr(REPO, 7).auto_merge.is_none());
}

/// The daemon was down when someone pushed: the first poll after it comes back catches it.
#[tokio::test]
async fn a_restart_keeps_the_confirmed_head_and_the_first_poll_catches_a_push() {
    let world = World::new("").await;
    world.turn_on("squash").await;
    let World {
        fake,
        daemon,
        session,
        _home,
        _folder,
    } = world;
    drop(daemon);
    tokio::time::sleep(Duration::from_millis(100)).await;
    fake.force_push(REPO, 7, "def456");

    let daemon = DaemonState::new(
        imperium_config::Paths::under(_home.path()),
        settings(""),
        None,
    );
    daemon.pr_monitor().attach(Arc::new(github(&fake)));
    assert_eq!(daemon.resume_sessions(), 1);
    assert!(daemon.session(&session).is_some());
    daemon.pr_monitor().poll(&daemon, true).await;

    let operations: Vec<String> = fake
        .mutations()
        .into_iter()
        .map(|(operation, _)| operation)
        .collect();
    assert_eq!(operations, ["EnableAutoMerge", "DisableAutoMerge"]);
    assert!(fake.pr(REPO, 7).auto_merge.is_none());
}

const PENDING_FILE: &str = "auto-merge-to-turn-off.json";

impl World {
    /// The pull requests whose auto-merge Imperium still has to turn off, as saved in the data
    /// folder.
    fn pending(&self) -> Vec<Value> {
        pending_in(self.daemon.data_dir())
    }

    /// Binds a second session on the same repository to the pull request, as an ordinary client.
    async fn second_session(&self) -> (String, tempfile::TempDir) {
        let folder = repository("https://github.com/owner/repo.git");
        let (session, _) = self
            .daemon
            .create_session(&folder.path().display().to_string(), None)
            .unwrap();
        let session = session.id().to_string();
        let mut anyone = self.other("second").await;
        result(
            call(
                &mut anyone,
                "pull_requests.bind",
                json!({"session_id": session, "url": URL}),
            )
            .await,
        );
        (session, folder)
    }
}

fn pending_in(data_dir: &std::path::Path) -> Vec<Value> {
    match std::fs::read_to_string(data_dir.join(PENDING_FILE)) {
        Ok(text) => serde_json::from_str(&text).unwrap(),
        Err(_) => Vec::new(),
    }
}

/// An enable that finishes while an unbind waits for GitHub to confirm its turn-off (another
/// session binds the pull request and turns auto-merge on at a newer head) records a head the
/// unbind must not erase, or a later push is never caught.
#[tokio::test]
async fn an_enable_during_an_unbind_keeps_its_confirmed_head() {
    let world = World::new("").await;
    world.turn_on("squash").await;
    world.fake.force_push(REPO, 7, "def456");
    // GitHub applies the turn-off and then answers late.
    world.fake.delay_next_disable(Duration::from_millis(2000));

    let race = async {
        while world.mutations().len() < 2 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let (second, folder) = world.second_session().await;
        let mut app = world.app().await;
        let challenge = result(
            call(
                &mut app,
                "pull_requests.auto_merge_challenge",
                json!({"session_id": second, "url": URL, "head_sha": "def456",
                       "merge_method": "squash"}),
            )
            .await,
        );
        let challenge = challenge["challenge"].as_str().unwrap().to_string();
        result(
            call(
                &mut app,
                "pull_requests.set_auto_merge",
                json!({"session_id": second, "url": URL, "head_sha": "def456", "enabled": true,
                       "merge_method": "squash", "challenge": challenge}),
            )
            .await,
        );
        folder
    };
    let (unbound, _folder) = tokio::join!(world.unbind(), race);
    assert_eq!(result(unbound)["was_bound"], true);
    assert_eq!(
        world.mutations(),
        ["EnableAutoMerge", "DisableAutoMerge", "EnableAutoMerge"]
    );
    assert!(world.fake.pr(REPO, 7).auto_merge.is_some());

    // The second session's confirmation survived: the next push is caught.
    world.fake.force_push(REPO, 7, "fed789");
    world.poll().await;
    assert_eq!(
        world.mutations(),
        [
            "EnableAutoMerge",
            "DisableAutoMerge",
            "EnableAutoMerge",
            "DisableAutoMerge"
        ]
    );
    assert!(world.fake.pr(REPO, 7).auto_merge.is_none());
}

/// A turn-off that fails when the session is deleted is saved (before anything is sent), tried
/// again at the next tick, announced once, and dropped when it works.
#[tokio::test]
async fn a_failed_turn_off_on_delete_is_saved_announced_and_retried_at_the_next_tick() {
    let world = World::new("").await;
    world.turn_on("squash").await;
    let mut announcements = world.daemon.announcements();

    world.daemon.delete_session(&world.session, false).unwrap();
    let saved = world.pending();
    assert_eq!(saved.len(), 1, "{saved:?}");
    assert_eq!(saved[0]["repo"], REPO);
    assert_eq!(saved[0]["number"], 7);
    assert_eq!(saved[0]["head"], HEAD);

    // The tick's attempt fails: the entry stays, auto-merge stays on, the user hears of it once.
    world.fake.fail_next(500);
    world.poll().await;
    assert_eq!(world.pending().len(), 1);
    assert!(world.fake.pr(REPO, 7).auto_merge.is_some());
    let mut told = Vec::new();
    while let Ok(notification) = announcements.try_recv() {
        if notification.method == "autofix.event" {
            told.push(notification.params);
        }
    }
    assert_eq!(told.len(), 1, "{told:?}");
    assert_eq!(told[0]["status"], "failed");
    assert_eq!(told[0]["pull_request"], 7);
    let summary = told[0]["summary"].as_str().unwrap();
    assert!(
        summary.contains("could not be turned off yet") && summary.contains("try again"),
        "{summary}"
    );
    assert!(!summary.contains(TOKEN));

    // The next tick turns it off, drops the entry and says nothing more.
    world.poll().await;
    assert!(world.pending().is_empty());
    assert!(world.fake.pr(REPO, 7).auto_merge.is_none());
    assert_eq!(world.mutations(), ["EnableAutoMerge", "DisableAutoMerge"]);
    assert!(
        !std::iter::from_fn(|| announcements.try_recv().ok())
            .any(|notification| notification.method == "autofix.event")
    );
}

/// A restart retries what was left on the list.
#[tokio::test]
async fn a_restart_retries_a_turn_off_that_was_left_on_the_list() {
    let world = World::new("").await;
    world.turn_on("squash").await;
    world.daemon.delete_session(&world.session, false).unwrap();
    world.fake.fail_next(500);
    world.poll().await;
    assert_eq!(world.pending().len(), 1);
    assert!(world.fake.pr(REPO, 7).auto_merge.is_some());

    let World {
        fake,
        daemon,
        _home,
        _folder,
        ..
    } = world;
    drop(daemon);
    tokio::time::sleep(Duration::from_millis(100)).await;

    let daemon = DaemonState::new(
        imperium_config::Paths::under(_home.path()),
        settings(""),
        None,
    );
    daemon.pr_monitor().attach(Arc::new(github(&fake)));
    assert_eq!(pending_in(daemon.data_dir()).len(), 1);
    daemon.pr_monitor().poll(&daemon, true).await;

    assert!(fake.pr(REPO, 7).auto_merge.is_none());
    assert!(pending_in(daemon.data_dir()).is_empty());
}

/// Auto-merge that was turned off some other way needs nothing sent: the entry is just dropped.
#[tokio::test]
async fn an_entry_whose_pull_request_is_already_off_is_dropped() {
    let world = World::new("").await;
    world.turn_on("squash").await;
    world.daemon.delete_session(&world.session, false).unwrap();
    assert_eq!(world.pending().len(), 1);
    github(&world.fake)
        .set_auto_merge(REPO, 7, &imperium_github::AutoMerge::Disable)
        .await
        .unwrap();

    world.poll().await;
    assert!(world.pending().is_empty());
    // Only the user's enable and the outside disable: nothing more was sent.
    assert_eq!(world.mutations(), ["EnableAutoMerge", "DisableAutoMerge"]);
}

/// After someone turns auto-merge off on GitHub the confirmed head is kept (a read that says it
/// is off may be stale), so when auto-merge is later turned on again on GitHub at a new head,
/// Imperium's guard turns it off. That fails safe; it is the one case of auto-merge Imperium
/// did not enable that it does not leave alone.
#[tokio::test]
async fn auto_merge_enabled_again_on_github_at_a_new_head_after_an_outside_disable_is_turned_off() {
    let world = World::new("").await;
    world.turn_on("squash").await;
    github(&world.fake)
        .set_auto_merge(REPO, 7, &imperium_github::AutoMerge::Disable)
        .await
        .unwrap();
    world.poll().await;
    assert_eq!(world.status().await["auto_merge"]["enabled"], false);

    world.fake.force_push(REPO, 7, "def456");
    let enable = imperium_github::AutoMerge::Enable {
        method: imperium_github::MergeMethod::Squash,
        expected_head_oid: "def456".into(),
    };
    github(&world.fake)
        .set_auto_merge(REPO, 7, &enable)
        .await
        .unwrap();
    world.poll().await;

    assert_eq!(
        world.mutations(),
        [
            "EnableAutoMerge",
            "DisableAutoMerge",
            "EnableAutoMerge",
            "DisableAutoMerge"
        ]
    );
    assert!(world.fake.pr(REPO, 7).auto_merge.is_none());
}
