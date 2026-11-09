//! Session search: an index of what was said and done in every session (running and archived),
//! built from the session journals, for the desktop's search box and the agents'
//! `search_sessions` tool.
//!
//! Each journal (`<data>/sessions/<id>.jsonl`) is read incrementally from where the last pass
//! stopped and split into chunks: the user's messages, the root agent's replies (split every
//! couple of thousand characters), tool calls with their summaries (which name the files they
//! touch), subagent briefs, and subagent reports. Every chunk keeps the index of the journal
//! event it starts at, so a hit opens the session at that point. Search is BM25 over the chunks'
//! words plus the session title, fused by reciprocal rank with vector similarity when an
//! [`Embedder`] has embedded the chunks; without one it is keyword-only.
//!
//! The index lives in one folder (`<data>/index/sessions/`), three files per session:
//! `<id>.state.json` (where reading stopped), `<id>.chunks.jsonl` (append-only chunks), and
//! `<id>.vec` (append-only vectors, when embedding). A session whose files are missing,
//! unreadable, or from another index version is indexed again from its journal.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use imperium_protocol::session::{AgentEvent, ROOT_AGENT, SessionEvent};
use serde::{Deserialize, Serialize};

use crate::retrieval::{self, Embedder, Snippet};

/// Bumped when the chunking changes, so older indexes are rebuilt.
const VERSION: u32 = 1;
/// A reply is split into chunks of about this many bytes.
const MAX_CHUNK: usize = 2000;
/// Tool summaries are cut to this many bytes.
const MAX_TOOL: usize = 400;
/// Shortest word indexed (two letters keeps `ci`, `ui`, `db`).
const MIN_WORD: usize = 2;
const SNIPPET_WIDTH: usize = 160;
/// Weakest similarity that counts as a semantic match.
const MIN_SIMILARITY: f32 = 0.3;
/// Semantic matches considered per search.
const SEMANTIC_CANDIDATES: usize = 50;
/// BM25 parameters.
const K1: f64 = 1.2;
const B: f64 = 0.75;
/// A title match weighs as much as this many matching chunks.
const TITLE_WEIGHT: f64 = 1.5;
/// The pseudo chunk index of a title match.
const TITLE: u32 = u32::MAX;

/// What a chunk holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChunkKind {
    /// A user message (or a steering message).
    User,
    /// Part of the root agent's reply.
    Assistant,
    /// A tool call: its tool and summary, which name the files and commands it used.
    Tool,
    /// A subagent's brief, or a follow-up given to one.
    Task,
    /// A subagent's report.
    Report,
    /// The session's title (search hits only; never stored).
    Title,
}

impl ChunkKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ChunkKind::User => "user",
            ChunkKind::Assistant => "assistant",
            ChunkKind::Tool => "tool",
            ChunkKind::Task => "task",
            ChunkKind::Report => "report",
            ChunkKind::Title => "title",
        }
    }

    fn weight(self) -> f64 {
        match self {
            ChunkKind::User => 1.2,
            ChunkKind::Tool => 0.8,
            _ => 1.0,
        }
    }

    /// Tool calls are many and short; they are found by keyword only.
    fn embedded(self) -> bool {
        !matches!(self, ChunkKind::Tool | ChunkKind::Title)
    }
}

/// A searchable piece of a session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chunk {
    pub kind: ChunkKind,
    /// The index of the journal event it starts at, as `session.subscribe` history counts.
    pub event: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at_ms: Option<u64>,
    /// The agent it came from, when not the root.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub agent: String,
    pub text: String,
}

/// A session the daemon knows, as the index needs it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionSource {
    pub id: String,
    pub journal: PathBuf,
    pub title: String,
    pub environment_id: Option<String>,
    /// The session's working directory, which scopes sessions outside any environment.
    pub project_dir: String,
    pub archived: bool,
}

/// Which sessions a search covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchScope {
    All,
    /// Sessions in this environment.
    Environment(String),
    /// Sessions outside any environment that work in this folder.
    Folder(String),
}

#[derive(Debug, Clone)]
pub struct SearchQuery<'a> {
    pub text: &'a str,
    pub scope: SearchScope,
    /// `Some(true)`: archived sessions only; `Some(false)`: running ones only; `None`: both.
    pub archived: Option<bool>,
    pub limit: usize,
    /// A session to leave out (the one searching).
    pub exclude: Option<&'a str>,
}

