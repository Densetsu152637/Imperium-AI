//! Ratio's host contract suite (`ratio::host_tests`, feature `host-tests`) run against
//! Imperium's `RatioHost`, through the raw trait (no guard). Each case is its own test, so CI
//! lists them one by one, and `run_all` runs them together.

mod support;

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use imperium_api::ToolProvider;
use imperium_ratio_host::{Answer, HostConfig, ModelEntry, RatioHost};
use ratio::frame::{Card, FrameOrigin, Location, Scope};
use ratio::host::{Host, ModelRef, ToolClass};
use ratio::host_tests::{Calls, Fixture};
use ratio::{Labels, Value};

type Made = (Arc<dyn Host>, Arc<RatioHost>);

struct ImperiumFixture {
    runtime: tokio::runtime::Runtime,
    made: Mutex<Vec<Made>>,
}

impl ImperiumFixture {
    fn new() -> ImperiumFixture {
        ImperiumFixture {
            runtime: support::runtime(),
            made: Mutex::new(Vec::new()),
        }
    }

    fn concrete(&self, host: &Arc<dyn Host>) -> Arc<RatioHost> {
        self.made
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .find(|(d, _)| Arc::ptr_eq(d, host))
            .map(|(_, c)| Arc::clone(c))
            .expect("a host this fixture made")
    }
}

fn call(tool: &str, builtin: &str, args: Vec<(&str, Value)>) -> ratio::host::ToolInvocation {
    ratio::host::ToolInvocation {
        tool: Arc::from(tool),
        builtin: Arc::from(builtin),
        args: args.into_iter().map(|(n, v)| (Arc::from(n), v)).collect(),
        secret: false,
        in_defer: false,
    }
}

impl Fixture for ImperiumFixture {
    fn host(&self, scope: &Scope) -> Arc<dyn Host> {
        let mut config = HostConfig::new(self.runtime.handle().clone(), "turn", scope.clone());
        let tools: Arc<dyn ToolProvider> = Arc::new(support::TestTools::default());
        let mcp: Arc<dyn ToolProvider> = Arc::new(support::TestMcp);
        config.providers = vec![tools, mcp];
        config.classes.set("slow_read", ToolClass::abort(false));
        config.classes.set("slow_remote", ToolClass::abort(true));
        config
            .classes
            .set("never_drain", ToolClass::drain(Duration::from_millis(300)));
        config
            .classes
            .set("needs_approval", ToolClass::drain(Duration::from_secs(60)));
        config.models = vec![
            ModelEntry {
                name: "hosted".to_owned(),
                model_id: "hosted-model".to_owned(),
                client: Arc::new(support::Echo),
                location: Location::Cloud,
                card: Card::default(),
            },
            ModelEntry {
                name: "local".to_owned(),
                model_id: "local-model".to_owned(),
                client: Arc::new(support::Counted),
                location: Location::Local,
                card: Card::default(),
            },
        ];
        let concrete = RatioHost::new(config);
        let host: Arc<dyn Host> = concrete.clone();
        self.made
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((Arc::clone(&host), concrete));
        host
    }

    fn calls(&self) -> Calls {
        Calls {
            network: call(
                "web_fetch",
                "tools.web_fetch",
                vec![("url", Value::text("https://example.invalid"))],
            ),
            command: call(
                "run_command",
                "tools.run_command",
                vec![("command", Value::text("echo"))],
            ),
            write: call(
                "write_file",
                "tools.write_file",
                vec![
                    ("path", Value::text("/work/out.txt")),
                    ("content", Value::text("k")),
                ],
            ),
            mcp: call(
                "mcp__srv__t",
                "tools.mcp_call",
                vec![
                    ("server", Value::text("srv")),
                    ("tool", Value::text("t")),
                    ("arguments", Value::text("{}")),
                ],
            ),
            any_tool: call(
                "read_file",
                "tools.call_tool",
                vec![
                    ("name", Value::text("read_file")),
                    ("arguments", Value::text("{}")),
                ],
            ),
            slow_abort: call(
                "slow_read",
                "tools.call_tool",
                vec![("name", Value::text("slow_read"))],
            ),
            slow_remote: call(
                "slow_remote",
                "tools.call_tool",
                vec![("name", Value::text("slow_remote"))],
            ),
            never_drain: call(
                "never_drain",
                "tools.call_tool",
                vec![("name", Value::text("never_drain"))],
            ),
            needs_approval: call(
                "needs_approval",
                "tools.call_tool",
                vec![("name", Value::text("needs_approval"))],
            ),
        }
    }

    fn tool_table(&self, host: &Arc<dyn Host>) -> Vec<String> {
        self.concrete(host).tool_names()
    }

    fn answer_approval(&self, host: &Arc<dyn Host>, approve: bool, for_session: bool) -> bool {
        self.concrete(host).desk().respond_last(Answer {
            approve,
            for_session,
            text: None,
        })
    }

    fn remembered(&self, host: &Arc<dyn Host>) -> (bool, bool) {
        self.concrete(host).remembered("needs_approval")
    }

    fn stop_session(&self, host: &Arc<dyn Host>) {
        self.concrete(host).stop_session();
    }

