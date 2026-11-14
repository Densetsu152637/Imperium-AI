//! Harness aids for small models: a de-reference step that rewrites the user's message into a
//! standalone prompt, a bounded JSON state kept beside the free-text history, and the blocks
//! that carry both to the model.
//!
//! Everything here is pure text handling; the agent decides when to call a model and where the
//! blocks go (see `Agent::run_turn_with`).
//!
//! The state is recorded in the conversation itself: a `<session-state>` block at the top of the
//! turn's user message, rewritten when the state changes mid-turn. A conversation that is
//! restored, or branched, therefore has the state as it stood at that point, and no file
//! beside it. Older turns' blocks are removed when a turn starts, so only the newest copy is
//! read.

use imperium_models::ChatMessage;
use serde::{Deserialize, Serialize};

use crate::breakdown::{Category, PART_SEPARATOR, restored_spans};

pub(crate) const STATE_OPEN: &str = "<session-state>";
pub(crate) const STATE_CLOSE: &str = "</session-state>";
pub(crate) const HINTS_OPEN: &str = "<ambiguity-hints>";
pub(crate) const HINTS_CLOSE: &str = "</ambiguity-hints>";
pub(crate) const RESOLVED_OPEN: &str = "<resolved-request>";
pub(crate) const RESOLVED_CLOSE: &str = "</resolved-request>";
pub(crate) const ORIGINAL_OPEN: &str = "<original-message>";
pub(crate) const ORIGINAL_CLOSE: &str = "</original-message>";

/// Most entities, goals and constraints the state keeps, whatever its character limit.
const MAX_ENTITIES: usize = 16;
const MAX_GOALS: usize = 5;
const MAX_CONSTRAINTS: usize = 10;
/// Most characters of any one name, note, goal or constraint.
const MAX_FIELD_CHARS: usize = 160;
/// Most characters of an entity's kind.
const MAX_KIND_CHARS: usize = 24;
/// The least the state's character limit can be: below this one goal would not fit.
const MIN_STATE_CHARS: usize = 300;

/// Something the conversation may refer to: a file, session, task, branch, error.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Entity {
    pub kind: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
}

/// The strict JSON object kept outside the free-text history: what is in play, what the user
/// wants, and what is settled. Bounded in size; the oldest entries go first.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct SessionState {
    pub entities: Vec<Entity>,
    pub goals: Vec<String>,
    pub constraints: Vec<String>,
}

/// A change to the state, from the `update_state` tool or the de-reference step.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct StateUpdate {
    /// Added, or updated when an entity of the same kind and name exists.
    pub entities: Vec<Entity>,
    /// Names of entities to remove.
    pub forget: Vec<String>,
    /// Replaces the goals when present.
    pub goals: Option<Vec<String>>,
    /// Added when not there yet.
    pub constraints: Vec<String>,
    pub drop_constraints: Vec<String>,
}

/// `text` on one line, at most `max` characters.
fn clean(text: &str, max: usize) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= max {
        line
    } else {
        line.chars().take(max).collect()
    }
}

impl SessionState {
    pub fn is_empty(&self) -> bool {
        self.entities.is_empty() && self.goals.is_empty() && self.constraints.is_empty()
    }

    /// The state as compact JSON.
    pub fn json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// The block that carries the state at the top of a turn; `None` while it is empty.
    pub fn block(&self) -> Option<String> {
        (!self.is_empty()).then(|| format!("{STATE_OPEN}\n{}\n{STATE_CLOSE}", self.json()))
    }

