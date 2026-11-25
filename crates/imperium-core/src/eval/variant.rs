//! Applying a harness variant to the root agent's requests (see [`EvalVariant`]).

use std::collections::HashMap;

use imperium_api::{ChatMessage, Effort, ToolSpec, encoding::hex};
use imperium_config::HarnessMode;
use imperium_protocol::methods::EvalVariant;
use sha2::{Digest, Sha256};

/// The paragraph separator the base prompt's parts are joined with.
const PARAGRAPH: &str = "\n\n";

/// Text the root prompt's first paragraph carries (it is one paragraph: the identity line, the
/// proverb, the tool rules and the knowledge line), so `prompt.remove` never drops a paragraph
/// that has any of it: use `replace` for a part of that paragraph, or a flag.
const PROTECTED: [&str; 2] = [
    "You are Imperium, a coding agent",
    "The fear of God is the beginning of all knowledge",
];

/// The loop flags a variant (or `--flag`) sets, with the defaults of a headless run.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LoopFlags {
    pub max_steps: u32,
    pub effort: Option<Effort>,
    pub auto_compact: bool,
    pub auto_compact_at: Option<f64>,
    pub knowledge: bool,
    pub attended: bool,
    pub harness: HarnessMode,
    /// Whether the tool list is the small fixed one with `tool_search` and `call_tool`
    /// (`harness.tool_loading`; docs/plans/reactive-tools.md). Unlike `harness`, its default
    /// is `off`, so evaluations run as before unless a run asks for it.
    pub tool_loading: HarnessMode,
    /// Whether a tool result too large for the conversation is saved to a file
    /// (`harness.result_spill`). Unlike the setting, its default is `false`, so evaluations
    /// stay comparable with earlier baselines unless a run asks for it.
    pub result_spill: bool,
    /// Whether stale tool results are stubbed before a compaction summary
    /// (`harness.clear_stale_results`); default on, as in a session.
    pub clear_stale_results: bool,
}

/// The flags and what they do, for error messages.
const FLAG_NAMES: [&str; 10] = [
    "max_steps",
    "effort",
    "auto_compact",
    "auto_compact_at",
    "knowledge",
    "attended",
    "harness",
    "tool_loading",
    "result_spill",
    "clear_stale_results",
];

impl LoopFlags {
    pub(crate) fn parse(
        flags: &std::collections::BTreeMap<String, String>,
    ) -> Result<Self, String> {
        let mut parsed = LoopFlags {
            max_steps: 50,
            effort: None,
            auto_compact: false,
            auto_compact_at: None,
            knowledge: true,
            attended: false,
            harness: HarnessMode::Auto,
            tool_loading: HarnessMode::Off,
            result_spill: false,
            clear_stale_results: true,
        };
        for (key, value) in flags {
            let bad = |what: &str| format!("flag {key}={value}: expected {what}");
            match key.as_str() {
                "max_steps" => {
                    parsed.max_steps = value
                        .parse::<u32>()
                        .ok()
                        .filter(|steps| *steps > 0)
                        .ok_or_else(|| bad("a whole number above 0"))?;
                }
                "effort" => {
                    parsed.effort = Some(
                        value
                            .parse::<Effort>()
                            .map_err(|_| bad("low, medium, high or max"))?,
                    );
                }
                "auto_compact" => {
                    parsed.auto_compact = value.parse().map_err(|_| bad("true or false"))?;
                }
                "auto_compact_at" => {
                    let share: f64 = value.parse().map_err(|_| bad("a number from 0 to 1"))?;
                    if !(0.0..=1.0).contains(&share) {
                        return Err(bad("a number from 0 to 1"));
                    }
                    parsed.auto_compact_at = Some(share);
                }
                "knowledge" => {
                    parsed.knowledge = value.parse().map_err(|_| bad("true or false"))?;
                }
                "attended" => {
                    parsed.attended = value.parse().map_err(|_| bad("true or false"))?;
                }
                "harness" => {
                    parsed.harness = match value.as_str() {
                        "on" => HarnessMode::On,
                        "off" => HarnessMode::Off,
                        "auto" => HarnessMode::Auto,
                        _ => return Err(bad("on, off or auto")),
                    };
                }
                "tool_loading" => {
                    parsed.tool_loading = match value.as_str() {
                        "on" => HarnessMode::On,
                        "off" => HarnessMode::Off,
                        "auto" => HarnessMode::Auto,
                        _ => return Err(bad("on, off or auto")),
                    };
                }
                "result_spill" => {
                    parsed.result_spill = value.parse().map_err(|_| bad("true or false"))?;
                }
                "clear_stale_results" => {
                    parsed.clear_stale_results = value.parse().map_err(|_| bad("true or false"))?;
                }
                other => {
                    return Err(format!(
                        "unknown flag {other:?}; the flags are {}",
                        FLAG_NAMES.join(", ")
                    ));
                }
            }
        }
        Ok(parsed)
    }
}

