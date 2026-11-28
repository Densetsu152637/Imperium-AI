//! A model's reasoning in `imperium chat`: one line, `thinking… (10 seconds)`, unless
//! `--show-thinking` asks for the text too. The wording matches the desktop app's
//! (`apps/desktop/src/sessions/thinking.ts`): keep the two in step.

use imperium_protocol::session::SessionEvent;

/// A duration in words: `less than a second`, `1 second`, `10 seconds`, `1 minute`,
/// `2 minutes 5 seconds`.
pub fn duration_words(duration_ms: u64) -> String {
    let total = duration_ms / 1000;
    if total < 1 {
        return "less than a second".to_string();
    }
    let unit =
        |count: u64, word: &str| format!("{count} {word}{}", if count == 1 { "" } else { "s" });
    let (minutes, seconds) = (total / 60, total % 60);
    match (minutes, seconds) {
        (0, seconds) => unit(seconds, "second"),
        (minutes, 0) => unit(minutes, "minute"),
        (minutes, seconds) => format!("{} {}", unit(minutes, "minute"), unit(seconds, "second")),
    }
}

/// What the terminal prints for a root agent's thinking event, or nothing. `open` says that a
/// run's text is being printed (`show_text` only): the run's first fragment starts with
/// `thinking… `, and its end puts the one-line summary on a fresh line.
pub fn print_for(show_text: bool, open: &mut bool, event: &SessionEvent) -> Option<String> {
    match event {
        SessionEvent::ThinkingDelta { text } if show_text => {
            let lead = if std::mem::replace(open, true) {
                ""
            } else {
                "\nthinking… "
            };
            Some(format!("{lead}{text}"))
        }
        SessionEvent::ThinkingFinished { duration_ms } => {
            let line = format!("thinking… ({})", duration_words(*duration_ms));
            // The summary starts on a fresh line, after any text that was shown.
            *open = false;
            Some(format!("\n{line}\n"))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_are_worded() {
        for (ms, words) in [
            (0, "less than a second"),
            (999, "less than a second"),
            (1000, "1 second"),
            (1999, "1 second"),
            (2000, "2 seconds"),
            (10_000, "10 seconds"),
            (59_999, "59 seconds"),
            (60_000, "1 minute"),
            (61_000, "1 minute 1 second"),
            (120_000, "2 minutes"),
            (125_000, "2 minutes 5 seconds"),
            (3_600_000, "60 minutes"),
        ] {
            assert_eq!(duration_words(ms), words, "{ms} ms");
        }
    }

    fn delta(text: &str) -> SessionEvent {
        SessionEvent::ThinkingDelta { text: text.into() }
    }

    #[test]
    fn collapsed_it_prints_one_line_and_never_the_text() {
        let mut open = false;
        assert_eq!(print_for(false, &mut open, &delta("secret plan")), None);
        assert_eq!(
            print_for(
                false,
                &mut open,
                &SessionEvent::ThinkingFinished {
                    duration_ms: 10_000
                }
            ),
            Some(String::from("\nthinking… (10 seconds)\n"))
        );
    }

    #[test]
    fn shown_it_prints_the_text_then_the_same_line() {
        let mut open = false;
        let mut out = String::new();
        for event in [
            delta("let me "),
            delta("think"),
            SessionEvent::ThinkingFinished { duration_ms: 1000 },
        ] {
            out.push_str(&print_for(true, &mut open, &event).unwrap_or_default());
        }
        assert_eq!(out, "\nthinking… let me think\nthinking… (1 second)\n");
        assert!(!open);
        // The next run starts its own lead again.
        assert_eq!(
            print_for(true, &mut open, &delta("again")),
            Some("\nthinking… again".into())
        );
    }
}
