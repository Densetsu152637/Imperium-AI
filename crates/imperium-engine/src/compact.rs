//! Fitting a conversation to a context window: older turns are replaced by a model-written
//! summary, while the system prompt and the most recent turns stay verbatim.
//!
//! The pieces here are pure: where a conversation may be cut, how much of it to keep, the
//! summarisation request, and the rebuilt conversation. The agent measures, asks a model for
//! the summary, and re-checks (see `Agent::fit_to`).

use std::collections::HashMap;

use imperium_api::truncate_chars;
use imperium_models::ChatMessage;

use crate::breakdown::{Category, Measured, PART_SEPARATOR, Spans, restored_spans};

/// Share of a context window held back for the model's reply (no per-model output limit is
/// configured, so a fixed share stands in for one).
pub(crate) const REPLY_RESERVE_PERCENT: u64 = 15;
/// When compacting, the conversation aims to use at most this share of the budget, so it can
/// keep going for a while before it is full again.
pub(crate) const TARGET_PERCENT: u64 = 60;
/// The longest summary asked for, in tokens.
pub(crate) const MAX_SUMMARY_TOKENS: u64 = 2_048;
/// The shortest summary worth keeping, in tokens.
pub(crate) const MIN_SUMMARY_TOKENS: u64 = 32;
/// The formatting a chat template adds around one message (as the measurement counts it).
pub(crate) const MESSAGE_FORMATTING: u64 = 4;
/// The user turns a manual compaction keeps verbatim.
pub(crate) const KEEP_TURNS: usize = 2;
/// Re-measurements before compaction gives up.
pub(crate) const MAX_ATTEMPTS: usize = 6;

/// Longest tool result, and longest other message, put in the text to summarise.
const TOOL_RESULT_CHARS: usize = 2_000;
const MESSAGE_CHARS: usize = 8_000;
const ARGUMENT_CHARS: usize = 1_000;

const SUMMARY_SYSTEM: &str = "You summarise a coding agent's conversation so that the agent can \
     continue the work from your summary and the most recent messages alone. Write plain, dense \
     notes in the third person. Do not add a preamble or address the reader.";

/// Why a session's model could not be switched, or its conversation compacted. The session
/// keeps its model and conversation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SwitchError {
    #[error("a turn is running; switch models or compact when it finishes")]
    Busy,
    #[error(
        "the conversation cannot fit {model}'s context window: the system prompt and tool \
         definitions alone need {needed} tokens, and {budget} are available after leaving room \
         for the reply"
    )]
    TooLarge {
        model: String,
        needed: u64,
        budget: u64,
    },
    #[error(
        "could not compact the conversation to fit {model}'s context window ({budget} tokens \
         after leaving room for the reply)"
    )]
    NoFit { model: String, budget: u64 },
    #[error("{0}'s context window is unknown, so there is nothing to compact against")]
    UnknownWindow(String),
    #[error("could not summarise the conversation: {0}")]
    Summary(String),
    #[error("{0}")]
    Unavailable(String),
    #[error("the session has no model yet; set one first")]
    NoModel,
    #[error("the session has stopped")]
    Closed,
}

/// The tokens a conversation may use in `window`: the window less the reply's reserve.
pub(crate) fn budget(window: u64) -> u64 {
    window - (window * REPLY_RESERVE_PERCENT).div_ceil(100)
}

/// Whether the kept part of `messages` may start at `at`: before a user or assistant message,
/// so an assistant's tool calls and their results (which follow it directly) stay together.
/// `at` is at least 2, so at least one message after the system prompt is summarised; `len`
/// summarises them all.
pub(crate) fn can_cut(messages: &[ChatMessage], at: usize) -> bool {
    at >= 2
        && (at == messages.len()
            || matches!(
                messages.get(at),
                Some(ChatMessage::User { .. } | ChatMessage::Assistant { .. })
            ))
}

/// Where the last `turns` user turns start, if there are more turns than that before them to
/// summarise.
pub(crate) fn recent_turns_start(messages: &[ChatMessage], turns: usize) -> Option<usize> {
    let starts: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter(|(_, message)| matches!(message, ChatMessage::User { .. }))
        .map(|(index, _)| index)
        .collect();
    let at = *starts.get(starts.len().checked_sub(turns)?)?;
    (at >= 2).then_some(at)
}

/// Where the kept tail starts: the longest tail whose measured tokens fit in `room`, starting
/// at a user turn if any such tail does, else at an assistant message within the last turn;
/// `messages.len()` when nothing fits. Never before `keep_from`.
pub(crate) fn choose_cut(
    messages: &[ChatMessage],
    measured: &Measured,
    room: u64,
    keep_from: Option<usize>,
) -> usize {
    let len = messages.len();
    let mut tail = 0u64;
    let mut user_start = None;
    let mut any_start = None;
    for at in (2..len).rev() {
        tail += measured.messages.get(at).copied().unwrap_or(0);
        if tail > room {
            break;
        }
        if !can_cut(messages, at) {
            continue;
        }
        any_start = Some(at);
        if matches!(messages[at], ChatMessage::User { .. }) {
            user_start = Some(at);
        }
    }
    let cut = user_start.or(any_start).unwrap_or(len);
    keep_from.map_or(cut, |limit| cut.max(limit))
}

