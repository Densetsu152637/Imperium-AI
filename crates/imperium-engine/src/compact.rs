//! Fitting a conversation to a context window: older turns are replaced by a model-written
//! summary, while the system prompt and the most recent turns stay verbatim.
//!
//! The pieces here are pure: where a conversation may be cut, how much of it to keep, the
//! summarisation request, and the rebuilt conversation. The agent measures, asks a model for
//! the summary, and re-checks (see `Agent::fit_to`).

use std::collections::HashMap;

use imperium_api::truncate_chars;
use imperium_models::ChatMessage;

use crate::breakdown::{
    Category, KEPT_CLOSE, KEPT_OPEN, Measured, PART_SEPARATOR, Spans, restored_spans,
};

/// Share of a context window held back for the model's reply (no per-model output limit is
/// configured, so a fixed share stands in for one).
pub(crate) const REPLY_RESERVE_PERCENT: u64 = 15;
/// When compacting, the conversation aims to use at most this share of the budget, so it can
/// keep going for a while before it is full again.
pub(crate) const TARGET_PERCENT: u64 = 60;
/// The lowest share of the window `context.auto_compact_at` may be: lower, compaction could
/// not get the conversation under its own trigger.
pub(crate) const MIN_TRIGGER: f64 = 0.5;
/// A mid-turn compaction aims at this share of its trigger.
pub(crate) const BELOW_TRIGGER_PERCENT: u64 = 80;
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