/// Checks what can be checked without the engine's prompt: the flags, and that no two renames
/// give the same new name.
pub(crate) fn validate(variant: &EvalVariant) -> Result<LoopFlags, String> {
    let flags = LoopFlags::parse(&variant.flags)?;
    let mut targets: Vec<&String> = variant.tools.rename.values().collect();
    targets.sort();
    if targets.iter().any(|target| target.trim().is_empty()) {
        return Err("a tools.rename target is empty".into());
    }
    if targets.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err("two tools.rename entries give the same new name".into());
    }
    Ok(flags)
}

/// SHA-256 over the edits, flags and overlay (not the id, description or file name), so two
/// variants that change the same things hash alike.
pub(crate) fn config_hash(variant: &EvalVariant) -> String {
    let value = serde_json::json!({
        "prompt": variant.prompt,
        "tools": variant.tools,
        "flags": variant.flags,
        "overlay": variant.overlay,
    });
    let bytes = serde_json::to_vec(&value).unwrap_or_default();
    format!("sha256:{}", hex(&Sha256::digest(&bytes)))
}

/// `sha256:` over the system prompt and the tools, as a prompt's identity.
pub(crate) fn prompt_hash(system_prompt: &str, tools: &[serde_json::Value]) -> String {
    let value = serde_json::json!({"system_prompt": system_prompt, "tools": tools});
    let bytes = serde_json::to_vec(&value).unwrap_or_default();
    format!("sha256:{}", hex(&Sha256::digest(&bytes)))
}

/// The numbers a variant's text may mention.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Placeholders {
    pub context_tokens: Option<u64>,
}

impl Placeholders {
    /// `text` with `{context_tokens}` (such as `32768`) and `{context_k}` (`32`) filled in; left
    /// as they are when no context size was given.
    pub(crate) fn fill(&self, text: &str) -> String {
        match self.context_tokens {
            Some(tokens) => text
                .replace("{context_tokens}", &tokens.to_string())
                .replace("{context_k}", &(tokens / 1024).to_string()),
            None => text.to_string(),
        }
    }
}

/// The system prompt after the variant's edits. An edit that finds nothing to change adds a line
/// to `warnings` (the run goes on, so a misspelt edit is visible but never silent).
pub(crate) fn edit_prompt(
    variant: &EvalVariant,
    system: &str,
    placeholders: Placeholders,
    warnings: &mut Vec<String>,
) -> String {
    let mut text = system.to_string();
    for replace in &variant.prompt.replace {
        if replace.find.is_empty() {
            warnings.push("a prompt.replace entry has an empty `find`".into());
        } else if text.contains(&replace.find) {
            text = text.replacen(&replace.find, &placeholders.fill(&replace.with), 1);
        } else {
            warnings.push(format!(
                "prompt.replace: {:?} is not in the system prompt",
                clip(&replace.find)
            ));
        }
    }
    for needle in &variant.prompt.remove {
        if needle.is_empty() {
            warnings.push("a prompt.remove entry is empty".into());
            continue;
        }
        let paragraphs: Vec<&str> = text.split(PARAGRAPH).collect();
        let mut kept: Vec<&str> = Vec::new();
        let mut matched = 0;
        for paragraph in paragraphs.iter().copied() {
            if !paragraph.contains(needle.as_str()) {
                kept.push(paragraph);
                continue;
            }
            matched += 1;
            if PROTECTED.iter().any(|marker| paragraph.contains(marker)) {
                warnings.push(format!(
                    "prompt.remove: {:?} is in the base paragraph (the identity line and the \
                     proverb), which is never dropped; use prompt.replace for part of it, or a \
                     flag such as knowledge=false",
                    clip(needle)
                ));
                kept.push(paragraph);
            }
        }
        if matched == 0 {
            warnings.push(format!(
                "prompt.remove: no paragraph of the system prompt contains {:?}",
                clip(needle)
            ));
        } else {
            text = kept.join(PARAGRAPH);
        }
    }
    if let Some(prepend) = &variant.prompt.prepend {
        text = format!("{}{PARAGRAPH}{text}", placeholders.fill(prepend));
    }
    if let Some(append) = &variant.prompt.append {
        text = format!("{text}{PARAGRAPH}{}", placeholders.fill(append));
    }
    if let Some(overlay) = &variant.overlay {
        text = format!("{text}{PARAGRAPH}{}", placeholders.fill(overlay));
    }
    text
}

