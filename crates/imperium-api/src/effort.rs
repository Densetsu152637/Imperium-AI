//! Reasoning effort: how hard a model that can reason thinks before it answers. Sessions carry
//! one level; each model client maps it to its backend's own control (see
//! `imperium_models::ReasoningStyle`), and models that cannot reason are sent nothing.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// How hard a reasoning model thinks, from least to most.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    Low,
    #[default]
    Medium,
    High,
    Max,
}

impl Effort {
    /// Every level, lowest first.
    pub const ALL: [Effort; 4] = [Effort::Low, Effort::Medium, Effort::High, Effort::Max];

    pub fn as_str(self) -> &'static str {
        match self {
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
            Effort::Max => "max",
        }
    }
}

impl fmt::Display for Effort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An effort level that is not `low`, `medium`, `high` or `max`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownEffort(pub String);

impl fmt::Display for UnknownEffort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown effort {:?}; use low, medium, high or max",
            self.0
        )
    }
}

impl std::error::Error for UnknownEffort {}

impl FromStr for Effort {
    type Err = UnknownEffort;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text.trim().to_ascii_lowercase().as_str() {
            "low" => Ok(Effort::Low),
            "medium" | "med" => Ok(Effort::Medium),
            "high" => Ok(Effort::High),
            "max" | "maximum" => Ok(Effort::Max),
            _ => Err(UnknownEffort(text.to_string())),
        }
    }
}

/// Whether a model's tags say it reasons: `reasoning` or `thinking` mark it, `no-reasoning` or
/// `no-thinking` rule it out; `None` when the tags say neither.
pub fn reasoning_from_tags<S: AsRef<str>>(tags: &[S]) -> Option<bool> {
    let has = |names: &[&str]| {
        tags.iter()
            .any(|tag| names.contains(&tag.as_ref().to_ascii_lowercase().as_str()))
    };
    if has(&["no-reasoning", "no-thinking"]) {
        Some(false)
    } else if has(&["reasoning", "thinking"]) {
        Some(true)
    } else {
        None
    }
}

/// Whether a model is known to reason, from its name at the provider (`o3`,
/// `anthropic/claude-sonnet-4.5`, `deepseek-r1`, ...). A guess for models nothing else
/// describes: a model table row's `reasoning` setting or tags, or a local model's chat
/// template, decide first.
pub fn reasoning_by_name(model: &str) -> bool {
    let lower = model.to_ascii_lowercase();
    // Drop a provider or organisation prefix (`openai/`, `Qwen/`) and a GGUF quant suffix.
    let name = lower.rsplit('/').next().unwrap_or(&lower);
    if name.contains("non-thinking") || name.contains("nothink") {
        return false;
    }
    if name.contains("thinking") || name.contains("reason") {
        return true;
    }
    let starts = |prefixes: &[&str]| prefixes.iter().any(|prefix| name.starts_with(prefix));
    // OpenAI's o-series, GPT-5 (but not its chat-only variant) and gpt-oss.
    if starts(&["o1", "o3", "o4", "gpt-oss"]) {
        return true;
    }
    if name.starts_with("gpt-5") {
        return !name.contains("chat");
    }
    // Claude 3.7 and every Claude from 4 on.
    if let Some(rest) = name.strip_prefix("claude-") {
        if rest.starts_with("3-7") || rest.starts_with("3.7") {
            return true;
        }
        if rest.starts_with(['2', '3']) || rest.starts_with("instant") {
            return false;
        }
        // `claude-sonnet-4-5`, `claude-opus-4.1`, `claude-4-opus`, `claude-haiku-5`, ...
        return rest
            .split(['-', '.'])
            .find_map(|part| part.parse::<u32>().ok())
            .is_some_and(|major| major >= 4);
    }
    // Qwen 3 thinks unless it is an `-Instruct` (non-thinking) release; QwQ always does.
    if name.starts_with("qwq") {
        return true;
    }
    if name.starts_with("qwen3") {
        return !name.contains("instruct") && !name.contains("coder");
    }
    starts(&[
        "deepseek-r1",
        "deepseek-reasoner",
        "gemini-2.5",
        "gemini-3",
        "grok-3-mini",
        "grok-4",
        "magistral",
        "glm-4.5",
        "glm-4.6",
        "glm-4.7",
        "glm-5",
        "kimi-k2-thinking",
        "minimax-m",
    ])
}

/// Whether a chat template (Jinja) renders reasoning: it has a thinking switch
/// (`enable_thinking`), think tags, or an effort variable (`reasoning_effort`).
pub fn template_reasons(template: &str) -> bool {
    template.contains("enable_thinking")
        || template.contains("<think>")
        || template.contains("reasoning_effort")
        || template.contains("thinking")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_round_trip_through_text_and_json() {
        for effort in Effort::ALL {
            assert_eq!(effort.as_str().parse::<Effort>().unwrap(), effort);
            let json = serde_json::to_string(&effort).unwrap();
            assert_eq!(json, format!("\"{effort}\""));
        }
        assert_eq!(" HIGH ".parse::<Effort>().unwrap(), Effort::High);
        assert!("extreme".parse::<Effort>().is_err());
        assert_eq!(Effort::default(), Effort::Medium);
        assert!(Effort::Low < Effort::Max);
    }

    #[test]
    fn tags_mark_or_rule_out_reasoning() {
        assert_eq!(reasoning_from_tags(&["code", "Reasoning"]), Some(true));
        assert_eq!(reasoning_from_tags(&["thinking"]), Some(true));
        assert_eq!(
            reasoning_from_tags(&["thinking", "no-reasoning"]),
            Some(false)
        );
        assert_eq!(reasoning_from_tags::<&str>(&[]), None);
    }

    #[test]
    fn known_reasoning_models_are_recognised_by_name() {
        for name in [
            "o3-mini",
            "openai/o4-mini",
            "gpt-5",
            "gpt-5.1-codex",
            "gpt-oss-20b",
            "claude-3-7-sonnet-latest",
            "claude-sonnet-4-5",
            "anthropic/claude-opus-4.1",
            "claude-haiku-4-5-20251001",
            "claude-opus-5-5",
            "deepseek-reasoner",
            "deepseek/deepseek-r1",
            "Qwen3-8B-Q4_K_M",
            "qwq-32b",
            "google/gemini-2.5-pro",
            "Qwen3-30B-A3B-Thinking-2507",
        ] {
            assert!(reasoning_by_name(name), "{name}");
        }
        for name in [
            "gpt-4o",
            "gpt-5-chat-latest",
            "claude-3-5-sonnet-latest",
            "claude-3-haiku",
            "Qwen3-30B-A3B-Instruct-2507",
            "llama-3.1-8b",
            "mistral-small",
            "deepseek-chat",
        ] {
            assert!(!reasoning_by_name(name), "{name}");
        }
    }

    #[test]
    fn templates_with_a_thinking_switch_reason() {
        assert!(template_reasons(
            "{%- if enable_thinking is defined and enable_thinking is false %}"
        ));
        assert!(template_reasons("{{ '<think>\\n' }}"));
        assert!(template_reasons("Reasoning: {{ reasoning_effort }}"));
        assert!(!template_reasons(
            "{% for message in messages %}{{ message.content }}{% endfor %}"
        ));
    }
}
