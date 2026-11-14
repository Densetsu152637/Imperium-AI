//! The clarification evaluation: how well a model asks instead of guessing.
//!
//! `schemas/eval/clarification-cases.json` holds messages that are either ambiguous (nothing in
//! the stated context settles what the user means, so a good agent calls `request_clarification`)
//! or resolvable (the context does settle it, so a good agent just acts). [`Observed`] is what
//! an agent did on one case, read from the events of its turn; [`score`] turns the pair into a
//! reward in `-1..=1` and [`report`] summarises a run. The same reward is what the Emperor
//! training repository optimises: it vendors this folder and reuses the cases and the table
//! below.
//!
//! | Case | Behaviour | Reward |
//! | --- | --- | --- |
//! | ambiguous | asks | 0.6, plus 0.2 for naming the missing parameter and 0.2 for listing an expected option |
//! | ambiguous | acts without asking (guesses) | -1.0 |
//! | resolvable | acts | 1.0 |
//! | resolvable | asks anyway | -0.5 |

use serde::{Deserialize, Serialize};

use crate::session::SessionEvent;

/// The name of the tool whose use the evaluation looks for.
pub const CLARIFICATION_TOOL: &str = "request_clarification";

/// One evaluation case.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClarificationCase {
    pub id: String,
    /// What the agent can see besides the message: the session state and the recent
    /// conversation, one line each, for whoever builds the prompt.
    #[serde(default)]
    pub context: Vec<String>,
    pub message: String,
    /// True when nothing in `context` settles what the user means.
    pub ambiguous: bool,
    /// For an ambiguous case: a word the missing parameter should contain.
    #[serde(default)]
    pub missing: Option<String>,
    /// For an ambiguous case: answers a good clarification lists (any one of them will do).
    #[serde(default)]
    pub options: Vec<String>,
}

/// The cases checked in under `schemas/eval/`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClarificationCases {
    pub cases: Vec<ClarificationCase>,
}

/// The checked-in cases.
pub fn cases() -> Vec<ClarificationCase> {
    serde_json::from_str::<ClarificationCases>(include_str!(
        "../schemas/eval/clarification-cases.json"
    ))
    .map(|file| file.cases)
    .unwrap_or_default()
}

/// What an agent did on one case.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Observed {
    /// It called `request_clarification`.
    pub asked: bool,
    /// The `missing_parameter` it named.
    pub missing_parameter: String,
    /// The `options` it listed.
    pub options: Vec<String>,
}

impl Observed {
    /// Reads the first clarification out of a turn's events (its tool call's arguments).
    pub fn from_events<'a>(events: impl IntoIterator<Item = &'a SessionEvent>) -> Observed {
        for event in events {
            let SessionEvent::ToolStarted {
                tool, arguments, ..
            } = event
            else {
                continue;
            };
            if tool != CLARIFICATION_TOOL {
                continue;
            }
            let parsed: serde_json::Value = arguments
                .as_deref()
                .and_then(|text| serde_json::from_str(text).ok())
                .unwrap_or_default();
            return Observed {
                asked: true,
                missing_parameter: parsed["missing_parameter"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                options: parsed["options"]
                    .as_array()
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(|item| item.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default(),
            };
        }
        Observed::default()
    }
}

/// How one case went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Ambiguous, and it asked.
    CorrectAsk,
    /// Resolvable, and it acted.
    CorrectAct,
    /// Ambiguous, and it acted without asking.
    Guessed,
    /// Resolvable, and it asked anyway.
    NeedlessAsk,
}

/// The reward and verdict for one case.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CaseScore {
    pub verdict: Verdict,
    pub reward: f64,
}

/// Scores what an agent did on `case`.
pub fn score(case: &ClarificationCase, observed: &Observed) -> CaseScore {
    match (case.ambiguous, observed.asked) {
        (true, true) => {
            let named = case.missing.as_ref().is_none_or(|word| {
                observed
                    .missing_parameter
                    .to_lowercase()
                    .contains(&word.to_lowercase())
            });
            let listed = case.options.is_empty()
                || case.options.iter().any(|expected| {
                    observed
                        .options
                        .iter()
                        .any(|option| option.to_lowercase().contains(&expected.to_lowercase()))
                });
            CaseScore {
                verdict: Verdict::CorrectAsk,
                reward: 0.6 + if named { 0.2 } else { 0.0 } + if listed { 0.2 } else { 0.0 },
            }
        }
        (true, false) => CaseScore {
            verdict: Verdict::Guessed,
            reward: -1.0,
        },
        (false, false) => CaseScore {
            verdict: Verdict::CorrectAct,
            reward: 1.0,
        },
        (false, true) => CaseScore {
            verdict: Verdict::NeedlessAsk,
            reward: -0.5,
        },
    }
}

/// A run over many cases.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Report {
    pub cases: usize,
    pub correct_asks: usize,
    pub correct_acts: usize,
    pub guesses: usize,
    pub needless_asks: usize,
    /// Mean reward over all cases, `-1..=1`.
    pub mean_reward: f64,
    /// Share of ambiguous cases where it asked; 1 when there are none.
    pub ask_recall: f64,
    /// Share of its asks that were on ambiguous cases; 1 when it never asked.
    pub ask_precision: f64,
}