    /// Applies `update`, then cuts the state to `max_chars` of JSON.
    pub fn apply(&mut self, update: StateUpdate, max_chars: usize) {
        for entity in update.entities {
            let kind = clean(&entity.kind, MAX_KIND_CHARS).to_lowercase();
            let name = clean(&entity.name, MAX_FIELD_CHARS);
            if kind.is_empty() || name.is_empty() {
                continue;
            }
            let note = clean(&entity.note, MAX_FIELD_CHARS);
            // The newest mention goes last, where it is dropped last.
            self.entities
                .retain(|have| !(have.kind == kind && have.name.eq_ignore_ascii_case(&name)));
            self.entities.push(Entity { kind, name, note });
        }
        for name in update.forget {
            let name = clean(&name, MAX_FIELD_CHARS);
            self.entities
                .retain(|have| !have.name.eq_ignore_ascii_case(&name));
        }
        if let Some(goals) = update.goals {
            self.goals = goals
                .iter()
                .map(|goal| clean(goal, MAX_FIELD_CHARS))
                .filter(|goal| !goal.is_empty())
                .collect();
        }
        for constraint in update.constraints {
            let constraint = clean(&constraint, MAX_FIELD_CHARS);
            if !constraint.is_empty() && !self.constraints.contains(&constraint) {
                self.constraints.push(constraint);
            }
        }
        for constraint in update.drop_constraints {
            let constraint = clean(&constraint, MAX_FIELD_CHARS);
            self.constraints.retain(|have| *have != constraint);
        }
        self.bound(max_chars);
    }

    /// Cuts the state to its entry limits and to `max_chars` of JSON: the oldest entities go
    /// first, then the oldest constraints, then the last goals.
    pub fn bound(&mut self, max_chars: usize) {
        let extra = self.entities.len().saturating_sub(MAX_ENTITIES);
        self.entities.drain(..extra);
        let extra = self.constraints.len().saturating_sub(MAX_CONSTRAINTS);
        self.constraints.drain(..extra);
        self.goals.truncate(MAX_GOALS);
        let max_chars = max_chars.max(MIN_STATE_CHARS);
        while self.json().chars().count() > max_chars {
            if !self.entities.is_empty() {
                self.entities.remove(0);
            } else if !self.constraints.is_empty() {
                self.constraints.remove(0);
            } else if self.goals.len() > 1 {
                self.goals.pop();
            } else {
                break;
            }
        }
    }
}

/// The index just after the first `open`..`close` block in `content` that stands as a part of
/// its own (at the start or after a part separator, and at the end or before one), with where
/// it starts: a pasted tag inside a user's text is not one.
fn find_block(content: &str, open: &str, close: &str) -> Option<(usize, usize)> {
    for (start, _) in content.match_indices(open) {
        if start != 0 && !content[..start].ends_with(PART_SEPARATOR) {
            continue;
        }
        let Some(found) = content[start..].find(close) else {
            continue;
        };
        let end = start + found + close.len();
        if end == content.len() || content[end..].starts_with(PART_SEPARATOR) {
            return Some((start, end));
        }
    }
    None
}

/// `content` without its `open`..`close` part (and the separator after it), if it has one.
fn remove_block(content: &str, open: &str, close: &str) -> Option<String> {
    let (start, end) = find_block(content, open, close)?;
    let rest = content[end..].strip_prefix(PART_SEPARATOR).unwrap_or("");
    Some(format!("{}{rest}", &content[..start]))
}

/// The state recorded in `content`, if it has a block that parses.
fn state_in(content: &str) -> Option<SessionState> {
    let (start, end) = find_block(content, STATE_OPEN, STATE_CLOSE)?;
    let json = &content[start + STATE_OPEN.len()..end - STATE_CLOSE.len()];
    serde_json::from_str(json.trim()).ok()
}

/// The newest state recorded in a conversation, or an empty one.
pub(crate) fn latest_state(messages: &[ChatMessage]) -> SessionState {
    messages
        .iter()
        .rev()
        .find_map(|message| match message {
            ChatMessage::User { content, .. } => state_in(content),
            _ => None,
        })
        .unwrap_or_default()
}

