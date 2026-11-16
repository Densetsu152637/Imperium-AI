//! Subagent replay: spawned subagents answer from recordings, with no model call, so a run is
//! deterministic and spends nothing on subagents.

use std::collections::HashSet;
use std::sync::{Mutex, PoisonError};

use imperium_engine::{ReplayAnswer, ReplayRequest, ReplayRole, SubagentReplay};
use imperium_protocol::methods::{EvalAnswered, EvalReplaySummary, EvalReply, EvalRole};

/// Checks the recordings: every reply needs at least one key.
pub(crate) fn validate(replies: &[EvalReply]) -> Result<(), String> {
    for (index, reply) in replies.iter().enumerate() {
        if reply.task_id.is_none()
            && reply.brief.is_none()
            && reply.brief_contains.is_none()
            && reply.role.is_none()
        {
            return Err(format!(
                "recording {index} has no key: give task_id, brief, brief_contains or role"
            ));
        }
    }
    Ok(())
}

fn role_of(role: ReplayRole) -> EvalRole {
    match role {
        ReplayRole::Task => EvalRole::Task,
        ReplayRole::Chunk { .. } => EvalRole::Chunk,
        ReplayRole::Reduce { .. } => EvalRole::Reduce,
    }
}

/// Answers subagents from recordings and keeps what it answered.
#[derive(Debug)]
pub(crate) struct Replay {
    replies: Vec<EvalReply>,
    answered: Mutex<Vec<EvalAnswered>>,
    /// The `once` replies already used.
    consumed: Mutex<HashSet<usize>>,
}

impl Replay {
    pub(crate) fn new(replies: Vec<EvalReply>) -> Self {
        Replay {
            replies,
            answered: Mutex::new(Vec::new()),
            consumed: Mutex::new(HashSet::new()),
        }
    }

