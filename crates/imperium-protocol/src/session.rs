//! Session events, pushed to subscribed clients as `session.event` notifications.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Notification method carrying [`SessionEventParams`].
pub const SESSION_EVENT: &str = "session.event";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionEventParams {
    pub session_id: String,
    pub event: SessionEvent,
}

/// Something that happened in a session, in order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export)]
pub enum SessionEvent {
    /// The agent started working on a user message.
    TurnStarted { text: String },
    /// A fragment of the agent's reply.
    TextDelta { text: String },
    /// A tool call is about to run (after any approval).
    ToolStarted {
        call_id: String,
        tool: String,
        summary: String,
    },
    /// A tool call needs the user's approval; answer with `session.respond`.
    PermissionRequested {
        #[ts(type = "number")]
        request_id: u64,
        tool: String,
        summary: String,
    },
    /// A tool call finished, failed, or was denied. `output` is truncated for display.
    ToolFinished {
        call_id: String,
        ok: bool,
        output: String,
    },
    /// One model call finished; emitted after every call, so a turn can have several.
    ModelCallCompleted {
        model: String,
        usage: TokenUsage,
        #[ts(type = "number")]
        duration_ms: u64,
        /// Time to the first streamed text; absent when the call produced no text.
        #[ts(type = "number | null")]
        time_to_first_token_ms: Option<u64>,
        /// Completion tokens per second after the first token (or over the whole call).
        tokens_per_sec: Option<f64>,
        cost_usd: Option<f64>,
    },
    /// The agent finished the turn.
    TurnCompleted {
        usage: TokenUsage,
        /// Absent when the model has no configured prices.
        cost_usd: Option<f64>,
        /// Model calls made in this turn.
        steps: u32,
    },
    /// The turn stopped because of an error.
    TurnFailed { message: String },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TokenUsage {
    #[ts(type = "number")]
    pub prompt_tokens: u64,
    #[ts(type = "number")]
    pub completion_tokens: u64,
    #[ts(type = "number")]
    pub cached_tokens: u64,
}

/// The user's answer to a permission request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum PermissionDecision {
    Allow,
    /// Allow this tool for the rest of the session.
    AllowForSession,
    Deny,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn events_are_tagged_by_type() {
        let event = SessionEvent::PermissionRequested {
            request_id: 3,
            tool: "write_file".into(),
            summary: "write a.txt".into(),
        };
        assert_eq!(
            serde_json::to_value(&event).unwrap(),
            json!({"type":"permission_requested","request_id":3,"tool":"write_file","summary":"write a.txt"})
        );
        assert_eq!(
            serde_json::to_value(PermissionDecision::AllowForSession).unwrap(),
            json!("allow_for_session")
        );
    }
}
