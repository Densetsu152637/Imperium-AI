//! A local model's card from its own metadata: the GGUF header's `general.*` fields, a vision
//! projector beside it, and the Hugging Face model card (`README.md` with front matter) in its
//! folder when one was downloaded with it. Agents read it, with the user's own card, when they
//! pick a model to delegate to (`list_models`); nothing here loads the model.

use std::io::Read;
use std::path::Path;

use crate::scan::{Format, LocalModel};

/// Most bytes of a `README.md` read for its front matter and first paragraph.
const README_BYTES: u64 = 64 * 1024;
/// Longest description or summary kept, in characters.
const SUMMARY_CHARS: usize = 400;

/// What a model is good at, guessed from the words of its name, tags and pipeline: each entry
/// is a strength and the words that suggest it.
const STRENGTHS: [(&str, &[&str]); 8] = [
    (
        "coding",
        &[
            "coder",
            "code",
            "codestral",
            "starcoder",
            "starcoder2",
            "devstral",
            "codegemma",
            "codellama",
            "programming",
        ],
    ),
    ("math", &["math", "mathstral"]),
    (
        "step-by-step reasoning",
        &["r1", "thinking", "reasoning", "qwq", "think"],
    ),
    (
        "reading images",
        &[
            "vl",
            "vision",
            "llava",
            "pixtral",
            "multimodal",
            "image-text-to-text",
        ],
    ),
    (
        "tool calling",
        &["tool", "tools", "function-calling", "hermes", "xlam"],
    ),
    (
        "creative writing",
        &["writer", "writing", "creative", "story", "roleplay"],
    ),
    (
        "following instructions and chat",
        &["instruct", "chat", "it", "conversational"],
    ),
    (
        "text embeddings (not chat)",
        &["embed", "embedding", "feature-extraction"],
    ),
];

/// The model's card from its metadata, as a few short lines; empty when nothing is known
/// beyond the id.
pub fn metadata_card(model: &LocalModel) -> String {
    let info = &model.info;
    let about = &info.about;
    let readme = readme_of(model);
    let mut lines = Vec::new();

    let mut identity: Vec<String> = Vec::new();
    if let Some(name) = info.name.as_ref().or(about.basename.as_ref()) {
        identity.push(name.clone());
    }
    if let Some(size) = &about.size_label {
        identity.push(format!("{size} parameters"));
    }
    if let Some(finetune) = &about.finetune {
        identity.push(format!("{finetune} fine-tune"));
    }
    if let Some(repo) = &model.repo {
        identity.push(format!("from {repo}"));
    }
    if !identity.is_empty() {
        lines.push(format!("{}.", identity.join(", ")));
    }

    let mut build = Vec::new();
    if let Some(arch) = &info.architecture {
        build.push(format!("architecture {arch}"));
    }
    if let Some(quant) = info.quantization() {
        build.push(format!("quantisation {quant}"));
    }
    if let Some(context) = info.context_length {
        build.push(format!("trained for {context} tokens of context"));
    }
    if !build.is_empty() {
        lines.push(format!("{}.", capitalise(&build.join("; "))));
    }

    // Words that suggest strengths: the id, names, tags and the model card's pipeline.
    let mut words: Vec<String> = Vec::new();
    let mut add_words = |text: &str| {
        words.extend(
            text.split(|c: char| !c.is_alphanumeric() && c != '-')
                .flat_map(|part| {
                    let part = part.to_lowercase();
                    let mut out: Vec<String> = part.split('-').map(str::to_string).collect();
                    out.push(part);
                    out
                })
                .filter(|word| !word.is_empty()),
        );
    };
    add_words(&model.id);
    for text in [&info.name, &about.basename, &about.finetune, &model.repo]
        .into_iter()
        .flatten()
    {
        add_words(text);
    }
    for tag in about.tags.iter().chain(&readme.tags) {
        add_words(tag);
    }
    if let Some(pipeline) = &readme.pipeline {
        add_words(pipeline);
    }
    let mut strengths: Vec<&str> = STRENGTHS
        .iter()
        .filter(|(_, cues)| cues.iter().any(|cue| words.iter().any(|w| w == cue)))
        .map(|(strength, _)| *strength)
        .collect();
    if model.mmproj.is_some() && !strengths.contains(&"reading images") {
        strengths.push("reading images");
    }
    if !strengths.is_empty() {
        lines.push(format!(
            "Likely good at (from its name and metadata): {}.",
            strengths.join(", ")
        ));
    }
    if model.mmproj.is_some() {
        lines.push("Reads images: a vision projector (mmproj) is beside it.".into());
    }
    if model.format == Format::Safetensors {
        lines.push("A Hugging Face folder, served by vLLM.".into());
    }

    let tags: Vec<&str> = about
        .tags
        .iter()
        .chain(&readme.tags)
        .map(String::as_str)
        .fold(Vec::new(), |mut seen, tag| {
            if !seen.contains(&tag) {
                seen.push(tag);
            }
            seen
        });
    if !tags.is_empty() {
        lines.push(format!("Tags: {}.", tags.join(", ")));
    }
    let languages: Vec<&str> = if about.languages.is_empty() {
        readme.languages.iter().map(String::as_str).collect()
    } else {
        about.languages.iter().map(String::as_str).collect()
    };
    if !languages.is_empty() {
        lines.push(format!("Languages: {}.", languages.join(", ")));
    }
    if let Some(base) = &readme.base_model {
        lines.push(format!("Base model: {base}."));
    }
    if let Some(license) = about.license.as_ref().or(readme.license.as_ref()) {
        lines.push(format!("License: {license}."));
    }
    if let Some(description) = about.description.as_ref().or(readme.summary.as_ref()) {
        lines.push(clip(description));
    }
    lines.join("\n")
}

