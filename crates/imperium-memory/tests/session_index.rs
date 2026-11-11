//! Session search: incremental indexing of journals, keyword ranking and snippets, environment
//! isolation, archived sessions, rebuilding a damaged index, and hybrid ranking with a fake
//! embedder (no network, no GPU).

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use imperium_memory::retrieval::Embedder;
use imperium_memory::sessions::{ChunkKind, SearchQuery, SearchScope, SessionIndex, SessionSource};
use imperium_protocol::session::{AgentEvent, SessionEvent, TokenUsage};

fn event(agent: &str, event: SessionEvent) -> AgentEvent {
    AgentEvent {
        agent_id: agent.into(),
        event,
    }
}

fn user(text: &str) -> AgentEvent {
    event("root", SessionEvent::TurnStarted { text: text.into() })
}

fn delta(text: &str) -> AgentEvent {
    event("root", SessionEvent::TextDelta { text: text.into() })
}

fn tool(summary: &str) -> AgentEvent {
    event(
        "root",
        SessionEvent::ToolStarted {
            call_id: "c1".into(),
            tool: "edit_file".into(),
            summary: summary.into(),
            arguments: None,
        },
    )
}

fn done() -> AgentEvent {
    event(
        "root",
        SessionEvent::TurnCompleted {
            usage: TokenUsage::default(),
            cost_usd: None,
            steps: 1,
        },
    )
}

/// Appends events to a journal the way the engine writes them.
fn append(journal: &Path, events: &[AgentEvent]) {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(journal)
        .unwrap();
    for (n, event) in events.iter().enumerate() {
        let mut value = serde_json::to_value(event).unwrap();
        value["at_ms"] = serde_json::json!(1_700_000_000_000u64 + n as u64);
        writeln!(file, "{value}").unwrap();
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    sessions: PathBuf,
    index_dir: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let sessions = dir.path().join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        Fixture {
            index_dir: dir.path().join("index").join("sessions"),
            sessions,
            _dir: dir,
        }
    }

    fn source(&self, id: &str, title: &str, env: Option<&str>, archived: bool) -> SessionSource {
        SessionSource {
            id: id.into(),
            journal: self.journal(id),
            title: title.into(),
            environment_id: env.map(str::to_string),
            project_dir: "/work".into(),
            archived,
        }
    }

    fn journal(&self, id: &str) -> PathBuf {
        self.sessions.join(format!("{id}.jsonl"))
    }

    fn index(&self, sources: &[SessionSource]) -> SessionIndex {
        let index = SessionIndex::open(&self.index_dir);
        index.sync(sources);
        for source in sources {
            while index.update(&source.id, 1 << 20).unwrap().more {}
        }
        index
    }
}

fn query(text: &str) -> SearchQuery<'_> {
    SearchQuery {
        text,
        scope: SearchScope::All,
        archived: None,
        limit: 10,
        exclude: None,
    }
}

#[test]
fn indexing_is_incremental_and_event_indexes_match_the_history() {
    let fx = Fixture::new();
    let source = fx.source("s1", "first", None, false);
    append(
        &fx.journal("s1"),
        &[
            user("fix the websocket reconnect"),
            delta("Looking at "),
            delta("the client."),
        ],
    );
    let index = fx.index(std::slice::from_ref(&source));
    assert_eq!(index.counts().0, 1, "the reply is still open");

    // The reply in progress is searchable before it is closed.
    let hits = index.search(&query("client"), None);
    assert_eq!(hits[0].kind, ChunkKind::Assistant);
    assert_eq!(hits[0].event, 1);

    // Only new lines are read on the next pass; the reply continues across passes.
    append(
        &fx.journal("s1"),
        &[
            delta(" Found it."),
            tool("crates/imperium-core/src/server.rs"),
            done(),
        ],
    );
    let progress = index.update("s1", 1 << 20).unwrap();
    assert!(progress.bytes > 0 && !progress.more);
    assert_eq!(index.update("s1", 1 << 20).unwrap().bytes, 0);
    assert_eq!(index.counts().0, 3);
    let hits = index.search(&query("found client"), None);
    assert_eq!(hits[0].event, 1, "the reply starts at its first delta");
    let hits = index.search(&query("server.rs"), None);
    assert_eq!(hits[0].kind, ChunkKind::Tool);
    assert_eq!(hits[0].event, 4);

    // A small budget reads a line at a time without losing or repeating anything.
    let fx2 = Fixture::new();
    let source2 = fx2.source("s2", "", None, false);
    append(
        &fx2.journal("s2"),
        &[user("alpha"), done(), user("beta"), done()],
    );
    let index2 = SessionIndex::open(&fx2.index_dir);
    index2.sync(std::slice::from_ref(&source2));
    let mut passes = 0;
    while index2.update("s2", 10).unwrap().more {
        passes += 1;
    }
    assert!(passes >= 3);
    assert_eq!(index2.counts().0, 2);
    assert_eq!(index2.search(&query("beta"), None)[0].event, 2);

    // Reopened from disk, it carries on where it stopped.
    drop(index);
    let reopened = SessionIndex::open(&fx.index_dir);
    reopened.sync(&[source]);
    assert_eq!(reopened.update("s1", 1 << 20).unwrap().bytes, 0);
    assert_eq!(reopened.counts().0, 3);
}

