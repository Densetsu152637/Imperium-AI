//! Session titles. After a session's first exchange it is given a short title, written by a
//! model that is cheap to ask now: the session's own model when it is local and loaded with a
//! free slot, another local model that is, or the session's hosted model. No model is loaded for
//! a title, and the turn never waits for one; without a model at hand (and for background
//! sessions and project agents, which call models only when the machine is quiet) the title is
//! the first message's first words. A title the user gave (`session.control` `rename`) is never
//! replaced.

use std::sync::Arc;
use std::time::Duration;

use imperium_api::ChatMessage;
use imperium_engine::{
    Availability, ChatClients, ChatRequest, Endpoint, LocalModels, LocalRequest, Priority,
    ResolvedModel, Session,
};
use imperium_protocol::methods::SESSION_UPDATED;
use imperium_protocol::session::{AgentEvent, ROOT_AGENT, SessionEvent};
use tokio::sync::broadcast;

use super::DaemonState;

/// Words a title has at most; a model's longer answer is cut.
const MAX_WORDS: usize = 6;
/// Characters a title has at most.
const MAX_CHARS: usize = 60;
/// How much of the first message and reply the model reads.
const EXCHANGE_CHARS: usize = 1500;
/// Tokens the model may write; thinking models need room before the answer.
const REPLY_TOKENS: u32 = 256;
/// A title that takes longer is not worth waiting for.
const TIMEOUT: Duration = Duration::from_secs(60);

const INSTRUCTIONS: &str = "You name chat sessions. Reply with only a title of 3 to 6 words \
    that says what the conversation below is about: no quotes, no trailing punctuation, nothing \
    else.";

impl DaemonState {
    /// Sets a session's title, records it with the session so it survives a restart, and tells
    /// every client. A generated title (`by_user` false) is dropped when the session already has
    /// a title. An empty title from the user clears it, so the session is titled automatically
    /// again. False when there is no such session or the title was dropped.
    pub fn set_title(&self, id: &str, title: &str, by_user: bool) -> std::io::Result<bool> {
        let title = clean(title);
        let Some(session) = self.session(id) else {
            return if by_user {
                self.rename_archived(id, title)
            } else {
                Ok(false)
            };
        };
        {
            let mut placements = self
                .placements
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let placement = placements.entry(id.to_string()).or_default();
            if !by_user && placement.title.is_some() {
                return Ok(false);
            }
            placement.title = title.clone();
            placement.title_by_user = by_user && title.is_some();
        }
        self.record_title(id, title.as_deref(), by_user)?;
        if by_user
            && title.is_none()
            && let Some(me) = self.me.upgrade()
        {
            // Titled again after the next exchange.
            watch(&me, id.to_string(), session.subscribe());
        }
        self.announce(SESSION_UPDATED, self.summary(&session));
        Ok(true)
    }

    /// Writes the title to the session's meta file. Sessions that are not remembered (project
    /// agents) have none; their title lasts until the daemon stops.
    pub(super) fn record_title(
        &self,
        id: &str,
        title: Option<&str>,
        by_user: bool,
    ) -> std::io::Result<()> {
        let path = self.meta_path(id);
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Ok(());
        };
        let mut meta: serde_json::Value =
            serde_json::from_str(&text).map_err(std::io::Error::other)?;
        meta["title"] = title.into();
        meta["title_by_user"] = (by_user && title.is_some()).into();
        std::fs::write(&path, meta.to_string())
    }

    /// The local runtime, as models are asked through it.
    fn local_models_handle(&self) -> Option<Arc<dyn LocalModels>> {
        self.local.get().map(|local| {
            let supervisor: Arc<dyn LocalModels> = local.supervisor.clone();
            supervisor
        })
    }

    /// The model to write a session's title, if one costs nothing to ask now (see the module
    /// documentation).
    fn title_model(&self, session: &Session) -> Option<ResolvedModel> {
        // Background work (schedules, project agents and what they start) calls models only
        // when the machine is quiet; a title is not worth one.
        let project_agent = self
            .placements
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(session.id())
            .is_some_and(|placement| placement.project_agent || placement.persistent);
        if session.is_background() || project_agent {
            return None;
        }
        let own = session
            .model()
            .and_then(|spec| self.resolve_model(Some(&spec)).ok());
        let local = self.local_models_handle();
        let loaded = |model: &ResolvedModel| {
            model.local
                && local
                    .as_ref()
                    .is_some_and(|local| local.availability(&model.model) == Availability::Loaded)
        };
        if let Some(own) = own.as_ref().filter(|own| loaded(own)) {
            return Some(own.clone());
        }
        if let Some(other) = self
            .catalog()
            .into_iter()
            .map(|entry| entry.model)
            .find(|model| loaded(model))
        {
            return Some(other);
        }
        own.filter(|own| !own.local)
    }
}

/// Watches a session for the end of its first exchange, then titles it in the background.
pub(super) fn watch(
    state: &Arc<DaemonState>,
    session_id: String,
    mut events: broadcast::Receiver<AgentEvent>,
) {
    let daemon = Arc::downgrade(state);
    tokio::spawn(async move {
        let mut message: Option<String> = None;
        let mut reply = String::new();
        loop {
            let event = match events.recv().await {
                Ok(event) => event,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return,
            };
            if event.agent_id != ROOT_AGENT {
                continue;
            }
            match event.event {
                SessionEvent::TurnStarted { text } => {
                    if message.is_none() {
                        message = Some(text);
                    }
                }
                SessionEvent::TextDelta { text } if reply.len() < EXCHANGE_CHARS => {
                    reply.push_str(&text);
                }
                SessionEvent::TurnCompleted { .. } | SessionEvent::TurnFailed { .. } => break,
                _ => {}
            }
        }
        let Some(state) = daemon.upgrade() else {
            return;
        };
        let message = message.unwrap_or_default();
        let title = match generate(&state, &session_id, &message, &reply).await {
            Some(title) => title,
            None => from_first_message(&message),
        };
        if title.is_empty() {
            return;
        }
        if let Err(error) = state.set_title(&session_id, &title, false) {
            tracing::warn!(session = %session_id, %error, "could not record the session's title");
        }
    });
}

