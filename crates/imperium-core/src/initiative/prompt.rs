//! The message an environment's persistent agent gets for an initiative review: a template
//! (`review.txt`, with the mode's rules and the digest task from the neighbouring files) filled
//! with facts the daemon knows. The agent reads the environment's documents itself.

use imperium_protocol::initiative::Initiative;

const REVIEW: &str = include_str!("review.txt");
const SUGGEST: &str = include_str!("suggest.txt");
const ACT: &str = include_str!("act.txt");
const DIGEST: &str = include_str!("digest.txt");

/// The start of every review message; the agent's instructions name it.
#[cfg(test)]
pub(crate) const MARKER: &str = "Initiative review for the environment";

const REVIEW_TASK: &str = "- Review: find the most valuable next work (see \"Good work to look for\" below) and report it as the mode below says.";

/// What the daemon knows that goes into a review message.
#[derive(Debug, Clone, Default)]
pub(crate) struct Facts {
    pub environment: String,
    /// The agent this review is for.
    pub agent: String,
    pub folders: Vec<String>,
    /// "today 14:05, you have been idle for 25 minutes", for the agent's sense of time.
    pub when: String,
    /// One line per recent session of the environment.
    pub sessions: Vec<String>,
    /// Titles of suggestions waiting for the user, and of ones handled lately.
    pub open: Vec<String>,
    pub closed: Vec<String>,
    /// The last digest's summary, so a reminder does not repeat itself word for word.
    pub last_digest: Option<String>,
    pub running: u32,
    pub max_concurrent: u32,
    pub started_today: u32,
    pub max_per_day: u32,
    pub max_suggestions: u32,
}

/// What this review does.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Tasks {
    pub review: bool,
    pub digest: bool,
}

fn fill(template: &str, values: &[(&str, String)]) -> String {
    let mut text = template.to_string();
    for (key, value) in values {
        text = text.replace(&format!("{{{{{key}}}}}"), value);
    }
    text
}

fn list(items: &[String], none: &str) -> String {
    if items.is_empty() {
        return format!("  {none}");
    }
    items
        .iter()
        .map(|item| format!("  - {item}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn facts_block(facts: &Facts, mode: Initiative) -> String {
    let mut lines = vec![
        format!("- Folders: {}", facts.folders.join(", ")),
        "- Recent sessions in this environment (status, last activity):".to_string(),
        list(&facts.sessions, "none"),
        "- Already suggested, waiting for the user:".to_string(),
        list(&facts.open, "nothing"),
        "- Handled or dismissed lately (do not suggest these again):".to_string(),
        list(&facts.closed, "nothing"),
    ];
    if let Some(summary) = &facts.last_digest {
        lines.push(format!("- Your last work-left reminder said: {summary}"));
    }
    if mode == Initiative::Act {
        lines.push(format!(
            "- Sessions you started today: {} of {}; running now: {} of {}.",
            facts.started_today, facts.max_per_day, facts.running, facts.max_concurrent
        ));
    }
    lines.join("\n")
}

/// The review message for `mode` (`suggest` or `act`; `off` never reviews).
pub(crate) fn render(facts: &Facts, mode: Initiative, tasks: Tasks) -> String {
    let mut task_lines = Vec::new();
    if tasks.review {
        task_lines.push(REVIEW_TASK.to_string());
    }
    if tasks.digest {
        task_lines.push(format!("- Work left: {}", DIGEST.trim()));
    }
    let rules = match mode {
        Initiative::Act => ACT,
        _ => SUGGEST,
    };
    let sessions_left = facts
        .max_per_day
        .saturating_sub(facts.started_today)
        .min(facts.max_concurrent.saturating_sub(facts.running));
    let rules = fill(
        rules,
        &[
            ("max_suggestions", facts.max_suggestions.to_string()),
            ("sessions_left", sessions_left.to_string()),
            ("running", facts.running.to_string()),
            ("max_concurrent", facts.max_concurrent.to_string()),
        ],
    );
    fill(
        REVIEW,
        &[
            ("environment", facts.environment.clone()),
            ("agent", facts.agent.clone()),
            ("when", facts.when.clone()),
            ("tasks", task_lines.join("\n")),
            ("mode_rules", rules.trim_end().to_string()),
            ("facts", facts_block(facts, mode)),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts() -> Facts {
        Facts {
            environment: "Imperium".into(),
            agent: "Planner".into(),
            folders: vec!["D:/code/imperium".into()],
            when: "Tuesday 14:05".into(),
            sessions: vec!["\"Fix the timeline\" (idle, 3 h ago)".into()],
            open: vec!["Tidy the roadmap".into()],
            closed: vec![],
            last_digest: Some("3 items left".into()),
            running: 0,
            max_concurrent: 1,
            started_today: 1,
            max_per_day: 2,
            max_suggestions: 5,
        }
    }

    #[test]
    fn a_review_fills_every_placeholder_and_cites_its_rules() {
        for mode in [Initiative::Suggest, Initiative::Act] {
            let text = render(
                &facts(),
                mode,
                Tasks {
                    review: true,
                    digest: true,
                },
            );
            assert!(!text.contains("{{"), "{text}");
            assert!(text.starts_with(MARKER), "{text}");
            assert!(text.contains("Never invent a task"), "{text}");
            assert!(text.contains("the way the user would type it"), "{text}");
            assert!(text.contains("Tidy the roadmap"), "{text}");
            assert!(text.contains("work_digest"), "{text}");
        }
    }

    #[test]
    fn modes_differ_in_what_the_agent_may_do() {
        let tasks = Tasks {
            review: true,
            digest: false,
        };
        let suggest = render(&facts(), Initiative::Suggest, tasks);
        assert!(suggest.contains("You cannot start sessions"), "{suggest}");
        assert!(!suggest.contains("work_digest"));
        assert!(!suggest.contains("Sessions you started today"));
        let act = render(&facts(), Initiative::Act, tasks);
        assert!(act.contains("call start_session"), "{act}");
        assert!(act.contains("start at most 1 in this review"), "{act}");
        assert!(act.contains("Sessions you started today: 1 of 2"), "{act}");
    }
}
