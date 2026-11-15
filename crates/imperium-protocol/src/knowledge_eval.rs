//! The knowledge-agent evaluation: does a model ask a knowledge agent instead of reading again.
//!
//! `schemas/eval/knowledge-cases.json` holds messages for a session that has knowledge agents
//! (docs/21-knowledge-agents.md). In some the answer is in what a knowledge agent already read
//! (`should_ask`), so a good agent calls `ask_agent` and does not read, search or list those
//! files itself; in the others it is not, so it reads or searches and has nothing to ask.
//! [`Observed`] is what an agent did, read from the events of its turn; [`score`] turns the pair
//! into a reward in `-1..=1`. The cases are a hold-out for measuring a trained model: do not
//! train on them.
//!
//! | Case | Behaviour | Reward |
//! | --- | --- | --- |
//! | should ask | asks and reads none of the covered files | 1.0 |
//! | should ask | asks, and reads a covered file too | 0.5 |
//! | should ask | reads a covered file without asking | -1.0 |
//! | should not ask | does not ask | 1.0 |
//! | should not ask | asks anyway | -0.5 |

use serde::{Deserialize, Serialize};

use crate::session::SessionEvent;

/// The tool whose use the evaluation looks for.
pub const ASK_TOOL: &str = "ask_agent";

/// The tools that read what a knowledge agent already read.
const READ_TOOLS: [&str; 4] = ["read_file", "grep", "glob", "list_dir"];

/// One evaluation case.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeCase {
    pub id: String,
    /// What the agent can see besides the message, one line each, for whoever builds the prompt.
    #[serde(default)]
    pub context: Vec<String>,
    pub message: String,
    /// Paths a knowledge agent read: a read of one of them is a re-read.
    #[serde(default)]
    pub covered: Vec<String>,
    /// The answer is in what a knowledge agent read.
    pub should_ask: bool,
}

/// The cases checked in under `schemas/eval/`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeCases {
    pub cases: Vec<KnowledgeCase>,
}

/// The checked-in cases.
pub fn cases() -> Vec<KnowledgeCase> {
    serde_json::from_str::<KnowledgeCases>(include_str!("../schemas/eval/knowledge-cases.json"))
        .map(|file| file.cases)
        .unwrap_or_default()
}

/// What an agent did on one case.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Observed {
    /// It called `ask_agent`.
    pub asked: bool,
    /// It read, searched or listed something the case says a knowledge agent read.
    pub reread: bool,
}

impl Observed {
    /// Reads what the agent did out of a turn's events (its tool calls).
    pub fn from_events<'a>(
        case: &KnowledgeCase,
        events: impl IntoIterator<Item = &'a SessionEvent>,
    ) -> Observed {
        let mut observed = Observed::default();
        for event in events {
            let SessionEvent::ToolStarted {
                tool, arguments, ..
            } = event
            else {
                continue;
            };
            if tool == ASK_TOOL {
                observed.asked = true;
            } else if READ_TOOLS.contains(&tool.as_str())
                && let Some(arguments) = arguments
                && case.covered.iter().any(|path| arguments.contains(path))
            {
                observed.reread = true;
            }
        }
        observed
    }
}

/// How one case went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Should ask, asked, and read nothing covered.
    CorrectAsk,
    /// Should ask, asked, and read a covered file as well.
    AskedAndReread,
    /// Should ask, and read a covered file instead.
    Reread,
    /// Should not ask, and did not.
    CorrectRead,
    /// Should not ask, and did.
    NeedlessAsk,
}

/// The reward and verdict for one case.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CaseScore {
    pub verdict: Verdict,
    pub reward: f64,
}

/// Scores what an agent did on `case`.
pub fn score(case: &KnowledgeCase, observed: Observed) -> CaseScore {
    let (verdict, reward) = match (case.should_ask, observed.asked, observed.reread) {
        (true, true, false) => (Verdict::CorrectAsk, 1.0),
        (true, true, true) => (Verdict::AskedAndReread, 0.5),
        (true, false, _) => (Verdict::Reread, -1.0),
        (false, false, _) => (Verdict::CorrectRead, 1.0),
        (false, true, _) => (Verdict::NeedlessAsk, -0.5),
    };
    CaseScore { verdict, reward }
}

/// The mean reward over scored cases; 0 for none.
pub fn mean_reward(scores: &[CaseScore]) -> f64 {
    scores.iter().map(|score| score.reward).sum::<f64>() / scores.len().max(1) as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn started(tool: &str, arguments: &str) -> SessionEvent {
        SessionEvent::ToolStarted {
            call_id: "c".into(),
            tool: tool.into(),
            summary: String::new(),
            arguments: Some(arguments.into()),
        }
    }

    fn case(id: &str) -> KnowledgeCase {
        cases().into_iter().find(|case| case.id == id).unwrap()
    }

    #[test]
    fn the_checked_in_cases_are_well_formed() {
        let cases = cases();
        assert!(cases.len() >= 6);
        let mut ids: Vec<&str> = cases.iter().map(|case| case.id.as_str()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), cases.len(), "ids are unique");
        assert!(cases.iter().any(|case| case.should_ask));
        assert!(cases.iter().any(|case| !case.should_ask));
        // A case that should be asked says what a knowledge agent read.
        assert!(
            cases
                .iter()
                .filter(|case| case.should_ask)
                .all(|case| !case.covered.is_empty() && !case.context.is_empty())
        );
    }

    #[test]
    fn asking_beats_reading_again() {
        let case = case("asks-about-what-t1-read");
        let asked = [started(ASK_TOOL, r#"{"agent":"t1","question":"..."}"#)];
        let reread = [started("read_file", r#"{"path":"src/parser.rs"}"#)];
        let both = [asked[0].clone(), reread[0].clone()];
        let elsewhere = [started("read_file", r#"{"path":"src/other.rs"}"#)];
        let reward = |events: &[SessionEvent]| score(&case, Observed::from_events(&case, events));
        assert_eq!(reward(&asked).verdict, Verdict::CorrectAsk);
        assert_eq!(reward(&both).verdict, Verdict::AskedAndReread);
        assert_eq!(reward(&reread).verdict, Verdict::Reread);
        assert_eq!(reward(&elsewhere).verdict, Verdict::Reread, "no ask at all");
        assert!(reward(&asked).reward > reward(&both).reward);
        assert!(reward(&both).reward > reward(&reread).reward);
    }

    #[test]
    fn asking_when_there_is_nothing_to_ask_costs() {
        let case = case("reads-what-no-one-read");
        let read = [started("read_file", r#"{"path":"src/config.rs"}"#)];
        let asked = [started(ASK_TOOL, r#"{"agent":"t1","question":"..."}"#)];
        assert_eq!(
            score(&case, Observed::from_events(&case, &read)).verdict,
            Verdict::CorrectRead
        );
        let wrong = score(&case, Observed::from_events(&case, &asked));
        assert_eq!((wrong.verdict, wrong.reward), (Verdict::NeedlessAsk, -0.5));
        let rewards = [score(&case, Observed::from_events(&case, &read)), wrong];
        assert_eq!(mean_reward(&rewards), 0.25);
        assert_eq!(mean_reward(&[]), 0.0);
    }
}