/// `content` with its state block replaced by `block` (put first), or removed when `None`.
pub(crate) fn with_state_block(content: &str, block: Option<&str>) -> String {
    let without = remove_block(content, STATE_OPEN, STATE_CLOSE).unwrap_or_else(|| content.into());
    match block {
        None => without,
        Some(block) if without.is_empty() => block.to_string(),
        Some(block) => format!("{block}{PART_SEPARATOR}{without}"),
    }
}

/// `content` without what a turn carries only for itself: the state block, the ambiguity hints
/// and the original message (the resolved request stays as the turn's record). `None` when
/// there was nothing to remove.
pub(crate) fn without_turn_blocks(content: &str) -> Option<String> {
    let mut text = content.to_string();
    let mut changed = false;
    for (open, close) in [(STATE_OPEN, STATE_CLOSE), (HINTS_OPEN, HINTS_CLOSE)] {
        if let Some(rest) = remove_block(&text, open, close) {
            text = rest;
            changed = true;
        }
    }
    let marker = format!("{PART_SEPARATOR}{ORIGINAL_OPEN}\n");
    if text.ends_with(ORIGINAL_CLOSE)
        && let Some(at) = text.rfind(&marker)
    {
        text.truncate(at);
        changed = true;
    }
    changed.then_some(text)
}

/// The part of a message's text that holds the resolved request and the original, in the form
/// the turn's user message takes when the de-reference step changed the message.
pub(crate) fn resolved_text(resolved: &str, original: &str) -> String {
    format!(
        "{RESOLVED_OPEN}\n{resolved}\n{RESOLVED_CLOSE}{PART_SEPARATOR}{ORIGINAL_OPEN}\n{original}\n{ORIGINAL_CLOSE}"
    )
}

/// What the user wrote in a user message's content: the resolved request when there is one,
/// without the blocks that precede the text.
fn user_text(content: &str) -> String {
    let spans = restored_spans(content);
    let own = spans
        .iter()
        .rev()
        .find(|(category, _)| *category == Category::User)
        .map_or(content, |(_, range)| &content[range.clone()]);
    if let (Some(open), Some(close)) = (own.find(RESOLVED_OPEN), own.find(RESOLVED_CLOSE))
        && open < close
    {
        return own[open + RESOLVED_OPEN.len()..close].trim().to_string();
    }
    own.trim().to_string()
}

/// The first `max` characters of `text` on one line, with an ellipsis when cut.
fn clip(text: &str, max: usize) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= max {
        return line;
    }
    let mut cut: String = line.chars().take(max).collect();
    cut.push('…');
    cut
}

/// The tail of a conversation as short lines for the de-reference step, newest last, within
/// `budget` characters: what the user asked, what the agent said and called, and the start of
/// each tool result (errors lead theirs).
pub(crate) fn digest(messages: &[ChatMessage], budget: usize) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut used = 0;
    for message in messages.iter().rev() {
        let line = match message {
            ChatMessage::System { .. } => continue,
            ChatMessage::User { content, .. } => {
                let text = user_text(content);
                if text.is_empty() {
                    continue;
                }
                format!("User: {}", clip(&text, 600))
            }
            ChatMessage::Assistant {
                content,
                tool_calls,
            } => {
                let mut parts = Vec::new();
                if let Some(content) = content.as_deref().filter(|text| !text.trim().is_empty()) {
                    parts.push(clip(content, 600));
                }
                for call in tool_calls {
                    parts.push(format!(
                        "[called {} {}]",
                        call.function.name,
                        clip(&call.function.arguments, 120)
                    ));
                }
                if parts.is_empty() {
                    continue;
                }
                format!("Assistant: {}", parts.join(" "))
            }
            ChatMessage::Tool { content, .. } => format!("Tool result: {}", clip(content, 300)),
        };
        // Characters, with the newline that ends the line.
        let size = line.chars().count() + 1;
        if used + size > budget {
            break;
        }
        used += size;
        lines.push(line);
    }
    lines.reverse();
    lines.join("\n")
}

