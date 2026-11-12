//! The initiative tools of an environment's persistent agent: `suggest_work` records a piece
//! of work for the user to start, `work_digest` records the reminder of what is left. Both work
//! only while the daemon's initiative review is running, and both refuse anything without a
//! source, so the agent cannot make up tasks.

use std::sync::Weak;

use imperium_api::{ToolCheck, ToolContext, ToolFuture, ToolProvider, ToolSpec, parse_arguments};
use imperium_protocol::initiative::DigestItem;
use serde::Deserialize;
use serde_json::json;

use crate::DaemonState;

pub(crate) const SUGGEST_WORK: &str = "suggest_work";
pub(crate) const WORK_DIGEST: &str = "work_digest";

const MAX_TITLE: usize = 100;
const MAX_PROMPT: usize = 2000;
const MAX_REASON: usize = 400;
const MAX_SOURCE: usize = 300;
const MAX_ITEMS: usize = 8;

const INSTRUCTIONS: &str = "Initiative: when the daemon sends an \"Initiative review\" message, \
     suggest_work records work for the user to start with one click, and work_digest records a \
     reminder of what is left. They work only during such a review. Every call needs a source \
     you actually read; never make up work.";

#[derive(Debug)]
pub(crate) struct InitiativeTools {
    pub(crate) daemon: Weak<DaemonState>,
    pub(crate) agent_id: String,
}

#[derive(Debug, Deserialize)]
struct SuggestArgs {
    title: String,
    prompt: String,
    reason: String,
    source: String,
}

#[derive(Debug, Deserialize)]
struct DigestArgs {
    summary: String,
    #[serde(default)]
    items: Vec<DigestItemArgs>,
}

#[derive(Debug, Deserialize)]
struct DigestItemArgs {
    text: String,
    source: String,
}

fn field(name: &str, value: &str, max: usize) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(format!(
            "{name} is empty; every suggestion needs a title, prompt, reason and the source you read"
        ));
    }
    if value.chars().count() > max {
        return Err(format!("{name} is over {max} characters; shorten it"));
    }
    Ok(value.to_string())
}

fn checked_suggestion(arguments: &str) -> Result<SuggestArgs, String> {
    let args: SuggestArgs = parse_arguments(arguments)?;
    Ok(SuggestArgs {
        title: field("title", &args.title, MAX_TITLE)?,
        prompt: field("prompt", &args.prompt, MAX_PROMPT)?,
        reason: field("reason", &args.reason, MAX_REASON)?,
        source: field("source", &args.source, MAX_SOURCE)?,
    })
}

fn checked_digest(arguments: &str) -> Result<(String, Vec<DigestItem>), String> {
    let args: DigestArgs = parse_arguments(arguments)?;
    if args.items.len() > MAX_ITEMS {
        return Err(format!(
            "at most {MAX_ITEMS} items; keep the most important"
        ));
    }
    let mut items = Vec::new();
    for item in &args.items {
        items.push(DigestItem {
            text: field("item text", &item.text, MAX_REASON)?,
            source: field("item source", &item.source, MAX_SOURCE)?,
        });
    }
    Ok((field("summary", &args.summary, MAX_REASON)?, items))
}

impl ToolProvider for InitiativeTools {
    fn for_subagents(&self) -> bool {
        false
    }

    fn instructions(&self) -> Option<&str> {
        Some(INSTRUCTIONS)
    }