/// Summarises scored cases.
pub fn report(scores: &[CaseScore]) -> Report {
    let count = |verdict: Verdict| scores.iter().filter(|s| s.verdict == verdict).count();
    let (correct_asks, correct_acts) = (count(Verdict::CorrectAsk), count(Verdict::CorrectAct));
    let (guesses, needless_asks) = (count(Verdict::Guessed), count(Verdict::NeedlessAsk));
    let ratio = |part: usize, whole: usize| {
        if whole == 0 {
            1.0
        } else {
            part as f64 / whole as f64
        }
    };
    let total: f64 = scores.iter().map(|s| s.reward).sum();
    Report {
        cases: scores.len(),
        correct_asks,
        correct_acts,
        guesses,
        needless_asks,
        mean_reward: if scores.is_empty() {
            0.0
        } else {
            total / scores.len() as f64
        },
        ask_recall: ratio(correct_asks, correct_asks + guesses),
        ask_precision: ratio(correct_asks, correct_asks + needless_asks),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asked(missing: &str, options: &[&str]) -> Observed {
        Observed {
            asked: true,
            missing_parameter: missing.into(),
            options: options.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn case(id: &str) -> ClarificationCase {
        cases()
            .into_iter()
            .find(|case| case.id == id)
            .unwrap_or_else(|| panic!("case {id}"))
    }

    #[test]
    fn the_checked_in_cases_load_and_cover_both_kinds() {
        let all = cases();
        assert!(all.len() >= 12, "{} cases", all.len());
        assert!(all.iter().any(|case| case.ambiguous));
        assert!(all.iter().any(|case| !case.ambiguous));
        let mut ids: Vec<_> = all.iter().map(|case| case.id.as_str()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), all.len(), "ids are unique");
        for case in &all {
            assert!(!case.message.trim().is_empty(), "{}", case.id);
            if !case.ambiguous {
                assert!(
                    case.missing.is_none() && case.options.is_empty(),
                    "{}",
                    case.id
                );
            }
        }
    }

    #[test]
    fn asking_well_on_an_ambiguous_case_scores_one() {
        let case = case("that-file-two-candidates");
        let good = score(&case, &asked("which file", &["src/main.rs", "src/lib.rs"]));
        assert_eq!(good.verdict, Verdict::CorrectAsk);
        assert!((good.reward - 1.0).abs() < 1e-9);
        // Asking without naming the parameter or the options still beats guessing.
        let vague = score(&case, &asked("clarification", &[]));
        assert!((vague.reward - 0.6).abs() < 1e-9);
    }

    #[test]
    fn guessing_and_needless_questions_are_penalised() {
        let ambiguous = case("earlier-issue-no-history");
        let guess = score(&ambiguous, &Observed::default());
        assert_eq!(guess.verdict, Verdict::Guessed);
        assert!((guess.reward + 1.0).abs() < 1e-9);
        let resolvable = case("that-file-one-candidate");
        let needless = score(&resolvable, &asked("which file", &[]));
        assert_eq!(needless.verdict, Verdict::NeedlessAsk);
        assert!((needless.reward + 0.5).abs() < 1e-9);
        let act = score(&resolvable, &Observed::default());
        assert_eq!(act.verdict, Verdict::CorrectAct);
        assert!((act.reward - 1.0).abs() < 1e-9);
    }

    #[test]
    fn a_report_separates_recall_from_precision() {
        let ambiguous = case("that-file-two-candidates");
        let resolvable = case("that-file-one-candidate");
        let scores = [
            score(&ambiguous, &asked("which file", &["src/lib.rs"])),
            score(&ambiguous, &Observed::default()),
            score(&resolvable, &asked("which file", &[])),
            score(&resolvable, &Observed::default()),
        ];
        let summary = report(&scores);
        assert_eq!(summary.cases, 4);
        assert!((summary.ask_recall - 0.5).abs() < 1e-9);
        assert!((summary.ask_precision - 0.5).abs() < 1e-9);
        assert!(report(&[]).mean_reward.abs() < 1e-9);
    }

    #[test]
    fn observations_come_from_the_clarification_tool_call() {
        let started = |tool: &str, arguments: Option<&str>| SessionEvent::ToolStarted {
            call_id: "c".into(),
            tool: tool.into(),
            summary: String::new(),
            arguments: arguments.map(str::to_string),
        };
        let events = [
            started("read_file", Some("{}")),
            started(
                CLARIFICATION_TOOL,
                Some(r#"{"missing_parameter":"which file","options":["a.rs","b.rs"]}"#),
            ),
        ];
        let observed = Observed::from_events(&events);
        assert_eq!(observed, asked("which file", &["a.rs", "b.rs"]));
        assert_eq!(
            Observed::from_events(&[started("read_file", None)]),
            Observed::default()
        );
        // A call whose arguments are lost still counts as asking.
        assert!(Observed::from_events(&[started(CLARIFICATION_TOOL, None)]).asked);
    }
}