const DEREFERENCE_SYSTEM: &str = "You rewrite the latest message of a conversation between a user and a coding assistant into a standalone prompt. Replace pronouns and references (\"it\", \"that file\", \"the previous step\", \"the issue from earlier\") with the concrete files, sessions, tasks, errors or values they refer to, using only the session state and the conversation excerpt you are given. Keep the user's intent, tone and every detail they gave; add nothing they did not ask for. If a reference cannot be resolved from what you are given, leave it as written and do not guess. If the message already stands alone, return it unchanged.\n\nAnswer with one JSON object and nothing else: {\"prompt\": \"<the standalone prompt>\", \"state\": {\"entities\": [{\"kind\": \"file\", \"name\": \"src/main.rs\", \"note\": \"what it is\"}], \"goals\": [\"the user's current goals, whole list\"], \"constraints\": [\"settled constraints to add\"]}}. \"state\" holds only what this message adds or changes; leave it out when nothing changed.";

/// The request that asks a model to de-reference `message`.
pub(crate) fn dereference_request(
    state: &SessionState,
    excerpt: &str,
    message: &str,
) -> Vec<ChatMessage> {
    let state = if state.is_empty() {
        "{}".to_string()
    } else {
        state.json()
    };
    let excerpt = if excerpt.is_empty() {
        "(nothing yet)"
    } else {
        excerpt
    };
    let user = format!(
        "{STATE_OPEN}\n{state}\n{STATE_CLOSE}\n\n<conversation-excerpt>\n{excerpt}\n</conversation-excerpt>\n\n<latest-message>\n{message}\n</latest-message>"
    );
    vec![
        ChatMessage::System {
            content: DEREFERENCE_SYSTEM.into(),
        },
        ChatMessage::user(user),
    ]
}

/// What the de-reference step produced.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Dereferenced {
    /// The standalone prompt, when it differs from the message.
    pub prompt: Option<String>,
    pub update: Option<StateUpdate>,
}

