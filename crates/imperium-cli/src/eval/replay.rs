//! Subagent replay: spawned subagents answer from a recordings file, with no model call, so a
//! run is deterministic and spends nothing on subagents.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Mutex;

use anyhow::{Context, bail};
use imperium_engine::{ReplayAnswer, SubagentReplay};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// A recordings file: the replies subagents give, in the order they are looked up.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Recordings {
    /// The first entry whose keys all match answers; later entries are not looked at.
    pub replies: Vec<Reply>,
}

/// One recorded subagent report and the keys that select it. At least one key is required; when
/// several are given, all of them must match.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Reply {
    /// The subagent's task id: `t1`, `t2`, ... in the order the root spawns them.
    #[serde(default)]
    pub task_id: Option<String>,
    /// The brief, exactly (blank space is not compared).
    #[serde(default)]
    pub brief: Option<String>,
    /// Text the brief must contain (compared without regard to case).
    #[serde(default)]
    pub brief_contains: Option<String>,
    /// What the subagent reports back.
    pub report: String,
    /// Whether the subagent counts as having finished its task; false makes the root read it
    /// as a failed task. Defaults to true.
    #[serde(default = "yes")]
    pub ok: bool,
}

fn yes() -> bool {
    true
}

impl Recordings {
    /// Reads recordings from a file: a `{"replies": [...]}` object, or any object that holds one
    /// under `subagent_replay` (so a task file can carry its own recordings).
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("could not read the recordings {}", path.display()))?;
        let value: Value = serde_json::from_str(&text)
            .with_context(|| format!("{} is not valid JSON", path.display()))?;
        Self::from_value(value).with_context(|| format!("recordings {}", path.display()))
    }

    /// The recordings in `value`, which is a recordings object or holds one under
    /// `subagent_replay`.
    pub fn from_value(value: Value) -> anyhow::Result<Self> {
        let value = match value {
            Value::Object(mut map) if !map.contains_key("replies") => map
                .remove("subagent_replay")
                .context("expected {\"replies\": [...]} or a \"subagent_replay\" object")?,
            other => other,
        };
        let recordings: Recordings =
            serde_json::from_value(value).context("the recordings are malformed")?;
        for (index, reply) in recordings.replies.iter().enumerate() {
            if reply.task_id.is_none() && reply.brief.is_none() && reply.brief_contains.is_none() {
                bail!("reply {index} has no key: give task_id, brief or brief_contains");
            }
        }
        Ok(recordings)
    }
}

/// One subagent the replay answered, for the run's output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answered {
    pub task_id: String,
    pub brief: String,
    /// The index of the reply that answered, or `None` for an unknown key.
    pub reply: Option<usize>,
    pub ok: bool,
    pub report: String,
}

/// Answers subagents from [`Recordings`] and keeps what it answered.
#[derive(Debug)]
pub struct Replay {
    recordings: Recordings,
    answered: Mutex<Vec<Answered>>,
}

impl Replay {
    pub fn new(recordings: Recordings) -> Self {
        Replay {
            recordings,
            answered: Mutex::new(Vec::new()),
        }
    }

    /// The subagents answered so far, in the order they asked.
    pub fn answered(&self) -> Vec<Answered> {
        self.answered
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// The replies nothing has asked for yet, as indexes into the file.
    pub fn unused(&self) -> Vec<usize> {
        let used: HashSet<usize> = self.answered().iter().filter_map(|a| a.reply).collect();
        (0..self.recordings.replies.len())
            .filter(|index| !used.contains(index))
            .collect()
    }

    /// The recordings as a JSON summary: how many replies there are, and which went unused.
    pub fn summary(&self) -> Value {
        json!({
            "replies": self.recordings.replies.len(),
            "unused": self.unused(),
        })
    }

    fn find(&self, task_id: &str, brief: &str) -> Option<usize> {
        let squashed = squash(brief);
        let lowered = brief.to_lowercase();
        self.recordings.replies.iter().position(|reply| {
            reply.task_id.as_deref().is_none_or(|key| key == task_id)
                && reply
                    .brief
                    .as_deref()
                    .is_none_or(|key| squash(key) == squashed)
                && reply
                    .brief_contains
                    .as_deref()
                    .is_none_or(|key| lowered.contains(&key.to_lowercase()))
        })
    }
}

impl SubagentReplay for Replay {
    fn answer(&self, task_id: &str, brief: &str) -> ReplayAnswer {
        let found = self.find(task_id, brief);
        let answer = match found {
            Some(index) => {
                let reply = &self.recordings.replies[index];
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
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(Answered {
                task_id: task_id.to_string(),
                brief: brief.to_string(),
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
    use super::*;

    fn recordings(value: Value) -> Replay {
        Replay::new(Recordings::from_value(value).unwrap())
    }

    #[test]
    fn replies_are_found_by_task_id_brief_and_fragment() {
        let replay = recordings(json!({"replies": [
            {"task_id": "t1", "report": "first"},
            {"brief": "Read   the\nfile", "report": "second"},
            {"brief_contains": "TESTS", "report": "third", "ok": false},
        ]}));
        assert_eq!(replay.answer("t1", "anything").report, "first");
        assert_eq!(replay.answer("t9", "Read the file").report, "second");
        let third = replay.answer("t3", "run the tests");
        assert_eq!((third.report.as_str(), third.ok), ("third", false));
        assert!(replay.unused().is_empty());
    }

    #[test]
    fn an_unknown_key_is_an_error_answer_not_a_wait() {
        let replay = recordings(json!({"replies": [{"task_id": "t1", "report": "x"}]}));
        let answer = replay.answer("t2", "something else");
        assert!(!answer.ok);
        assert!(
            answer.report.starts_with("replay error:"),
            "{}",
            answer.report
        );
        assert_eq!(replay.answered()[0].reply, None);
        assert_eq!(replay.unused(), vec![0]);
    }

    #[test]
    fn a_task_file_can_carry_its_recordings() {
        let value = json!({"prompt": "x", "subagent_replay": {"replies": [
            {"task_id": "t1", "report": "r"}
        ]}});
        assert_eq!(Recordings::from_value(value).unwrap().replies.len(), 1);
        assert!(Recordings::from_value(json!({"prompt": "x"})).is_err());
        assert!(Recordings::from_value(json!({"replies": [{"report": "keyless"}]})).is_err());
    }
}