/// The best match in one session.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchHit {
    pub session_id: String,
    pub title: String,
    pub environment_id: Option<String>,
    pub archived: bool,
    pub kind: ChunkKind,
    /// The journal event to open the session at.
    pub event: u32,
    pub at_ms: Option<u64>,
    pub snippet: Snippet,
    pub score: f64,
    /// Other matching chunks in the same session.
    pub more: u32,
    /// Found by meaning only, with none of the query's words.
    pub semantic: bool,
}

/// What an indexing pass did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Progress {
    /// Journal bytes read.
    pub bytes: u64,
    /// Chunks added.
    pub chunks: usize,
    /// More of the journal is left to read.
    pub more: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct State {
    version: u32,
    /// Journal bytes read (always at a line end).
    offset: u64,
    /// Journal events read, which numbers the next one.
    events: u32,
    /// Closed chunks in the chunks file.
    chunks: usize,
    /// The reply still being written, when the journal ends in one.
    open: Option<Chunk>,
    #[serde(default)]
    vector_model: String,
    #[serde(default)]
    dims: usize,
    #[serde(default)]
    vectors: usize,
    /// Chunks before this one have been embedded (or skipped as not embedded).
    #[serde(default)]
    embedded_upto: usize,
}

#[derive(Debug, Default)]
struct Indexed {
    source: SessionSource,
    /// Among the sessions the daemon listed last; unlisted ones are never returned.
    listed: bool,
    offset: u64,
    events: u32,
    chunks: Vec<Chunk>,
    open: Option<Chunk>,
    /// Word → (chunk, occurrences), for closed chunks.
    postings: HashMap<String, Vec<(u32, u32)>>,
    /// Words per closed chunk.
    lengths: Vec<u32>,
    vector_model: String,
    dims: usize,
    vectors: Vec<(u32, Vec<f32>)>,
    embedded_upto: usize,
}

impl Indexed {
    fn add_chunk(&mut self, chunk: Chunk) {
        let index = u32::try_from(self.chunks.len()).unwrap_or(u32::MAX);
        let words = retrieval::words(&chunk.text, MIN_WORD);
        self.lengths
            .push(u32::try_from(words.len()).unwrap_or(u32::MAX));
        let mut counts: HashMap<String, u32> = HashMap::new();
        for word in words {
            *counts.entry(word).or_default() += 1;
        }
        for (word, count) in counts {
            self.postings.entry(word).or_default().push((index, count));
        }
        self.chunks.push(chunk);
    }

    fn state(&self) -> State {
        State {
            version: VERSION,
            offset: self.offset,
            events: self.events,
            chunks: self.chunks.len(),
            open: self.open.clone(),
            vector_model: self.vector_model.clone(),
            dims: self.dims,
            vectors: self.vectors.len(),
            embedded_upto: self.embedded_upto,
        }
    }

    fn clear_vectors(&mut self) {
        self.vector_model.clear();
        self.dims = 0;
        self.vectors.clear();
        self.embedded_upto = 0;
    }

    fn in_scope(&self, query: &SearchQuery) -> bool {
        let source = &self.source;
        self.listed
            && query.exclude != Some(source.id.as_str())
            && query
                .archived
                .is_none_or(|archived| archived == source.archived)
            && match &query.scope {
                SearchScope::All => true,
                SearchScope::Environment(id) => source.environment_id.as_deref() == Some(id),
                SearchScope::Folder(dir) => {
                    source.environment_id.is_none() && source.project_dir == *dir
                }
            }
    }

    /// The chunk at `index`, the open reply being one past the closed chunks.
    fn chunk(&self, index: u32) -> Option<&Chunk> {
        let index = index as usize;
        self.chunks.get(index).or_else(|| {
            (index == self.chunks.len())
                .then_some(self.open.as_ref())
                .flatten()
        })
    }
}

/// The session index; shared by the daemon's indexing pass, `session.search`, and the
/// `search_sessions` tool.
#[derive(Debug)]
pub struct SessionIndex {
    dir: PathBuf,
    sessions: Mutex<HashMap<String, Indexed>>,
}

