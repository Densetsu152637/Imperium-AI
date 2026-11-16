//! Harness variants: a small, explicit set of edits to what the root agent is given, so one
//! change at a time can be measured. A variant changes the root agent's system prompt (add,
//! remove or replace text), the tools it is offered (drop, rename, redescribe) and the loop
//! flags of the run. It never changes how a tool runs, and it never touches subagents.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use imperium_api::encoding::hex;
use imperium_api::{ChatMessage, ToolSpec};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The id of the run with no variant: the engine's own prompt and tools.
pub const BASELINE: &str = "baseline";

/// Where named variants are looked up, under the configuration folder.
pub const VARIANTS_DIR: &str = "harness-variants";

/// The paragraph separator the base prompt's parts are joined with.
const PARAGRAPH: &str = "\n\n";

/// A variant file (`<config>/harness-variants/<name>.toml`, or any path).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VariantFile {
    /// The variant's id; the file name without `.toml` when left out.
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub prompt: PromptEdits,
    #[serde(default)]
    pub tools: ToolEdits,
    /// Loop flags, as `--flag key=value` sets them (the command line wins).
    #[serde(default)]
    pub flags: BTreeMap<String, FlagValue>,
}

/// Edits to the root agent's system prompt, applied in this order: `replace`, `remove`,
/// `prepend`, `append`. The text may use `{context_tokens}` and `{context_k}`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PromptEdits {
    /// Text put before the base prompt, as its own paragraph.
    #[serde(default)]
    pub prepend: Option<String>,
    /// Text put after the base prompt, as its own paragraph.
    #[serde(default)]
    pub append: Option<String>,
    /// Each entry drops every paragraph (text between blank lines) that contains it.
    #[serde(default)]
    pub remove: Vec<String>,
    /// Exact text replaced by other text, first match only.
    #[serde(default)]
    pub replace: Vec<Replace>,
}

/// One exact replacement in the system prompt.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Replace {
    pub find: String,
    pub with: String,
}

/// Edits to the tools the root agent is offered, by the engine's tool names.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ToolEdits {
    /// Tools the model is not offered.
    #[serde(default)]
    pub drop: Vec<String>,
    /// Tools offered under another name (`old = "new"`); the engine still runs them.
    #[serde(default)]
    pub rename: BTreeMap<String, String>,
    /// New descriptions (`name = "text"`).
    #[serde(default)]
    pub description: BTreeMap<String, String>,
}

/// A flag value as a TOML file writes it.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(untagged)]
pub enum FlagValue {
    Bool(bool),
    Int(i64),
    Float(f64),
    Text(String),
}

impl FlagValue {
    fn render(&self) -> String {
        match self {
            FlagValue::Bool(value) => value.to_string(),
            FlagValue::Int(value) => value.to_string(),
            FlagValue::Float(value) => value.to_string(),
            FlagValue::Text(value) => value.clone(),
        }
    }
}

impl Eq for FlagValue {}

/// The numbers a variant's text may mention.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Placeholders {
    pub context_tokens: Option<u64>,
}

impl Placeholders {
    /// `text` with `{context_tokens}` (such as `32768`) and `{context_k}` (`32`) filled in; left
    /// as they are when no context size was given.
    pub fn fill(&self, text: &str) -> String {
        match self.context_tokens {
            Some(tokens) => text
                .replace("{context_tokens}", &tokens.to_string())
                .replace("{context_k}", &(tokens / 1024).to_string()),
            None => text.to_string(),
        }
    }
}

/// A variant with its flags and overlay resolved: what a run applies and what its hash covers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Variant {
    pub id: String,
    pub description: Option<String>,
    /// The file it came from, when it came from one.
    pub source: Option<String>,
    pub prompt: PromptEdits,
    pub tools: ToolEdits,
    /// Loop flags, from the file and then the command line, as text.
    pub flags: BTreeMap<String, String>,
    /// Text appended to the system prompt after the variant's own edits (`--prompt-overlay`).
    pub overlay: Option<String>,
}

impl Variant {
    /// No edits: the engine's own prompt and tools.
    pub fn baseline() -> Self {
        Variant {
            id: BASELINE.into(),
            description: None,
            source: None,
            prompt: PromptEdits::default(),
            tools: ToolEdits::default(),
            flags: BTreeMap::new(),
            overlay: None,
        }
    }