    fn specs(&self) -> Vec<ToolSpec> {
        vec![
            ToolSpec {
                name: SUGGEST_WORK.into(),
                description: "During an initiative review: record one piece of work for the user to start with one click. `prompt` is what the new session is told, worded as the user would write it (\"improve my timeline\"). `source` is what you read that points to it.".into(),
                parameters: json!({"type": "object", "properties": {
                    "title": {"type": "string", "description": "A short name for the work"},
                    "prompt": {"type": "string", "description": "What the session is told, as the user would word it"},
                    "reason": {"type": "string", "description": "Why now, in a sentence"},
                    "source": {"type": "string", "description": "File and heading, issue number or session id you read"}
                }, "required": ["title", "prompt", "reason", "source"]}),
            },
            ToolSpec {
                name: WORK_DIGEST.into(),
                description: "During an initiative review: record the reminder of work still left in the roadmap or plan documents and unfinished sessions.".into(),
                parameters: json!({"type": "object", "properties": {
                    "summary": {"type": "string", "description": "One or two sentences"},
                    "items": {"type": "array", "items": {"type": "object", "properties": {
                        "text": {"type": "string"},
                        "source": {"type": "string", "description": "Where it comes from"}
                    }, "required": ["text", "source"]}}
                }, "required": ["summary", "items"]}),
            },
        ]
    }

    fn handles(&self, name: &str) -> bool {
        matches!(name, SUGGEST_WORK | WORK_DIGEST)
    }

    fn check(&self, name: &str, arguments: &str) -> Result<ToolCheck, String> {
        let summary = match name {
            SUGGEST_WORK => format!("suggest: {}", checked_suggestion(arguments)?.title),
            WORK_DIGEST => {
                let (_, items) = checked_digest(arguments)?;
                format!("record the work-left reminder ({} items)", items.len())
            }
            other => return Err(format!("unknown tool {other}")),
        };
        Ok(ToolCheck {
            summary,
            ask: false,
            needs_context: false,
            command: None,
        })
    }

    fn call<'a>(
        &'a self,
        _context: &'a ToolContext<'a>,
        name: &'a str,
        arguments: &'a str,
    ) -> ToolFuture<'a> {
        Box::pin(async move {
            let daemon = self
                .daemon
                .upgrade()
                .ok_or_else(|| "the daemon is stopping".to_string())?;
            let book = daemon.initiative();
            match name {
                SUGGEST_WORK => {
                    let args = checked_suggestion(arguments)?;
                    let suggestion = book.add_suggestion(
                        &self.agent_id,
                        args.title,
                        args.prompt,
                        args.reason,
                        args.source,
                    )?;
                    Ok(format!(
                        "Recorded suggestion {}: {}. The user sees it in the environment's panel \
                         and starts it with one click.",
                        suggestion.id, suggestion.title
                    ))
                }
                WORK_DIGEST => {
                    let (summary, items) = checked_digest(arguments)?;
                    book.set_digest(&self.agent_id, summary, items)?;
                    Ok("Recorded the work-left reminder for the user.".to_string())
                }
                other => Err(format!("unknown tool {other}")),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_suggestion_needs_every_field_and_a_source() {
        let ok = r#"{"title":"Tidy","prompt":"tidy the roadmap","reason":"it drifted","source":"docs/12-roadmap.md#next"}"#;
        assert_eq!(
            checked_suggestion(ok).unwrap().source,
            "docs/12-roadmap.md#next"
        );
        let no_source =
            r#"{"title":"Tidy","prompt":"tidy the roadmap","reason":"it drifted","source":"  "}"#;
        let error = checked_suggestion(no_source).unwrap_err();
        assert!(error.contains("source is empty"), "{error}");
        assert!(checked_suggestion(r#"{"title":"Tidy"}"#).is_err());
    }

    #[test]
    fn a_digest_item_needs_a_source_and_the_list_is_capped() {
        let one = r#"{"summary":"2 left","items":[{"text":"finish X","source":"PLAN.md"}]}"#;
        assert_eq!(checked_digest(one).unwrap().1.len(), 1);
        let none = r#"{"summary":"2 left","items":[{"text":"finish X","source":""}]}"#;
        assert!(checked_digest(none).is_err());
        let item = r#"{"text":"a","source":"b"}"#;
        let many = format!(
            r#"{{"summary":"s","items":[{}]}}"#,
            [item; MAX_ITEMS + 1].join(",")
        );
        assert!(checked_digest(&many).unwrap_err().contains("at most"));
    }
}