/// Reads the de-reference reply: a JSON object (anywhere in the text) with a `prompt` and an
/// optional `state`. `None` when it holds no usable prompt, so the original message is used.
pub(crate) fn parse_dereference(reply: &str, message: &str) -> Option<Dereferenced> {
    let start = reply.find('{')?;
    let end = reply.rfind('}')?;
    if end < start {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(&reply[start..=end]).ok()?;
    let prompt = value.get("prompt")?.as_str()?.trim();
    // A rewrite far longer than the message is the model talking, not rewriting.
    let longest = message.chars().count() * 4 + 400;
    if prompt.is_empty() || prompt.chars().count() > longest {
        return None;
    }
    let update = value
        .get("state")
        .and_then(|state| serde_json::from_value::<StateUpdate>(state.clone()).ok())
        .filter(|update| *update != StateUpdate::default());
    Some(Dereferenced {
        prompt: (prompt != message.trim()).then(|| prompt.to_string()),
        update,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn update(json: &str) -> StateUpdate {
        serde_json::from_str(json).expect("an update")
    }

    #[test]
    fn an_update_adds_entities_goals_and_constraints() {
        let mut state = SessionState::default();
        state.apply(
            update(
                r#"{"entities":[{"kind":"File","name":"src/main.rs","note":"entry point"}],
                    "goals":["add a --verbose flag"],"constraints":["no new dependencies"]}"#,
            ),
            1500,
        );
        assert_eq!(state.entities[0].kind, "file");
        assert_eq!(state.goals, ["add a --verbose flag"]);
        assert_eq!(state.constraints, ["no new dependencies"]);
        let json: serde_json::Value = serde_json::from_str(&state.json()).unwrap();
        assert_eq!(json["entities"][0]["name"], "src/main.rs");
        assert!(state.block().unwrap().starts_with(STATE_OPEN));
    }

    #[test]
    fn mentioning_an_entity_again_updates_it_and_makes_it_the_newest() {
        let mut state = SessionState::default();
        state.apply(
            update(r#"{"entities":[{"kind":"file","name":"a.rs"},{"kind":"file","name":"b.rs"}]}"#),
            1500,
        );
        state.apply(
            update(r#"{"entities":[{"kind":"file","name":"A.rs","note":"edited"}]}"#),
            1500,
        );
        let names: Vec<_> = state.entities.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["b.rs", "A.rs"]);
        assert_eq!(state.entities[1].note, "edited");
    }

    #[test]
    fn forgetting_goals_and_constraints_work_as_the_tool_says() {
        let mut state = SessionState::default();
        state.apply(
            update(
                r#"{"entities":[{"kind":"task","name":"t1"}],"goals":["one","two"],
                    "constraints":["keep tests","no unsafe"]}"#,
            ),
            1500,
        );
        state.apply(
            update(r#"{"forget":["T1"],"goals":["three"],"drop_constraints":["keep tests"]}"#),
            1500,
        );
        assert!(state.entities.is_empty());
        assert_eq!(state.goals, ["three"]);
        assert_eq!(state.constraints, ["no unsafe"]);
        // Leaving the goals out keeps them.
        state.apply(update(r#"{"constraints":["more"]}"#), 1500);
        assert_eq!(state.goals, ["three"]);
    }

    #[test]
    fn the_state_stays_within_its_limits_and_the_oldest_entries_go_first() {
        let mut state = SessionState::default();
        let many: Vec<String> = (0..40)
            .map(|n| format!(r#"{{"kind":"file","name":"src/module_{n}/file_{n}.rs"}}"#))
            .collect();
        state.apply(
            update(&format!(r#"{{"entities":[{}]}}"#, many.join(","))),
            600,
        );
        assert!(state.json().chars().count() <= 600, "{}", state.json());
        assert!(state.entities.len() <= MAX_ENTITIES);
        assert_eq!(
            state.entities.last().unwrap().name,
            "src/module_39/file_39.rs"
        );
        // A field is cut to its limit.
        state.apply(
            update(&format!(r#"{{"goals":["{}"]}}"#, "x".repeat(1000))),
            1500,
        );
        assert_eq!(state.goals[0].chars().count(), MAX_FIELD_CHARS);
    }

    #[test]
    fn unknown_fields_are_refused() {
        assert!(serde_json::from_str::<StateUpdate>(r#"{"entites":[]}"#).is_err());
        assert!(serde_json::from_str::<SessionState>(r#"{"entities":[],"mood":"x"}"#).is_err());
    }

    #[test]
    fn the_state_block_is_written_replaced_and_read_back() {
        let mut state = SessionState::default();
        state.apply(update(r#"{"goals":["ship"]}"#), 1500);
        let first = with_state_block("hello", state.block().as_deref());
        assert!(first.starts_with(STATE_OPEN));
        assert!(first.ends_with("\n\nhello"));
        state.apply(update(r#"{"constraints":["fast"]}"#), 1500);
        let second = with_state_block(&first, state.block().as_deref());
        assert_eq!(second.matches(STATE_OPEN).count(), 1);
        assert_eq!(with_state_block(&second, None), "hello");
        let messages = vec![ChatMessage::user(second)];
        assert_eq!(latest_state(&messages), state);
        assert!(latest_state(&[ChatMessage::user("no block")]).is_empty());
    }

    #[test]
    fn a_pasted_tag_inside_the_users_text_is_not_a_block() {
        let content = "see <session-state>{}</session-state> in my text";
        assert!(without_turn_blocks(content).is_none());
        assert!(latest_state(&[ChatMessage::user(content)]).is_empty());
    }

    #[test]
    fn turn_blocks_are_removed_but_the_resolved_request_stays() {
        let content = format!(
            "{STATE_OPEN}\n{{}}\n{STATE_CLOSE}\n\n{HINTS_OPEN}\n- a: b\n{HINTS_CLOSE}\n\n{}",
            resolved_text("open src/main.rs", "open that file")
        );
        let stripped = without_turn_blocks(&content).expect("blocks");
        assert_eq!(
            stripped,
            format!("{RESOLVED_OPEN}\nopen src/main.rs\n{RESOLVED_CLOSE}")
        );
        assert!(without_turn_blocks(&stripped).is_none());
    }

    #[test]
    fn the_digest_keeps_the_newest_lines_within_the_budget() {
        let messages = vec![
            ChatMessage::System {
                content: "system".into(),
            },
            ChatMessage::user("first question"),
            ChatMessage::Assistant {
                content: Some("an answer".into()),
                tool_calls: Vec::new(),
            },
            ChatMessage::Tool {
                content: "error: build failed\nat src/lib.rs:3".into(),
                tool_call_id: "c1".into(),
            },
            ChatMessage::user(format!(
                "{STATE_OPEN}\n{{}}\n{STATE_CLOSE}\n\n{}",
                resolved_text("open src/lib.rs", "open that file")
            )),
        ];
        let all = digest(&messages, 10_000);
        assert!(!all.contains("system"));
        assert!(all.starts_with("User: first question\nAssistant: an answer\n"));
        assert!(all.contains("Tool result: error: build failed at src/lib.rs:3"));
        assert!(all.ends_with("User: open src/lib.rs"), "{all}");
        let small = digest(&messages, 40);
        assert_eq!(small, "User: open src/lib.rs");
        assert_eq!(digest(&[], 100), "");
    }

    #[test]
    fn the_request_holds_the_state_the_excerpt_and_the_message() {
        let mut state = SessionState::default();
        state.apply(update(r#"{"goals":["ship"]}"#), 1500);
        let request = dereference_request(&state, "User: hi", "do it");
        assert_eq!(request.len(), 2);
        let ChatMessage::User { content, .. } = &request[1] else {
            panic!("a user message");
        };
        assert!(content.contains("\"goals\":[\"ship\"]"));
        assert!(content.contains("User: hi"));
        assert!(content.contains("<latest-message>\ndo it\n</latest-message>"));
    }

    #[test]
    fn a_reply_with_a_prompt_and_state_is_read_from_anywhere_in_the_text() {
        let reply = "Sure!\n```json\n{\"prompt\": \"Open src/main.rs and change the timeout\", \"state\": {\"entities\": [{\"kind\": \"file\", \"name\": \"src/main.rs\"}]}}\n```";
        let found = parse_dereference(reply, "Open that file and change the timeout").unwrap();
        assert_eq!(
            found.prompt.as_deref(),
            Some("Open src/main.rs and change the timeout")
        );
        assert_eq!(found.update.unwrap().entities[0].name, "src/main.rs");
    }

    #[test]
    fn an_unchanged_prompt_gives_no_rewrite_and_bad_replies_give_nothing() {
        let same = parse_dereference(r#"{"prompt":"run the tests"}"#, "run the tests").unwrap();
        assert_eq!(same, Dereferenced::default());
        for bad in [
            "",
            "no json here",
            "{not json}",
            r#"{"prompt":""}"#,
            r#"{"prompt": 3}"#,
            r#"{"state":{}}"#,
        ] {
            assert!(parse_dereference(bad, "msg").is_none(), "{bad}");
        }
        // A rewrite that runs on far beyond the message is rejected.
        let long = format!(r#"{{"prompt":"{}"}}"#, "word ".repeat(400));
        assert!(parse_dereference(&long, "msg").is_none());
        // A state that does not fit the schema is dropped; the prompt is kept.
        let loose = parse_dereference(r#"{"prompt":"x y","state":{"mood":1}}"#, "msg").unwrap();
        assert_eq!(loose.prompt.as_deref(), Some("x y"));
        assert!(loose.update.is_none());
    }
}