/// The tools the model is offered after the variant's edits, and the names to translate back
/// (offered name to the engine's). Names that match no tool add a line to `warnings`.
pub(crate) fn edit_tools(
    variant: &EvalVariant,
    tools: &[ToolSpec],
    warnings: &mut Vec<String>,
) -> (Vec<ToolSpec>, HashMap<String, String>) {
    let known = |name: &str| tools.iter().any(|tool| tool.name == name);
    for name in &variant.tools.drop {
        if !known(name) {
            warnings.push(format!("tools.drop: no tool named {name:?} is offered"));
        }
    }
    for name in variant.tools.rename.keys() {
        if !known(name) {
            warnings.push(format!("tools.rename: no tool named {name:?} is offered"));
        }
    }
    for name in variant.tools.description.keys() {
        if !known(name) {
            warnings.push(format!(
                "tools.description: no tool named {name:?} is offered"
            ));
        }
    }
    let mut back = HashMap::new();
    let mut edited = Vec::with_capacity(tools.len());
    // Names a rename may not take: those of the tools that stay as they are.
    let unchanged: Vec<&str> = tools
        .iter()
        .filter(|tool| {
            !variant.tools.drop.contains(&tool.name)
                && !variant.tools.rename.contains_key(&tool.name)
        })
        .map(|tool| tool.name.as_str())
        .collect();
    for tool in tools {
        if variant.tools.drop.contains(&tool.name) {
            continue;
        }
        let mut tool = tool.clone();
        if let Some(text) = variant.tools.description.get(&tool.name) {
            tool.description.clone_from(text);
        }
        if let Some(new) = variant.tools.rename.get(&tool.name) {
            if unchanged.contains(&new.as_str()) || back.contains_key(new) {
                warnings.push(format!(
                    "tools.rename: {:?} cannot become {new:?}, which another tool already has; \
                     the rename is skipped",
                    tool.name
                ));
            } else {
                back.insert(new.clone(), tool.name.clone());
                tool.name.clone_from(new);
            }
        }
        edited.push(tool);
    }
    (edited, back)
}

/// The conversation as the model sees it: earlier tool calls carry the renamed tools' new names
/// (`back` maps new names to the engine's, from [`edit_tools`]), so a renamed tool is called by
/// one name throughout.
pub(crate) fn rename_calls(back: &HashMap<String, String>, messages: &mut [ChatMessage]) {
    if back.is_empty() {
        return;
    }
    let forward: HashMap<&str, &str> = back
        .iter()
        .map(|(new, original)| (original.as_str(), new.as_str()))
        .collect();
    for message in messages {
        if let ChatMessage::Assistant { tool_calls, .. } = message {
            for call in tool_calls {
                if let Some(new) = forward.get(call.function.name.as_str()) {
                    call.function.name = (*new).to_string();
                }
            }
        }
    }
}

