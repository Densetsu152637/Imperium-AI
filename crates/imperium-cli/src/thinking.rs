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

/// What the terminal prints for a root agent's thinking event, or nothing.
///
/// The first fragment of a run prints `thinking…` at once, so the user sees the model is
/// thinking; `open` says that line is still open. On a terminal the run's end finishes that same
/// line with ` (10 seconds)`. Piped (`tty` false), nothing is printed until the end, which
/// writes the whole `thinking… (10 seconds)` as a line of its own. With `show_text`
/// (`--show-thinking`) the line is followed by the text as it streams, and the end writes the
/// summary line after it.
pub fn print_for(
    show_text: bool,
    tty: bool,
    open: &mut bool,
    event: &SessionEvent,
) -> Option<String> {
    match event {
        SessionEvent::ThinkingDelta { text, .. } => {
            let first = !*open;
            *open = *open || show_text || tty;
            match (show_text, first, tty) {
                (true, true, _) => Some(format!("\nthinking… {text}")),
                (true, false, _) => Some(text.clone()),
                (false, true, true) => Some("\nthinking…".to_string()),
                _ => None,
            }
        }
        SessionEvent::ThinkingFinished { duration_ms } => {
            let words = duration_words(*duration_ms);
            let in_place = std::mem::take(open) && tty && !show_text;
            Some(if in_place {
                format!(" ({words})\n")
            } else {
                format!("\nthinking… ({words})\n")
            })
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
        SessionEvent::ThinkingDelta {
            text: text.into(),
            at_ms: None,
        }
    }

    fn finished(duration_ms: u64) -> SessionEvent {
        SessionEvent::ThinkingFinished { duration_ms }
    }

    /// What a run of events prints, joined.
    fn run(show_text: bool, tty: bool, events: &[SessionEvent]) -> String {
        let mut open = false;
        events
            .iter()
            .filter_map(|event| print_for(show_text, tty, &mut open, event))
            .collect()
    }

    #[test]
    fn on_a_terminal_the_line_opens_at_once_and_ends_in_place() {
        let mut open = false;
        // Printed on the first fragment, before any duration is known; later ones add nothing.
        assert_eq!(
            print_for(false, true, &mut open, &delta("secret plan")),
            Some("\nthinking…".to_string())
        );
        assert_eq!(print_for(false, true, &mut open, &delta("more")), None);
        assert_eq!(
            print_for(false, true, &mut open, &finished(10_000)),
            Some(" (10 seconds)\n".to_string())
        );
        assert!(!open);
        assert_eq!(
            run(false, true, &[delta("secret plan"), finished(1000)]),
            "\nthinking… (1 second)\n"
        );
    }

    #[test]
    fn piped_it_prints_one_whole_line_at_the_end() {
        let mut open = false;
        assert_eq!(print_for(false, false, &mut open, &delta("secret")), None);
        assert_eq!(
            run(false, false, &[delta("secret"), finished(10_000)]),
            "\nthinking… (10 seconds)\n"
        );
    }

    #[test]
    fn collapsed_it_never_prints_the_text() {
        for tty in [true, false] {
            let out = run(false, tty, &[delta("secret plan"), finished(5000)]);
            assert!(!out.contains("secret"), "{out}");
        }
    }

    #[test]
    fn shown_it_prints_the_text_then_the_summary_line() {
        for tty in [true, false] {
            assert_eq!(
                run(
                    true,
                    tty,
                    &[delta("let me "), delta("think"), finished(1000)]
                ),
                "\nthinking… let me think\nthinking… (1 second)\n"
            );
        }
        // The next run starts its own line again.
        let mut open = false;
        print_for(true, true, &mut open, &delta("a"));
        print_for(true, true, &mut open, &finished(1));
        assert_eq!(
            print_for(true, true, &mut open, &delta("again")),
            Some("\nthinking… again".into())
        );
    }

    #[test]
    fn a_finish_without_a_start_is_still_one_line() {
        assert_eq!(
            run(false, true, &[finished(2000)]),
            "\nthinking… (2 seconds)\n"
        );
    }
}
