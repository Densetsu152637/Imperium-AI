//! The per-call effort policy behind `harness.adaptive_effort` (docs/04-models-and-routing.md,
//! "Per-call effort"): a call that follows a tool result that worked thinks at `low` effort,
//! since the model is carrying out a plan it already made; the first call of a turn and a call
//! after a failure think at the session's effort.

use imperium_api::{ChatMessage, Effort};

/// The effort for the next model call. `session` is the effort the session (or agent) is set
/// to; `after_ok_tool` is whether the call follows a round of tool calls whose last one worked
/// (see [`follows_ok_tool`]). Never raises the effort above the session's.
pub(crate) fn call_effort(adaptive: bool, session: Effort, after_ok_tool: bool) -> Effort {
    if adaptive && after_ok_tool {
        Effort::Low.min(session)
    } else {
        session
    }
}

/// Whether the call about to be made follows a tool result that worked: the conversation ends
/// in a tool result (so it is not a turn's first call) and `last_tool_ok` says the last tool
/// call succeeded.
pub(crate) fn follows_ok_tool(messages: &[ChatMessage], last_tool_ok: bool) -> bool {
    last_tool_ok && matches!(messages.last(), Some(ChatMessage::Tool { .. }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_keeps_the_session_effort_everywhere() {
        for effort in Effort::ALL {
            assert_eq!(call_effort(false, effort, true), effort);
            assert_eq!(call_effort(false, effort, false), effort);
        }
    }

    #[test]
    fn on_drops_to_low_only_after_a_tool_that_worked() {
        for effort in Effort::ALL {
            assert_eq!(call_effort(true, effort, true), Effort::Low);
            assert_eq!(call_effort(true, effort, false), effort);
        }
    }

    #[test]
    fn a_turns_first_call_and_a_call_after_a_failure_are_not_after_an_ok_tool() {
        let tool = ChatMessage::Tool {
            tool_call_id: "1".into(),
            content: "done".into(),
        };
        let user = ChatMessage::user("hi");
        assert!(follows_ok_tool(&[user.clone(), tool.clone()], true));
        assert!(!follows_ok_tool(&[user.clone(), tool], false));
        assert!(!follows_ok_tool(&[user], true));
        assert!(!follows_ok_tool(&[], true));
    }
}