/// A short form of `text` for a warning.
fn clip(text: &str) -> String {
    let mut short: String = text.chars().take(60).collect();
    if text.chars().count() > 60 {
        short.push_str("...");
    }
    short
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use imperium_protocol::methods::Replace;

    use super::*;

    fn variant() -> EvalVariant {
        EvalVariant {
            id: "t".into(),
            ..EvalVariant::default()
        }
    }

    fn tool(name: &str) -> ToolSpec {
        ToolSpec {
            name: name.into(),
            description: format!("{name} does a thing"),
            parameters: serde_json::json!({"type": "object"}),
        }
    }

    #[test]
    fn edits_change_the_prompt_and_the_hash() {
        let baseline = variant();
        let mut edited = variant();
        edited.prompt.append = Some("Budget: {context_k}K tokens.".into());
        edited.prompt.remove = vec!["drop me".into()];
        edited.prompt.replace = vec![Replace {
            find: "one".into(),
            with: "1".into(),
        }];
        let mut warnings = Vec::new();
        let placeholders = Placeholders {
            context_tokens: Some(32_768),
        };
        let text = edit_prompt(
            &edited,
            "first one\n\ndrop me please\n\nlast",
            placeholders,
            &mut warnings,
        );
        assert_eq!(text, "first 1\n\nlast\n\nBudget: 32K tokens.");
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_ne!(config_hash(&baseline), config_hash(&edited));
        assert_eq!(config_hash(&baseline), config_hash(&variant()));
    }

    #[test]
    fn the_base_paragraph_is_never_removed() {
        let mut edited = variant();
        edited.prompt.remove = vec!["knowledge line".into()];
        let mut warnings = Vec::new();
        let system = "You are Imperium, a coding agent. The knowledge line.\n\nOther.";
        let text = edit_prompt(&edited, system, Placeholders::default(), &mut warnings);
        assert_eq!(text, system);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
    }

    #[test]
    fn edits_that_find_nothing_warn() {
        let mut edited = variant();
        edited.prompt.remove = vec!["absent".into()];
        edited.tools.drop = vec!["nope".into()];
        let mut warnings = Vec::new();
        let text = edit_prompt(&edited, "text", Placeholders::default(), &mut warnings);
        let (tools, _) = edit_tools(&edited, &[tool("a")], &mut warnings);
        assert_eq!(text, "text");
        assert_eq!(tools.len(), 1);
        assert_eq!(warnings.len(), 2, "{warnings:?}");
    }

    #[test]
    fn tools_are_dropped_renamed_and_redescribed() {
        let mut edited = variant();
        edited.tools.drop = vec!["b".into()];
        edited.tools.rename = BTreeMap::from([("a".to_string(), "alpha".to_string())]);
        edited.tools.description = BTreeMap::from([("c".to_string(), "new words".to_string())]);
        let mut warnings = Vec::new();
        let (tools, back) = edit_tools(&edited, &[tool("a"), tool("b"), tool("c")], &mut warnings);
        let names: Vec<&str> = tools.iter().map(|tool| tool.name.as_str()).collect();
        assert_eq!(names, ["alpha", "c"]);
        assert_eq!(tools[1].description, "new words");
        assert_eq!(back.get("alpha").map(String::as_str), Some("a"));
        assert!(warnings.is_empty());
    }

    #[test]
    fn a_rename_onto_an_existing_tool_is_skipped() {
        let mut edited = variant();
        edited.tools.rename = BTreeMap::from([("a".to_string(), "b".to_string())]);
        let mut warnings = Vec::new();
        let (tools, back) = edit_tools(&edited, &[tool("a"), tool("b")], &mut warnings);
        assert_eq!(tools.len(), 2);
        assert!(back.is_empty());
        assert_eq!(warnings.len(), 1);
    }

    #[test]
    fn flags_are_checked_and_duplicate_rename_targets_refused() {
        let mut edited = variant();
        edited
            .flags
            .insert("max_steps".to_string(), "12".to_string());
        assert_eq!(validate(&edited).unwrap().max_steps, 12);
        edited.flags.insert("bogus".to_string(), "1".to_string());
        assert!(validate(&edited).is_err());
        let mut twice = variant();
        twice.tools.rename = BTreeMap::from([
            ("a".to_string(), "x".to_string()),
            ("b".to_string(), "x".to_string()),
        ]);
        assert!(validate(&twice).is_err());
    }
}