/// The next place after `cut` where the kept part may start, if any.
pub(crate) fn next_cut(messages: &[ChatMessage], cut: usize) -> Option<usize> {
    (cut + 1..=messages.len()).find(|&at| can_cut(messages, at))
}

/// The request asking a model to summarise `older` in about `words` words.
pub(crate) fn summary_request(older: &[ChatMessage], words: u64) -> Vec<ChatMessage> {
    let prompt = format!(
        "Summarise the conversation below in at most {words} words. Preserve:\n\
         - the user's goals, requirements, and constraints;\n\
         - decisions made, and why;\n\
         - file paths, commands, identifiers, URLs, and versions that matter;\n\
         - what was done, and what the important tool results showed (errors, test results, \
         findings);\n\
         - open tasks, unanswered questions, and the next steps.\n\
         Leave out pleasantries and anything later superseded. If the conversation starts with \
         an earlier summary, fold it in.\n\n<conversation>\n{}</conversation>",
        transcript(older)
    );
    vec![
        ChatMessage::System {
            content: SUMMARY_SYSTEM.into(),
        },
        ChatMessage::User { content: prompt },
    ]
}

/// The messages as plain text for the summariser, with long tool results and arguments cut.
fn transcript(messages: &[ChatMessage]) -> String {
    let mut out = String::new();
    for message in messages {
        match message {
            ChatMessage::System { content } => {
                out.push_str(&format!(
                    "### System\n{}\n\n",
                    truncate_chars(content, MESSAGE_CHARS)
                ));
            }
            ChatMessage::User { content } => {
                out.push_str(&format!(
                    "### User\n{}\n\n",
                    truncate_chars(content, MESSAGE_CHARS)
                ));
            }
            ChatMessage::Assistant {
                content,
                tool_calls,
            } => {
                out.push_str("### Assistant\n");
                if let Some(content) = content {
                    out.push_str(&truncate_chars(content, MESSAGE_CHARS));
                    out.push('\n');
                }
                for call in tool_calls {
                    out.push_str(&format!(
                        "[called {} with {}]\n",
                        call.function.name,
                        truncate_chars(&call.function.arguments, ARGUMENT_CHARS)
                    ));
                }
                out.push('\n');
            }
            ChatMessage::Tool { content, .. } => {
                out.push_str(&format!(
                    "### Tool result\n{}\n\n",
                    truncate_chars(content, TOOL_RESULT_CHARS)
                ));
            }
        }
    }
    out
}

/// The summary as it is put into the conversation, tagged so a restored conversation's
/// breakdown recognises it.
pub(crate) fn summary_block(summary: &str) -> String {
    format!(
        "<conversation-summary>\nThe earlier part of this conversation, summarised to fit the \
         context window:\n\n{summary}\n</conversation-summary>"
    )
}

/// The conversation with `messages[1..cut]` replaced by `block`, and its recorded parts moved
/// to the new message indices. The block goes before the first kept message when that is a
/// user message (so roles keep alternating), else in a user message of its own.
pub(crate) fn rebuild(
    messages: &[ChatMessage],
    spans: &HashMap<usize, Spans>,
    cut: usize,
    block: &str,
) -> (Vec<ChatMessage>, HashMap<usize, Spans>) {
    let mut out = vec![messages[0].clone()];
    let mut new_spans = HashMap::new();
    if let Some(system) = spans.get(&0) {
        new_spans.insert(0, system.clone());
    }
    let rest = match messages.get(cut) {
        Some(ChatMessage::User { content }) => {
            let offset = block.len() + PART_SEPARATOR.len();
            let inner = spans
                .get(&cut)
                .cloned()
                .unwrap_or_else(|| restored_spans(content));
            let mut parts = vec![(Category::Summary, 0..block.len())];
            parts.extend(
                inner
                    .into_iter()
                    .map(|(category, range)| (category, range.start + offset..range.end + offset)),
            );
            new_spans.insert(1, parts);
            out.push(ChatMessage::User {
                content: format!("{block}{PART_SEPARATOR}{content}"),
            });
            cut + 1
        }
        _ => {
            new_spans.insert(1, vec![(Category::Summary, 0..block.len())]);
            out.push(ChatMessage::User {
                content: block.to_string(),
            });
            cut
        }
    };
    for (index, message) in messages.iter().enumerate().skip(rest) {
        if let Some(parts) = spans.get(&index) {
            new_spans.insert(out.len(), parts.clone());
        }
        out.push(message.clone());
    }
    (out, new_spans)
}

/// Whether the first kept message carries the summary block (it is a user message).
pub(crate) fn merges(messages: &[ChatMessage], cut: usize) -> bool {
    matches!(messages.get(cut), Some(ChatMessage::User { .. }))
}

