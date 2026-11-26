//! A whole turn through `ratio::evaluate_with` with Imperium's host behind a recording
//! `TapeHost` (`Evaluation::with_tape`), stored as `.jsonl.zst`, then replayed from the stored
//! tape with no live tool call.

mod support;

use std::sync::Arc;
use std::sync::atomic::Ordering;

use imperium_ratio_host::tape::{TAPE_EXTENSION, read_tape, record_turn, replay_turn, write_tape};
use imperium_ratio_host::{HostConfig, RatioHost};
use ratio::frame::{Scope, Session};
use ratio::host::Host;
use ratio::names::{
    Loader, MemoryRegistry, PackageId, PackageName, QualifiedName, RelPath, Source,
};
use ratio::{Finished, Outcome, Prepared};

const PROGRAM: &str = r#"
fn main() -> Str {
  tools.read_file("notes.txt");
  "done"
}
"#;

const KEY: [u8; 32] = [7; 32];

fn prepared(source: &str) -> Arc<Prepared> {
    let package = PackageId::new(Source::User, PackageName::parse("app").expect("a name"));
    let mut registry = MemoryRegistry::new();
    let rel = RelPath::parse("main.ratio").expect("a path");
    registry.set_entry(&package, rel.clone());
    registry.add_file(&package, rel, source.as_bytes().to_vec());
    let builtin = MemoryRegistry::new();
    let loaded = Loader::new(&registry, &builtin)
        .load(&QualifiedName::parse("user:app").expect("a qualified name"));
    assert!(loaded.is_clean(), "loads: {:?}", loaded.diagnostics);
    let checked = ratio::check(&loaded);
    assert!(
        checked.is_clean(),
        "checks: {:#?}",
        checked
            .diagnostics
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
    );
    let program = ratio::lower(&loaded, &checked).expect("lowers");
    ratio::prepare(&program).expect("prepares")
}

fn shown(p: &Prepared, f: &Finished) -> String {
    match &f.outcome {
        Outcome::Returned(v) => ratio::interp::to_json_text(p, v),
        Outcome::Raised(e) => format!("raise {} {}", e.kind(p), e.message().unwrap_or_default()),
    }
}

fn host(runtime: &tokio::runtime::Runtime, tools: Arc<support::TestTools>) -> Arc<dyn Host> {
    let mut config = HostConfig::new(runtime.handle().clone(), "turn", Scope::session());
    config.providers = vec![tools as Arc<dyn imperium_api::ToolProvider>];
    RatioHost::new(config)
}

fn session() -> Session {
    Session {
        scope: Scope::session(),
        root: Arc::from("turn"),
    }
}

#[test]
fn a_turn_is_recorded_to_a_zstd_tape_and_replayed_without_live_calls() {
    let runtime = support::runtime();
    let p = prepared(PROGRAM);

    let live = Arc::new(support::TestTools::default());
    let entry = p.find("main.ratio", "main").expect("a main");
    let (finished, text) = record_turn(
        Arc::clone(&p),
        entry,
        Vec::new(),
        &session(),
        host(&runtime, Arc::clone(&live)),
        KEY,
    )
    .expect("records");
    let recorded = shown(&p, &finished);
    eprintln!(
        "recorded: {recorded}; live tool calls: {}",
        live.calls.load(Ordering::SeqCst)
    );
    eprintln!("tape ({} lines):\n{text}", text.lines().count());
    assert_eq!(recorded, "\"done\"");
    assert_eq!(live.calls.load(Ordering::SeqCst), 1, "the read ran live");

    let path = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("ratio-turn.{TAPE_EXTENSION}"));
    write_tape(&path, &text).expect("writes the tape");
    let stored = read_tape(&path).expect("reads the tape");
    assert_eq!(stored, text);

    let replayed_tools = Arc::new(support::TestTools::default());
    let entry = p.find("main.ratio", "main").expect("a main");
    let replayed = replay_turn(
        Arc::clone(&p),
        entry,
        Vec::new(),
        &session(),
        host(&runtime, Arc::clone(&replayed_tools)),
        &stored,
        KEY,
    )
    .expect("replays");
    let again = shown(&p, &replayed);
    eprintln!("replayed: {again}");
    assert_eq!(again, recorded);
    assert_eq!(
        replayed_tools.calls.load(Ordering::SeqCst),
        0,
        "a replay makes no live tool call"
    );
    let _ = std::fs::remove_file(&path);
}
