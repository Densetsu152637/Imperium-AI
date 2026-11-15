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
                && case
                    .covered
                    .iter()
                    .any(|covered| rereads(tool, arguments, covered))
            {
                observed.reread = true;
            }
        }
        observed
    }
}

/// Whether a call of `tool` with these JSON `arguments` reads the `covered` file again:
/// `read_file` of it, `list_dir` or `grep` over it or a folder holding it (a `grep`'s `glob`
/// must match its name), or `glob` with a pattern that matches it under the folder searched.
fn rereads(tool: &str, arguments: &str, covered: &str) -> bool {
    let Ok(serde_json::Value::Object(arguments)) = serde_json::from_str(arguments) else {
        // Arguments that are not a JSON object: fall back to looking for the path in the text.
        return arguments.replace('\\', "/").contains(&normalise(covered));
    };
    let text = |key: &str| arguments.get(key).and_then(serde_json::Value::as_str);
    let covered = normalise(covered);
    // The folder or file named; the project root when none.
    let Some(rest) = rest_under(&normalise(text("path").unwrap_or("")), &covered) else {
        return false;
    };
    match tool {
        "read_file" => text("path").is_some() && rest.is_empty(),
        "grep" => text("glob").is_none_or(|glob| glob_matches(glob, &rest)),
        "glob" => text("pattern").is_some_and(|pattern| glob_matches(pattern, &rest)),
        _ => true,
    }
}

/// A path with `/` separators, no `./` start and no trailing `/`.
fn normalise(path: &str) -> String {
    let path = path.replace('\\', "/");
    let segments: Vec<&str> = path
        .split('/')
        .filter(|segment| !segment.is_empty() && *segment != ".")
        .collect();
    let joined = segments.join("/");
    // An absolute path keeps its first slash only so a root alone is not empty.
    if path.starts_with('/') && !joined.is_empty() {
        format!("/{joined}")
    } else {
        joined
    }
}

/// What lies below `folder` on the way to `file` ("" when they are the same path), when `file`
/// is in it. An empty folder is the project root, which holds everything. One path may be
/// absolute and the other relative to the project, so leading segments of either may be dropped.
fn rest_under(folder: &str, file: &str) -> Option<String> {
    if folder.is_empty() {
        return Some(file.trim_start_matches('/').to_string());
    }
    let folder: Vec<&str> = folder.split('/').filter(|part| !part.is_empty()).collect();
    let file: Vec<&str> = file.split('/').filter(|part| !part.is_empty()).collect();
    for skip in 0..folder.len() {
        if file.starts_with(&folder[skip..]) {
            return Some(file[folder.len() - skip..].join("/"));
        }
    }
    for skip in 0..file.len() {
        if file[skip..].starts_with(&folder) {
            return Some(file[skip + folder.len()..].join("/"));
        }
    }
    None
}

/// Whether `glob` (`*`, `**`, `?`, `{a,b}`) matches `rest`: a glob without `/` matches the
/// file's name at any depth, one with it the path below the folder.
fn glob_matches(glob: &str, rest: &str) -> bool {
    let glob = glob.replace('\\', "/");
    let subject = if glob.contains('/') {
        rest
    } else {
        rest.rsplit('/').next().unwrap_or(rest)
    };
    let glob = glob.trim_start_matches("./");
    expand_braces(glob)
        .iter()
        .any(|alternative| wildcard(alternative.as_bytes(), subject.as_bytes()))
}

fn expand_braces(glob: &str) -> Vec<String> {
    let Some(open) = glob.find('{') else {
        return vec![glob.to_string()];
    };
    let Some(close) = glob[open..].find('}').map(|at| open + at) else {
        return vec![glob.to_string()];
    };
    let (head, tail) = (&glob[..open], &glob[close + 1..]);
    glob[open + 1..close]
        .split(',')
        .flat_map(|choice| expand_braces(&format!("{head}{choice}{tail}")))
        .collect()
}

fn wildcard(pattern: &[u8], text: &[u8]) -> bool {
    match pattern {
        [] => text.is_empty(),
        [b'*', b'*', rest @ ..] => {
            let rest = rest.strip_prefix(b"/").unwrap_or(rest);
            (0..=text.len()).any(|at| wildcard(rest, &text[at..]))
        }
        [b'*', rest @ ..] => {
            let reach = text
                .iter()
                .position(|byte| *byte == b'/')
                .unwrap_or(text.len());
            (0..=reach).any(|at| wildcard(rest, &text[at..]))
        }
        [b'?', rest @ ..] => text
            .split_first()
            .is_some_and(|(byte, tail)| *byte != b'/' && wildcard(rest, tail)),
        [byte, rest @ ..] => text
            .split_first()
            .is_some_and(|(head, tail)| head == byte && wildcard(rest, tail)),
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
    fn a_re_read_is_found_whatever_the_path_looks_like() {
        let case = case("asks-about-what-t1-read");
        let rereads = |tool: &str, arguments: &str| {
            Observed::from_events(&case, [&started(tool, arguments)]).reread
        };
        // Separators, a leading ./, and absolute or relative forms of the same file.
        assert!(rereads("read_file", r#"{"path":"src\\parser.rs"}"#));
        assert!(rereads("read_file", r#"{"path":"./src/parser.rs"}"#));
        assert!(rereads(
            "read_file",
            r#"{"path":"/work/project/src/parser.rs"}"#
        ));
        assert!(rereads(
            "read_file",
            r#"{"path":"C:\\work\\src\\lexer.rs"}"#
        ));
        assert!(!rereads("read_file", r#"{"path":"src/parser_tests.rs"}"#));
        assert!(
            !rereads("read_file", r#"{"path":"src"}"#),
            "a folder is not a file read"
        );
        // A listing, search or glob over a folder that holds a covered file.
        assert!(rereads("list_dir", r#"{"path":"src"}"#));
        assert!(rereads("list_dir", r#"{"path":"/work/project/docs"}"#));
        assert!(!rereads("list_dir", r#"{"path":"tests"}"#));
        assert!(
            rereads("grep", r#"{"pattern":"fn parse"}"#),
            "the project root covers it"
        );
        assert!(rereads("grep", r#"{"pattern":"x","path":"src"}"#));
        assert!(rereads("grep", r#"{"pattern":"x","path":"src/parser.rs"}"#));
        assert!(rereads(
            "grep",
            r#"{"pattern":"x","path":"src","glob":"*.rs"}"#
        ));
        assert!(!rereads(
            "grep",
            r#"{"pattern":"x","path":"src","glob":"*.toml"}"#
        ));
        assert!(!rereads("grep", r#"{"pattern":"x","path":"tests"}"#));
        assert!(rereads("glob", r#"{"pattern":"**/*.rs"}"#));
        assert!(rereads("glob", r#"{"pattern":"src/*.{rs,toml}"}"#));
        assert!(rereads("glob", r#"{"pattern":"*.rs","path":"src"}"#));
        assert!(rereads("glob", r#"{"pattern":"parser.rs"}"#));
        assert!(!rereads("glob", r#"{"pattern":"*.toml"}"#));
        assert!(!rereads("glob", r#"{"pattern":"*.rs","path":"tests"}"#));
        // Other tools never count, and arguments that are not JSON fall back to the text.
        assert!(!rereads(
            "run_command",
            r#"{"command":"cat src/parser.rs"}"#
        ));
        assert!(rereads("read_file", "src/parser.rs"));
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