#[test]
fn keyword_search_ranks_by_relevance_and_highlights_matches() {
    let fx = Fixture::new();
    let sources = [
        fx.source("a", "Fix flaky websocket test", Some("env"), false),
        fx.source("b", "Tidy the docs", Some("env"), false),
        fx.source("c", "Release notes", Some("env"), false),
    ];
    append(
        &fx.journal("a"),
        &[
            user("the websocket test drops the websocket connection on reconnect"),
            done(),
        ],
    );
    append(
        &fx.journal("b"),
        &[
            user("rewrite the README"),
            delta("I mentioned websocket once."),
            done(),
        ],
    );
    append(&fx.journal("c"), &[user("write release notes"), done()]);
    let index = fx.index(&sources);

    let hits = index.search(&query("websocket reconnect"), None);
    let ids: Vec<&str> = hits.iter().map(|hit| hit.session_id.as_str()).collect();
    assert_eq!(ids, ["a", "b"]);
    assert!(hits[0].score > hits[1].score);
    assert_eq!(hits[0].kind, ChunkKind::User);
    assert_eq!(hits[0].title, "Fix flaky websocket test");
    assert!(hits[0].more >= 1, "the title also matched");
    let units: Vec<u16> = hits[0].snippet.text.encode_utf16().collect();
    let marked: Vec<String> = hits[0]
        .snippet
        .highlights
        .iter()
        .map(|(start, end)| String::from_utf16(&units[*start as usize..*end as usize]).unwrap())
        .collect();
    assert_eq!(marked, ["websocket", "websocket", "reconnect"]);

    // A title alone is a match.
    let hits = index.search(&query("tidy"), None);
    assert_eq!(hits[0].session_id, "b");
    assert_eq!(hits[0].kind, ChunkKind::Title);

    assert!(index.search(&query("nothing-like-this"), None).is_empty());
    let one = SearchQuery {
        limit: 1,
        ..query("websocket")
    };
    assert_eq!(index.search(&one, None).len(), 1);
}

#[test]
fn searches_stay_inside_their_environment_or_folder() {
    let fx = Fixture::new();
    let mut outside = fx.source("free", "", None, false);
    outside.project_dir = "/elsewhere".into();
    let sources = [
        fx.source("mine", "", Some("one"), false),
        fx.source("theirs", "", Some("two"), false),
        fx.source("bare", "", None, false),
        outside,
    ];
    for source in &sources {
        append(
            &source.journal,
            &[user("deploy the secret project"), done()],
        );
    }
    let index = fx.index(&sources);
    let ids = |scope: SearchScope, exclude: Option<&str>| -> Vec<String> {
        let mut ids: Vec<String> = index
            .search(
                &SearchQuery {
                    scope,
                    exclude,
                    ..query("deploy")
                },
                None,
            )
            .into_iter()
            .map(|hit| hit.session_id)
            .collect();
        ids.sort();
        ids
    };
    assert_eq!(ids(SearchScope::Environment("one".into()), None), ["mine"]);
    assert_eq!(
        ids(SearchScope::Environment("two".into()), None),
        ["theirs"]
    );
    assert_eq!(ids(SearchScope::Folder("/work".into()), None), ["bare"]);
    assert!(ids(SearchScope::Environment("one".into()), Some("mine")).is_empty());
    assert_eq!(ids(SearchScope::All, None).len(), 4);

    // A session moved to another environment is found there on the next sync.
    let mut moved = sources.clone();
    moved[1].environment_id = Some("one".into());
    index.sync(&moved);
    assert_eq!(
        ids(SearchScope::Environment("one".into()), None),
        ["mine", "theirs"]
    );
}

#[test]
fn archived_sessions_are_included_and_can_be_filtered() {
    let fx = Fixture::new();
    let sources = [
        fx.source("live", "", None, false),
        fx.source("old", "", None, true),
    ];
    for source in &sources {
        append(
            &source.journal,
            &[user("migrate the database schema"), done()],
        );
    }
    let index = fx.index(&sources);
    let with = |archived: Option<bool>| -> Vec<String> {
        let mut ids: Vec<String> = index
            .search(
                &SearchQuery {
                    archived,
                    ..query("schema")
                },
                None,
            )
            .into_iter()
            .map(|hit| hit.session_id)
            .collect();
        ids.sort();
        ids
    };
    assert_eq!(with(None), ["live", "old"]);
    assert_eq!(with(Some(true)), ["old"]);
    assert_eq!(with(Some(false)), ["live"]);
    let hit = index.search(&query("schema"), None);
    assert!(
        hit.iter()
            .any(|hit| hit.session_id == "old" && hit.archived)
    );

    // A deleted session leaves the index, files and all.
    index.sync(&sources[..1]);
    assert_eq!(with(None), ["live"]);
    assert!(!fx.index_dir.join("old.state.json").exists());
}