/// A title written by a model, when one costs nothing to ask and answers sensibly.
async fn generate(
    state: &DaemonState,
    session_id: &str,
    message: &str,
    reply: &str,
) -> Option<String> {
    let session = state.session(session_id)?;
    let model = state.title_model(&session)?;
    drop(session);
    let exchange = format!(
        "User: {}\n\nAssistant: {}",
        cut(message, EXCHANGE_CHARS),
        cut(reply, EXCHANGE_CHARS)
    );
    let messages = [
        ChatMessage::System {
            content: INSTRUCTIONS.into(),
        },
        ChatMessage::User { content: exchange },
    ];
    let holder = format!("{session_id}:title");
    let call = async {
        let (endpoint, lease) = if model.local {
            let local = state.local_models_handle()?;
            // Checked loaded just now; ranked below the user's calls.
            let lease = local
                .acquire_with(
                    &model.model,
                    &holder,
                    LocalRequest {
                        priority: Priority::Background,
                        prompt_tokens: Some(512),
                        uncapped: false,
                    },
                )
                .await
                .ok()?;
            let endpoint = Endpoint {
                base_url: lease.base_url.clone(),
                api_key: None,
            };
            (endpoint, Some(lease))
        } else {
            (model.endpoint.clone(), None)
        };
        let client =
            ChatClients::default().build(&model.kind, &reqwest::Client::new(), endpoint)?;
        let max_tokens = lease
            .as_ref()
            .and_then(|lease| lease.max_tokens)
            .map_or(REPLY_TOKENS, |cap| cap.min(REPLY_TOKENS));
        let request = ChatRequest {
            model: &model.model,
            messages: &messages,
            tools: &[],
            slot: lease.as_ref().and_then(|lease| lease.slot),
            max_tokens: Some(max_tokens),
        };
        let completion = client.stream(request, &mut |_| {}).await;
        drop(lease);
        match completion {
            Ok(completion) => Some(completion.text),
            Err(error) => {
                tracing::info!(session = %session_id, %error, "no generated title");
                None
            }
        }
    };
    let text = tokio::time::timeout(TIMEOUT, call).await.ok()??;
    clean(&from_answer(&text))
}

/// The title in a model's answer: its first line with any reasoning, label, or quotes removed.
fn from_answer(text: &str) -> String {
    let text = match text.rfind("</think>") {
        Some(end) => &text[end + "</think>".len()..],
        None => text,
    };
    let line = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    let line = line
        .strip_prefix("Title:")
        .or_else(|| line.strip_prefix("title:"))
        .unwrap_or(line);
    shorten(line.trim_matches(|c: char| {
        c.is_whitespace() || matches!(c, '"' | '\'' | '`' | '*' | '#' | '“' | '”')
    }))
}

/// A title from a session's first message, for when no model writes one: its first words.
pub(super) fn from_first_message(message: &str) -> String {
    let line = message
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    let title = shorten(line);
    let mut chars = title.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

/// At most [`MAX_WORDS`] words and [`MAX_CHARS`] characters, cut at a word, without trailing
/// punctuation.
fn shorten(text: &str) -> String {
    let mut title = String::new();
    for word in text.split_whitespace().take(MAX_WORDS) {
        if title.chars().count() + word.chars().count() + 1 > MAX_CHARS {
            if title.is_empty() {
                title = word.chars().take(MAX_CHARS).collect();
            }
            break;
        }
        if !title.is_empty() {
            title.push(' ');
        }
        title.push_str(word);
    }
    title
        .trim_end_matches(|c: char| c.is_ascii_punctuation() && c != ')' && c != '"')
        .to_string()
}

/// A title as it is kept: trimmed, on one line, and not empty.
fn clean(title: &str) -> Option<String> {
    let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
    let title: String = title.chars().take(200).collect();
    (!title.is_empty()).then_some(title)
}

fn cut(text: &str, chars: usize) -> String {
    text.chars().take(chars).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_message_gives_its_first_words() {
        assert_eq!(
            from_first_message(
                "\n  fix the login bug in auth.rs, it fails on empty passwords\nmore"
            ),
            "Fix the login bug in auth.rs"
        );
        assert_eq!(from_first_message("hello?"), "Hello");
        assert_eq!(from_first_message("   "), "");
    }

    #[test]
    fn a_long_word_is_cut_to_the_limit() {
        let word = "x".repeat(100);
        assert_eq!(from_first_message(&word).chars().count(), MAX_CHARS);
    }

    #[test]
    fn a_model_answer_loses_reasoning_labels_and_quotes() {
        assert_eq!(
            from_answer("<think>hmm</think>\n\n\"Fixing the login bug.\""),
            "Fixing the login bug"
        );
        assert_eq!(
            from_answer("Title: Rust build caching\n"),
            "Rust build caching"
        );
        assert_eq!(
            from_answer("A very long answer that keeps going past six words"),
            "A very long answer that keeps"
        );
    }

    #[test]
    fn titles_are_kept_on_one_line() {
        assert_eq!(clean("  Two\nlines "), Some("Two lines".into()));
        assert_eq!(clean(" \n "), None);
    }
}
