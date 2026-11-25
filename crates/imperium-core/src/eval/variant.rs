//! Applying a harness variant to the root agent's requests (see [`EvalVariant`]).

use imperium_api::{Effort, ToolSpec, encoding::hex};
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
    /// Whether stale tool results are stubbed before a compaction summary
    /// (`harness.clear_stale_results`); default on, as in a session.
    pub clear_stale_results: bool,
    /// Most read-only calls of one step that run at the same time
    /// (`harness.parallel_reads`). Unlike a session's, its default is 0 (one after another), so
    /// evaluations run as before unless a run asks for it.
    pub parallel_reads: u32,
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
    "clear_stale_results",
    "parallel_reads",
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
            clear_stale_results: true,
            parallel_reads: 0,
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
                "clear_stale_results" => {
                    parsed.clear_stale_results = value.parse().map_err(|_| bad("true or false"))?;
                }
                "parallel_reads" => {
                    parsed.parallel_reads = value
                        .parse::<u32>()
                        .ok()
                        .filter(|reads| *reads <= 8)
                        .ok_or_else(|| bad("a whole number from 0 to 8"))?;
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
    for loader in ["tool_search", "call_tool"] {
        if variant.tools.rename.contains_key(loader)
            || variant.tools.drop.iter().any(|n| n == loader)
        {
            return Err(format!(
                "tools.rename and tools.drop cannot change {loader}: the loader tools keep their names"
            ));
        }
    }
    if variant.tools.core.iter().any(|name| name.trim().is_empty()) {
        return Err("a tools.core entry is empty".into());
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

/// A tool as `tool_search` returns it: name, description and parameters.
pub(crate) fn tool_entry(spec: &ToolSpec) -> serde_json::Value {
    serde_json::json!({
        "name": spec.name,
        "description": spec.description,
        "parameters": spec.parameters,
    })
}

/// `sha256:` over the deferred tools' full schemas in catalog order.
pub(crate) fn catalog_hash(deferred: &[serde_json::Value]) -> String {
    let bytes = serde_json::to_vec(deferred).unwrap_or_default();
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
        for (key, tool) in [
            ("rename", "call_tool"),
            ("rename", "tool_search"),
            ("drop", "call_tool"),
        ] {
            let mut loader = variant();
            if key == "rename" {
                loader.tools.rename = BTreeMap::from([(tool.to_string(), "x".to_string())]);
            } else {
                loader.tools.drop = vec![tool.to_string()];
            }
            assert!(validate(&loader).is_err(), "{key} {tool}");
        }
        let mut empty = variant();
        empty.tools.core = vec![" ".into()];
        assert!(validate(&empty).is_err());
    }

    #[test]
    fn the_catalog_hash_follows_the_schemas_and_their_order() {
        let one = ToolSpec {
            name: "a".into(),
            description: "A.".into(),
            parameters: serde_json::json!({"type": "object"}),
        };
        let two = ToolSpec {
            name: "b".into(),
            ..one.clone()
        };
        let forward = [tool_entry(&one), tool_entry(&two)];
        let backward = [tool_entry(&two), tool_entry(&one)];
        assert_eq!(catalog_hash(&forward), catalog_hash(&forward));
        assert_ne!(catalog_hash(&forward), catalog_hash(&backward));
        assert!(catalog_hash(&[]).starts_with("sha256:"));
    }
}