/// The tokens at which a mid-turn compaction starts: `at` (kept between [`MIN_TRIGGER`] and 1)
/// of `window`.
pub(crate) fn trigger_tokens(window: u64, at: f64) -> u64 {
    (window as f64 * at.clamp(MIN_TRIGGER, 1.0)) as u64
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
        ChatMessage::user(prompt),
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
            ChatMessage::User { content, .. } => {
                out.push_str(&format!(
                    "### User\n{}\n\n",
                    truncate_chars(&without_pinned(content), MESSAGE_CHARS)
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

/// The block a compaction carries workflow context in; `restored_spans` recognises it by the
/// same tags, so a conversation restored after a restart reads it as one part.
const PINNED_HEAD: &str = "<context-manifest kept=\"true\">\nWorkflow context loaded earlier in this conversation, kept verbatim:\n\n";
const PINNED_CLOSE: &str = KEPT_CLOSE;
/// Between the items in the block, so a later compaction can take them apart again.
const ITEM_SEP: &str = "\n[next kept item]\n";
/// The tools whose results are workflow context: loaded workflow companions and skills.
const PINNED_TOOLS: [&str; 2] = ["context_load", "load_skill"];

/// The workflow context in `messages[1..cut]`, which a compaction keeps verbatim instead of
/// summarising: the latest context manifest, the references `context_load` returned, and
/// the instructions `load_skill` returned (the system prompt, which holds the policy index,
/// project instruction files, and eager skills, is never summarised at all). Context carried
/// over by an earlier compaction is carried again, each item once: a manifest replaces the
/// one before it, and a result that holds an earlier one replaces it. Empty when there is
/// none.
pub(crate) fn pinned_context(
    messages: &[ChatMessage],
    spans: &HashMap<usize, Spans>,
    cut: usize,
) -> String {
    let mut names: HashMap<&str, &str> = HashMap::new();
    let mut items: Vec<String> = Vec::new();
    let mut add = |item: &str| {
        if item.trim().is_empty() {
            return;
        }
        if item.starts_with("<context-manifest") {
            items.retain(|kept| !kept.starts_with("<context-manifest"));
        } else if items.iter().any(|kept| kept.contains(item)) {
            return;
        } else {
            items.retain(|kept| !item.contains(kept.as_str()));
        }
        items.push(item.to_string());
    };
    let mut keep = |text: &str| match text
        .strip_prefix(PINNED_HEAD)
        .and_then(|inner| inner.strip_suffix(PINNED_CLOSE))
    {
        Some(inner) => inner.split(ITEM_SEP).for_each(&mut add),
        None => add(text),
    };
    for (index, message) in messages.iter().enumerate().take(cut).skip(1) {
        match message {
            ChatMessage::Assistant { tool_calls, .. } => {
                for call in tool_calls {
                    names.insert(&call.id, &call.function.name);
                }
            }
            ChatMessage::User { content, .. } => {
                let parts = spans
                    .get(&index)
                    .cloned()
                    .unwrap_or_else(|| restored_spans(content));
                for (category, range) in parts {
                    if category == Category::Manifest
                        && let Some(text) = content.get(range)
                    {
                        keep(text);
                    }
                }
            }
            ChatMessage::Tool {
                tool_call_id,
                content,
            } => {
                let pinned = names
                    .get(tool_call_id.as_str())
                    .is_some_and(|name| PINNED_TOOLS.contains(name));
                if pinned
                    && !content.starts_with("error")
                    && !content.contains("skill is already loaded")
                {
                    keep(content);
                }
            }
            ChatMessage::System { .. } => {}
        }
    }
    if items.is_empty() {
        return String::new();
    }
    format!("{PINNED_HEAD}{}{PINNED_CLOSE}", items.join(ITEM_SEP))
}

/// `content` without a block of carried-over workflow context.
fn without_pinned(content: &str) -> String {
    if let Some(start) = content.find(KEPT_OPEN)
        && let Some(end) = content[start..].find(PINNED_CLOSE)
    {
        let end = start + end + PINNED_CLOSE.len();
        return format!("{}{}", &content[..start], &content[end..]);
    }
    content.to_string()
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
/// user message (so roles keep alternating), else in a user message of its own. `pinned` (the
/// [`pinned_context`], if any) follows the summary in the same message.
pub(crate) fn rebuild(
    messages: &[ChatMessage],
    spans: &HashMap<usize, Spans>,
    cut: usize,
    summary: &str,
    pinned: &str,
) -> (Vec<ChatMessage>, HashMap<usize, Spans>) {
    let (block, head) = if pinned.is_empty() {
        (
            summary.to_string(),
            vec![(Category::Summary, 0..summary.len())],
        )
    } else {
        let start = summary.len() + PART_SEPARATOR.len();
        (
            format!("{summary}{PART_SEPARATOR}{pinned}"),
            vec![
                (Category::Summary, 0..summary.len()),
                (Category::Manifest, start..start + pinned.len()),
            ],
        )
    };
    let block = block.as_str();
    let mut out = vec![messages[0].clone()];
    let mut new_spans = HashMap::new();
    if let Some(system) = spans.get(&0) {
        new_spans.insert(0, system.clone());
    }
    let rest = match messages.get(cut) {
        Some(ChatMessage::User { content, images }) => {
            let offset = block.len() + PART_SEPARATOR.len();
            let inner = spans
                .get(&cut)
                .cloned()
                .unwrap_or_else(|| restored_spans(content));
            let mut parts = head;
            parts.extend(
                inner
                    .into_iter()
                    .map(|(category, range)| (category, range.start + offset..range.end + offset)),
            );
            new_spans.insert(1, parts);
            out.push(ChatMessage::User {
                content: format!("{block}{PART_SEPARATOR}{content}"),
                images: images.clone(),
            });
            cut + 1
        }
        _ => {
            new_spans.insert(1, head);
            out.push(ChatMessage::user(block));
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
        ChatMessage::user(text)
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
        let (rebuilt, new_spans) = rebuild(&messages, &spans, 5, &block, "");
        assert_eq!(rebuilt.len(), 5);
        let ChatMessage::User { content, .. } = &rebuilt[1] else {
            panic!("a user message")
        };
        assert!(content.starts_with("<conversation-summary>"));
        let parts = &new_spans[&1];
        assert_eq!(parts[0].0, Category::Summary);
        assert_eq!(&content[parts[1].1.clone()], "two");
        assert_eq!(rebuilt[2..], messages[6..]);

        // Cut inside a turn: the summary is a user message of its own before the assistant.
        let (rebuilt, _) = rebuild(&messages, &spans, 6, &block, "");
        assert!(matches!(&rebuilt[1], ChatMessage::User { content, .. } if content == &block));
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
    fn the_trigger_is_kept_in_a_sane_range() {
        assert_eq!(trigger_tokens(10_000, 0.85), 8_500);
        assert_eq!(trigger_tokens(10_000, 0.1), 5_000);
        assert_eq!(trigger_tokens(10_000, 7.0), 10_000);
    }

    #[test]
    fn the_budget_leaves_room_for_the_reply() {
        assert_eq!(budget(1_000), 850);
        assert_eq!(budget(32_768), 27_852);
    }

    fn calling_tool(id: &str, name: &str) -> ChatMessage {
        ChatMessage::Assistant {
            content: None,
            tool_calls: vec![ToolCall {
                id: id.into(),
                kind: "function".into(),
                function: FunctionCall {
                    name: name.into(),
                    arguments: "{}".into(),
                },
            }],
        }
    }

    fn tool_result(id: &str, content: &str) -> ChatMessage {
        ChatMessage::Tool {
            tool_call_id: id.into(),
            content: content.into(),
        }
    }

    /// A conversation with workflow context: a manifest in the first user message, a
    /// `context_load` and a `load_skill` result, and ordinary work.
    fn with_context() -> (Vec<ChatMessage>, HashMap<usize, Spans>, [String; 3]) {
        let manifest =
            "<context-manifest role=\"root\">\n- AGENTS.md (required)\n</context-manifest>";
        let loaded =
            "<loaded reference=\"workflows/AGENTS.leaf.md\">\n# Leaf\nKeep  spacing.\n</loaded>\n";
        let skill = "# Skill: review\nRead the diff first.\n";
        let first = format!("{manifest}{PART_SEPARATOR}fix the parser");
        let spans = HashMap::from([(
            1,
            vec![
                (Category::Manifest, 0..manifest.len()),
                (
                    Category::User,
                    manifest.len() + PART_SEPARATOR.len()..first.len(),
                ),
            ],
        )]);
        let messages = vec![
            ChatMessage::System {
                content: "system prompt\n\nProject instructions: AGENTS.md".into(),
            },
            user(&first),
            calling_tool("a", "context_load"),
            tool_result("a", loaded),
            calling_tool("b", "load_skill"),
            tool_result("b", skill),
            calling("c"),
            tool_result("c", "ordinary output that may be summarised"),
            reply("done"),
            user("next"),
            reply("ok"),
        ];
        (
            messages,
            spans,
            [manifest.into(), loaded.into(), skill.into()],
        )
    }

    #[test]
    fn compaction_keeps_the_system_prompt_and_workflow_context_verbatim() {
        let (messages, spans, [manifest, loaded, skill]) = with_context();
        let cut = 9;
        let pinned = pinned_context(&messages, &spans, cut);
        for text in [&manifest, &loaded, &skill] {
            assert!(pinned.contains(text.as_str()), "{text}");
        }
        assert!(!pinned.contains("ordinary output"));

        let (rebuilt, new_spans) = rebuild(&messages, &spans, cut, &summary_block("s"), &pinned);
        // The system prompt is untouched, byte for byte.
        assert_eq!(rebuilt[0], messages[0]);
        let ChatMessage::User { content, .. } = &rebuilt[1] else {
            panic!("a user message")
        };
        // Workflow context is in the kept message verbatim; older work is only summarised.
        for text in [&manifest, &loaded, &skill] {
            assert!(content.contains(text.as_str()), "{text}");
        }
        assert!(content.contains("<conversation-summary>"));
        assert!(!content.contains("ordinary output"));
        assert!(!content.contains("fix the parser"));
        assert!(new_spans[&1].iter().any(|(c, _)| *c == Category::Manifest));

        // A second compaction carries the same context forward without repeating it.
        let mut again = rebuilt.clone();
        again.push(user("more"));
        again.push(reply("fine"));
        let carried = pinned_context(&again, &new_spans, 3);
        for text in [&manifest, &loaded, &skill] {
            assert_eq!(carried.matches(text.as_str()).count(), 1, "{text}");
        }
        let (second, _) = rebuild(&again, &new_spans, 3, &summary_block("t"), &carried);
        assert_eq!(second[0], messages[0]);
        let ChatMessage::User { content, .. } = &second[1] else {
            panic!("a user message")
        };
        for text in [&manifest, &loaded, &skill] {
            assert_eq!(content.matches(text.as_str()).count(), 1, "{text}");
        }
        // The summariser is not shown the carried context.
        assert!(!transcript(&rebuilt[1..2]).contains("Read the diff first"));
    }

    #[test]
    fn carried_context_survives_restarts_and_repeated_compactions() {
        let (messages, spans, [manifest, loaded, skill]) = with_context();
        let pinned = pinned_context(&messages, &spans, 9);
        let (rebuilt, _) = rebuild(&messages, &spans, 9, &summary_block("s"), &pinned);
        // After a restart the parts are recognised from the text alone.
        let restored = HashMap::new();
        assert_eq!(pinned_context(&rebuilt, &restored, 2), pinned);
        let ChatMessage::User { content, .. } = &rebuilt[1] else {
            panic!("a user message")
        };
        let parts = restored_spans(content);
        let kept = parts
            .iter()
            .find(|(category, _)| *category == Category::Manifest)
            .expect("the kept block is one manifest part");
        assert!(content[kept.1.clone()].ends_with("</context-manifest>"));
        assert!(content[kept.1.clone()].contains(&skill));

        // Twice more, each time from the restored text.
        let mut conversation = rebuilt;
        for round in 0..2 {
            conversation.push(user("more"));
            conversation.push(reply("fine"));
            let carried = pinned_context(&conversation, &restored, conversation.len() - 2);
            assert_eq!(carried, pinned, "round {round}");
            let (next, _) = rebuild(
                &conversation,
                &restored,
                conversation.len() - 2,
                &summary_block("t"),
                &carried,
            );
            assert_eq!(next[0], messages[0]);
            let ChatMessage::User { content, .. } = &next[1] else {
                panic!("a user message")
            };
            for text in [&manifest, &loaded, &skill] {
                assert_eq!(content.matches(text.as_str()).count(), 1, "{text}");
            }
            conversation = next;
        }
    }

    #[test]
    fn only_the_latest_manifest_and_the_fullest_load_are_carried() {
        let old = "<context-manifest role=\"root\">\n- old\n</context-manifest>";
        let new = "<context-manifest role=\"root\">\n- new\n</context-manifest>";
        let small = "<loaded reference=\"a.md\">\nA\n</loaded>\n";
        let big = format!("{small}<loaded reference=\"b.md\">\nB\n</loaded>\n");
        let messages = vec![
            ChatMessage::System {
                content: "sys".into(),
            },
            user(old),
            calling_tool("a", "context_load"),
            tool_result("a", small),
            user(new),
            calling_tool("b", "context_load"),
            tool_result("b", &big),
            reply("done"),
        ];
        let restored = HashMap::from([
            (1, vec![(Category::Manifest, 0..old.len())]),
            (4, vec![(Category::Manifest, 0..new.len())]),
        ]);
        let pinned = pinned_context(&messages, &restored, messages.len());
        assert!(pinned.contains("- new") && !pinned.contains("- old"));
        assert_eq!(pinned.matches("<loaded reference=\"a.md\">").count(), 1);
        assert!(pinned.contains("<loaded reference=\"b.md\">"));
    }

    #[test]
    fn a_conversation_without_workflow_context_pins_nothing() {
        let messages = conversation();
        assert_eq!(pinned_context(&messages, &HashMap::new(), 9), "");
    }
}