    /// The subagents answered so far, in the order they asked.
    pub(crate) fn answered(&self) -> Vec<EvalAnswered> {
        self.answered
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// How many replies there are and which nothing has asked for yet.
    pub(crate) fn summary(&self) -> EvalReplaySummary {
        let used: HashSet<usize> = self.answered().iter().filter_map(|a| a.reply).collect();
        EvalReplaySummary {
            replies: self.replies.len(),
            unused: (0..self.replies.len())
                .filter(|index| !used.contains(index))
                .collect(),
        }
    }

    /// The first reply whose keys all match; a `once` reply already used is passed over.
    /// Finding it uses a `once` reply up.
    fn find(&self, request: &ReplayRequest<'_>) -> Option<usize> {
        let (task_id, brief) = (request.task_id, request.brief);
        let squashed = squash(brief);
        let lowered = brief.to_lowercase();
        let role = role_of(request.role);
        let number = match request.role {
            ReplayRole::Chunk { number, .. } => Some(number),
            _ => None,
        };
        let mut consumed = self.consumed.lock().unwrap_or_else(PoisonError::into_inner);
        let found = self.replies.iter().enumerate().position(|(index, reply)| {
            !(reply.once && consumed.contains(&index))
                && reply.role.is_none_or(|key| key == role)
                && reply.chunk.is_none_or(|key| Some(key) == number)
                && reply.task_id.as_deref().is_none_or(|key| key == task_id)
                && reply
                    .brief
                    .as_deref()
                    .is_none_or(|key| squash(key) == squashed)
                && reply
                    .brief_contains
                    .as_deref()
                    .is_none_or(|key| lowered.contains(&key.to_lowercase()))
        });
        if let Some(index) = found
            && self.replies[index].once
        {
            consumed.insert(index);
        }
        found
    }
}

impl SubagentReplay for Replay {
    fn answer(&self, request: &ReplayRequest<'_>) -> ReplayAnswer {
        let (task_id, brief) = (request.task_id, request.brief);
        let found = self.find(request);
        let answer = match found {
            Some(index) => {
                let reply = &self.replies[index];
                ReplayAnswer {
                    ok: reply.ok,
                    report: reply.report.clone(),
                }
            }
            None => ReplayAnswer {
                ok: false,
                report: format!(
                    "replay error: no recording matches task {task_id} with the brief {:?}",
                    brief.chars().take(200).collect::<String>()
                ),
            },
        };
        self.answered
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(EvalAnswered {
                task_id: task_id.to_string(),
                brief: brief.to_string(),
                role: role_of(request.role),
                reply: found,
                ok: answer.ok,
                report: answer.report.clone(),
            });
        answer
    }
}

/// `text` with every run of blank space made one space.
fn squash(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn recordings(value: serde_json::Value) -> Replay {
        let replies: Vec<EvalReply> = serde_json::from_value(value).unwrap();
        validate(&replies).unwrap();
        Replay::new(replies)
    }

    fn ask(replay: &Replay, task_id: &str, brief: &str) -> ReplayAnswer {
        replay.answer(&ReplayRequest {
            task_id,
            brief,
            role: ReplayRole::Task,
        })
    }

    #[test]
    fn replies_are_found_by_task_id_brief_and_fragment() {
        let replay = recordings(json!([
            {"task_id": "t1", "report": "first"},
            {"brief": "Read   the\nfile", "report": "second"},
            {"brief_contains": "TESTS", "report": "third", "ok": false},
        ]));
        assert_eq!(ask(&replay, "t1", "anything").report, "first");
        assert_eq!(ask(&replay, "t9", "Read the file").report, "second");
        let third = ask(&replay, "t3", "run the tests");
        assert_eq!((third.report.as_str(), third.ok), ("third", false));
        assert!(replay.summary().unused.is_empty());
    }

    #[test]
    fn an_unknown_key_is_an_error_answer_not_a_wait() {
        let replay = recordings(json!([{"task_id": "t1", "report": "x"}]));
        let answer = ask(&replay, "t2", "something else");
        assert!(!answer.ok);
        assert!(
            answer.report.starts_with("replay error:"),
            "{}",
            answer.report
        );
        assert_eq!(replay.answered()[0].reply, None);
        assert_eq!(replay.summary().unused, vec![0]);
    }

    #[test]
    fn a_once_reply_answers_one_subagent_and_is_then_passed_over() {
        let replay = recordings(json!([
            {"brief_contains": "critic", "report": "first look", "once": true},
            {"brief_contains": "critic", "report": "every later look"},
        ]));
        assert_eq!(ask(&replay, "t1", "a critic task").report, "first look");
        assert_eq!(
            ask(&replay, "t2", "a critic task").report,
            "every later look"
        );
        assert_eq!(
            ask(&replay, "t3", "a critic task").report,
            "every later look"
        );
        // Without a later reply, the used-up one leaves the key unknown.
        let alone = recordings(json!([{"task_id": "t1", "report": "x", "once": true}]));
        assert!(ask(&alone, "t1", "a").ok);
        assert!(!ask(&alone, "t1", "a").ok);
    }

    #[test]
    fn chunk_and_reduce_agents_match_on_the_bare_brief() {
        let replay = recordings(json!([
            {"role": "chunk", "chunk": 2, "brief": "list rows", "report": "second"},
            {"role": "chunk", "brief": "list rows", "report": "any chunk"},
            {"role": "reduce", "brief": "merge them", "report": "merged"},
        ]));
        let chunk = |number| {
            replay
                .answer(&ReplayRequest {
                    task_id: "t2",
                    brief: "list rows",
                    role: ReplayRole::Chunk { number, total: 3 },
                })
                .report
        };
        assert_eq!(chunk(2), "second");
        assert_eq!(chunk(1), "any chunk");
        let reduce = replay.answer(&ReplayRequest {
            task_id: "t5",
            brief: "merge them",
            role: ReplayRole::Reduce { chunks: 3 },
        });
        assert_eq!(reduce.report, "merged");
        assert!(!ask(&replay, "t1", "list rows").ok);
    }

    #[test]
    fn a_reply_without_a_key_is_refused() {
        let replies: Vec<EvalReply> =
            serde_json::from_value(json!([{"report": "keyless"}])).unwrap();
        assert!(validate(&replies).is_err());
    }
}