    /// Reads a variant from a file, or finds the named one under `config_dir`.
    /// `spec` is a path when it has a separator or ends in `.toml`, otherwise a name.
    pub fn load(spec: &str, config_dir: Option<&Path>) -> anyhow::Result<Self> {
        let path: PathBuf = if spec.ends_with(".toml") || spec.contains(['/', '\\']) {
            PathBuf::from(spec)
        } else {
            let dir = config_dir.context("no configuration folder to look for variants in")?;
            dir.join(VARIANTS_DIR).join(format!("{spec}.toml"))
        };
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("could not read the variant file {}", path.display()))?;
        let file: VariantFile = toml::from_str(&text)
            .with_context(|| format!("{} is not a valid variant file", path.display()))?;
        let stem = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or(spec)
            .to_string();
        let id = file.name.clone().unwrap_or(stem);
        if id.trim().is_empty() {
            bail!("{}: the variant needs a name", path.display());
        }
        Ok(Variant {
            id,
            description: file.description,
            source: Some(path.display().to_string()),
            prompt: file.prompt,
            tools: file.tools,
            flags: file
                .flags
                .iter()
                .map(|(key, value)| (key.clone(), value.render()))
                .collect(),
            overlay: None,
        })
    }

    /// Sets the flags given as `key=value` on the command line over the file's.
    pub fn with_flags(mut self, pairs: &[String]) -> anyhow::Result<Self> {
        for pair in pairs {
            let Some((key, value)) = pair.split_once('=') else {
                bail!("--flag {pair:?} must be written key=value");
            };
            let key = key.trim();
            if key.is_empty() {
                bail!("--flag {pair:?} has no key");
            }
            self.flags.insert(key.to_string(), value.trim().to_string());
        }
        Ok(self)
    }

    /// Appends the overlay text to the system prompt (after the variant's own edits).
    #[must_use]
    pub fn with_overlay(mut self, text: String) -> Self {
        self.overlay = Some(text);
        if self.id == BASELINE {
            self.id = "overlay".into();
        }
        self
    }

    /// SHA-256 over the edits, flags and overlay (not the id, description or file name), so two
    /// variants that change the same things hash alike.
    pub fn config_hash(&self) -> String {
        let value = serde_json::json!({
            "prompt": self.prompt,
            "tools": self.tools,
            "flags": self.flags,
            "overlay": self.overlay,
        });
        let bytes = serde_json::to_vec(&value).unwrap_or_default();
        format!("sha256:{}", hex(&Sha256::digest(&bytes)))
    }

    /// Whether the variant changes nothing about the prompt or the tools.
    pub fn is_neutral(&self) -> bool {
        self.prompt == PromptEdits::default()
            && self.tools == ToolEdits::default()
            && self.overlay.is_none()
    }

    /// The system prompt after the variant's edits. An edit that finds nothing to change adds a
    /// line to `warnings` (the run goes on, so a misspelt edit is visible but never silent).
    pub fn edit_prompt(
        &self,
        system: &str,
        placeholders: Placeholders,
        warnings: &mut Vec<String>,
    ) -> String {
        let mut text = system.to_string();
        for replace in &self.prompt.replace {
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
        for needle in &self.prompt.remove {
            if needle.is_empty() {
                warnings.push("a prompt.remove entry is empty".into());
                continue;
            }
            let paragraphs: Vec<&str> = text.split(PARAGRAPH).collect();
            let kept: Vec<&str> = paragraphs
                .iter()
                .copied()
                .filter(|paragraph| !paragraph.contains(needle.as_str()))
                .collect();
            if kept.len() == paragraphs.len() {
                warnings.push(format!(
                    "prompt.remove: no paragraph of the system prompt contains {:?}",
                    clip(needle)
                ));
            } else {
                text = kept.join(PARAGRAPH);
            }
        }
        if let Some(prepend) = &self.prompt.prepend {
            text = format!("{}{PARAGRAPH}{text}", placeholders.fill(prepend));
        }
        if let Some(append) = &self.prompt.append {
            text = format!("{text}{PARAGRAPH}{}", placeholders.fill(append));
        }
        if let Some(overlay) = &self.overlay {
            text = format!("{text}{PARAGRAPH}{}", placeholders.fill(overlay));
        }
        text
    }

    /// The tools the model is offered after the variant's edits, and the names to translate back
    /// (offered name to the engine's). Names that match no tool add a line to `warnings`.
    pub fn edit_tools(
        &self,
        tools: &[ToolSpec],
        warnings: &mut Vec<String>,
    ) -> (Vec<ToolSpec>, HashMap<String, String>) {
        let known = |name: &str| tools.iter().any(|tool| tool.name == name);
        for name in &self.tools.drop {
            if !known(name) {
                warnings.push(format!("tools.drop: no tool named {name:?} is offered"));
            }
        }
        for name in self.tools.rename.keys() {
            if !known(name) {
                warnings.push(format!("tools.rename: no tool named {name:?} is offered"));
            }
        }
        for name in self.tools.description.keys() {
            if !known(name) {
                warnings.push(format!(
                    "tools.description: no tool named {name:?} is offered"
                ));
            }
        }
        let mut back = HashMap::new();
        let mut edited = Vec::with_capacity(tools.len());
        for tool in tools {
            if self.tools.drop.contains(&tool.name) {
                continue;
            }
            let mut tool = tool.clone();
            if let Some(text) = self.tools.description.get(&tool.name) {
                tool.description.clone_from(text);
            }
            if let Some(new) = self.tools.rename.get(&tool.name) {
                back.insert(new.clone(), tool.name.clone());
                tool.name.clone_from(new);
            }
            edited.push(tool);
        }
        (edited, back)
    }

    /// The conversation as the model sees it: earlier tool calls carry the renamed tools' new
    /// names, so a renamed tool is called by one name throughout.
    pub fn rename_calls(&self, messages: &mut [ChatMessage]) {
        if self.tools.rename.is_empty() {
            return;
        }
        for message in messages {
            if let ChatMessage::Assistant { tool_calls, .. } = message {
                for call in tool_calls {
                    if let Some(new) = self.tools.rename.get(&call.function.name) {
                        call.function.name.clone_from(new);
                    }
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
    use super::*;

    fn tool(name: &str) -> ToolSpec {
        ToolSpec {
            name: name.into(),
            description: format!("{name} does a thing"),
            parameters: serde_json::json!({"type": "object"}),
        }
    }

    #[test]
    fn edits_change_the_prompt_and_the_hash() {
        let baseline = Variant::baseline();
        let mut variant = Variant::baseline();
        variant.prompt.append = Some("Budget: {context_k}K tokens.".into());
        variant.prompt.remove = vec!["drop me".into()];
        variant.prompt.replace = vec![Replace {
            find: "one".into(),
            with: "1".into(),
        }];
        let mut warnings = Vec::new();
        let placeholders = Placeholders {
            context_tokens: Some(32_768),
        };
        let text = variant.edit_prompt(
            "first one\n\ndrop me please\n\nlast",
            placeholders,
            &mut warnings,
        );
        assert_eq!(text, "first 1\n\nlast\n\nBudget: 32K tokens.");
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_ne!(baseline.config_hash(), variant.config_hash());
        assert_eq!(baseline.config_hash(), Variant::baseline().config_hash());
    }

    #[test]
    fn edits_that_find_nothing_warn() {
        let mut variant = Variant::baseline();
        variant.prompt.remove = vec!["absent".into()];
        variant.tools.drop = vec!["nope".into()];
        let mut warnings = Vec::new();
        let text = variant.edit_prompt("text", Placeholders::default(), &mut warnings);
        let (tools, _) = variant.edit_tools(&[tool("a")], &mut warnings);
        assert_eq!(text, "text");
        assert_eq!(tools.len(), 1);
        assert_eq!(warnings.len(), 2, "{warnings:?}");
    }

    #[test]
    fn tools_are_dropped_renamed_and_redescribed() {
        let mut variant = Variant::baseline();
        variant.tools.drop = vec!["b".into()];
        variant.tools.rename = BTreeMap::from([("a".to_string(), "alpha".to_string())]);
        variant.tools.description = BTreeMap::from([("c".to_string(), "new words".to_string())]);
        let mut warnings = Vec::new();
        let (tools, back) = variant.edit_tools(&[tool("a"), tool("b"), tool("c")], &mut warnings);
        let names: Vec<&str> = tools.iter().map(|tool| tool.name.as_str()).collect();
        assert_eq!(names, ["alpha", "c"]);
        assert_eq!(tools[1].description, "new words");
        assert_eq!(back.get("alpha").map(String::as_str), Some("a"));
        assert!(warnings.is_empty());
    }

    #[test]
    fn a_variant_file_is_read_with_its_flags() {
        let text = r#"
            name = "thin"
            description = "less prompt"
            [prompt]
            append = "Be brief."
            [tools]
            drop = ["web_fetch"]
            [flags]
            max_steps = 12
            auto_compact = true
        "#;
        let file: VariantFile = toml::from_str(text).unwrap();
        assert_eq!(file.name.as_deref(), Some("thin"));
        assert_eq!(file.prompt.append.as_deref(), Some("Be brief."));
        assert_eq!(file.flags["max_steps"].render(), "12");
        assert_eq!(file.flags["auto_compact"].render(), "true");
        assert!(toml::from_str::<VariantFile>("surprise = 1").is_err());
    }

    #[test]
    fn command_line_flags_win_over_the_file() {
        let mut variant = Variant::baseline();
        variant.flags.insert("max_steps".into(), "5".into());
        let variant = variant
            .with_flags(&["max_steps=9".to_string(), "effort=high".to_string()])
            .unwrap();
        assert_eq!(variant.flags["max_steps"], "9");
        assert_eq!(variant.flags["effort"], "high");
        assert!(
            Variant::baseline()
                .with_flags(&["nokey".to_string()])
                .is_err()
        );
    }
}