    fn hosted_model(&self) -> ModelRef {
        ModelRef(Arc::from("hosted"))
    }

    fn loaded_harness(&self, host: &Arc<dyn Host>) -> FrameOrigin {
        let origin = FrameOrigin::Harness {
            package: Arc::from("app"),
            entry_hash: [0x11; 32],
            key: Arc::from("main.ratio::h"),
        };
        self.concrete(host)
            .load_packages([(Arc::from("app"), [0x11; 32])], [origin.clone()], []);
        origin
    }

    fn loaded_block(&self, host: &Arc<dyn Host>) -> ([u8; 32], u32) {
        let site = ([0x11; 32], 7);
        self.concrete(host).load_packages([], [], [site]);
        site
    }

    fn started_entry(&self, host: &Arc<dyn Host>) -> FrameOrigin {
        let origin = FrameOrigin::Entry {
            package: Arc::from("app"),
            entry_hash: [0x11; 32],
            key: Arc::from("main.ratio::turn_entry"),
        };
        self.concrete(host).start_entry(origin.clone());
        origin
    }
}

#[test]
fn contract_frames() {
    ratio::host_tests::frames(&ImperiumFixture::new());
}

#[test]
fn contract_sinks() {
    ratio::host_tests::sinks(&ImperiumFixture::new());
}

#[test]
fn contract_cancellation() {
    ratio::host_tests::cancellation(&ImperiumFixture::new());
}

#[test]
fn contract_run_all() {
    ratio::host_tests::run_all(&ImperiumFixture::new());
}

/// Not in the suite: the host's timer fires when polled from a thread with no tokio context.
#[test]
fn the_timer_fires_outside_a_runtime_context() {
    let f = ImperiumFixture::new();
    let host = f.host(&ratio::host_tests::narrow_scope());
    let at = std::time::Instant::now() + Duration::from_millis(50);
    ratio::block_on(host.timer(at));
    assert!(std::time::Instant::now() >= at);
}

/// Not in the suite: a labelled question is shown redacted, as `[secret: source N]`.
#[test]
fn a_secret_question_is_shown_redacted() {
    let f = ImperiumFixture::new();
    let mut scope = ratio::host_tests::narrow_scope();
    scope.attended = true;
    let host = f.host(&scope);
    let concrete = f.concrete(&host);
    let op = ratio::host::Operation {
        family: ratio::host::OpFamily::Session,
        builtin: Arc::from("session.ask"),
        args: vec![(
            Arc::from("question"),
            Value::text("is the key hunter2?").with_labels(Labels::source(3)),
        )],
        secret: true,
    };
    let fut = Arc::clone(&host).operate(root_frame(&scope), effect(), op);
    let asked = concrete.desk().list();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].summary, "[secret: source 3]");
    assert!(concrete.desk().respond_last(Answer {
        approve: true,
        for_session: false,
        text: Some("no".to_owned()),
    }));
    let v = ratio::block_on(fut).expect("an answer");
    assert_eq!(v.as_text(), Some("no"));
}

fn root_frame(scope: &Scope) -> ratio::frame::FrameCtx {
    let (ledgers, node) = ratio::frame::Ledgers::new(scope.budget, scope.width.max(1));
    ratio::frame::FrameCtx {
        path: Arc::from("turn"),
        parent: None,
        kind: ratio::frame::FrameKind::Entry,
        harness: Arc::from("turn"),
        scope: Arc::new(scope.clone()),
        ledgers,
        node,
        settings: Arc::new(ratio::frame::Settings::default()),
        origin: FrameOrigin::Session,
    }
}

fn effect() -> ratio::host::EffectCtx {
    ratio::host::EffectCtx {
        key: ratio::host::EffectKey {
            frame: Arc::from("turn"),
            branch: Arc::from(""),
            site: ([0; 32], 1),
            occurrence: 0,
        },
        cancel: ratio::CancelToken::new(),
        deadline: None,
    }
}

/// Not in the suite: `count_tokens` asks the model server's own tokenizer (asynchronously), and
/// never sends a secret to a hosted model's.
#[test]
fn tokens_are_counted_by_the_model_servers_tokenizer() {
    use ratio::host::{Countable, Labelled};
    let f = ImperiumFixture::new();
    let scope = ratio::host_tests::narrow_scope();
    let host = f.host(&scope);
    let mut frame = root_frame(&scope);
    frame.settings = Arc::new(ratio::frame::Settings {
        model: Some(Value::text("local")),
        ..ratio::frame::Settings::default()
    });
    let exact = ratio::block_on(Arc::clone(&host).count_tokens(
        frame,
        Countable::Text(Labelled::plain("twelve chars".to_owned())),
    ));
    assert_eq!(exact.expect("a count"), support::COUNTED);
    let mut hosted = root_frame(&scope);
    hosted.settings = Arc::new(ratio::frame::Settings {
        model: Some(Value::text("hosted")),
        ..ratio::frame::Settings::default()
    });
    let estimated = ratio::block_on(Arc::clone(&host).count_tokens(
        hosted,
        Countable::Text(Labelled::new("twelve chars".to_owned(), Labels::source(0))),
    ));
    assert_eq!(estimated.expect("a count"), 3);
}