fn capitalise(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

/// `text` on one line, cut at [`SUMMARY_CHARS`] characters.
fn clip(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= SUMMARY_CHARS {
        return flat;
    }
    let cut: String = flat.chars().take(SUMMARY_CHARS).collect();
    format!("{}…", cut.trim_end())
}

/// What a Hugging Face model card says.
#[derive(Debug, Default, PartialEq, Eq)]
struct Readme {
    tags: Vec<String>,
    languages: Vec<String>,
    pipeline: Option<String>,
    base_model: Option<String>,
    license: Option<String>,
    /// The first paragraph of prose after the front matter.
    summary: Option<String>,
}

/// The model card in the model's folder, when it is a Hugging Face one (a `README.md` that
/// starts with front matter); other READMEs say nothing about this model in particular.
fn readme_of(model: &LocalModel) -> Readme {
    let dir = if model.path.is_dir() {
        Some(model.path.as_path())
    } else {
        model.path.parent()
    };
    dir.and_then(|dir| read_prefix(&dir.join("README.md")))
        .map(|text| parse_readme(&text))
        .unwrap_or_default()
}

fn read_prefix(path: &Path) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.take(README_BYTES).read_to_end(&mut bytes).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// Reads the front matter's top-level keys (a value on the key's line, or a list of `- item`
/// lines under it) and the first paragraph of prose after it.
fn parse_readme(text: &str) -> Readme {
    let mut readme = Readme::default();
    let text = text.trim_start_matches('\u{feff}');
    let Some(rest) = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))
    else {
        return readme;
    };
    let Some(end) = rest.find("\n---") else {
        return readme;
    };
    let (front, body) = (&rest[..end], &rest[end + 4..]);
    let mut key = String::new();
    for line in front.lines() {
        let unquote = |value: &str| value.trim().trim_matches(['"', '\'']).to_string();
        if let Some(item) = line.trim_start().strip_prefix("- ") {
            let item = unquote(item);
            if item.is_empty() {
                continue;
            }
            match key.as_str() {
                "tags" => readme.tags.push(item),
                "language" => readme.languages.push(item),
                "base_model" if readme.base_model.is_none() => readme.base_model = Some(item),
                _ => {}
            }
            continue;
        }
        if line.starts_with([' ', '\t']) {
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        key = name.trim().to_string();
        let value = unquote(value);
        if value.is_empty() {
            continue;
        }
        let list = |value: &str| -> Vec<String> {
            value
                .trim_matches(['[', ']'])
                .split(',')
                .map(unquote)
                .filter(|item| !item.is_empty())
                .collect()
        };
        match key.as_str() {
            "pipeline_tag" => readme.pipeline = Some(value),
            "license" => readme.license = Some(value),
            "base_model" => readme.base_model = list(&value).into_iter().next(),
            "tags" => readme.tags = list(&value),
            "language" => readme.languages = list(&value),
            _ => {}
        }
    }
    readme.summary = body
        .split("\n\n")
        .map(str::trim)
        .find(|paragraph| {
            paragraph.chars().next().is_some_and(char::is_alphabetic)
                && !paragraph.starts_with(['#', '<', '!', '[', '|'])
        })
        .map(clip);
    readme
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gguf::{About, GgufInfo};
    use std::path::PathBuf;

    fn model(id: &str, path: PathBuf) -> LocalModel {
        LocalModel {
            id: id.into(),
            path,
            parts: 1,
            size_bytes: 1,
            info: GgufInfo::default(),
            mmproj: None,
            format: Format::Gguf,
            repo: None,
        }
    }

    #[test]
    fn the_card_says_what_the_header_and_name_suggest() {
        let mut coder = model(
            "Qwen3-Coder-30B-A3B-Instruct-Q4_K_M",
            PathBuf::from("/nowhere/x.gguf"),
        );
        coder.info = GgufInfo {
            architecture: Some("qwen3moe".into()),
            name: Some("Qwen3 Coder 30B A3B Instruct".into()),
            context_length: Some(262_144),
            file_type: Some(15),
            about: About {
                size_label: Some("30B-A3B".into()),
                tags: vec!["text-generation".into()],
                license: Some("apache-2.0".into()),
                ..About::default()
            },
            ..GgufInfo::default()
        };
        coder.repo = Some("unsloth/Qwen3-Coder-30B-A3B-Instruct-GGUF".into());
        let card = metadata_card(&coder);
        assert!(
            card.starts_with("Qwen3 Coder 30B A3B Instruct, 30B-A3B parameters"),
            "{card}"
        );
        assert!(card.contains("Architecture qwen3moe"), "{card}");
        assert!(card.contains("trained for 262144 tokens"), "{card}");
        assert!(
            card.contains("Likely good at (from its name and metadata): coding, following instructions and chat."),
            "{card}"
        );
        assert!(card.contains("License: apache-2.0."), "{card}");

        let mut eyes = model("gemma-3-4b-it", PathBuf::from("/nowhere/g.gguf"));
        eyes.mmproj = Some(PathBuf::from("/nowhere/mmproj.gguf"));
        let card = metadata_card(&eyes);
        assert!(card.contains("reading images"), "{card}");
        assert!(card.contains("vision projector"), "{card}");

        assert_eq!(
            metadata_card(&model("x", PathBuf::from("/nowhere/x.gguf"))),
            ""
        );
    }

    #[test]
    fn a_hugging_face_model_card_beside_the_model_is_read() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("README.md"),
            "---\nlicense: mit\npipeline_tag: text-generation\nbase_model:\n- org/Base-7B\ntags:\n- writing\n- \"roleplay\"\nlanguage: [en, fr]\n---\n\n# Story Writer\n\n![badge](x)\n\nA model tuned for long-form fiction and essays.\nIt keeps a consistent voice.\n\nMore text.\n",
        )
        .unwrap();
        let writer = model("story-7b-q4", dir.path().join("story-7b-q4.gguf"));
        let card = metadata_card(&writer);
        assert!(card.contains("creative writing"), "{card}");
        assert!(card.contains("Tags: writing, roleplay."), "{card}");
        assert!(card.contains("Languages: en, fr."), "{card}");
        assert!(card.contains("Base model: org/Base-7B."), "{card}");
        assert!(card.contains("License: mit."), "{card}");
        assert!(
            card.contains(
                "A model tuned for long-form fiction and essays. It keeps a consistent voice."
            ),
            "{card}"
        );

        // A README without front matter is not a model card.
        std::fs::write(dir.path().join("README.md"), "# My models\n\nAll sorts.\n").unwrap();
        assert!(!metadata_card(&writer).contains("All sorts"));
    }
}