#[test]
fn a_damaged_or_missing_index_is_rebuilt_from_the_journal() {
    let fx = Fixture::new();
    let source = fx.source("s", "", None, false);
    append(
        &fx.journal("s"),
        &[
            user("profile the renderer"),
            done(),
            user("cache the glyphs"),
            done(),
        ],
    );
    drop(fx.index(std::slice::from_ref(&source)));

    // Garbage in the chunk file: the session is indexed again.
    std::fs::write(fx.index_dir.join("s.chunks.jsonl"), "not json\n").unwrap();
    let index = fx.index(std::slice::from_ref(&source));
    assert_eq!(index.search(&query("glyphs"), None)[0].event, 2);
    assert_eq!(index.counts().0, 2);
    drop(index);

    // A state file that does not parse.
    std::fs::write(fx.index_dir.join("s.state.json"), "{").unwrap();
    let index = fx.index(std::slice::from_ref(&source));
    assert_eq!(index.counts().0, 2);
    drop(index);

    // The whole index folder gone.
    std::fs::remove_dir_all(&fx.index_dir).unwrap();
    let index = fx.index(std::slice::from_ref(&source));
    assert_eq!(index.search(&query("renderer"), None)[0].event, 0);

    // A journal that was replaced by a shorter one is read again from the start.
    std::fs::remove_file(fx.journal("s")).unwrap();
    append(&fx.journal("s"), &[user("brand new")]);
    index.update("s", 1 << 20).unwrap();
    assert!(index.search(&query("glyphs"), None).is_empty());
    assert_eq!(index.search(&query("brand"), None)[0].event, 0);
}

/// Words that mean the same thing share a dimension, so similarity reflects meaning.
struct FakeEmbedder {
    model: &'static str,
    calls: AtomicUsize,
}

const CONCEPTS: [&[&str]; 4] = [
    &["car", "automobile", "vehicle"],
    &["bug", "defect", "crash"],
    &["database", "schema", "sql"],
    &["login", "authentication", "password"],
];

impl Embedder for FakeEmbedder {
    fn model(&self) -> &str {
        self.model
    }

    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(texts
            .iter()
            .map(|text| {
                let lower = text.to_lowercase();
                CONCEPTS
                    .iter()
                    .map(|words| words.iter().filter(|word| lower.contains(**word)).count() as f32)
                    .collect()
            })
            .collect())
    }
}

#[test]
fn vectors_add_matches_by_meaning_and_follow_the_model() {
    let fx = Fixture::new();
    let sources = [
        fx.source("auth", "", None, false),
        fx.source("cars", "", None, false),
    ];
    append(
        &fx.journal("auth"),
        &[
            user("the authentication flow rejects a valid password"),
            done(),
        ],
    );
    append(
        &fx.journal("cars"),
        &[
            user("list every automobile in the fleet"),
            tool("read fleet.csv"),
            done(),
        ],
    );
    let index = fx.index(&sources);
    let embedder = FakeEmbedder {
        model: "fake-1",
        calls: AtomicUsize::new(0),
    };
    while index.embed_pending(&embedder, 1).unwrap() > 0 {}
    assert_eq!(index.counts(), (3, 2), "tool calls are not embedded");
    assert_eq!(index.embed_pending(&embedder, 8).unwrap(), 0);

    // "login" appears nowhere, but means what the auth session talks about.
    let vector = embedder.embed(&["login".to_string()]).unwrap().remove(0);
    let hits = index.search(&query("login"), Some(&vector));
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].session_id, "auth");
    assert!(hits[0].semantic);
    assert!(hits[0].snippet.highlights.is_empty());
    // Keyword-only search finds nothing.
    assert!(index.search(&query("login"), None).is_empty());

    // Hybrid: a keyword match and a meaning match both rank.
    let vector = embedder.embed(&["vehicle".to_string()]).unwrap().remove(0);
    let hits = index.search(&query("fleet vehicle"), Some(&vector));
    assert_eq!(hits[0].session_id, "cars");
    assert!(!hits[0].semantic);

    // Vectors survive a reopen, and a different model replaces them.
    drop(index);
    let index = SessionIndex::open(&fx.index_dir);
    index.sync(&sources);
    assert_eq!(index.counts(), (3, 2));
    let other = FakeEmbedder {
        model: "fake-2",
        calls: AtomicUsize::new(0),
    };
    while index.embed_pending(&other, 8).unwrap() > 0 {}
    assert_eq!(index.counts(), (3, 2));
    assert!(other.calls.load(Ordering::SeqCst) >= 1);

    // Damaged vectors are dropped, not the chunks.
    drop(index);
    std::fs::write(fx.index_dir.join("auth.vec"), [1u8, 2, 3]).unwrap();
    let index = SessionIndex::open(&fx.index_dir);
    index.sync(&sources);
    assert_eq!(index.counts(), (3, 1));
}