/// `summary` cut to about `target` of its `current` tokens, at a character boundary.
pub(crate) fn shorten(summary: &str, target: u64, current: u64) -> String {
    let chars = summary.chars().count() as u64;
    let keep = if current == 0 {
        chars
    } else {
        // Aim a little under, since the cut and the marker change the count slightly.
        chars * target * 9 / (current * 10)
    };
    let kept: String = summary
        .chars()
        .take(usize::try_from(keep).unwrap_or(usize::MAX))
        .collect();
    format!("{}\n[summary shortened to fit]", kept.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;
    use imperium_models::{FunctionCall, ToolCall};
    use imperium_protocol::session::TokenCountMethod;

    fn user(text: &str) -> ChatMessage {
        ChatMessage::User {
            content: text.into(),
        }
    }

    fn calling(id: &str) -> ChatMessage {
        ChatMessage::Assistant {
            content: None,
            tool_calls: vec![ToolCall {
                id: id.into(),
                kind: "function".into(),
                function: FunctionCall {
                    name: "read_file".into(),
                    arguments: "{}".into(),
                },
            }],
        }
    }

    fn result(id: &str) -> ChatMessage {
        ChatMessage::Tool {
            tool_call_id: id.into(),
            content: "text".into(),
        }
    }

    fn reply(text: &str) -> ChatMessage {
        ChatMessage::Assistant {
            content: Some(text.into()),
            tool_calls: Vec::new(),
        }
    }

    /// system, then two turns: user, call, result, reply.
    fn conversation() -> Vec<ChatMessage> {
        vec![
            ChatMessage::System {
                content: "sys".into(),
            },
            user("one"),
            calling("a"),
            result("a"),
            reply("done one"),
            user("two"),
            calling("b"),
            result("b"),
            reply("done two"),
        ]
    }

    fn measured(each: u64, count: usize) -> Measured {
        Measured {
            messages: vec![each; count],
            fixed: 0,
            method: TokenCountMethod::Estimated,
            window: None,
        }
    }

    #[test]
    fn cuts_never_fall_between_a_call_and_its_result() {
        let messages = conversation();
        let cuts: Vec<usize> = (0..=messages.len())
            .filter(|&at| can_cut(&messages, at))
            .collect();
        assert_eq!(cuts, [2, 4, 5, 6, 8, 9]);
    }

    #[test]
    fn the_cut_prefers_a_turn_boundary_and_falls_back_inside_the_last_turn() {
        let messages = conversation();
        let tokens = measured(10, messages.len());
        // Room for the whole last turn: it starts at its user message.
        assert_eq!(choose_cut(&messages, &tokens, 45, None), 5);
        // Room for part of it: the latest assistant message that fits.
        assert_eq!(choose_cut(&messages, &tokens, 35, None), 6);
        // Room for nothing: everything is summarised.
        assert_eq!(choose_cut(&messages, &tokens, 5, None), 9);
        // A manual compaction keeps at most the recent turns it was asked to.
        assert_eq!(choose_cut(&messages, &tokens, 1_000, Some(5)), 5);
    }

    #[test]
    fn recent_turns_are_found_only_when_there_is_something_before_them() {
        let messages = conversation();
        assert_eq!(recent_turns_start(&messages, 1), Some(5));
        assert_eq!(recent_turns_start(&messages, 2), None);
    }

    #[test]
    fn rebuilding_merges_the_summary_into_the_first_kept_user_message() {
        let messages = conversation();
        let spans = HashMap::from([(5, vec![(Category::User, 0..3)])]);
        let block = summary_block("earlier work");
        let (rebuilt, new_spans) = rebuild(&messages, &spans, 5, &block);
        assert_eq!(rebuilt.len(), 5);
        let ChatMessage::User { content } = &rebuilt[1] else {
            panic!("a user message")
        };
        assert!(content.starts_with("<conversation-summary>"));
        let parts = &new_spans[&1];
        assert_eq!(parts[0].0, Category::Summary);
        assert_eq!(&content[parts[1].1.clone()], "two");
        assert_eq!(rebuilt[2..], messages[6..]);

        // Cut inside a turn: the summary is a user message of its own before the assistant.
        let (rebuilt, _) = rebuild(&messages, &spans, 6, &block);
        assert!(matches!(&rebuilt[1], ChatMessage::User { content } if content == &block));
        assert_eq!(rebuilt[2..], messages[6..]);
    }

    #[test]
    fn shortened_summaries_keep_their_start() {
        let long = "word ".repeat(100);
        let short = shorten(&long, 50, 100);
        assert!(short.len() < long.len() / 2 + 40);
        assert!(short.starts_with("word word"));
        assert!(short.ends_with("[summary shortened to fit]"));
    }

    #[test]
    fn the_budget_leaves_room_for_the_reply() {
        assert_eq!(budget(1_000), 850);
        assert_eq!(budget(32_768), 27_852);
    }
}
