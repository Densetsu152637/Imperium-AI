//! Harness variants, client side: reading a variant file (or finding a named one), setting its
//! flags and overlay. The daemon checks the result and applies it (see
//! `imperium_protocol::methods::EvalVariant`); this module never changes a prompt itself.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use imperium_protocol::methods::{EvalVariant, PromptEdits, ToolEdits};
use serde::Deserialize;

/// The id of the run with no variant: the engine's own prompt and tools.
pub const BASELINE: &str = "baseline";

/// Where named variants are looked up, under the configuration folder.
pub const VARIANTS_DIR: &str = "harness-variants";

/// A variant file (`<config>/harness-variants/<name>.toml`, or any path).
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
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

/// A flag value as a TOML file writes it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
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

/// No edits: the engine's own prompt and tools.
pub fn baseline() -> EvalVariant {
    EvalVariant {
        id: BASELINE.into(),
        ..EvalVariant::default()
    }
}

/// Reads a variant from a file, or finds the named one under `config_dir`.
/// `spec` is a path when it has a separator or ends in `.toml`, otherwise a name.
pub fn load(spec: &str, config_dir: Option<&Path>) -> anyhow::Result<EvalVariant> {
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
    Ok(EvalVariant {
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

/// Sets the flags given as `key=value` on the command line over the variant's.
pub fn with_flags(mut variant: EvalVariant, pairs: &[String]) -> anyhow::Result<EvalVariant> {
    for pair in pairs {
        let Some((key, value)) = pair.split_once('=') else {
            bail!("--flag {pair:?} must be written key=value");
        };
        let key = key.trim();
        if key.is_empty() {
            bail!("--flag {pair:?} has no key");
        }
        variant
            .flags
            .insert(key.to_string(), value.trim().to_string());
    }
    Ok(variant)
}

/// Appends the overlay text to the system prompt (after the variant's own edits).
pub fn with_overlay(mut variant: EvalVariant, text: String) -> EvalVariant {
    variant.overlay = Some(text);
    if variant.id == BASELINE {
        variant.id = "overlay".into();
    }
    variant
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let mut variant = baseline();
        variant.flags.insert("max_steps".into(), "5".into());
        let variant = with_flags(
            variant,
            &["max_steps=9".to_string(), "effort=high".to_string()],
        )
        .unwrap();
        assert_eq!(variant.flags["max_steps"], "9");
        assert_eq!(variant.flags["effort"], "high");
        assert!(with_flags(baseline(), &["nokey".to_string()]).is_err());
    }

    #[test]
    fn an_overlay_names_a_baseline_run() {
        let variant = with_overlay(baseline(), "Extra.".into());
        assert_eq!(variant.id, "overlay");
        assert_eq!(variant.overlay.as_deref(), Some("Extra."));
    }
}