/// Session ids name files, so only plain ones are indexed.
fn plain_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// `text` cut to at most `max` bytes, on a character boundary.
fn cut(mut text: String, max: usize) -> String {
    if text.len() > max {
        let mut end = max;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    text
}

/// Turns journal events into chunks, carrying the reply in progress between passes.
struct Chunker {
    open: Option<Chunk>,
    out: Vec<Chunk>,
}

impl Chunker {
    fn take(&mut self, index: u32, at_ms: Option<u64>, event: AgentEvent) {
        let root = event.agent_id == ROOT_AGENT;
        if let SessionEvent::TextDelta { text } = &event.event {
            // Subagents' text is summed up by their reports.
            if !root {
                return;
            }
            match &mut self.open {
                Some(open) if open.text.len() < MAX_CHUNK => open.text.push_str(text),
                _ => {
                    self.out.extend(self.open.take());
                    self.open = Some(Chunk {
                        kind: ChunkKind::Assistant,
                        event: index,
                        at_ms,
                        agent: String::new(),
                        text: text.clone(),
                    });
                }
            }
            return;
        }
        if root {
            self.out.extend(self.open.take());
        }
        let (kind, text) = match event.event {
            SessionEvent::TurnStarted { text } if root => (ChunkKind::User, text),
            SessionEvent::Steered { text, .. } if root => (ChunkKind::User, text),
            SessionEvent::ToolStarted { tool, summary, .. } => {
                (ChunkKind::Tool, cut(format!("{tool} {summary}"), MAX_TOOL))
            }
            SessionEvent::TaskSpawned { brief, .. } => (ChunkKind::Task, brief),
            SessionEvent::TaskResumed { text, .. } => (ChunkKind::Task, text),
            SessionEvent::TaskFinished { report, .. } => (ChunkKind::Report, report),
            _ => return,
        };
        if text.trim().is_empty() {
            return;
        }
        self.out.push(Chunk {
            kind,
            event: index,
            at_ms,
            agent: if root { String::new() } else { event.agent_id },
            text: cut(text, MAX_CHUNK),
        });
    }
}

/// Parses journal lines the way the engine reads its history, so event numbers agree: a line
/// that is not an event is skipped and not counted.
fn parse_line(line: &str) -> Option<(Option<u64>, AgentEvent)> {
    #[derive(Deserialize)]
    struct Stored {
        #[serde(flatten)]
        event: AgentEvent,
    }
    #[derive(Deserialize)]
    struct At {
        at_ms: Option<u64>,
    }
    let stored: Stored = serde_json::from_str(line).ok()?;
    let at = serde_json::from_str::<At>(line)
        .ok()
        .and_then(|at| at.at_ms);
    Some((at, stored.event))
}

impl SessionIndex {
    /// Opens the index in `dir`, loading what earlier passes saved; damaged sessions are
    /// dropped (and indexed again on the next pass).
    pub fn open(dir: &Path) -> Self {
        let index = SessionIndex {
            dir: dir.to_path_buf(),
            sessions: Mutex::new(HashMap::new()),
        };
        if let Err(error) = fs::create_dir_all(dir) {
            tracing::warn!(dir = %dir.display(), %error, "could not create the session index");
        }
        let mut loaded = HashMap::new();
        for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(id) = name.strip_suffix(".state.json") else {
                continue;
            };
            if !plain_id(id) {
                continue;
            }
            match index.load(id) {
                Some(indexed) => {
                    loaded.insert(id.to_string(), indexed);
                }
                None => {
                    tracing::info!(session = id, "session index damaged; indexing it again");
                    index.remove_files(id);
                }
            }
        }
        *index.lock() = loaded;
        index
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<String, Indexed>> {
        self.sessions.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn path(&self, id: &str, suffix: &str) -> PathBuf {
        self.dir.join(format!("{id}.{suffix}"))
    }

    fn remove_files(&self, id: &str) {
        for suffix in ["state.json", "chunks.jsonl", "vec"] {
            let _ = fs::remove_file(self.path(id, suffix));
        }
    }

    fn load(&self, id: &str) -> Option<Indexed> {
        let state: State =
            serde_json::from_str(&fs::read_to_string(self.path(id, "state.json")).ok()?).ok()?;
        if state.version != VERSION {
            return None;
        }
        let text = match fs::read_to_string(self.path(id, "chunks.jsonl")) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound && state.chunks == 0 => {
                String::new()
            }
            Err(_) => return None,
        };
        let mut indexed = Indexed {
            offset: state.offset,
            events: state.events,
            open: state.open,
            ..Indexed::default()
        };
        for line in text.lines() {
            indexed.add_chunk(serde_json::from_str(line).ok()?);
        }
        if indexed.chunks.len() != state.chunks {
            return None;
        }
        // Vectors are only an addition: when they cannot be read they are made again.
        if state.vectors > 0 {
            match read_vectors(&self.path(id, "vec"), state.dims, state.vectors) {
                Some(vectors) => {
                    indexed.vector_model = state.vector_model;
                    indexed.dims = state.dims;
                    indexed.vectors = vectors;
                    indexed.embedded_upto = state.embedded_upto.min(indexed.chunks.len());
                }
                None => {
                    let _ = fs::remove_file(self.path(id, "vec"));
                }
            }
        } else {
            indexed.vector_model = state.vector_model;
            indexed.dims = state.dims;
            indexed.embedded_upto = state.embedded_upto.min(indexed.chunks.len());
        }
        Some(indexed)
    }

    fn save_state(&self, id: &str, indexed: &Indexed) -> io::Result<()> {
        let path = self.path(id, "state.json");
        let temporary = self.path(id, "state.json.tmp");
        fs::write(&temporary, serde_json::to_vec(&indexed.state())?)?;
        fs::rename(&temporary, &path)
    }

    /// Records the sessions the daemon knows (their titles, environments, and whether they are
    /// archived) and forgets the index of any it no longer knows (deleted sessions).
    pub fn sync(&self, sources: &[SessionSource]) {
        let known: HashSet<&str> = sources.iter().map(|source| source.id.as_str()).collect();
        let mut sessions = self.lock();
        let gone: Vec<String> = sessions
            .keys()
            .filter(|id| !known.contains(id.as_str()))
            .cloned()
            .collect();
        for id in gone {
            sessions.remove(&id);
            self.remove_files(&id);
        }
        for source in sources.iter().filter(|source| plain_id(&source.id)) {
            let indexed = sessions.entry(source.id.clone()).or_default();
            indexed.source = source.clone();
            indexed.listed = true;
        }
    }

    /// Ids of the sessions known to the index.
    pub fn session_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.lock().keys().cloned().collect();
        ids.sort();
        ids
    }

    /// Reads up to `max_bytes` more of a session's journal into the index (a single longer line
    /// is read whole). Call [`SessionIndex::sync`] first so the session is known.
    pub fn update(&self, id: &str, max_bytes: u64) -> io::Result<Progress> {
        let (journal, mut offset, mut events, mut open) = {
            let sessions = self.lock();
            let Some(indexed) = sessions.get(id) else {
                return Ok(Progress::default());
            };
            (
                indexed.source.journal.clone(),
                indexed.offset,
                indexed.events,
                indexed.open.clone(),
            )
        };
        let len = match fs::metadata(&journal) {
            Ok(meta) => meta.len(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(Progress::default());
            }
            Err(error) => return Err(error),
        };
        let restart = len < offset;
        if restart {
            // The journal was replaced: start over.
            (offset, events, open) = (0, 0, None);
        }
        if len == offset {
            return Ok(Progress::default());
        }
        let mut file = File::open(&journal)?;
        file.seek(SeekFrom::Start(offset))?;
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take(max_bytes.max(1))
            .read_to_end(&mut bytes)?;
        let mut end = bytes.iter().rposition(|b| *b == b'\n').map(|at| at + 1);
        if end.is_none() && offset + (bytes.len() as u64) < len {
            // One line longer than the budget: read just that line.
            file.read_to_end(&mut bytes)?;
            end = bytes.iter().position(|b| *b == b'\n').map(|at| at + 1);
        }
        let Some(end) = end else {
            // Only a line still being written.
            return Ok(Progress::default());
        };
        bytes.truncate(end);
        let mut chunker = Chunker {
            open,
            out: Vec::new(),
        };
        for line in String::from_utf8_lossy(&bytes).lines() {
            if let Some((at_ms, event)) = parse_line(line) {
                chunker.take(events, at_ms, event);
                events = events.saturating_add(1);
            }
        }
        let read = bytes.len() as u64;
        let new_offset = offset + read;

        let mut sessions = self.lock();
        let Some(indexed) = sessions.get_mut(id) else {
            return Ok(Progress::default());
        };
        if restart {
            self.remove_files(id);
            let source = std::mem::take(&mut indexed.source);
            *indexed = Indexed {
                source,
                listed: true,
                ..Indexed::default()
            };
        } else if indexed.offset != offset {
            // Another pass got here first.
            return Ok(Progress::default());
        }
        let added = chunker.out.len();
        if added > 0 {
            let mut lines = Vec::new();
            for chunk in &chunker.out {
                serde_json::to_writer(&mut lines, chunk)?;
                lines.push(b'\n');
            }
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.path(id, "chunks.jsonl"))?
                .write_all(&lines)?;
        }
        for chunk in chunker.out {
            indexed.add_chunk(chunk);
        }
        indexed.open = chunker.open;
        indexed.offset = new_offset;
        indexed.events = events;
        self.save_state(id, indexed)?;
        Ok(Progress {
            bytes: read,
            chunks: added,
            more: new_offset < len,
        })
    }

    /// Embeds up to `batch` chunks not embedded yet (with `embedder`'s model; vectors from
    /// another model are dropped first) and returns how many it embedded. The embedder runs
    /// without holding the index.
    pub fn embed_pending(&self, embedder: &dyn Embedder, batch: usize) -> Result<usize, String> {
        let model = embedder.model().to_string();
        let mut items: Vec<(String, u32, String)> = Vec::new();
        let mut upto: Vec<(String, usize)> = Vec::new();
        {
            let mut sessions = self.lock();
            let mut ids: Vec<String> = sessions.keys().cloned().collect();
            ids.sort();
            for id in ids {
                let Some(indexed) = sessions.get_mut(&id) else {
                    continue;
                };
                if !indexed.listed {
                    continue;
                }
                if indexed.vector_model != model {
                    indexed.clear_vectors();
                    indexed.vector_model = model.clone();
                    let _ = fs::remove_file(self.path(&id, "vec"));
                }
                let mut scanned = indexed.embedded_upto;
                while scanned < indexed.chunks.len() && items.len() < batch {
                    let chunk = &indexed.chunks[scanned];
                    if chunk.kind.embedded() {
                        items.push((
                            id.clone(),
                            u32::try_from(scanned).unwrap_or(u32::MAX),
                            chunk.text.clone(),
                        ));
                    }
                    scanned += 1;
                }
                if scanned > indexed.embedded_upto {
                    upto.push((id.clone(), scanned));
                }
                if items.len() >= batch {
                    break;
                }
            }
        }
        if upto.is_empty() {
            return Ok(0);
        }
        let texts: Vec<String> = items.iter().map(|(_, _, text)| text.clone()).collect();
        let vectors = if texts.is_empty() {
            Vec::new()
        } else {
            embedder.embed(&texts)?
        };
        if vectors.len() != texts.len() {
            return Err(format!(
                "the embedding model returned {} vectors for {} texts",
                vectors.len(),
                texts.len()
            ));
        }
        let mut by_session: HashMap<String, Vec<(u32, Vec<f32>)>> = HashMap::new();
        for ((id, chunk, _), vector) in items.into_iter().zip(vectors) {
            by_session.entry(id).or_default().push((chunk, vector));
        }
        let mut sessions = self.lock();
        let mut embedded = 0;
        for (id, scanned) in upto {
            let Some(indexed) = sessions.get_mut(&id) else {
                continue;
            };
            if indexed.vector_model != model {
                continue;
            }
            let fresh = by_session.remove(&id).unwrap_or_default();
            let mut record = Vec::new();
            for (chunk, vector) in fresh {
                if indexed.dims == 0 {
                    indexed.dims = vector.len();
                }
                if vector.len() != indexed.dims || vector.is_empty() {
                    continue;
                }
                record.extend_from_slice(&chunk.to_le_bytes());
                for value in &vector {
                    record.extend_from_slice(&value.to_le_bytes());
                }
                indexed.vectors.push((chunk, vector));
                embedded += 1;
            }
            if !record.is_empty() {
                OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(self.path(&id, "vec"))
                    .and_then(|mut file| file.write_all(&record))
                    .map_err(|error| error.to_string())?;
            }
            indexed.embedded_upto = scanned.max(indexed.embedded_upto);
            self.save_state(&id, indexed)
                .map_err(|error| error.to_string())?;
        }
        Ok(embedded)
    }

    /// Chunks indexed, and how many of them have vectors.
    pub fn counts(&self) -> (usize, usize) {
        let sessions = self.lock();
        sessions
            .values()
            .fold((0, 0), |(chunks, vectors), indexed| {
                (
                    chunks + indexed.chunks.len(),
                    vectors + indexed.vectors.len(),
                )
            })
    }

    /// The best match per session, best first. `query_vector` is the query embedded with the
    /// model the chunks were embedded with; without it, ranking is by keyword alone.
    pub fn search(&self, query: &SearchQuery, query_vector: Option<&[f32]>) -> Vec<SearchHit> {
        let terms = retrieval::query_terms(query.text, MIN_WORD);
        let sessions = self.lock();
        let candidates: Vec<(&String, &Indexed)> = sessions
            .iter()
            .filter(|(_, indexed)| indexed.in_scope(query))
            .collect();

        // Keyword ranking (BM25), the open reply and the title counting as chunks.
        let open_words: HashMap<&str, Vec<String>> = candidates
            .iter()
            .filter_map(|(id, indexed)| {
                indexed
                    .open
                    .as_ref()
                    .map(|open| (id.as_str(), retrieval::words(&open.text, MIN_WORD)))
            })
            .collect();
        let titles: HashMap<&str, Vec<String>> = candidates
            .iter()
            .map(|(id, indexed)| {
                (
                    id.as_str(),
                    retrieval::words(&indexed.source.title, MIN_WORD),
                )
            })
            .collect();
        let has_title = |id: &str, term: &String| titles.get(id).is_some_and(|w| w.contains(term));
        let total: usize = candidates
            .iter()
            .map(|(_, indexed)| indexed.chunks.len() + usize::from(indexed.open.is_some()))
            .sum();
        let words: u64 = candidates
            .iter()
            .flat_map(|(_, indexed)| indexed.lengths.iter().map(|n| u64::from(*n)))
            .sum::<u64>()
            + open_words.values().map(|w| w.len() as u64).sum::<u64>();
        let average = if total == 0 {
            1.0
        } else {
            (words as f64 / total as f64).max(1.0)
        };
        let mut keyword: HashMap<(&str, u32), (f64, usize)> = HashMap::new();
        for term in &terms {
            let containing: usize = candidates
                .iter()
                .map(|(id, indexed)| {
                    indexed.postings.get(term).map_or(0, Vec::len)
                        + usize::from(
                            open_words
                                .get(id.as_str())
                                .is_some_and(|words| words.contains(term)),
                        )
                })
                .sum();
            let titled = candidates
                .iter()
                .filter(|(id, _)| has_title(id, term))
                .count();
            if containing == 0 && titled == 0 {
                continue;
            }
            let n = total as f64;
            let df = containing.max(1) as f64;
            let idf = ((n - df + 0.5) / (df + 0.5) + 1.0).ln();
            let bm25 = |tf: f64, length: f64| {
                idf * tf * (K1 + 1.0) / (tf + K1 * (1.0 - B + B * length / average))
            };
            for (id, indexed) in &candidates {
                for (chunk, tf) in indexed.postings.get(term).into_iter().flatten() {
                    let kind = indexed.chunks[*chunk as usize].kind;
                    let length = f64::from(indexed.lengths[*chunk as usize]);
                    let entry = keyword.entry((id.as_str(), *chunk)).or_default();
                    entry.0 += kind.weight() * bm25(f64::from(*tf), length);
                    entry.1 += 1;
                }
                if let Some(words) = open_words.get(id.as_str()) {
                    let tf = words.iter().filter(|word| *word == term).count();
                    if tf > 0 {
                        let chunk = u32::try_from(indexed.chunks.len()).unwrap_or(u32::MAX);
                        let entry = keyword.entry((id.as_str(), chunk)).or_default();
                        entry.0 += bm25(tf as f64, words.len() as f64);
                        entry.1 += 1;
                    }
                }
                if has_title(id, term) {
                    let entry = keyword.entry((id.as_str(), TITLE)).or_default();
                    entry.0 += TITLE_WEIGHT * idf;
                    entry.1 += 1;
                }
            }
        }
        // Chunks with more of the query's words come first.
        let mut keyword: Vec<((&str, u32), f64)> = keyword
            .into_iter()
            .map(|(key, (score, matched))| {
                let coverage = matched as f64 / terms.len().max(1) as f64;
                (key, score * (0.5 + 0.5 * coverage))
            })
            .collect();
        keyword.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

        // Semantic ranking.
        let mut semantic: Vec<((&str, u32), f32)> = Vec::new();
        if let Some(vector) = query_vector.filter(|vector| !vector.is_empty()) {
            for (id, indexed) in &candidates {
                if indexed.dims != vector.len() {
                    continue;
                }
                for (chunk, chunk_vector) in &indexed.vectors {
                    let similarity = retrieval::cosine(vector, chunk_vector);
                    if similarity >= MIN_SIMILARITY {
                        semantic.push(((id.as_str(), *chunk), similarity));
                    }
                }
            }
            semantic.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            semantic.truncate(SEMANTIC_CANDIDATES);
        }

        let keyword_keys: HashSet<(&str, u32)> = keyword.iter().map(|(key, _)| *key).collect();
        let ranked: Vec<((&str, u32), f64)> = if semantic.is_empty() {
            keyword
        } else {
            retrieval::fuse(&[
                keyword.iter().map(|(key, _)| *key).collect(),
                semantic.iter().map(|(key, _)| *key).collect(),
            ])
        };

        // The best chunk per session; the rest are counted.
        let mut best: Vec<((&str, u32), f64)> = Vec::new();
        let mut more: HashMap<&str, u32> = HashMap::new();
        for (key, score) in ranked {
            if best.iter().any(|((id, _), _)| *id == key.0) {
                *more.entry(key.0).or_default() += 1;
            } else {
                best.push((key, score));
            }
        }
        best.truncate(query.limit);
        best.into_iter()
            .filter_map(|((id, chunk), score)| {
                let indexed = sessions.get(id)?;
                let source = &indexed.source;
                let (kind, event, at_ms, text) = if chunk == TITLE {
                    let first = indexed.chunks.first();
                    (
                        ChunkKind::Title,
                        first.map_or(0, |chunk| chunk.event),
                        first.and_then(|chunk| chunk.at_ms),
                        source.title.as_str(),
                    )
                } else {
                    let found = indexed.chunk(chunk)?;
                    (found.kind, found.event, found.at_ms, found.text.as_str())
                };
                Some(SearchHit {
                    session_id: id.to_string(),
                    title: source.title.clone(),
                    environment_id: source.environment_id.clone(),
                    archived: source.archived,
                    kind,
                    event,
                    at_ms,
                    snippet: retrieval::snippet(text, &terms, SNIPPET_WIDTH),
                    score,
                    more: more.get(id).copied().unwrap_or(0),
                    semantic: !keyword_keys.contains(&(id, chunk)),
                })
            })
            .collect()
    }
}

/// Reads `count` vector records of `dims` values; `None` when the file does not hold exactly
/// that.
fn read_vectors(path: &Path, dims: usize, count: usize) -> Option<Vec<(u32, Vec<f32>)>> {
    let bytes = fs::read(path).ok()?;
    let record = 4 + 4 * dims;
    if dims == 0 || bytes.len() != record * count {
        return None;
    }
    Some(
        bytes
            .chunks_exact(record)
            .map(|record| {
                let chunk = u32::from_le_bytes(record[..4].try_into().unwrap_or_default());
                let (values, _) = record[4..].as_chunks::<4>();
                let vector = values
                    .iter()
                    .map(|value| f32::from_le_bytes(*value))
                    .collect();
                (chunk, vector)
            })
            .collect(),
    )
}
